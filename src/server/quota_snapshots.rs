//! Demand-driven, bounded quota observations shared by OpenCode readers.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use crate::core::proxy::resolve_proxy_target;
use crate::core::usage::http::{fetch_with_client, Observation};
use crate::oauth::token_refresh::{connection_credential_generation, CredentialGeneration};
use crate::server::{api::usage, state::AppState};
use crate::types::ProviderConnection;

pub(crate) const REFRESH_SECONDS: u64 = 180;
const MAX_ACCOUNTS: usize = 128;
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountLimits {
    id: String,
    provider: String,
    label: String,
    status: &'static str,
    observed_at: Option<String>,
    plan: Option<String>,
    quotas: Value,
    error: Option<&'static str>,
    error_status: Option<u16>,
    refreshing: bool,
    next_refresh_at: Option<String>,
}

struct Entry {
    generation: CredentialGeneration,
    version: u64,
    next_attempt: Instant,
    observed: Option<Instant>,
    failures: u32,
    task: Option<AbortHandle>,
    limits: AccountLimits,
}

#[derive(Default)]
struct Entries {
    stopped: bool,
    accounts: HashMap<String, Entry>,
}

pub struct QuotaSnapshots {
    entries: Mutex<Entries>,
    permits: Arc<Semaphore>,
}

impl Default for QuotaSnapshots {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
            permits: Arc::new(Semaphore::new(4)),
        }
    }
}

impl QuotaSnapshots {
    pub(crate) fn read(self: &Arc<Self>, state: &AppState) -> (Vec<AccountLimits>, bool) {
        let snapshot = state.db.snapshot();
        let mut connections = snapshot
            .provider_connections
            .iter()
            .filter(|connection| connection.is_active.unwrap_or(true));
        let connections: Vec<_> = connections.by_ref().take(MAX_ACCOUNTS).collect();
        let truncated = snapshot
            .provider_connections
            .iter()
            .filter(|connection| connection.is_active.unwrap_or(true))
            .count()
            > MAX_ACCOUNTS;
        let now = Instant::now();
        let mut entries = self.entries.lock();
        if entries.stopped {
            return (Vec::new(), false);
        }
        entries.accounts.retain(|id, entry| {
            let keep = connections.iter().any(|connection| connection.id == *id);
            if !keep {
                if let Some(task) = &entry.task {
                    task.abort();
                }
            }
            keep
        });
        let mut indices = HashMap::<&str, usize>::new();
        for connection in &connections {
            let index = indices.entry(&connection.provider).or_default();
            *index += 1;
            let generation = connection_credential_generation(connection);
            let entry = entries
                .accounts
                .entry(connection.id.clone())
                .or_insert_with(|| Entry {
                    generation,
                    version: 0,
                    next_attempt: now,
                    observed: None,
                    failures: 0,
                    task: None,
                    limits: AccountLimits {
                        id: uuid::Uuid::new_v5(
                            &uuid::Uuid::NAMESPACE_OID,
                            connection.id.as_bytes(),
                        )
                        .to_string(),
                        provider: clean_string(&connection.provider),
                        label: format!("Account {index}"),
                        status: if usage::supports_quota(connection) {
                            "loading"
                        } else {
                            "unsupported"
                        },
                        observed_at: None,
                        plan: None,
                        quotas: json!({}),
                        error: None,
                        error_status: None,
                        refreshing: false,
                        next_refresh_at: None,
                    },
                });
            if entry.generation != generation {
                if let Some(task) = entry.task.take() {
                    task.abort();
                }
                entry.generation = generation;
                entry.version += 1;
                entry.observed = None;
                entry.failures = 0;
                entry.limits.quotas = json!({});
                entry.limits.observed_at = None;
                entry.limits.plan = None;
                entry.limits.error = None;
                entry.limits.error_status = None;
            }
            entry.limits.provider = clean_string(&connection.provider);
            entry.limits.label = format!("Account {index}");
            // Claude/anthropic quota is passively observed from response
            // headers on live generation traffic (see `quota_headers`) and
            // persisted under `settings.extra["claudeQuotaSnapshot:<id>"]`.
            // Mirror it here — the limits route serves it without ever
            // spawning a usage-endpoint collector for these providers.
            let passive_snapshot = if connection.auth_type == "oauth"
                && matches!(connection.provider.as_str(), "claude" | "anthropic")
            {
                // Subscription tier (Pro / Max x5 / …) is captured at login
                // from the profile payload the login already fetches — no
                // extra upstream call — and lives in the connection
                // settings. It renders beside the provider name regardless
                // of whether a quota snapshot exists yet.
                entry.limits.plan = connection
                    .provider_specific_data
                    .get("subscription")
                    .and_then(Value::as_str)
                    .map(clean_string)
                    .filter(|plan| !plan.trim().is_empty());
                snapshot
                    .settings
                    .extra
                    .get(&format!("claudeQuotaSnapshot:{}", connection.id))
                    .cloned()
            } else {
                None
            };
            if let Some(observed) = passive_snapshot {
                let quotas = project_quotas(&observed, &connection.provider);
                let observed_at = observed
                    .get("observedAt")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if quotas.as_object().is_some_and(|quotas| !quotas.is_empty()) {
                    entry.limits.quotas = quotas;
                    entry.limits.observed_at = observed_at;
                    entry.limits.error = None;
                    entry.limits.error_status = None;
                    entry.limits.status = "fresh";
                } else {
                    // A snapshot without usable windows is no better than no
                    // snapshot; keep waiting for live traffic.
                    entry.limits.status = "loading";
                }
            } else {
                entry.limits.status = if connection.auth_type == "oauth"
                    && matches!(connection.provider.as_str(), "claude" | "anthropic")
                {
                    // No observed traffic yet: the snapshot appears after the
                    // first request through the proxy. That is waiting, not
                    // a failure — and never "unsupported", which would render
                    // as "Limits unsupported" in clients.
                    "loading"
                } else if !usage::supports_quota(connection) {
                    "unsupported"
                } else if entry.observed.is_some_and(|time| {
                    now.duration_since(time) < Duration::from_secs(REFRESH_SECONDS)
                }) {
                    if entry.limits.error.is_some() {
                        "stale"
                    } else {
                        "fresh"
                    }
                } else if entry.observed.is_some() {
                    "stale"
                } else if entry.failures > 0 {
                    "unavailable"
                } else {
                    "loading"
                };
            }
        }
        // Oldest due accounts go first; a large configured set cannot starve
        // behind the same first four accounts on every read.
        let mut due = connections.clone();
        due.sort_by_key(|connection| entries.accounts[&connection.id].next_attempt);
        if !entries.stopped {
            for connection in due {
                let entry = entries
                    .accounts
                    .get_mut(&connection.id)
                    .expect("registered account");
                if !usage::supports_quota(connection)
                    || entry.task.is_some()
                    || now < entry.next_attempt
                {
                    continue;
                }
                let Ok(permit) = self.permits.clone().try_acquire_owned() else {
                    break;
                };
                entry.version += 1;
                entry.next_attempt = now + Duration::from_secs(REFRESH_SECONDS);
                let version = entry.version;
                let connection = connection.clone();
                let state = state.clone();
                let owner = self.clone();
                entry.task = Some(
                    tokio::spawn(async move {
                        let _permit = permit;
                        let fetched =
                            tokio::time::timeout(FETCH_TIMEOUT, fetch(&state, &connection)).await;
                        let (prepared, result, observation) =
                            fetched.ok().flatten().unwrap_or_else(|| {
                                (connection.clone(), json!({}), Observation::default())
                            });
                        let canonical = state.db.snapshot();
                        let current = canonical
                            .provider_connections
                            .iter()
                            .find(|entry| entry.id == prepared.id);
                        if current.is_some_and(|current| {
                            current.is_active.unwrap_or(true)
                                && connection_credential_generation(current)
                                    == connection_credential_generation(&prepared)
                        }) {
                            owner.complete(&prepared, version, &result, observation);
                        } else {
                            owner.discard(&connection.id, version);
                        }
                    })
                    .abort_handle(),
                );
            }
        }
        for connection in &connections {
            let entry = entries
                .accounts
                .get_mut(&connection.id)
                .expect("registered account");
            entry.limits.refreshing = entry.task.is_some();
            entry.limits.next_refresh_at = if usage::supports_quota(connection) {
                chrono::Duration::from_std(entry.next_attempt.saturating_duration_since(now))
                    .ok()
                    .and_then(|delay| chrono::Utc::now().checked_add_signed(delay))
                    .map(|date| date.to_rfc3339())
            } else {
                None
            };
        }
        (
            connections
                .iter()
                .map(|connection| entries.accounts[&connection.id].limits.clone())
                .collect(),
            truncated,
        )
    }

    fn discard(&self, id: &str, version: u64) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries
            .accounts
            .get_mut(id)
            .filter(|entry| entry.version == version)
        {
            entry.task = None;
        }
    }

    fn complete(
        &self,
        connection: &ProviderConnection,
        version: u64,
        result: &Value,
        observation: Observation,
    ) {
        let mut entries = self.entries.lock();
        let Some(entry) = entries
            .accounts
            .get_mut(&connection.id)
            .filter(|entry| entry.version == version)
        else {
            return;
        };
        entry.task = None;
        entry.generation = connection_credential_generation(connection);
        publish(entry, result, observation);
    }

    /// Reuse successful observations already paid for by dashboard/auto-ping.
    /// Their failures must not cancel collectors or slide the retry deadline.
    /// No snapshot slot is created until a client has requested it.
    pub(crate) fn observe(
        &self,
        state: &AppState,
        connection: &ProviderConnection,
        result: &Value,
    ) {
        if project_quotas(result, &connection.provider)
            .as_object()
            .is_none_or(|quotas| quotas.is_empty())
        {
            return;
        }
        let canonical = state.db.snapshot();
        if !canonical.provider_connections.iter().any(|current| {
            current.id == connection.id
                && current.is_active.unwrap_or(true)
                && connection_credential_generation(current)
                    == connection_credential_generation(connection)
        }) {
            return;
        }
        let mut entries = self.entries.lock();
        if entries.stopped {
            return;
        }
        let Some(entry) = entries.accounts.get_mut(&connection.id) else {
            return;
        };
        if let Some(task) = entry.task.take() {
            task.abort();
        }
        entry.version += 1;
        if entry.generation != connection_credential_generation(connection) {
            entry.observed = None;
            entry.limits.observed_at = None;
            entry.limits.quotas = json!({});
            entry.limits.plan = None;
            entry.failures = 0;
        }
        entry.generation = connection_credential_generation(connection);
        publish(entry, result, Observation::default());
    }

    pub(crate) fn shutdown(&self) {
        let mut entries = self.entries.lock();
        entries.stopped = true;
        for entry in entries.accounts.values_mut() {
            if let Some(task) = entry.task.take() {
                task.abort();
            }
        }
        entries.accounts.clear();
    }
}

async fn fetch(
    state: &AppState,
    connection: &ProviderConnection,
) -> Option<(ProviderConnection, Value, Observation)> {
    let mut connection = if connection.auth_type == "oauth" {
        usage::refresh_oauth_connection(state, connection, false)
            .await
            .ok()?
    } else {
        connection.clone()
    };
    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);
    let client = state
        .client_pool
        .get_or_insert_with(
            &format!("quota:{}", connection.provider),
            proxy.as_ref(),
            || {
                let mut builder = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(FETCH_TIMEOUT)
                    .pool_idle_timeout(crate::core::executor::CLIENT_POOL_IDLE_TIMEOUT);
                if let Some(proxy) = proxy.as_ref().filter(|proxy| !proxy.url.is_empty()) {
                    builder = builder.proxy(
                        reqwest::Proxy::all(&proxy.url)?
                            .no_proxy(reqwest::NoProxy::from_string(&proxy.no_proxy)),
                    );
                }
                builder.build().map(Arc::new)
            },
        )
        .ok()?;
    let (mut result, mut observation) = fetch_with_client(
        (*client).clone(),
        usage::fetch_connection_quota(&connection),
    )
    .await;
    if connection.auth_type == "oauth"
        && observation.status == 401
        && connection.refresh_token.is_some()
        && result
            .get("quotas")
            .and_then(Value::as_object)
            .is_none_or(|quotas| quotas.is_empty())
    {
        connection = usage::refresh_oauth_connection(state, &connection, true)
            .await
            .ok()?;
        (result, observation) = fetch_with_client(
            (*client).clone(),
            usage::fetch_connection_quota(&connection),
        )
        .await;
    }
    Some((connection, result, observation))
}

fn publish(entry: &mut Entry, result: &Value, observation: Observation) {
    let quotas = project_quotas(result, &entry.limits.provider);
    let now = Instant::now();
    if quotas.as_object().is_some_and(|quotas| !quotas.is_empty()) {
        entry.observed = Some(now);
        entry.limits.observed_at = Some(chrono::Utc::now().to_rfc3339());
        entry.limits.quotas = quotas;
        entry.limits.plan = if matches!(entry.limits.provider.as_str(), "github" | "github-copilot")
        {
            None
        } else {
            result.get("plan").and_then(Value::as_str).map(clean_string)
        };
        entry.limits.status = "fresh";
        entry.limits.error = None;
        entry.limits.error_status = None;
        entry.failures = 0;
    } else {
        entry.failures = (entry.failures + 1).min(5);
        entry.limits.status = if entry.observed.is_some() {
            "stale"
        } else {
            "unavailable"
        };
        entry.limits.error = Some(match observation.status {
            200..=299 => "Invalid quota response",
            401 => "Quota authentication failed",
            403 => "Quota access denied",
            429 => "Quota rate limited",
            500..=599 => "Quota provider unavailable",
            _ => "Quota request failed",
        });
        entry.limits.error_status = (100..=599)
            .contains(&observation.status)
            .then_some(observation.status);
    }
    let delay = REFRESH_SECONDS * (1 << entry.failures.saturating_sub(1));
    // Check addition so an invalid Retry-After cannot overflow Instant.
    entry.next_attempt = now
        .checked_add(Duration::from_secs(delay.max(observation.retry_after)))
        .unwrap_or(now + Duration::from_secs(delay));
}

fn clean_string(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect()
}

fn project_quotas(result: &Value, provider: &str) -> Value {
    let mut quotas = Map::new();
    if let Some(rows) = result.get("quotas").and_then(Value::as_object) {
        for (label, quota) in rows.iter().take(32) {
            if !quota.is_object() {
                continue;
            }
            // These legacy dashboard rows describe balances, not a replenishing
            // percentage allowance; preserve the amount without an "unlimited" claim.
            if provider == "vercel-ai-gateway" && label == "Used (USD)" {
                continue;
            }
            let mut row = Map::new();
            for key in ["used", "total", "remaining", "remainingPercentage"] {
                let number = quota
                    .get(key)
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(|value| {
                        if key == "remainingPercentage" {
                            value.min(100.0)
                        } else {
                            value
                        }
                    });
                row.insert(key.into(), json!(number));
            }
            let reset = quota
                .get("resetAt")
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.to_rfc3339());
            row.insert("resetAt".into(), json!(reset));
            row.insert(
                "unlimited".into(),
                json!(quota
                    .get("unlimited")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)),
            );
            if let Some(unit) = quota.get("unit").and_then(Value::as_str) {
                row.insert("unit".into(), json!(clean_string(unit)));
            }
            if provider == "deepseek" || provider == "vercel-ai-gateway" {
                let amount = if provider == "deepseek" {
                    row["total"].clone()
                } else {
                    row["remaining"].clone()
                };
                for key in ["used", "total", "remainingPercentage"] {
                    row.insert(key.into(), Value::Null);
                }
                row.insert("remaining".into(), amount);
                row.insert("unlimited".into(), json!(false));
                let unit = if provider == "deepseek" {
                    label
                        .strip_prefix("Balance (")
                        .and_then(|label| label.strip_suffix(')'))
                        .unwrap_or("")
                } else {
                    "USD"
                };
                row.insert("unit".into(), json!(clean_string(unit)));
            }
            let label = clean_string(label);
            if !label.trim().is_empty() {
                quotas.insert(label, Value::Object(row));
            }
        }
    }
    Value::Object(quotas)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::types::ApiKey;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    async fn state() -> (tempfile::TempDir, AppState, ProviderConnection) {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::load_from(directory.path()).await.unwrap());
        // Missing credentials exercise the collector without external traffic.
        let connection = ProviderConnection {
            id: "private-account-id".into(),
            provider: "commandcode".into(),
            auth_type: "apikey".into(),
            name: Some("private account name".into()),
            email: Some("private@example.invalid".into()),
            is_active: Some(true),
            ..Default::default()
        };
        db.update(|db| {
            db.settings.require_api_key = false;
            db.settings.require_login = false;
            db.api_keys.push(ApiKey {
                id: "reader".into(),
                key: "reader-fixture".into(),
                ..Default::default()
            });
            db.provider_connections.push(connection.clone());
        })
        .await
        .unwrap();
        (directory, AppState::new(db), connection)
    }

    fn quotas() -> Value {
        json!({"plan":"fixture", "quotas":{"5h": {
            "used":25, "total":100, "remaining":75, "remainingPercentage":75,
            "resetAt":"2026-12-01T00:00:00Z", "unlimited":false,
            "private": "must-not-be-published"
        }}, "message":"private upstream response"})
    }

    #[tokio::test(start_paused = true)]
    async fn readers_share_collectors_and_success_error_and_rotation_deadlines() {
        let (_directory, state, connection) = state().await;
        let owner = state.quota_snapshots.clone();
        let readers: Vec<_> = (0..20)
            .map(|_| {
                let state = state.clone();
                tokio::spawn(async move {
                    state.quota_snapshots.read(&state);
                })
            })
            .collect();
        for reader in readers {
            reader.await.unwrap();
        }
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        {
            let entries = owner.entries.lock();
            let entry = &entries.accounts[&connection.id];
            assert_eq!(
                entry.version, 1,
                "twenty readers start one collector, even on failure"
            );
            assert_eq!(entry.failures, 1);
        }
        owner.observe(&state, &connection, &quotas());
        let version = owner.entries.lock().accounts[&connection.id].version;
        for _ in 0..20 {
            assert_eq!(owner.read(&state).0[0].status, "fresh");
        }
        tokio::time::advance(Duration::from_secs(179)).await;
        owner.read(&state);
        assert_eq!(
            owner.entries.lock().accounts[&connection.id].version,
            version
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(owner.read(&state).0[0].status, "stale");
        let version = owner.entries.lock().accounts[&connection.id].version;
        assert_eq!(version, 3);
        owner.complete(
            &connection,
            version,
            &json!({}),
            Observation {
                status: 429,
                retry_after: 600,
            },
        );
        assert_eq!(owner.read(&state).0[0].quotas["5h"]["used"], 25.0);
        tokio::time::advance(Duration::from_secs(599)).await;
        owner.read(&state);
        assert_eq!(
            owner.entries.lock().accounts[&connection.id].version,
            version
        );
        let mut rotated = connection.clone();
        rotated
            .provider_specific_data
            .insert("accountId".into(), json!("changed-account"));
        state
            .db
            .update(|db| db.provider_connections[0] = rotated.clone())
            .await
            .unwrap();
        let row = &owner.read(&state).0[0];
        assert!(row.quotas.as_object().unwrap().is_empty());
        assert_eq!(row.observed_at, None);
        // A late prior-generation operation cannot republish old account data.
        owner.complete(&connection, version, &quotas(), Observation::default());
        assert!(owner.read(&state).0[0]
            .quotas
            .as_object()
            .unwrap()
            .is_empty());
        state
            .db
            .update(|db| db.provider_connections.clear())
            .await
            .unwrap();
        assert!(owner.read(&state).0.is_empty());
        assert!(owner.entries.lock().accounts.is_empty());
        state.signal_shutdown();
        assert_eq!(owner.permits.available_permits(), 4);
    }

    // Finish a scheduled collector with fixture data, without live quota calls.
    fn finish(
        owner: &QuotaSnapshots,
        connection: &ProviderConnection,
        result: &Value,
        observation: Observation,
    ) {
        let version = {
            let mut entries = owner.entries.lock();
            let entry = entries.accounts.get_mut(&connection.id).unwrap();
            entry.task.take().unwrap().abort();
            entry.version
        };
        owner.complete(connection, version, result, observation);
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_hour_refreshes_on_schedule_without_errors() {
        let (_directory, state, connection) = state().await;
        let owner = &state.quota_snapshots;
        let mut attempts = 0;
        for minute in 0..=60 {
            let row = owner.read(&state).0.remove(0);
            assert_eq!(row.refreshing, minute % 3 == 0);
            assert_eq!(row.error, None, "TTL expiry is not a fetch failure");
            assert_eq!(row.error_status, None);
            assert!(
                chrono::DateTime::parse_from_rfc3339(row.next_refresh_at.as_deref().unwrap())
                    .is_ok()
            );
            if row.refreshing {
                attempts += 1;
                finish(owner, &connection, &quotas(), Observation::default());
            }
            let row = owner.read(&state).0.remove(0);
            assert_eq!(row.status, "fresh");
            assert!(!row.refreshing);
            assert_eq!(row.error, None);
            assert_eq!(row.quotas["5h"]["used"], 25.0);
            tokio::time::advance(Duration::from_secs(60)).await;
        }
        assert_eq!(
            attempts, 21,
            "readers do not increase the 180-second refresh rate"
        );
        state.signal_shutdown();
    }

    #[tokio::test(start_paused = true)]
    async fn external_failures_neither_cancel_collectors_nor_slide_recovery() {
        let (_directory, state, connection) = state().await;
        let owner = &state.quota_snapshots;
        owner.read(&state);
        let version = owner.entries.lock().accounts[&connection.id].version;
        // An external failure cannot cancel an otherwise successful collector.
        owner.observe(&state, &connection, &json!({"message":"private failure"}));
        assert_eq!(
            owner.entries.lock().accounts[&connection.id].version,
            version
        );
        assert!(!owner.entries.lock().accounts[&connection.id]
            .task
            .as_ref()
            .unwrap()
            .is_finished());
        finish(owner, &connection, &quotas(), Observation::default());
        tokio::time::advance(Duration::from_secs(180)).await;
        owner.read(&state);
        finish(
            owner,
            &connection,
            &json!({}),
            Observation {
                status: 429,
                retry_after: 600,
            },
        );
        let (version, deadline) = {
            let entries = owner.entries.lock();
            let entry = &entries.accounts[&connection.id];
            (entry.version, entry.next_attempt)
        };
        for _ in 0..10 {
            owner.observe(
                &state,
                &connection,
                &json!({"message":"another private failure"}),
            );
            let row = owner.read(&state).0.remove(0);
            assert_eq!(row.status, "stale");
            assert_eq!(row.error, Some("Quota rate limited"));
            assert_eq!(row.error_status, Some(429));
            assert_eq!(row.quotas["5h"]["used"], 25.0);
            assert!(!row.refreshing);
            let entries = owner.entries.lock();
            let entry = &entries.accounts[&connection.id];
            assert_eq!(entry.version, version);
            assert_eq!(entry.failures, 1);
            assert_eq!(entry.next_attempt, deadline);
            drop(entries);
            tokio::time::advance(Duration::from_secs(60)).await;
        }
        let row = owner.read(&state).0.remove(0);
        assert!(
            row.refreshing,
            "retry starts at the original deadline despite recurring external errors"
        );
        assert_eq!(row.error_status, Some(429));
        finish(owner, &connection, &quotas(), Observation::default());
        let row = owner.read(&state).0.remove(0);
        assert_eq!(row.status, "fresh");
        assert_eq!(row.error, None);
        assert_eq!(row.error_status, None);
        assert!(!row.refreshing);
        assert_eq!(owner.entries.lock().accounts[&connection.id].failures, 0);
        state.signal_shutdown();
    }

    #[tokio::test]
    async fn endpoint_requires_an_inference_key_and_projects_only_safe_fields() {
        let (_directory, state, connection) = state().await;
        let app = usage::snapshot_routes().with_state(state.clone());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/usage/limits")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "auth remains required when login/inference auth is disabled"
        );
        state.quota_snapshots.read(&state);
        state
            .quota_snapshots
            .observe(&state, &connection, &quotas());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/usage/limits")
                    .header("authorization", "Bearer reader-fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        for private in [
            "private-account-id",
            "private account name",
            "private@example.invalid",
            "must-not-be-published",
            "private upstream response",
        ] {
            assert!(!text.contains(private));
        }
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["refreshIntervalSeconds"], 180);
        assert_eq!(body["accounts"][0]["status"], "fresh");
        assert_eq!(body["accounts"][0]["quotas"]["5h"]["used"], 25.0);
        assert_eq!(body["accounts"][0]["refreshing"], false);
        assert!(body["accounts"][0]["errorStatus"].is_null());
        assert!(chrono::DateTime::parse_from_rfc3339(
            body["accounts"][0]["nextRefreshAt"].as_str().unwrap()
        )
        .is_ok());
        let version = state.quota_snapshots.entries.lock().accounts[&connection.id].version;
        state.quota_snapshots.complete(
            &connection,
            version,
            &json!({"message":"private upstream failure"}),
            Observation {
                status: 429,
                retry_after: 600,
            },
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/usage/limits")
                    .header("authorization", "Bearer reader-fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        assert!(!std::str::from_utf8(&body)
            .unwrap()
            .contains("private upstream failure"));
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["accounts"][0]["status"], "stale");
        assert_eq!(body["accounts"][0]["error"], "Quota rate limited");
        assert_eq!(body["accounts"][0]["errorStatus"], 429);
        assert_eq!(body["accounts"][0]["quotas"]["5h"]["used"], 25.0);
        state.signal_shutdown();
    }

    #[tokio::test]
    async fn bounded_admission_accounts_and_shutdown() {
        let (_directory, state, connection) = state().await;
        state
            .db
            .update(|db| {
                db.provider_connections = (0..130)
                    .map(|index| ProviderConnection {
                        id: format!("account-{index}"),
                        ..connection.clone()
                    })
                    .collect();
            })
            .await
            .unwrap();
        let owner = &state.quota_snapshots;
        let permits: Vec<_> = (0..4)
            .map(|_| owner.permits.clone().try_acquire_owned().unwrap())
            .collect();
        let (accounts, truncated) = owner.read(&state);
        assert_eq!(accounts.len(), MAX_ACCOUNTS);
        assert!(truncated);
        assert!(owner
            .entries
            .lock()
            .accounts
            .values()
            .all(|entry| entry.task.is_none()));
        drop(permits);
        owner.read(&state);
        assert_eq!(owner.permits.available_permits(), 0);
        assert_eq!(
            owner
                .entries
                .lock()
                .accounts
                .values()
                .filter(|entry| entry.task.is_some())
                .count(),
            4
        );
        state.signal_shutdown();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(owner.permits.available_permits(), 4);
        assert!(owner.read(&state).0.is_empty());
    }

    #[test]
    fn projection_retains_unknowns_and_currency_balances() {
        let unknown = project_quotas(&json!({"quotas":{"5h":{}}}), "codex");
        assert!(unknown["5h"]["used"].is_null());
        let balance = project_quotas(
            &json!({"quotas":{"Balance (USD)": {
                "used":0, "total":12.5, "remainingPercentage":100, "unlimited":true
            }}}),
            "deepseek",
        );
        assert_eq!(balance["Balance (USD)"]["remaining"], 12.5);
        assert_eq!(balance["Balance (USD)"]["unit"], "USD");
        assert_eq!(balance["Balance (USD)"]["unlimited"], false);
        assert!(balance["Balance (USD)"]["remainingPercentage"].is_null());
    }

    async fn claude_state(
        snapshot: Option<Value>,
    ) -> (tempfile::TempDir, AppState, ProviderConnection) {
        claude_state_with_plan(snapshot, None).await
    }

    async fn claude_state_with_plan(
        snapshot: Option<Value>,
        subscription: Option<&str>,
    ) -> (tempfile::TempDir, AppState, ProviderConnection) {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::load_from(directory.path()).await.unwrap());
        let mut connection = ProviderConnection {
            id: "claude-fixture".into(),
            provider: "claude".into(),
            auth_type: "oauth".into(),
            access_token: Some("stored-access-token".into()),
            is_active: Some(true),
            ..Default::default()
        };
        if let Some(subscription) = subscription {
            connection
                .provider_specific_data
                .insert("subscription".into(), json!(subscription));
        }
        db.update(|db| {
            db.settings.require_api_key = false;
            db.settings.require_login = false;
            db.provider_connections.push(connection.clone());
            if let Some(snapshot) = snapshot {
                db.settings
                    .extra
                    .insert("claudeQuotaSnapshot:claude-fixture".into(), snapshot);
            }
        })
        .await
        .unwrap();
        (directory, AppState::new(db), connection)
    }

    fn passive_snapshot() -> Value {
        json!({
            "quotas": {
                "session (5h)": {
                    "used": 0.0, "total": 100.0, "remaining": 100.0,
                    "remainingPercentage": 100.0, "unlimited": false,
                    "resetAt": "2026-10-07T22:50:00+00:00"
                },
                "weekly (7d)": {
                    "used": 22.0, "total": 100.0, "remaining": 78.0,
                    "remainingPercentage": 78.0, "unlimited": false,
                    "resetAt": "2026-10-10T19:00:00+00:00"
                }
            },
            "observedAt": "2026-10-07T17:53:37Z"
        })
    }

    #[tokio::test(start_paused = true)]
    async fn claude_passive_snapshot_is_served_without_a_collector() {
        let (_directory, state, connection) =
            claude_state_with_plan(Some(passive_snapshot()), Some("Max x5")).await;
        let (accounts, _truncated) = state.quota_snapshots.read(&state);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].provider, "claude");
        assert_eq!(accounts[0].status, "fresh");
        assert_eq!(accounts[0].plan.as_deref(), Some("Max x5"));
        assert_eq!(
            accounts[0].observed_at.as_deref(),
            Some("2026-10-07T17:53:37Z")
        );
        assert_eq!(accounts[0].quotas["session (5h)"]["used"], 0.0);
        assert_eq!(accounts[0].quotas["weekly (7d)"]["used"], 22.0);
        assert_eq!(
            accounts[0].quotas["weekly (7d)"]["remainingPercentage"],
            78.0
        );
        assert_eq!(
            accounts[0].quotas["session (5h)"]["resetAt"],
            "2026-10-07T22:50:00+00:00"
        );
        // Passive observation never spawns a usage-endpoint collector.
        assert!(
            state.quota_snapshots.entries.lock().accounts[&connection.id]
                .task
                .is_none()
        );
        assert_eq!(accounts[0].next_refresh_at, None);
        assert!(!accounts[0].refreshing);
        state.signal_shutdown();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn claude_without_a_snapshot_waits_for_live_traffic() {
        let (_directory, state, _connection) = claude_state(None).await;
        let (accounts, _truncated) = state.quota_snapshots.read(&state);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].status, "loading");
        assert!(accounts[0]
            .quotas
            .as_object()
            .is_some_and(|quotas| quotas.is_empty()));
        assert_eq!(accounts[0].observed_at, None);
        // A snapshot whose windows project to nothing is no better than none.
        let empty = json!({"quotas": {}, "observedAt": "2026-10-07T17:53:37Z"});
        state
            .db
            .update(|db| {
                db.settings
                    .extra
                    .insert("claudeQuotaSnapshot:claude-fixture".into(), empty);
            })
            .await
            .unwrap();
        let (accounts, _truncated) = state.quota_snapshots.read(&state);
        assert_eq!(accounts[0].status, "loading");
        assert!(accounts[0]
            .quotas
            .as_object()
            .is_some_and(|quotas| quotas.is_empty()));
        state.signal_shutdown();
    }

    #[tokio::test(start_paused = true)]
    async fn claude_subscription_plan_renders_before_any_traffic() {
        // The login-time profile tier must show up on the sidebar even when
        // no quota snapshot exists yet — the plan does not depend on traffic.
        let (_directory, state, _connection) = claude_state_with_plan(None, Some("Max x5")).await;
        let (accounts, _truncated) = state.quota_snapshots.read(&state);
        assert_eq!(accounts[0].status, "loading");
        assert_eq!(accounts[0].plan.as_deref(), Some("Max x5"));
        state.signal_shutdown();
    }
}
