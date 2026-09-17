//! Token refresh and connection-scoped coordination.
//!
//! Provides connection/generation-scoped singleflight plus the provider wire
//! functions that call each upstream token-refresh API.
//!
//! # Coordination guarantees
//!
//! Every configured caller enters through [`ConnectionRefreshCoordinator`].
//! It stores only active operations keyed by configured provider/connection
//! identity; a credential-generation digest prevents stale callers from using
//! or overwriting a newer token pair. Current waiters share one result, and the
//! active entry is removed immediately after publication. There is no cache of
//! completed refresh results and no full access or refresh token in a map key.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::db::Db;
use crate::types::ProviderConnection;

use super::TOKEN_EXPIRY_BUFFER_MS;

// ---------------------------------------------------------------------------
// Retry with jittered backoff
// ---------------------------------------------------------------------------

/// Constants for the retry loop.
const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: u64 = 500;
const MAX_DELAY_MS: u64 = 5_000;

/// Retry `refresh_fn` up to `MAX_RETRIES` times with jittered exponential
/// backoff.  Only retries transient-looking errors (network / 5xx); permanent
/// errors (4xx) are returned immediately.
pub async fn refresh_with_retry<F, Fut>(refresh_fn: F) -> Result<RefreshResult, String>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<RefreshResult, String>>,
{
    let mut last_err = String::new();

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay = jittered_delay(attempt);
            tokio::time::sleep(delay).await;
        }

        match refresh_fn().await {
            Ok(result) => return Ok(result),
            Err(err) => {
                let is_transient = refresh_error_is_transient(&err);
                if !is_transient {
                    return Err(err);
                }
                last_err = err;
            }
        }
    }

    Err(format!(
        "token refresh failed after {MAX_RETRIES} attempts: {last_err}"
    ))
}

fn refresh_error_is_transient(error: &str) -> bool {
    let status = error
        .split_once("HTTP ")
        .and_then(|(_, suffix)| suffix.get(..3))
        .and_then(|value| value.parse::<u16>().ok());

    match status {
        Some(429) => true,
        Some(400..=499) => false,
        Some(500..=599) => true,
        Some(_) => false,
        None => true,
    }
}

/// Jittered exponential backoff: `BASE * 2^(attempt-1) + random(0, BASE/2)`.
fn jittered_delay(attempt: u32) -> Duration {
    let base = BASE_DELAY_MS * (1u64 << (attempt.saturating_sub(1)));
    let capped = base.min(MAX_DELAY_MS);
    let jitter = rand::random::<u64>() % (BASE_DELAY_MS / 2);
    Duration::from_millis(capped + jitter)
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of a successful (or failed) token refresh.
#[derive(Debug, Clone)]
pub struct RefreshResult {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

impl RefreshResult {
    /// Wrap an access-token-only response (no refresh_token, no expires_in).
    pub fn access_only(access_token: String) -> Self {
        Self {
            access_token,
            refresh_token: None,
            expires_in: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Connection-scoped refresh coordination (C16)
// ---------------------------------------------------------------------------

/// Opaque fingerprint of the canonical credential generation for one
/// configured connection. The coordinator retains this digest, never the old
/// access or refresh token that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialGeneration([u8; 32]);

/// Compute a deterministic generation from fields that can affect OAuth
/// refresh or authorization. Provider-specific data is included because some
/// refresh protocols select an endpoint/client/device from that map.
pub fn connection_credential_generation(connection: &ProviderConnection) -> CredentialGeneration {
    let encoded = serde_json::to_vec(&(
        &connection.id,
        &connection.provider,
        &connection.auth_type,
        &connection.access_token,
        &connection.refresh_token,
        &connection.expires_at,
        &connection.expires_in,
        &connection.created_at,
        &connection.updated_at,
        &connection.token_type,
        &connection.scope,
        &connection.id_token,
        &connection.project_id,
        &connection.api_key,
        &connection.provider_specific_data,
    ))
    .expect("provider credentials are JSON serializable");
    let digest = Sha256::digest(encoded);
    CredentialGeneration(digest.into())
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoordinatedRefreshResult {
    /// Canonical, published connection state after the operation.
    pub connection: ProviderConnection,
    /// True only when this operation persisted the returned refresh result.
    /// A stale caller instead receives the already-current canonical state.
    pub refreshed: bool,
}

type SharedRefreshResult = Result<CoordinatedRefreshResult, String>;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ConnectionRefreshKey {
    provider: String,
    connection_id: String,
}

struct ActiveConnectionRefresh {
    result: AsyncMutex<Option<SharedRefreshResult>>,
    completed: Notify,
}

impl ActiveConnectionRefresh {
    fn new() -> Self {
        Self {
            result: AsyncMutex::new(None),
            completed: Notify::new(),
        }
    }

    async fn wait(&self) -> SharedRefreshResult {
        loop {
            let completed = self.completed.notified();
            if let Some(result) = self.result.lock().await.clone() {
                return result;
            }
            completed.await;
        }
    }

    async fn finish(&self, result: SharedRefreshResult) {
        *self.result.lock().await = Some(result);
        self.completed.notify_waiters();
    }
}

/// In-flight-only refresh singleflight keyed by configured connection.
///
/// The map contains no completed results and is empty while idle. A detached
/// operation owns the upstream result through persistence, so cancelling any
/// waiter cannot lose a token pair after the provider has returned it. This is
/// process-local coordination, not an exactly-once promise: process death or
/// an uncertain network result can still require reauthorization.
pub struct ConnectionRefreshCoordinator {
    active: Mutex<HashMap<ConnectionRefreshKey, Arc<ActiveConnectionRefresh>>>,
    accepting: AtomicBool,
    idle: Notify,
}

impl ConnectionRefreshCoordinator {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            accepting: AtomicBool::new(true),
            idle: Notify::new(),
        }
    }

    /// Refresh one canonical connection generation. Concurrent callers for
    /// the same configured connection share one active result, including an
    /// error. Different connections never share a lock while provider HTTP is
    /// in progress.
    pub async fn refresh<F, Fut>(
        self: &Arc<Self>,
        db: Arc<Db>,
        provider: &str,
        connection_id: &str,
        observed_generation: CredentialGeneration,
        refresh_fn: F,
    ) -> SharedRefreshResult
    where
        F: FnOnce(ProviderConnection) -> Fut + Send + 'static,
        Fut: Future<Output = Result<RefreshResult, String>> + Send + 'static,
    {
        if !self.accepting.load(Ordering::Acquire) {
            return Err("refresh coordinator is shutting down".to_string());
        }

        let key = ConnectionRefreshKey {
            provider: provider.to_string(),
            connection_id: connection_id.to_string(),
        };
        let (operation, starts_operation) = {
            let mut active = self.active.lock();
            if !self.accepting.load(Ordering::Acquire) {
                return Err("refresh coordinator is shutting down".to_string());
            }
            if let Some(operation) = active.get(&key) {
                (Arc::clone(operation), false)
            } else {
                let operation = Arc::new(ActiveConnectionRefresh::new());
                active.insert(key.clone(), Arc::clone(&operation));
                (operation, true)
            }
        };

        if starts_operation {
            let coordinator = Arc::clone(self);
            let operation_for_task = Arc::clone(&operation);
            tokio::spawn(async move {
                let worker = {
                    let coordinator = Arc::clone(&coordinator);
                    let db = Arc::clone(&db);
                    let key = key.clone();
                    async move {
                        coordinator
                            .run_refresh(db, &key, observed_generation, refresh_fn)
                            .await
                    }
                };
                let result = AssertUnwindSafe(worker)
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| Err("refresh operation panicked".to_string()));

                // Publish the shared result before removing the active entry.
                // A caller racing this boundary either joins this operation or
                // re-reads the newly published canonical generation.
                operation_for_task.finish(result).await;
                {
                    let mut active = coordinator.active.lock();
                    if active
                        .get(&key)
                        .is_some_and(|current| Arc::ptr_eq(current, &operation_for_task))
                    {
                        active.remove(&key);
                    }
                }
                coordinator.idle.notify_waiters();
            });
        }

        operation.wait().await
    }

    /// Production adapter for the existing provider refresh dispatcher.
    /// C16 exposes this seam without wiring callers; C17A/C17B migrate each
    /// caller only after their provider-specific behavior is covered.
    pub async fn refresh_connection(
        self: &Arc<Self>,
        db: Arc<Db>,
        provider: &str,
        connection_id: &str,
        observed_generation: CredentialGeneration,
    ) -> SharedRefreshResult {
        self.refresh_connection_with_token(db, provider, connection_id, observed_generation, None)
            .await
    }

    /// Refresh a configured connection while allowing an explicit manual
    /// refresh token. The configured connection still supplies identity,
    /// provider-specific refresh metadata, generation checks, and persistence.
    pub async fn refresh_connection_with_token(
        self: &Arc<Self>,
        db: Arc<Db>,
        provider: &str,
        connection_id: &str,
        observed_generation: CredentialGeneration,
        refresh_token_override: Option<String>,
    ) -> SharedRefreshResult {
        let refresh_provider = provider.to_string();
        self.refresh(
            db,
            provider,
            connection_id,
            observed_generation,
            move |connection| async move {
                let refresh_token = refresh_token_override
                    .as_deref()
                    .or(connection.refresh_token.as_deref())
                    .ok_or_else(|| "connection has no refresh token".to_string())?;
                dispatch_oauth_refresh(
                    &refresh_provider,
                    refresh_token,
                    &connection.provider_specific_data,
                )
                .await
            },
        )
        .await
    }

    async fn run_refresh<F, Fut>(
        &self,
        db: Arc<Db>,
        key: &ConnectionRefreshKey,
        observed_generation: CredentialGeneration,
        refresh_fn: F,
    ) -> SharedRefreshResult
    where
        F: FnOnce(ProviderConnection) -> Fut,
        Fut: Future<Output = Result<RefreshResult, String>>,
    {
        let canonical = canonical_connection(&db, key)?;
        let base_generation = connection_credential_generation(&canonical);
        if base_generation != observed_generation {
            return Ok(CoordinatedRefreshResult {
                connection: canonical,
                refreshed: false,
            });
        }

        let refreshed = refresh_fn(canonical).await?;
        let new_access = refreshed.access_token;
        let new_refresh = refreshed.refresh_token;
        let new_expires_at = refreshed
            .expires_in
            .map(|seconds| (chrono::Utc::now() + chrono::Duration::seconds(seconds)).to_rfc3339());
        let last_refresh_at = chrono::Utc::now().to_rfc3339();
        let updated_at = last_refresh_at.clone();
        let expires_in = refreshed.expires_in;
        let connection_id = key.connection_id.clone();
        let provider = key.provider.clone();
        let applied = Arc::new(AtomicBool::new(false));
        let applied_in_update = Arc::clone(&applied);

        let published = db
            .update(move |state| {
                let Some(connection) = state.provider_connections.iter_mut().find(|candidate| {
                    candidate.id == connection_id && candidate.provider == provider
                }) else {
                    return;
                };
                if connection_credential_generation(connection) != base_generation {
                    return;
                }

                connection.access_token = Some(new_access);
                if let Some(refresh_token) = new_refresh {
                    connection.refresh_token = Some(refresh_token);
                }
                if let Some(expires_at) = new_expires_at {
                    connection.expires_at = Some(expires_at);
                }
                if let Some(expires_in) = expires_in {
                    connection.expires_in = Some(expires_in);
                }
                connection.updated_at = Some(updated_at);
                connection
                    .provider_specific_data
                    .insert("lastRefreshAt".into(), Value::String(last_refresh_at));
                connection.last_error = None;
                connection.last_error_at = None;
                connection.error_code = None;
                connection.backoff_level = Some(0);
                applied_in_update.store(true, Ordering::Release);
            })
            .await
            .map_err(|error| format!("persist refreshed credentials: {error}"))?;

        let connection = published
            .provider_connections
            .iter()
            .find(|candidate| {
                candidate.id == key.connection_id && candidate.provider == key.provider
            })
            .cloned()
            .ok_or_else(|| {
                format!(
                    "connection {} was deleted while refresh was active",
                    key.connection_id
                )
            })?;

        Ok(CoordinatedRefreshResult {
            connection,
            refreshed: applied.load(Ordering::Acquire),
        })
    }

    pub fn active_count(&self) -> usize {
        self.active.lock().len()
    }

    /// Stop admitting operations and wait for every already-issued refresh to
    /// publish its result. The caller chooses the outer timeout; remote API
    /// exactly-once behavior is intentionally not claimed.
    pub async fn shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        loop {
            let idle = self.idle.notified();
            if self.active.lock().is_empty() {
                return;
            }
            idle.await;
        }
    }
}

impl Default for ConnectionRefreshCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared service prepared by C16. It has no background task and stores only
/// currently active operations. Caller migration is intentionally C17 scope.
pub(crate) static CONNECTION_REFRESH_COORDINATOR: Lazy<Arc<ConnectionRefreshCoordinator>> =
    Lazy::new(|| Arc::new(ConnectionRefreshCoordinator::new()));

#[doc(hidden)]
pub fn active_connection_refresh_count() -> usize {
    CONNECTION_REFRESH_COORDINATOR.active_count()
}

fn canonical_connection(db: &Db, key: &ConnectionRefreshKey) -> Result<ProviderConnection, String> {
    db.snapshot()
        .provider_connections
        .iter()
        .find(|candidate| candidate.id == key.connection_id && candidate.provider == key.provider)
        .cloned()
        .ok_or_else(|| format!("connection {} is not configured", key.connection_id))
}

/// Bootstrap-only refresh for a manual control-plane request that has no
/// configured connection identity yet. Configured connections must always use
/// [`ConnectionRefreshCoordinator`]. The caller must persist a newly created
/// connection before exposing the result.
pub async fn refresh_unconfigured_connection(
    provider: &str,
    refresh_token: &str,
    provider_specific_data: &BTreeMap<String, Value>,
) -> Result<RefreshResult, String> {
    dispatch_oauth_refresh(provider, refresh_token, provider_specific_data).await
}

/// Execute the provider-specific refresh wire protocol for a canonical
/// configured connection. Coordination and persistence remain the caller's
/// responsibility; production callers normally use `refresh_connection`.
pub async fn refresh_provider_connection_credentials(
    connection: &ProviderConnection,
) -> Result<RefreshResult, String> {
    let refresh_token = connection
        .refresh_token
        .as_deref()
        .ok_or_else(|| "connection has no refresh token".to_string())?;
    dispatch_oauth_refresh(
        &connection.provider,
        refresh_token,
        &connection.provider_specific_data,
    )
    .await
}

// ---------------------------------------------------------------------------
// Refresh lead times (how early before expiry we proactively refresh)
// ---------------------------------------------------------------------------

pub const REFRESH_LEAD_CODEX_MS: u64 = 5 * 24 * 60 * 60 * 1000; // 5 days
pub const REFRESH_LEAD_OPENAI_MS: u64 = 5 * 24 * 60 * 60 * 1000; // 5 days
pub const REFRESH_LEAD_CLAUDE_MS: u64 = 4 * 60 * 60 * 1000; // 4 hours
pub const REFRESH_LEAD_QWEN_MS: u64 = 20 * 60 * 1000; // 20 minutes
pub const REFRESH_LEAD_KIMI_CODING_MS: u64 = 5 * 60 * 1000; // 5 minutes
pub const REFRESH_LEAD_ANTIGRAVITY_MS: u64 = 5 * 60 * 1000; // 5 minutes
pub const REFRESH_LEAD_XAI_MS: u64 = 5 * 60 * 1000; // 5 minutes

// ---------------------------------------------------------------------------
// Constants shared by refresh functions
// ---------------------------------------------------------------------------

const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub(crate) const CLAUDE_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";

const QWEN_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
const QWEN_TOKEN_URL: &str = "https://chat.qwen.ai/api/v1/oauth2/token";

const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

const CLINE_REFRESH_URL: &str = "https://api.cline.bot/api/v1/auth/refresh";

const KIRO_AUTH_SERVICE: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";

const GITHUB_OAUTH_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";

const GITLAB_TOKEN_URL: &str = "https://gitlab.com/oauth/token";

/// Codex max refresh age: 8 days (9router CODEX_MAX_REFRESH_AGE_MS).
pub const CODEX_MAX_REFRESH_AGE_MS: u64 = 8 * 24 * 60 * 60 * 1000;

/// Check whether an access token needs refreshing based on its `expires_at`
/// RFC 3339 timestamp.
///
/// Missing expires_at → false for proactive path (9router isTokenExpiringSoon).
/// Reactive 401 path still refreshes when refresh_token is present.
pub fn needs_refresh(expires_at: &Option<String>) -> bool {
    let Some(expires_at) = expires_at else {
        return false;
    };

    match chrono::DateTime::parse_from_rfc3339(expires_at) {
        Ok(expires_at) => {
            let expires_at = expires_at.with_timezone(&chrono::Utc);
            let now = chrono::Utc::now();
            let buffer = chrono::Duration::milliseconds(TOKEN_EXPIRY_BUFFER_MS as i64);
            expires_at - buffer < now
        }
        Err(_) => true,
    }
}

/// Check whether an access token needs refreshing with a provider-specific
/// lead time.  Used by openai.rs, xai.rs, and antigravity.rs.
pub fn needs_refresh_with_lead(expires_at: &Option<String>, lead_ms: u64) -> bool {
    let Some(expires_at) = expires_at else {
        return false;
    };

    match chrono::DateTime::parse_from_rfc3339(expires_at) {
        Ok(expires_at) => {
            let expires_at = expires_at.with_timezone(&chrono::Utc);
            let now = chrono::Utc::now();
            let lead = chrono::Duration::milliseconds(lead_ms as i64);
            expires_at - lead < now
        }
        Err(_) => true,
    }
}

/// Full 9router shouldRefreshCredentials: lead-time OR max-refresh-age (codex).
pub fn should_refresh_credentials(
    provider: &str,
    expires_at: &Option<String>,
    last_refresh_at: Option<&str>,
    has_refresh_token: bool,
    lead_ms: u64,
) -> bool {
    if needs_refresh_with_lead(expires_at, lead_ms) {
        return true;
    }
    // Codex: refresh every 8 days even if access token still valid
    if has_refresh_token && matches!(provider, "codex" | "opencode" | "cx") {
        let max_age = chrono::Duration::milliseconds(CODEX_MAX_REFRESH_AGE_MS as i64);
        let now = chrono::Utc::now();
        match last_refresh_at.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) {
            Some(last) => now - last.with_timezone(&chrono::Utc) >= max_age,
            None => true, // never refreshed → stale
        }
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Per-provider refresh functions
// ---------------------------------------------------------------------------

/// Refresh a Claude OAuth access-token.
///
/// POST JSON to `https://api.anthropic.com/v1/oauth/token`.
pub async fn refresh_claude_oauth_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLAUDE_CLIENT_ID,
    });
    let resp = client
        .post(CLAUDE_TOKEN_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Claude refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Refresh a Codex / ChatGPT access token.
///
/// POST form-urlencoded to the OpenAI Auth0 token endpoint.
pub async fn refresh_codex_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        &codex_token_url(),
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
        ],
    )
    .await
}

/// Resolve the codex token URL (allows env-override).
fn codex_token_url() -> String {
    std::env::var("OPENPROXY_CODEX_TOKEN_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| CODEX_TOKEN_URL.to_string())
}

/// Refresh a Google OAuth token (used by antigravity).
pub async fn refresh_google_token(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<RefreshResult, String> {
    refresh_form_token(
        GOOGLE_TOKEN_URL,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ],
    )
    .await
}

/// Refresh a Qwen access token.
pub async fn refresh_qwen_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        QWEN_TOKEN_URL,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", QWEN_CLIENT_ID),
        ],
    )
    .await
}

/// Official GitHub OAuth app client id used by 9router / GitHub Copilot flows.
const GITHUB_OAUTH_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

/// Refresh a GitHub OAuth token.
///
/// Note: GitHub's token refresh response does *not* include a `refresh_token`
/// field. The `refresh_token` in the returned `RefreshResult` will be `None`.
pub async fn refresh_github_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        GITHUB_OAUTH_TOKEN_URL,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", GITHUB_OAUTH_CLIENT_ID),
        ],
    )
    .await
}

/// Refresh a GitHub Copilot session token via the internal v2 token endpoint.
///
/// Unlike the other refresh functions this takes a *GitHub OAuth access token*
/// (not a refresh token) and performs a **GET** request.
/// Auth scheme is `token` (not Bearer) — 9router parity.
pub async fn refresh_copilot_token(access_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(GITHUB_COPILOT_TOKEN_URL)
        .header(AUTHORIZATION, format!("token {access_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.38.0")
        .header("Editor-Version", "vscode/1.110.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.38.0")
        .header(ACCEPT, "application/json")
        .header("x-github-api-version", "2025-04-01")
        .send()
        .await
        .map_err(|e| format!("Copilot token refresh request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Copilot token refresh returned HTTP {}",
            resp.status().as_u16()
        ));
    }

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("Copilot refresh parse failed: {e}"))?;

    let token = payload
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Copilot refresh response missing 'token'".to_string())?;

    Ok(RefreshResult {
        access_token: token.to_string(),
        refresh_token: None,
        expires_in: None,
    })
}

/// Refresh a Kiro access token.
///
/// Branches on `authMethod` / credentials in `provider_specific_data`:
/// - **external_idp**: form-urlencoded POST to Microsoft Entra `tokenEndpoint`
///   (`grant_type=refresh_token&client_id&refresh_token&scope`).
/// - **AWS OIDC** (`clientId` + `clientSecret`): JSON POST to
///   `https://oidc.{region}.amazonaws.com/token`.
/// - **else** (social / imported): JSON POST to Kiro Cognito `/refreshToken`.
pub async fn refresh_kiro_token(
    refresh_token: &str,
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
) -> Result<RefreshResult, String> {
    // Enterprise Microsoft Entra (CLIProxyAPI) — must be checked before the
    // clientId+clientSecret OIDC branch, because external_idp also carries
    // clientId (without clientSecret) in PSD.
    if crate::oauth::kiro::is_external_idp_auth(provider_specific_data) {
        let poll =
            crate::oauth::kiro::refresh_external_idp_token(refresh_token, provider_specific_data)
                .await
                .map_err(|e| e.to_string())?;

        let access_token = poll
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| "Kiro external_idp refresh response missing access token".to_string())?;

        return Ok(RefreshResult {
            access_token: access_token.to_string(),
            refresh_token: poll.refresh_token,
            expires_in: poll.expires_in,
        });
    }

    let client = reqwest::Client::new();
    let (url, body) = if let (Some(client_id), Some(client_secret)) = (
        provider_specific_data
            .get("clientId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty()),
        provider_specific_data
            .get("clientSecret")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    ) {
        let region = provider_specific_data
            .get("region")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("us-east-1");
        (
            format!("https://oidc.{region}.amazonaws.com/token"),
            serde_json::json!({
                "clientId": client_id,
                "clientSecret": client_secret,
                "refreshToken": refresh_token,
                "grantType": "refresh_token",
            }),
        )
    } else {
        (
            format!("{KIRO_AUTH_SERVICE}/refreshToken"),
            serde_json::json!({ "refreshToken": refresh_token }),
        )
    };

    let resp = client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Kiro refresh request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Kiro refresh returned HTTP {status}: {text}"));
    }

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("Kiro refresh parse failed: {e}"))?;

    let access_token = payload
        .get("accessToken")
        .or_else(|| payload.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Kiro refresh response missing access token".to_string())?;

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: payload
            .get("refreshToken")
            .or_else(|| payload.get("refresh_token"))
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in: payload
            .get("expiresIn")
            .or_else(|| payload.get("expires_in"))
            .and_then(Value::as_i64),
    })
}

/// Refresh an xAI access token via form-urlencoded request.
///
/// Uses the standard xAI auth endpoint.
pub async fn refresh_xai_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let token_url = resolve_xai_token_url();
    refresh_form_token(
        &token_url,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", XAI_CLIENT_ID),
        ],
    )
    .await
}

/// Resolve xAI's token URL (env override or default).
fn resolve_xai_token_url() -> String {
    std::env::var("OPENPROXY_XAI_TOKEN_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "https://auth.x.ai/oauth2/token".to_string())
}

/// Refresh an OpenAI access token (same flow as codex).
pub async fn refresh_openai_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        &codex_token_url(),
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
        ],
    )
    .await
}

/// Refresh a Kimi Coding access token.
/// Kimi OAuth merged into the dual-auth `kimi` provider (68566f5): endpoints
/// moved to auth.kimi.com and requests carry the X-Msh-* device headers.
pub async fn refresh_kimi_coding_token(
    refresh_token: &str,
    device_id: Option<&str>,
) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let mut request = client
        .post("https://auth.kimi.com/api/oauth/token")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "17e5f671-d194-4dfb-9706-5516cb48c098"),
        ]);
    // X-Msh-* device headers (buildKimiHeaders parity) — the device id comes
    // from the connection's persisted psd so restarts keep the same identity.
    let msh = crate::core::config::app_constants::build_kimi_headers(device_id);
    if let Some(obj) = msh.as_object() {
        for (key, value) in obj {
            if let Some(v) = value.as_str() {
                request = request.header(key.as_str(), v);
            }
        }
    }
    let resp = request
        .send()
        .await
        .map_err(|e| format!("Kimi Coding refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Refresh a KiloCode access token.
pub async fn refresh_kilocode_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.kilo.ai/oauth/token")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "kilocode-openproxy"),
        ])
        .send()
        .await
        .map_err(|e| format!("KiloCode refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Refresh a Cline access token.
pub async fn refresh_cline_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(CLINE_REFRESH_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .json(&serde_json::json!({
            "refreshToken": refresh_token,
            "grantType": "refresh_token",
            "clientType": "extension"
        }))
        .send()
        .await
        .map_err(|e| format!("Cline refresh request failed: {e}"))?;

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("Cline refresh parse failed: {e}"))?;

    let data = payload.get("data").unwrap_or(&payload);
    let access_token = data
        .get("accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Cline refresh response missing access token".to_string())?;

    let expires_in = data
        .get("expiresAt")
        .and_then(Value::as_str)
        .and_then(|expires_at| chrono::DateTime::parse_from_rfc3339(expires_at).ok())
        .map(|expires_at| (expires_at.timestamp() - chrono::Utc::now().timestamp()).max(1));

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: data
            .get("refreshToken")
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in,
    })
}

/// Refresh a GitLab access token.
pub async fn refresh_gitlab_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        GITLAB_TOKEN_URL,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "openproxy"),
        ],
    )
    .await
}

/// Refresh a CodeBuddy access token.
pub async fn refresh_codebuddy_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://copilot.tencent.com/oauth/token")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "codebuddy-openproxy"),
        ])
        .send()
        .await
        .map_err(|e| format!("CodeBuddy refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Refresh a CodeBuddy CN access token.
///
/// 9router wire format: POST refreshUrl with JSON body `"{}"` and
/// `X-Refresh-Token` header (not form-urlencoded OAuth). Response:
/// `{ code: 0, data: { accessToken, refreshToken?, expiresIn? } }`.
pub async fn refresh_codebuddy_cn_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://copilot.tencent.com/v2/plugin/auth/token/refresh")
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .header("User-Agent", "CLI/2.63.2 CodeBuddy/2.63.2")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("X-Domain", "copilot.tencent.com")
        .header("X-Refresh-Token", refresh_token)
        .header("X-Auth-Refresh-Source", "plugin")
        .header("X-Product", "SaaS")
        .body("{}")
        .send()
        .await
        .map_err(|e| format!("CodeBuddy CN refresh request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "CodeBuddy CN refresh returned HTTP {status}: {text}"
        ));
    }

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("CodeBuddy CN refresh parse failed: {e}"))?;

    let code = payload.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        return Err(format!(
            "CodeBuddy CN refresh code={code} msg={:?}",
            payload.get("msg")
        ));
    }

    let data = payload
        .get("data")
        .ok_or_else(|| "CodeBuddy CN refresh missing data".to_string())?;

    let access = data
        .get("accessToken")
        .or_else(|| data.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "CodeBuddy CN refresh missing accessToken".to_string())?;

    let new_refresh = data
        .get("refreshToken")
        .or_else(|| data.get("refresh_token"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let expires_in = data
        .get("expiresIn")
        .or_else(|| data.get("expires_in"))
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)));

    Ok(RefreshResult {
        access_token: access.to_string(),
        refresh_token: new_refresh,
        expires_in,
    })
}

/// Refresh a Trae (ByteDance marscode) access token.
///
/// 9router parity: `open-sse/services/tokenRefresh/providers.js:619-688`.
/// POST ExchangeToken with JSON body `{ClientID, RefreshToken, ClientSecret, UserID}`.
/// Response: `{Result: {AccessToken, RefreshToken, TokenType, ExpiresAt}}`.
pub async fn refresh_trae_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.marscode.com/cloudide/api/v3/trae/oauth/ExchangeToken")
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .header("User-Agent", "Trae/1.0.0 antigravity-cockpit-tools")
        .json(&serde_json::json!({
            "ClientID": "ono9krqynydwx5",
            "RefreshToken": refresh_token,
            "ClientSecret": "-",
            "UserID": "",
        }))
        .send()
        .await
        .map_err(|e| format!("Trae refresh request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Trae refresh returned HTTP {status}: {text}"));
    }

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("Trae refresh parse failed: {e}"))?;

    // Response may nest under `Result` (PascalCase) or `result` (camelCase).
    let result = payload
        .get("Result")
        .or_else(|| payload.get("result"))
        .unwrap_or(&payload);

    let access = result
        .get("AccessToken")
        .or_else(|| result.get("accessToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Trae refresh missing AccessToken".to_string())?;

    let new_refresh = result
        .get("RefreshToken")
        .or_else(|| result.get("refreshToken"))
        .and_then(Value::as_str)
        .map(str::to_string);

    // ExpiresAt may be a Unix timestamp (number) or RFC3339 string.
    let expires_in = result
        .get("ExpiresAt")
        .or_else(|| result.get("expiresAt"))
        .and_then(|v| {
            if let Some(ts) = v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)) {
                let now = chrono::Utc::now().timestamp();
                Some((ts - now).max(1))
            } else if let Some(s) = v.as_str() {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| (dt.timestamp() - chrono::Utc::now().timestamp()).max(1))
            } else {
                None
            }
        });

    Ok(RefreshResult {
        access_token: access.to_string(),
        refresh_token: new_refresh,
        expires_in,
    })
}

/// Refresh a CodeBuddy Intl access token.
///
/// 9router parity: `open-sse/services/tokenRefresh/providers.js:566-615`.
/// Same wire format as codebuddy-cn but with `X-Domain: "www.codebuddy.ai"`.
pub async fn refresh_codebuddy_intl_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://www.codebuddy.ai/v2/plugin/auth/token/refresh")
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .header("User-Agent", "IDE/2.63.2 CodeBuddy/2.63.2")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("X-Domain", "www.codebuddy.ai")
        .header("X-Refresh-Token", refresh_token)
        .header("X-Auth-Refresh-Source", "plugin")
        .header("X-Product", "SaaS")
        .body("{}")
        .send()
        .await
        .map_err(|e| format!("CodeBuddy Intl refresh request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "CodeBuddy Intl refresh returned HTTP {status}: {text}"
        ));
    }

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("CodeBuddy Intl refresh parse failed: {e}"))?;

    let code = payload.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        return Err(format!(
            "CodeBuddy Intl refresh code={code} msg={:?}",
            payload.get("msg")
        ));
    }

    let data = payload
        .get("data")
        .ok_or_else(|| "CodeBuddy Intl refresh missing data".to_string())?;

    let access = data
        .get("accessToken")
        .or_else(|| data.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "CodeBuddy Intl refresh missing accessToken".to_string())?;

    let new_refresh = data
        .get("refreshToken")
        .or_else(|| data.get("refresh_token"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let expires_in = data
        .get("expiresIn")
        .or_else(|| data.get("expires_in"))
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)));

    Ok(RefreshResult {
        access_token: access.to_string(),
        refresh_token: new_refresh,
        expires_in,
    })
}

/// Dispatch to the correct per-provider token refresh function.
///
/// Connection-level singleflight is owned by [`ConnectionRefreshCoordinator`].
/// This function performs only the provider wire call and its bounded
/// transient HTTP retry policy; it retains no token-keyed or completed result.
pub async fn dispatch_oauth_refresh(
    provider: &str,
    refresh_token: &str,
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
) -> Result<RefreshResult, String> {
    match provider {
        "claude" | "anthropic" => {
            refresh_with_retry(|| refresh_claude_oauth_token(refresh_token)).await
        }
        "codex" | "opencode" => refresh_with_retry(|| refresh_codex_token(refresh_token)).await,
        "antigravity" => {
            let client_secret = crate::oauth::secret::antigravity_client_secret();
            refresh_with_retry(|| {
                refresh_google_token(refresh_token, ANTIGRAVITY_CLIENT_ID, client_secret)
            })
            .await
        }
        "qwen" => refresh_with_retry(|| refresh_qwen_token(refresh_token)).await,
        "xai" => refresh_with_retry(|| refresh_xai_token(refresh_token)).await,
        "kimi" | "kimi-coding" => {
            let device_id = provider_specific_data
                .get("deviceId")
                .and_then(Value::as_str);
            refresh_with_retry(|| refresh_kimi_coding_token(refresh_token, device_id)).await
        }
        "kilocode" => refresh_with_retry(|| refresh_kilocode_token(refresh_token)).await,
        "cline" | "clinepass" => refresh_with_retry(|| refresh_cline_token(refresh_token)).await,
        "gitlab" => refresh_with_retry(|| refresh_gitlab_token(refresh_token)).await,
        "codebuddy" => refresh_with_retry(|| refresh_codebuddy_token(refresh_token)).await,
        "codebuddy-cn" => refresh_with_retry(|| refresh_codebuddy_cn_token(refresh_token)).await,
        "openai" => refresh_with_retry(|| refresh_openai_token(refresh_token)).await,
        "kiro" => {
            refresh_with_retry(|| refresh_kiro_token(refresh_token, provider_specific_data)).await
        }
        "github" => refresh_with_retry(|| refresh_github_token(refresh_token)).await,
        "grok-cli" | "gcli" | "gb" => refresh_with_retry(|| refresh_xai_token(refresh_token)).await,
        "trae" | "marscode" => refresh_with_retry(|| refresh_trae_token(refresh_token)).await,
        "codebuddy-intl" | "cbai" => {
            refresh_with_retry(|| refresh_codebuddy_intl_token(refresh_token)).await
        }
        _ => Err(format!("No refresh handler for provider: {}", provider)),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Send a form-urlencoded POST and parse the JSON response into a RefreshResult.
async fn refresh_form_token(url: &str, fields: Vec<(&str, &str)>) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&fields)
        .send()
        .await
        .map_err(|e| format!("Refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Parse a JSON token-refresh response into a RefreshResult.
///
/// Handles both camelCase and snake_case field names for cross-provider
/// compatibility.
async fn parse_json_refresh_response(resp: reqwest::Response) -> Result<RefreshResult, String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read refresh response: {e}"))?;
    let payload: Value = serde_json::from_str(&body).map_err(|e| {
        if status.is_success() {
            format!("Failed to parse refresh response: {e}")
        } else {
            format!("Refresh request returned HTTP {}", status.as_u16())
        }
    })?;

    if !status.is_success() {
        let error = payload
            .get("error")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let detail = error.map(|error| format!(": {error}")).unwrap_or_default();
        return Err(format!(
            "Refresh request returned HTTP {}{detail}",
            status.as_u16()
        ));
    }

    let access_token = payload
        .get("access_token")
        .or_else(|| payload.get("accessToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Refresh response did not include access_token".to_string())?;

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: payload
            .get("refresh_token")
            .or_else(|| payload.get("refreshToken"))
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in: payload
            .get("expires_in")
            .or_else(|| payload.get("expiresIn"))
            .and_then(Value::as_i64),
    })
}
