//! Quota auto-ping (9router `quotaAutoPing` parity).
//!
//! Keeps the settings contract (`claudeAutoPing` / `codexAutoPing` in settings
//! extra) and runs a 60s tick (dashboard POST + background spawn from `main`).
//!
//! On each tick, for enabled OAuth Claude/Codex connections:
//! 1. Optionally refresh credentials when a refresh token is present
//! 2. Fetch live quota and decide whether a warm ping is due
//! 3. Send a minimal synthetic request (Claude messages / Codex responses)
//! 4. Persist `lastPingedResetAt` / `lastPingedResetKey` / `lastPingAt`
//!
//! # Cooldowns (from 9router `QUOTA_AUTOPING_CONFIG`)
//! - `pingLeadMs` 5s — Claude fires once reset is within lead
//! - `refreshAheadMs` 5min — skip usage refetch far from reset (Claude only)
//! - `failureCooldownMs` 15min — avoid spam after refresh/ping failure
//! - Codex uses `pingWhenResetAtSlides` + `resetAtDriftMs` + `minPingIntervalMs`
//!
//! # Remaining limits
//! - Proxy is resolved via `resolve_proxy_target` (connection / pool / outbound).
//!   Per-connection vercel relay is not modeled (same as most OP executors).
//! - Claude spoof headers are a static subset of 9r `CLAUDE_CLI_SPOOF_HEADERS`
//!   (version/beta/UA/x-app); stainless arch/os are fixed, not host-mapped.
//! - No per-connection concurrent ping mutex beyond the global tick lock.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing::post, Json, Router};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::core::executor::{CodexExecutionRequest, CodexExecutor, UpstreamResponse};
use crate::core::proxy::resolve_proxy_target;
use crate::oauth::token_refresh::{
    dispatch_oauth_refresh, should_refresh_credentials, REFRESH_LEAD_CODEX_MS,
};
use crate::server::api::usage::fetch_oauth_quota;
use crate::server::state::AppState;
use crate::types::{ProviderConnection, Settings};

const TICK_INTERVAL: Duration = Duration::from_secs(60);
const PING_LEAD_MS: i64 = 5_000;
const REFRESH_AHEAD_MS: i64 = 300_000;
const FAILURE_COOLDOWN_MS: u64 = 900_000;
const CODEX_RESET_DRIFT_MS: i64 = 30_000;
const CODEX_MIN_PING_INTERVAL_MS: i64 = 600_000;
const CODEX_PING_MODEL: &str = "gpt-5.6-luna";
const CODEX_PENDING_KEY: &str = "codexAutoPingPending";

const CLAUDE_PING_URL: &str = "https://api.anthropic.com/v1/messages?beta=true";
const CLAUDE_PING_MODEL: &str = "claude-haiku-4-5-20251001";
const CLAUDE_PING_TEXT: &str = "hi";
const CLAUDE_PING_MAX_TOKENS: u32 = 1;
const CLAUDE_ANTHROPIC_VERSION: &str = "2023-06-01";
const CLAUDE_ANTHROPIC_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,effort-2025-11-24,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28";

const CODEX_PING_TEXT: &str = "hi";
const CODEX_PING_INSTRUCTIONS: &str = "Reply with OK.";
const CODEX_PING_REASONING_EFFORT: &str = "low";

static TICK_RUNNING: AtomicBool = AtomicBool::new(false);

/// Process-local caches matching 9r `global.__quotaAutoPing`.
struct AutoPingState {
    /// Last observed Claude session resetAt per `provider:connectionId`.
    reset_cache: BTreeMap<String, String>,
    /// Last valid Codex quota observation per connection.
    codex_observations: BTreeMap<String, CodexObservation>,
    /// Pending Codex reset events, mirrored to connection extra for restart retry.
    codex_pending: BTreeMap<String, CodexPending>,
    /// Successful Codex generations retained if their DB marker cannot be written.
    codex_completed: BTreeMap<String, String>,
    /// Failure timestamps for cooldown.
    failure_cache: BTreeMap<String, Instant>,
}

static AUTO_PING_STATE: Lazy<Mutex<AutoPingState>> = Lazy::new(|| {
    Mutex::new(AutoPingState {
        reset_cache: BTreeMap::new(),
        codex_observations: BTreeMap::new(),
        codex_pending: BTreeMap::new(),
        codex_completed: BTreeMap::new(),
        failure_cache: BTreeMap::new(),
    })
});

#[derive(Clone, Copy)]
struct ProviderPingConfig {
    settings_key: &'static str,
    quota_key: &'static str,
}

const CLAUDE_CFG: ProviderPingConfig = ProviderPingConfig {
    settings_key: "claudeAutoPing",
    quota_key: "session (5h)",
};

const CODEX_CFG: ProviderPingConfig = ProviderPingConfig {
    settings_key: "codexAutoPing",
    quota_key: "session",
};

#[derive(Clone, Debug, PartialEq)]
struct CodexObservation {
    quota_key: String,
    window_minutes: i64,
    used: f64,
    reset_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexPending {
    quota_key: String,
    generation_key: String,
    trigger_reason: String,
    reset_at: Option<String>,
    detected_at: String,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/quota/auto-ping/tick", post(tick_handler))
}

async fn tick_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let result = run_quota_auto_ping_tick(&state).await;
    Json(result).into_response()
}

/// Spawn a background interval that ticks auto-ping every 60s.
/// Best-effort — safe to call once at process boot.
pub fn spawn_quota_auto_ping(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            let _ = run_quota_auto_ping_tick(&state).await;
            tokio::time::sleep(TICK_INTERVAL).await;
        }
    });
}

pub async fn run_quota_auto_ping_tick(state: &AppState) -> Value {
    if TICK_RUNNING.swap(true, Ordering::SeqCst) {
        return json!({
            "ok": true,
            "skipped": true,
            "reason": "tick already running",
        });
    }

    let result = run_tick_inner(state).await;
    TICK_RUNNING.store(false, Ordering::SeqCst);
    result
}

async fn run_tick_inner(state: &AppState) -> Value {
    let snapshot = state.db.snapshot();
    let settings = &snapshot.settings;

    let mut results: Vec<Value> = Vec::new();
    let mut target_count = 0u32;
    let mut ping_attempts = 0u32;
    let mut ping_successes = 0u32;

    for (provider, cfg) in [("claude", CLAUDE_CFG), ("codex", CODEX_CFG)] {
        let enabled_map = auto_ping_connections(settings, cfg.settings_key);
        if enabled_map.is_empty() {
            continue;
        }

        for conn in snapshot
            .provider_connections
            .iter()
            .filter(|c| c.provider == provider && c.is_active() && c.auth_type == "oauth")
        {
            if enabled_map.get(&conn.id) != Some(&true) {
                continue;
            }
            target_count += 1;

            match process_connection(state, conn, provider, cfg).await {
                TickOutcome::Observe {
                    reset_at,
                    near_reset,
                    quota_key,
                } => {
                    results.push(json!({
                        "provider": provider,
                        "connectionId": conn.id,
                        "resetAt": reset_at,
                        "nearReset": near_reset,
                        "quotaKey": quota_key,
                        "action": "observe",
                    }));
                }
                TickOutcome::Skip {
                    reason,
                    reset_at,
                    quota_key,
                    trigger_reason,
                } => {
                    let mut entry = json!({
                        "provider": provider,
                        "connectionId": conn.id,
                        "resetAt": reset_at,
                        "nearReset": false,
                        "quotaKey": quota_key,
                        "action": "skip",
                        "reason": reason,
                    });
                    if let Some(trigger) = trigger_reason {
                        entry["triggerReason"] = json!(trigger);
                    }
                    results.push(entry);
                }
                TickOutcome::Ping {
                    ok,
                    reset_at,
                    error,
                    quota_key,
                    trigger_reason,
                } => {
                    ping_attempts += 1;
                    if ok {
                        ping_successes += 1;
                    }
                    let mut entry = json!({
                        "provider": provider,
                        "connectionId": conn.id,
                        "resetAt": reset_at,
                        "nearReset": true,
                        "quotaKey": quota_key,
                        "action": if ok { "ping_success" } else { "ping_failed" },
                    });
                    entry["triggerReason"] = json!(trigger_reason);
                    if let Some(err) = error {
                        entry["error"] = json!(err);
                    }
                    results.push(entry);
                }
            }
        }
    }

    if target_count == 0 {
        return json!({
            "ok": true,
            "targets": 0,
            "results": [],
            "note": "No claudeAutoPing/codexAutoPing connections enabled",
        });
    }

    json!({
        "ok": true,
        "targets": target_count,
        "pingAttempts": ping_attempts,
        "pingSuccesses": ping_successes,
        "results": results,
        "warmPing": "active",
    })
}

enum TickOutcome {
    Observe {
        reset_at: Option<String>,
        near_reset: bool,
        quota_key: String,
    },
    Skip {
        reason: String,
        reset_at: Option<String>,
        quota_key: String,
        trigger_reason: Option<String>,
    },
    Ping {
        ok: bool,
        reset_at: Option<String>,
        error: Option<String>,
        quota_key: String,
        trigger_reason: String,
    },
}

async fn process_connection(
    state: &AppState,
    conn: &ProviderConnection,
    provider: &str,
    cfg: ProviderPingConfig,
) -> TickOutcome {
    let key = cache_key(provider, &conn.id);
    let is_codex = provider == "codex";

    // Codex must continue observing quota while a failed ping cools down so a
    // reset event is not consumed. Claude keeps its existing early cooldown.
    if !is_codex {
        let st = AUTO_PING_STATE.lock();
        if let Some(failed_at) = st.failure_cache.get(&key) {
            if failed_at.elapsed() < Duration::from_millis(FAILURE_COOLDOWN_MS) {
                return TickOutcome::Skip {
                    reason: "failure_cooldown".into(),
                    reset_at: st.reset_cache.get(&key).cloned(),
                    quota_key: cfg.quota_key.into(),
                    trigger_reason: None,
                };
            }
        }
    }

    // Claude: skip far from reset using cached resetAt (refreshAheadMs)
    if !is_codex {
        let st = AUTO_PING_STATE.lock();
        if let Some(cached) = st.reset_cache.get(&key) {
            if let Some(reset_ms) = parse_reset_ms(cached) {
                let now = chrono::Utc::now().timestamp_millis();
                if now < reset_ms - REFRESH_AHEAD_MS {
                    return TickOutcome::Observe {
                        reset_at: Some(cached.clone()),
                        near_reset: false,
                        quota_key: cfg.quota_key.into(),
                    };
                }
            }
        }
    }

    // Claude retains the upstream always-refresh behavior. Codex refreshes only
    // when due so quota observation does not rotate a healthy credential.
    let mut connection = conn.clone();
    if let Some(rt) = connection
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let refresh_due = should_refresh_for_auto_ping(&connection, provider);
        let refresh_cooling_down = is_codex && failure_cooldown_active(&key);
        if refresh_due && !refresh_cooling_down {
            match dispatch_oauth_refresh(provider, rt, &connection.provider_specific_data).await {
                Ok(result) => {
                    let conn_id = connection.id.clone();
                    let new_access = result.access_token.clone();
                    let new_refresh = result.refresh_token.clone();
                    let expires_at = result.expires_in.map(|secs| {
                        (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339()
                    });
                    let last_refresh_at = chrono::Utc::now().to_rfc3339();
                    let _ = state
                        .db
                        .update({
                            let conn_id = conn_id.clone();
                            let new_access = new_access.clone();
                            let new_refresh = new_refresh.clone();
                            let expires_at = expires_at.clone();
                            let last_refresh_at = last_refresh_at.clone();
                            move |db| {
                                if let Some(c) =
                                    db.provider_connections.iter_mut().find(|c| c.id == conn_id)
                                {
                                    c.access_token = Some(new_access);
                                    if let Some(rt) = new_refresh {
                                        c.refresh_token = Some(rt);
                                    }
                                    if let Some(exp) = expires_at {
                                        c.expires_at = Some(exp);
                                    }
                                    c.provider_specific_data.insert(
                                        "lastRefreshAt".into(),
                                        Value::String(last_refresh_at),
                                    );
                                }
                            }
                        })
                        .await;
                    connection.access_token = Some(result.access_token);
                    if let Some(rt) = result.refresh_token {
                        connection.refresh_token = Some(rt);
                    }
                    if let Some(exp) = expires_at {
                        connection.expires_at = Some(exp);
                    }
                }
                Err(e) => {
                    mark_failure(&key);
                    warn!(
                        target: "openproxy::auto_ping",
                        provider = provider,
                        connection_id = %conn.id,
                        error = %e,
                        "quota auto-ping: credential refresh failed"
                    );
                    if !is_codex || !has_access_token(&connection) {
                        return TickOutcome::Skip {
                            reason: format!("refresh_failed: {e}"),
                            reset_at: None,
                            quota_key: cfg.quota_key.into(),
                            trigger_reason: None,
                        };
                    }
                }
            }
        }
    }

    let usage = fetch_oauth_quota(&connection).await;
    let quotas = usage.get("quotas").cloned().unwrap_or_else(|| json!({}));

    if is_codex {
        return process_codex_quota(state, &connection, &key, &usage, &quotas).await;
    }

    let quota = quotas.get(cfg.quota_key).cloned().unwrap_or(Value::Null);
    let reset_at = quota
        .get("resetAt")
        .or_else(|| quota.get("reset_at"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let Some(reset_at) = reset_at else {
        return TickOutcome::Skip {
            reason: "no_reset_at".into(),
            reset_at: None,
            quota_key: cfg.quota_key.into(),
            trigger_reason: None,
        };
    };

    AUTO_PING_STATE
        .lock()
        .reset_cache
        .insert(key.clone(), reset_at.clone());

    if is_quota_exhausted(&quota) {
        return TickOutcome::Skip {
            reason: "session_quota_exhausted".into(),
            reset_at: Some(reset_at),
            quota_key: cfg.quota_key.into(),
            trigger_reason: None,
        };
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let should_ping = should_ping_for_reset(&reset_at, now_ms);
    if !should_ping {
        let near = parse_reset_ms(&reset_at).is_some_and(|ms| now_ms >= ms - PING_LEAD_MS);
        return TickOutcome::Observe {
            reset_at: Some(reset_at),
            near_reset: near,
            quota_key: cfg.quota_key.into(),
        };
    }

    let reset_key = normalize_reset_key(&reset_at);
    let last_pinged_key = connection
        .extra
        .get("lastPingedResetKey")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            connection
                .extra
                .get("lastPingedResetAt")
                .and_then(|v| v.as_str())
                .map(normalize_reset_key)
        });
    if last_pinged_key.as_deref() == Some(reset_key.as_str()) {
        return TickOutcome::Skip {
            reason: "already_pinged_this_reset".into(),
            reset_at: Some(reset_at),
            quota_key: cfg.quota_key.into(),
            trigger_reason: None,
        };
    }

    let ping_result = match provider {
        "claude" => send_claude_ping(state, &connection).await,
        _ => Err("unsupported provider".into()),
    };

    match ping_result {
        Ok(()) => {
            clear_failure(&key);
            let pinged_at = chrono::Utc::now().to_rfc3339();
            let conn_id = connection.id.clone();
            let reset_at_store = reset_at.clone();
            let reset_key_store = reset_key.clone();
            let _ = state
                .db
                .update(move |db| {
                    if let Some(c) = db.provider_connections.iter_mut().find(|c| c.id == conn_id) {
                        c.extra
                            .insert("lastPingedResetAt".into(), json!(reset_at_store));
                        c.extra
                            .insert("lastPingedResetKey".into(), json!(reset_key_store));
                        c.extra.insert("lastPingAt".into(), json!(pinged_at));
                        c.updated_at = Some(chrono::Utc::now().to_rfc3339());
                    }
                })
                .await;
            info!(
                target: "openproxy::auto_ping",
                provider = provider,
                connection_id = %connection.id,
                reset_at = %reset_at,
                "quota auto-ping: warm ping sent"
            );
            TickOutcome::Ping {
                ok: true,
                reset_at: Some(reset_at),
                error: None,
                quota_key: cfg.quota_key.into(),
                trigger_reason: "scheduled_reset".into(),
            }
        }
        Err(e) => {
            mark_failure(&key);
            warn!(
                target: "openproxy::auto_ping",
                provider = provider,
                connection_id = %connection.id,
                reset_at = %reset_at,
                error = %e,
                "quota auto-ping: warm ping failed"
            );
            TickOutcome::Ping {
                ok: false,
                reset_at: Some(reset_at),
                error: Some(e),
                quota_key: cfg.quota_key.into(),
                trigger_reason: "scheduled_reset".into(),
            }
        }
    }
}

async fn process_codex_quota(
    state: &AppState,
    connection: &ProviderConnection,
    cache_key: &str,
    usage: &Value,
    quotas: &Value,
) -> TickOutcome {
    let Some(target) = select_codex_target(quotas) else {
        return TickOutcome::Skip {
            reason: if let Some(message) = usage.get("message").and_then(Value::as_str) {
                format!("quota_fetch_failed: {message}")
            } else {
                "no_supported_quota".into()
            },
            reset_at: None,
            quota_key: "session".into(),
            trigger_reason: None,
        };
    };

    let now_ms = chrono::Utc::now().timestamp_millis();
    let persisted_pending = connection
        .extra
        .get(CODEX_PENDING_KEY)
        .and_then(|value| serde_json::from_value::<CodexPending>(value.clone()).ok());
    let persisted_last_key = connection
        .extra
        .get("lastPingedResetKey")
        .and_then(Value::as_str)
        .map(str::to_string);

    let (previous, mut pending, completed_key) = {
        let mut st = AUTO_PING_STATE.lock();
        let previous = st.codex_observations.get(cache_key).cloned();
        if !st.codex_pending.contains_key(cache_key) {
            if let Some(pending) = persisted_pending {
                st.codex_pending.insert(cache_key.to_string(), pending);
            }
        }
        (
            previous,
            st.codex_pending.get(cache_key).cloned(),
            st.codex_completed.get(cache_key).cloned(),
        )
    };

    if pending
        .as_ref()
        .is_some_and(|pending| pending.quota_key != target.quota_key)
    {
        AUTO_PING_STATE.lock().codex_pending.remove(cache_key);
        pending = None;
        if let Err(error) = persist_codex_pending(state, &connection.id, None).await {
            warn!(
                target: "openproxy::auto_ping",
                connection_id = %connection.id,
                error = %error,
                "quota auto-ping: failed to clear incompatible pending reset"
            );
        }
    }

    let same_window = previous.as_ref().is_some_and(|previous| {
        previous.quota_key == target.quota_key && previous.window_minutes == target.window_minutes
    });
    let last_key = completed_key.or(persisted_last_key);
    let accept_observation = should_accept_codex_observation(previous.as_ref(), &target);
    let detected = (same_window && accept_observation)
        .then(|| {
            detect_codex_reset(
                previous.as_ref().unwrap(),
                &target,
                now_ms,
                last_key.as_deref(),
            )
        })
        .flatten();

    if accept_observation {
        AUTO_PING_STATE
            .lock()
            .codex_observations
            .insert(cache_key.to_string(), target.clone());
    }

    if let Some(event) = detected {
        if last_key.as_deref() != Some(event.generation_key.as_str())
            && pending.as_ref().map(|p| &p.generation_key) != Some(&event.generation_key)
        {
            AUTO_PING_STATE
                .lock()
                .codex_pending
                .insert(cache_key.to_string(), event.clone());
            pending = Some(event);
        }
    }

    let Some(pending_event) = pending else {
        return TickOutcome::Observe {
            reset_at: target.reset_at,
            near_reset: false,
            quota_key: target.quota_key,
        };
    };

    let pending_value = serde_json::to_value(&pending_event).unwrap_or(Value::Null);
    if connection.extra.get(CODEX_PENDING_KEY) != Some(&pending_value) {
        if let Err(error) = persist_codex_pending(state, &connection.id, Some(&pending_event)).await
        {
            mark_failure(cache_key);
            return TickOutcome::Skip {
                reason: format!("pending_persist_failed: {error}"),
                reset_at: pending_event.reset_at,
                quota_key: pending_event.quota_key,
                trigger_reason: Some(pending_event.trigger_reason),
            };
        }
    }

    if let Some(failed_at) = AUTO_PING_STATE.lock().failure_cache.get(cache_key) {
        if failed_at.elapsed() < Duration::from_millis(FAILURE_COOLDOWN_MS) {
            return TickOutcome::Skip {
                reason: "failure_cooldown".into(),
                reset_at: pending_event.reset_at,
                quota_key: pending_event.quota_key,
                trigger_reason: Some(pending_event.trigger_reason),
            };
        }
    }

    if pending_event.quota_key == "session" && codex_weekly_blocks_session(quotas, now_ms) {
        return TickOutcome::Skip {
            reason: "blocking_quota_exhausted".into(),
            reset_at: pending_event.reset_at,
            quota_key: pending_event.quota_key,
            trigger_reason: Some(pending_event.trigger_reason),
        };
    }

    if was_pinged_recently(connection, CODEX_MIN_PING_INTERVAL_MS, now_ms) {
        return TickOutcome::Skip {
            reason: "min_ping_interval".into(),
            reset_at: pending_event.reset_at,
            quota_key: pending_event.quota_key,
            trigger_reason: Some(pending_event.trigger_reason),
        };
    }

    let model = match state
        .codex_models
        .models_for_connection(state, connection)
        .await
    {
        Ok(inventory) => select_codex_ping_model(&inventory.models),
        Err(_) => None,
    };
    let Some(model) = model else {
        return TickOutcome::Skip {
            reason: "model_unavailable".into(),
            reset_at: pending_event.reset_at,
            quota_key: pending_event.quota_key,
            trigger_reason: Some(pending_event.trigger_reason),
        };
    };

    let ping_result = send_codex_ping(state, connection, &model).await;
    match ping_result {
        Ok(()) => {
            clear_failure(cache_key);
            let pinged_at = chrono::Utc::now().to_rfc3339();
            let conn_id = connection.id.clone();
            let reset_at = pending_event.reset_at.clone();
            let generation_key = pending_event.generation_key.clone();
            let update_result = state
                .db
                .update(move |db| {
                    if let Some(c) = db.provider_connections.iter_mut().find(|c| c.id == conn_id) {
                        c.extra.remove(CODEX_PENDING_KEY);
                        c.extra.insert("lastPingedResetAt".into(), json!(reset_at));
                        c.extra
                            .insert("lastPingedResetKey".into(), json!(generation_key));
                        c.extra.insert("lastPingAt".into(), json!(pinged_at));
                        c.updated_at = Some(chrono::Utc::now().to_rfc3339());
                    }
                })
                .await;
            {
                let mut st = AUTO_PING_STATE.lock();
                st.codex_pending.remove(cache_key);
                st.codex_completed
                    .insert(cache_key.to_string(), pending_event.generation_key.clone());
            }
            if let Err(error) = update_result {
                warn!(
                    target: "openproxy::auto_ping",
                    connection_id = %connection.id,
                    error = %error,
                    "quota auto-ping: failed to persist successful Codex ping"
                );
            }
            info!(
                target: "openproxy::auto_ping",
                provider = "codex",
                connection_id = %connection.id,
                trigger = %pending_event.trigger_reason,
                "quota auto-ping: warm ping sent"
            );
            TickOutcome::Ping {
                ok: true,
                reset_at: pending_event.reset_at,
                error: None,
                quota_key: pending_event.quota_key,
                trigger_reason: pending_event.trigger_reason,
            }
        }
        Err(error) => {
            mark_failure(cache_key);
            warn!(
                target: "openproxy::auto_ping",
                provider = "codex",
                connection_id = %connection.id,
                error = %error,
                "quota auto-ping: warm ping failed"
            );
            TickOutcome::Ping {
                ok: false,
                reset_at: pending_event.reset_at,
                error: Some(error),
                quota_key: pending_event.quota_key,
                trigger_reason: pending_event.trigger_reason,
            }
        }
    }
}

fn select_codex_target(quotas: &Value) -> Option<CodexObservation> {
    [("session", 300_i64), ("weekly", 10_080_i64)]
        .into_iter()
        .find_map(|(quota_key, expected_minutes)| {
            let quota = quotas.get(quota_key)?;
            let window_minutes = quota.get("windowMinutes")?.as_i64()?;
            if window_minutes != expected_minutes {
                return None;
            }
            Some(CodexObservation {
                quota_key: quota_key.to_string(),
                window_minutes,
                used: quota.get("used").and_then(to_finite_number)?,
                reset_at: quota
                    .get("resetAt")
                    .or_else(|| quota.get("reset_at"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
}

fn select_codex_ping_model(
    models: &[crate::server::codex_catalog::CodexModelMetadata],
) -> Option<crate::server::codex_catalog::CodexModelMetadata> {
    models
        .iter()
        .find(|model| {
            model.id == CODEX_PING_MODEL
                && model
                    .reasoning_efforts
                    .iter()
                    .any(|effort| effort == CODEX_PING_REASONING_EFFORT)
        })
        .cloned()
}

fn codex_ping_body(model_id: &str) -> Value {
    json!({
        "model": model_id,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": CODEX_PING_TEXT }],
        }],
        "instructions": CODEX_PING_INSTRUCTIONS,
        "reasoning": {
            "effort": CODEX_PING_REASONING_EFFORT,
            "summary": "auto",
        },
        "store": false,
        "stream": true,
    })
}

fn detect_codex_reset(
    previous: &CodexObservation,
    current: &CodexObservation,
    now_ms: i64,
    last_pinged_key: Option<&str>,
) -> Option<CodexPending> {
    let detected_at = chrono::DateTime::from_timestamp_millis(now_ms)?.to_rfc3339();
    let pending = |generation_key: String, trigger_reason: &str| CodexPending {
        quota_key: current.quota_key.clone(),
        generation_key,
        trigger_reason: trigger_reason.to_string(),
        reset_at: current
            .reset_at
            .clone()
            .or_else(|| previous.reset_at.clone()),
        detected_at: detected_at.clone(),
    };

    if let Some(previous_reset_ms) = previous.reset_at.as_deref().and_then(parse_reset_ms) {
        if now_ms >= previous_reset_ms {
            let key = format!("codex:{}:deadline:{}", current.quota_key, previous_reset_ms);
            // A new WHAM deadline after a successful deadline ping acknowledges
            // the same rollover; it is not another reset generation.
            if last_pinged_key == Some(key.as_str()) {
                return None;
            }
            return Some(pending(key, "scheduled_reset"));
        }
    }

    if let (Some(previous_reset), Some(current_reset)) =
        (previous.reset_at.as_deref(), current.reset_at.as_deref())
    {
        if get_reset_drift_ms(previous_reset, current_reset) >= CODEX_RESET_DRIFT_MS
            && current.used <= previous.used
        {
            let current_reset_ms = parse_reset_ms(current_reset)?;
            return Some(pending(
                format!("codex:{}:window:{}", current.quota_key, current_reset_ms),
                "reset_at_changed",
            ));
        }
    }

    if previous.used > 0.0 && current.used == 0.0 {
        return Some(pending(
            format!("codex:{}:usage_reset:{}", current.quota_key, now_ms),
            "usage_reset",
        ));
    }

    None
}

fn should_accept_codex_observation(
    previous: Option<&CodexObservation>,
    current: &CodexObservation,
) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    if previous.quota_key != current.quota_key || previous.window_minutes != current.window_minutes
    {
        return true;
    }
    match (
        previous.reset_at.as_deref().and_then(parse_reset_ms),
        current.reset_at.as_deref().and_then(parse_reset_ms),
    ) {
        (Some(previous_ms), Some(current_ms)) => current_ms >= previous_ms,
        _ => true,
    }
}

fn codex_weekly_blocks_session(quotas: &Value, now_ms: i64) -> bool {
    let Some(weekly) = quotas.get("weekly") else {
        return false;
    };
    if !is_quota_exhausted(weekly) {
        return false;
    }
    weekly
        .get("resetAt")
        .and_then(Value::as_str)
        .and_then(parse_reset_ms)
        .is_none_or(|reset_ms| reset_ms > now_ms)
}

async fn persist_codex_pending(
    state: &AppState,
    connection_id: &str,
    pending: Option<&CodexPending>,
) -> Result<(), String> {
    let connection_id = connection_id.to_string();
    let pending = pending
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| format!("serialize pending reset: {error}"))?;
    state
        .db
        .update(move |db| {
            if let Some(connection) = db
                .provider_connections
                .iter_mut()
                .find(|connection| connection.id == connection_id)
            {
                match pending {
                    Some(value) => {
                        connection.extra.insert(CODEX_PENDING_KEY.into(), value);
                    }
                    None => {
                        connection.extra.remove(CODEX_PENDING_KEY);
                    }
                }
                connection.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
        })
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn send_claude_ping(state: &AppState, connection: &ProviderConnection) -> Result<(), String> {
    let token = connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing access token".to_string())?;

    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);
    let client = state
        .client_pool
        .get("claude-auto-ping", proxy.as_ref())
        .map_err(|e| format!("client pool: {e}"))?;

    let body = json!({
        "model": CLAUDE_PING_MODEL,
        "max_tokens": CLAUDE_PING_MAX_TOKENS,
        "messages": [{ "role": "user", "content": CLAUDE_PING_TEXT }],
    });

    let response = client
        .post(CLAUDE_PING_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("anthropic-version", CLAUDE_ANTHROPIC_VERSION)
        .header("anthropic-beta", CLAUDE_ANTHROPIC_BETA)
        .header("anthropic-dangerous-direct-browser-access", "true")
        .header("user-agent", "claude-cli/2.1.92 (external, sdk-cli)")
        .header("x-app", "cli")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("claude ping request failed: {e}"))?;

    let status = response.status();
    // Drain body so the connection is reusable.
    let _ = response.bytes().await;
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("claude ping HTTP {}", status.as_u16()))
    }
}

async fn send_codex_ping(
    state: &AppState,
    connection: &ProviderConnection,
    model: &crate::server::codex_catalog::CodexModelMetadata,
) -> Result<(), String> {
    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);

    let executor = CodexExecutor::new(state.client_pool.clone(), None)
        .map_err(|e| format!("codex executor init: {e:?}"))?;

    let body = codex_ping_body(&model.id);

    let request = CodexExecutionRequest {
        model: model.id.clone(),
        body,
        stream: true,
        web_search_context_size: None,
        credentials: connection.clone(),
        proxy,
    };

    let result = executor
        .execute(request)
        .await
        .map_err(|e| format!("codex ping execute: {e:?}"))?;

    let status = result.response.status();
    // The Codex quota window starts only after the stream completes.
    match result.response {
        UpstreamResponse::Reqwest(resp) => {
            let ok = status.is_success();
            resp.bytes()
                .await
                .map_err(|e| format!("codex ping stream: {e}"))?;
            if ok {
                Ok(())
            } else {
                Err(format!("codex ping HTTP {}", status.as_u16()))
            }
        }
        UpstreamResponse::Hyper(resp) => {
            let ok = status.is_success();
            http_body_util::BodyExt::collect(resp.into_body())
                .await
                .map_err(|e| format!("codex ping stream: {e}"))?;
            if ok {
                Ok(())
            } else {
                Err(format!("codex ping HTTP {}", status.as_u16()))
            }
        }
    }
}

fn should_ping_for_reset(reset_at: &str, now_ms: i64) -> bool {
    parse_reset_ms(reset_at).is_some_and(|ms| now_ms >= ms - PING_LEAD_MS)
}

fn get_reset_drift_ms(previous: &str, next: &str) -> i64 {
    match (parse_reset_ms(previous), parse_reset_ms(next)) {
        (Some(a), Some(b)) => b - a,
        _ => 0,
    }
}

fn parse_reset_ms(reset_at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(reset_at)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn normalize_reset_key(reset_at: &str) -> String {
    match parse_reset_ms(reset_at) {
        Some(ms) => {
            let floored = (ms / 60_000) * 60_000;
            chrono::DateTime::from_timestamp_millis(floored)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| reset_at.to_string())
        }
        None => reset_at.to_string(),
    }
}

fn was_pinged_recently(connection: &ProviderConnection, interval_ms: i64, now_ms: i64) -> bool {
    let Some(last) = connection.extra.get("lastPingAt").and_then(|v| v.as_str()) else {
        return false;
    };
    match parse_reset_ms(last) {
        Some(last_ms) => now_ms - last_ms < interval_ms,
        None => false,
    }
}

fn to_finite_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|i| i as f64))
        .or_else(|| value.as_u64().map(|u| u as f64))
        .or_else(|| {
            value
                .as_str()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|n| n.is_finite())
        })
}

fn is_quota_exhausted(quota: &Value) -> bool {
    if quota.is_null() {
        return false;
    }
    if quota.get("unlimited").and_then(|v| v.as_bool()) == Some(true) {
        return false;
    }
    if let Some(remaining) = quota.get("remaining").and_then(to_finite_number) {
        return remaining <= 0.0;
    }
    let used = quota.get("used").and_then(to_finite_number);
    let total = quota.get("total").and_then(to_finite_number);
    match (used, total) {
        (Some(u), Some(t)) if t > 0.0 => u >= t,
        _ => false,
    }
}

fn cache_key(provider: &str, connection_id: &str) -> String {
    format!("{provider}:{connection_id}")
}

fn should_refresh_for_auto_ping(connection: &ProviderConnection, provider: &str) -> bool {
    if provider != "codex" {
        return true;
    }

    let last_refresh_at = connection
        .provider_specific_data
        .get("lastRefreshAt")
        .and_then(Value::as_str);
    should_refresh_credentials(
        provider,
        &connection.expires_at,
        last_refresh_at,
        connection
            .refresh_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty()),
        REFRESH_LEAD_CODEX_MS,
    )
}

fn has_access_token(connection: &ProviderConnection) -> bool {
    connection
        .access_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
}

fn failure_cooldown_active(key: &str) -> bool {
    AUTO_PING_STATE
        .lock()
        .failure_cache
        .get(key)
        .is_some_and(|failed_at| failed_at.elapsed() < Duration::from_millis(FAILURE_COOLDOWN_MS))
}

fn mark_failure(key: &str) {
    AUTO_PING_STATE
        .lock()
        .failure_cache
        .insert(key.to_string(), Instant::now());
}

fn clear_failure(key: &str) {
    AUTO_PING_STATE.lock().failure_cache.remove(key);
}

fn auto_ping_connections(settings: &Settings, key: &str) -> BTreeMap<String, bool> {
    let value = settings.extra.get(key);
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return BTreeMap::new();
    };
    let Some(connections) = obj.get("connections").and_then(|v| v.as_object()) else {
        return BTreeMap::new();
    };
    connections
        .iter()
        .filter_map(|(id, v)| v.as_bool().map(|b| (id.clone(), b)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_auto_ping_does_not_refresh_fresh_credentials() {
        let now = chrono::Utc::now();
        let mut connection = ProviderConnection {
            provider: "codex".into(),
            refresh_token: Some("refresh-token".into()),
            expires_at: Some((now + chrono::Duration::days(9)).to_rfc3339()),
            ..Default::default()
        };
        connection.provider_specific_data.insert(
            "lastRefreshAt".into(),
            Value::String((now - chrono::Duration::days(1)).to_rfc3339()),
        );

        assert!(!should_refresh_for_auto_ping(&connection, "codex"));
    }

    #[test]
    fn normalize_reset_key_floors_to_minute() {
        let key = normalize_reset_key("2026-05-12T18:30:45.123Z");
        assert!(key.starts_with("2026-05-12T18:30:00"));
    }

    #[test]
    fn should_ping_claude_near_reset() {
        let now = chrono::Utc::now().timestamp_millis();
        let reset = chrono::DateTime::from_timestamp_millis(now + 1_000)
            .unwrap()
            .to_rfc3339();
        assert!(should_ping_for_reset(&reset, now));
        let far = chrono::DateTime::from_timestamp_millis(now + 60_000)
            .unwrap()
            .to_rfc3339();
        assert!(!should_ping_for_reset(&far, now));
    }

    #[test]
    fn codex_target_prefers_session_and_supports_weekly_only() {
        let plus = json!({
            "session": { "used": 20, "windowMinutes": 300 },
            "weekly": { "used": 40, "windowMinutes": 10_080 },
            "review_session": { "used": 0, "windowMinutes": 300 },
        });
        assert_eq!(select_codex_target(&plus).unwrap().quota_key, "session");

        let pro = json!({
            "weekly": { "used": 36, "windowMinutes": 10_080 },
            "review_weekly": { "used": 100, "windowMinutes": 10_080 },
        });
        assert_eq!(select_codex_target(&pro).unwrap().quota_key, "weekly");
    }

    #[test]
    fn codex_scheduled_reset_and_late_slide_are_one_generation() {
        let now = chrono::Utc::now().timestamp_millis();
        let previous = CodexObservation {
            quota_key: "weekly".into(),
            window_minutes: 10_080,
            used: 100.0,
            reset_at: chrono::DateTime::from_timestamp_millis(now).map(|dt| dt.to_rfc3339()),
        };
        let current = CodexObservation {
            used: 0.0,
            reset_at: chrono::DateTime::from_timestamp_millis(now + 604_800_000)
                .map(|dt| dt.to_rfc3339()),
            ..previous.clone()
        };

        let event = detect_codex_reset(&previous, &current, now, None).unwrap();
        assert_eq!(event.trigger_reason, "scheduled_reset");
        assert!(
            detect_codex_reset(&previous, &current, now, Some(&event.generation_key)).is_none()
        );
    }

    #[test]
    fn codex_detects_repeated_usage_resets() {
        let now = chrono::Utc::now().timestamp_millis();
        let reset_at =
            chrono::DateTime::from_timestamp_millis(now + 604_800_000).map(|dt| dt.to_rfc3339());
        let before = CodexObservation {
            quota_key: "weekly".into(),
            window_minutes: 10_080,
            used: 36.0,
            reset_at: reset_at.clone(),
        };
        let after = CodexObservation {
            used: 0.0,
            ..before.clone()
        };
        let first = detect_codex_reset(&before, &after, now, None).unwrap();
        let used_again = CodexObservation {
            used: 12.0,
            ..after.clone()
        };
        let second = detect_codex_reset(&used_again, &after, now + 86_400_000, None).unwrap();

        assert_eq!(first.trigger_reason, "usage_reset");
        assert_eq!(second.trigger_reason, "usage_reset");
        assert_ne!(first.generation_key, second.generation_key);
    }

    #[test]
    fn codex_detects_mid_window_reset_at_change() {
        let now = chrono::Utc::now().timestamp_millis();
        let before = CodexObservation {
            quota_key: "weekly".into(),
            window_minutes: 10_080,
            used: 36.0,
            reset_at: chrono::DateTime::from_timestamp_millis(now + 86_400_000)
                .map(|dt| dt.to_rfc3339()),
        };
        let after = CodexObservation {
            used: 0.0,
            reset_at: chrono::DateTime::from_timestamp_millis(now + 604_800_000)
                .map(|dt| dt.to_rfc3339()),
            ..before.clone()
        };

        let event = detect_codex_reset(&before, &after, now, None).unwrap();
        assert_eq!(event.trigger_reason, "reset_at_changed");
    }

    #[test]
    fn codex_uses_only_luna_with_low_reasoning() {
        use crate::server::codex_catalog::CodexModelMetadata;

        let models = vec![
            CodexModelMetadata {
                id: "gpt-5.5".into(),
                name: "GPT-5.5".into(),
                context_window: None,
                capabilities: vec![],
                reasoning_efforts: vec!["low".into()],
            },
            CodexModelMetadata {
                id: CODEX_PING_MODEL.into(),
                name: "GPT-5.6 Luna".into(),
                context_window: None,
                capabilities: vec![],
                reasoning_efforts: vec!["low".into()],
            },
        ];
        let model = select_codex_ping_model(&models).unwrap();
        let body = codex_ping_body(&model.id);

        assert_eq!(model.id, CODEX_PING_MODEL);
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(select_codex_ping_model(&models[..1]).is_none());
    }

    #[test]
    fn codex_pending_survives_connection_serialization() {
        let pending = CodexPending {
            quota_key: "weekly".into(),
            generation_key: "codex:weekly:deadline:1".into(),
            trigger_reason: "scheduled_reset".into(),
            reset_at: Some("2026-09-19T08:09:55Z".into()),
            detected_at: "2026-09-14T20:00:00Z".into(),
        };
        let mut connection = ProviderConnection {
            id: "codex-1".into(),
            provider: "codex".into(),
            auth_type: "oauth".into(),
            ..Default::default()
        };
        connection.extra.insert(
            CODEX_PENDING_KEY.into(),
            serde_json::to_value(&pending).unwrap(),
        );

        let restored: ProviderConnection =
            serde_json::from_value(serde_json::to_value(connection).unwrap()).unwrap();
        let restored_pending: CodexPending =
            serde_json::from_value(restored.extra.get(CODEX_PENDING_KEY).unwrap().clone()).unwrap();

        assert_eq!(restored_pending, pending);
    }

    #[test]
    fn quota_exhausted_from_remaining() {
        assert!(is_quota_exhausted(&json!({ "remaining": 0 })));
        assert!(!is_quota_exhausted(&json!({ "remaining": 1 })));
        assert!(!is_quota_exhausted(
            &json!({ "unlimited": true, "remaining": 0 })
        ));
        assert!(is_quota_exhausted(&json!({ "used": 10, "total": 10 })));
    }

    #[test]
    fn codex_only_main_weekly_blocks_session() {
        let now = chrono::Utc::now().timestamp_millis();
        let future = chrono::DateTime::from_timestamp_millis(now + 60_000)
            .unwrap()
            .to_rfc3339();
        assert!(!codex_weekly_blocks_session(
            &json!({ "review_weekly": { "remaining": 0, "resetAt": future } }),
            now
        ));
        assert!(codex_weekly_blocks_session(
            &json!({ "weekly": { "remaining": 0, "resetAt": future } }),
            now
        ));
    }
}
