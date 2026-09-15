use std::path::PathBuf;
use std::sync::Arc;

use std::collections::HashMap;
use tokio::sync::{Notify, RwLock};

use crate::core::account_fallback::AccountRegistry;
use crate::core::circuit_breaker::CircuitBreakerRegistry;
use crate::core::executor::ClientPool;
use crate::core::health::{health_registry, HealthRegistry};
use crate::core::model::models_dev::ModelsDevCatalog;
use crate::db::Db;
use crate::oauth::pending::PendingFlowStore;
use crate::server::api::oauth::{CodexProxyState, XaiProxyState, ZedProxyState};
use crate::server::auth::login_limiter::LoginLimiter;
use crate::server::codex_catalog::CodexModelCatalog;
use crate::server::console_logs::{shared_console_log_buffer, ConsoleLogBuffer};

/// Session info stored server-side
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub api_key_id: String,
    pub created_at: i64,
    pub last_active: i64,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub client_pool: Arc<ClientPool>,
    pub pending_flows: PendingFlowStore,
    pub account_registry: Arc<AccountRegistry>,
    pub console_logs: Arc<ConsoleLogBuffer>,
    pub sessions: Arc<RwLock<HashMap<String, SessionInfo>>>,
    pub codex_proxy: Arc<CodexProxyState>,
    pub xai_proxy: Arc<XaiProxyState>,
    pub zed_proxy: Arc<ZedProxyState>,

    /// Progressive lockout store for `POST /api/auth/login`. Tracks failed
    /// attempts per client IP and escalates lockout duration on repeat
    /// offenders (see `login_limiter.rs` for the exact schedule).
    pub login_limiter: Arc<LoginLimiter>,

    /// Optional reverse-proxy target for the dashboard.
    ///
    /// When `Some`, all dashboard fallback requests are forwarded to this URL
    /// instead of being served from the embedded `web/dist/` assets. Used in
    /// development against the Astro/Vite dev server (`pnpm --dir web run dev`).
    pub dashboard_sidecar_url: Option<String>,

    /// HTTP client used by the dashboard reverse proxy. `Some` iff
    /// `dashboard_sidecar_url` is set — there is no point allocating a
    /// reqwest client in the embedded-only path.
    pub dashboard_client: Option<Arc<reqwest::Client>>,

    /// Optional on-disk override for the dashboard. When set, the embedded
    /// assets are bypassed and files are served from this directory via
    /// `tower-http::services::ServeDir`. Useful for UI iteration without
    /// rebuilding the Rust binary.
    ///
    /// Precedence (first match wins):
    ///   1. `dashboard_sidecar_url` — reverse proxy
    ///   2. `web_dir` — disk
    ///   3. embedded assets (default)
    pub web_dir: Option<PathBuf>,

    /// Triggered on graceful shutdown (SIGTERM, SIGINT, or API call).
    /// Await `.notified()` to block until shutdown is requested.
    pub shutdown_signal: Arc<Notify>,

    /// Circuit breaker registry for provider endpoint resilience.
    /// Tracked per provider+endpoint to fast-fail when upstreams are down.
    pub circuit_breaker: Arc<CircuitBreakerRegistry>,

    /// Provider health records written by the health daemon. Shares the
    /// process-global registry (`core::health::health_registry`) so the combo
    /// dispatcher and account fallback observe the same degrade windows.
    pub health: Arc<HealthRegistry>,

    pub models_dev: Arc<ModelsDevCatalog>,
    pub codex_models: Arc<CodexModelCatalog>,
}

impl AppState {
    /// Construct an AppState with the embedded dashboard as the default
    /// fallback. No reverse-proxy client is allocated until
    /// `with_dashboard_sidecar_url` is called.
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db: db.clone(),
            client_pool: Arc::new(ClientPool::new()),
            pending_flows: PendingFlowStore::new(),
            account_registry: Arc::new(AccountRegistry::default()),
            console_logs: shared_console_log_buffer(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            codex_proxy: Arc::new(CodexProxyState::new()),
            xai_proxy: Arc::new(XaiProxyState::new()),
            zed_proxy: Arc::new(ZedProxyState::new()),
            login_limiter: Arc::new(LoginLimiter::new(&db.data_dir)),
            dashboard_sidecar_url: None,
            dashboard_client: None,
            web_dir: None,
            shutdown_signal: Arc::new(Notify::new()),
            circuit_breaker: Arc::new(CircuitBreakerRegistry::default()),
            health: health_registry(),
            models_dev: Arc::new(ModelsDevCatalog::default()),
            codex_models: Arc::new(CodexModelCatalog::default()),
        }
    }

    /// Enable dashboard reverse proxy mode: all dashboard fallback requests
    /// are forwarded to `url`. Allocates a reqwest client lazily.
    ///
    /// Pass `None` to disable. Empty/whitespace strings are also treated as
    /// disabled — that matches the behaviour CLI flag handling expects from
    /// `clap`'s default empty string.
    pub fn with_dashboard_sidecar_url(mut self, url: Option<String>) -> Self {
        let normalized = url.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if normalized.is_some() {
            self.dashboard_client = Some(Arc::new(reqwest::Client::new()));
        } else {
            self.dashboard_client = None;
        }
        self.dashboard_sidecar_url = normalized;
        self
    }

    /// Serve the dashboard from `path` on disk instead of the embedded
    /// assets. Sidecar mode (if set) still wins.
    pub fn with_web_dir(mut self, path: Option<PathBuf>) -> Self {
        self.web_dir = path;
        self
    }

    /// Trigger graceful shutdown. Notifies all waiters and
    /// signals axum to stop accepting new connections.
    pub fn signal_shutdown(&self) {
        self.shutdown_signal.notify_waiters();
    }
}
