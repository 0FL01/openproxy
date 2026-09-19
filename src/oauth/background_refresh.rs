//! Background proactive OAuth token-refresh scheduler — port of 9router
//! `src/sse/services/backgroundTokenRefresh.js`.
//!
//! Independent of inbound requests. Fail-open everywhere: tick errors and
//! per-connection failures never kill the interval.
//!
//! - Tick every 5 minutes, first pass after 10 seconds.
//! - Select active OAuth connections with a refresh token whose access token
//!   expires within `max(provider lead, BACKGROUND_REFRESH_LEAD_MS)` (30 min).
//! - Join the same connection/generation refresh coordinator used by the
//!   request path. The coordinator persists before admitting another refresh.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::oauth::token_refresh::{
    connection_credential_generation, CONNECTION_REFRESH_COORDINATOR,
};

/// Refresh when expiry is within 30 minutes (or the provider on-request
/// lead, whichever is larger) — JS BACKGROUND_REFRESH_LEAD_MS.
pub const BACKGROUND_REFRESH_LEAD_MS: u64 = 30 * 60 * 1000;
const TICK_INTERVAL_SECS: u64 = 5 * 60;
const INITIAL_DELAY_SECS: u64 = 10;

static TICK_RUNNING: AtomicBool = AtomicBool::new(false);

/// Per-provider on-request refresh lead (JS getRefreshLeadMs). Providers not
/// listed fall back to BACKGROUND_REFRESH_LEAD_MS alone.
fn provider_lead_ms(provider: &str) -> Option<u64> {
    use crate::oauth::token_refresh as tr;
    let lead = match provider {
        "codex" | "opencode" | "cx" => tr::REFRESH_LEAD_CODEX_MS,
        "openai" => tr::REFRESH_LEAD_OPENAI_MS,
        "claude" | "anthropic" => tr::REFRESH_LEAD_CLAUDE_MS,
        "kimi-coding" | "kimi" => tr::REFRESH_LEAD_KIMI_CODING_MS,
        "antigravity" | "gemini" => tr::REFRESH_LEAD_ANTIGRAVITY_MS,
        "xai" => tr::REFRESH_LEAD_XAI_MS,
        _ => return None,
    };
    Some(lead)
}

/// Pure selection: OAuth connections with a refreshToken whose access token
/// expires within max(provider lead, BACKGROUND_REFRESH_LEAD_MS).
/// Mirrors JS selectConnectionsNeedingRefresh.
pub fn select_connections_needing_refresh(
    connections: &[crate::types::ProviderConnection],
    now_ms: i64,
) -> Vec<crate::types::ProviderConnection> {
    connections
        .iter()
        .filter(|conn| {
            if !conn.is_active() {
                return false;
            }
            let auth_type = conn.auth_type.to_ascii_lowercase().replace('_', "");
            if auth_type != "oauth" {
                return false;
            }
            let Some(refresh_token) = conn.refresh_token.as_deref().filter(|r| !r.is_empty())
            else {
                return false;
            };
            let _ = refresh_token;
            let Some(expires_at) = conn.expires_at.as_deref() else {
                return false;
            };
            let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
                return false;
            };
            let expires_at_ms = expires_at.timestamp_millis();
            let lead = provider_lead_ms(&conn.provider)
                .unwrap_or(0)
                .max(BACKGROUND_REFRESH_LEAD_MS);
            expires_at_ms - now_ms < lead as i64
        })
        .cloned()
        .collect()
}

/// One scheduler tick. Fail-open at top level and per connection.
#[doc(hidden)]
pub async fn run_tick(state: &crate::server::state::AppState) {
    if TICK_RUNNING.swap(true, Ordering::SeqCst) {
        tracing::debug!(target: "openproxy::bg_token_refresh", "tick already running, skip");
        return;
    }
    let _guard = TickGuard;

    let snapshot = state.db.snapshot();
    let due = select_connections_needing_refresh(&snapshot.provider_connections, now_ms());
    if due.is_empty() {
        return;
    }
    tracing::info!(target: "openproxy::bg_token_refresh", "refreshing {} due OAuth connection(s)", due.len());

    for conn in &due {
        if conn.refresh_token.is_none() {
            continue;
        }
        let observed_generation = connection_credential_generation(conn);
        match CONNECTION_REFRESH_COORDINATOR
            .refresh_connection(
                state.db.clone(),
                &conn.provider,
                &conn.id,
                observed_generation,
            )
            .await
        {
            Ok(_) => {
                tracing::info!(target: "openproxy::bg_token_refresh",
                    "connection {} ({}) refreshed", conn.id, conn.provider);
            }
            Err(e) => {
                // Fail-open: log and move on.
                tracing::warn!(target: "openproxy::bg_token_refresh",
                    "connection {} ({}) refresh failed: {e}", conn.id, conn.provider);
            }
        }
    }
}

struct TickGuard;
impl Drop for TickGuard {
    fn drop(&mut self) {
        TICK_RUNNING.store(false, Ordering::SeqCst);
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Spawn the scheduler loop (JS startBackgroundTokenRefresh): initial pass
/// after 10s, then every 5 minutes. Never returns.
pub fn spawn_background_token_refresh(state: std::sync::Arc<crate::server::state::AppState>) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(INITIAL_DELAY_SECS)).await;
        loop {
            run_tick(&state).await;
            tokio::time::sleep(std::time::Duration::from_secs(TICK_INTERVAL_SECS)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderConnection;

    fn conn(
        provider: &str,
        auth_type: &str,
        refresh: Option<&str>,
        expires_in_secs: i64,
    ) -> ProviderConnection {
        ProviderConnection {
            provider: provider.to_string(),
            auth_type: auth_type.to_string(),
            refresh_token: refresh.map(String::from),
            expires_at: Some(
                (chrono::Utc::now() + chrono::Duration::seconds(expires_in_secs)).to_rfc3339(),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn selects_oauth_conn_expiring_within_30min() {
        let conns = vec![
            conn("claude", "oauth", Some("rt"), 10 * 60), // 10 min → due
            conn("claude", "oauth", Some("rt"), 6 * 60 * 60), // 6 h → not due
            conn("claude", "apikey", Some("rt"), 10 * 60), // wrong auth type
            conn("claude", "oauth", None, 10 * 60),       // no refresh token
        ];
        let due = select_connections_needing_refresh(&conns, now_ms());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].provider, "claude");
    }

    #[test]
    fn provider_lead_extends_window() {
        // codex lead is 5 days → a connection expiring in 4 days IS due.
        let conns = vec![conn("codex", "oauth", Some("rt"), 4 * 24 * 60 * 60)];
        assert_eq!(
            select_connections_needing_refresh(&conns, now_ms()).len(),
            1
        );
        // claude lead is 4h > the 30-min floor → max() wins; a connection
        // expiring in 2h IS due (window = max(lead, floor)).
        let conns = vec![conn("claude", "oauth", Some("rt"), 2 * 60 * 60)];
        assert_eq!(
            select_connections_needing_refresh(&conns, now_ms()).len(),
            1
        );
        // A connection expiring beyond claude's 4h lead is NOT due.
        let conns = vec![conn("claude", "oauth", Some("rt"), 6 * 60 * 60)];
        assert!(select_connections_needing_refresh(&conns, now_ms()).is_empty());
    }

    #[test]
    fn inactive_or_missing_expiry_skipped() {
        let mut inactive = conn("claude", "oauth", Some("rt"), 60);
        inactive.is_active = Some(false);
        let no_expiry = ProviderConnection {
            provider: "claude".into(),
            auth_type: "oauth".into(),
            refresh_token: Some("rt".into()),
            expires_at: None,
            ..Default::default()
        };
        let conns = vec![inactive, no_expiry];
        assert!(select_connections_needing_refresh(&conns, now_ms()).is_empty());
    }
}
