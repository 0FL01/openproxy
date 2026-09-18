//! Bounded Antigravity onboarding tied to configured connection generations.
//!
//! Onboarding is control-plane work. Generation requests never call this
//! module. Concurrent callers for one connection generation share one active
//! polling session; completed results are not cached.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::core::config::app_constants::{agy_cli_user_agent, agy_load_metadata, cloud_code_api};
use crate::core::executor::ClientPool;
use crate::core::proxy::resolve_proxy_target;
use crate::core::utils::antigravity_project::antigravity_project_id;
use crate::db::Db;
use crate::oauth::token_refresh::{connection_credential_generation, CredentialGeneration};

const DEFAULT_MAX_ATTEMPTS: usize = 3;
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(3);
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(8);

type SharedOnboardingResult = Result<(), String>;

struct ActiveOnboarding {
    generation: CredentialGeneration,
    result: AsyncMutex<Option<SharedOnboardingResult>>,
    completed: Notify,
    cancelled: AtomicBool,
    cancel: Notify,
}

impl ActiveOnboarding {
    fn new(generation: CredentialGeneration) -> Self {
        Self {
            generation,
            result: AsyncMutex::new(None),
            completed: Notify::new(),
            cancelled: AtomicBool::new(false),
            cancel: Notify::new(),
        }
    }

    async fn wait(&self) -> SharedOnboardingResult {
        loop {
            let completed = self.completed.notified();
            if let Some(result) = self.result.lock().await.clone() {
                return result;
            }
            completed.await;
        }
    }

    async fn finish(&self, result: SharedOnboardingResult) {
        *self.result.lock().await = Some(result);
        self.completed.notify_waiters();
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancel.notify_waiters();
    }
}

/// In-flight-only onboarding singleflight keyed by configured connection id.
///
/// The active map is empty while idle and never retains completed generations.
/// Polling is bounded by `max_attempts`; cancellation is checked before and
/// during every HTTP attempt and wait interval.
pub struct AntigravityOnboardingCoordinator {
    active: Mutex<HashMap<String, Arc<ActiveOnboarding>>>,
    accepting: AtomicBool,
    idle: Notify,
    endpoint: String,
    max_attempts: usize,
    poll_interval: Duration,
}

impl Default for AntigravityOnboardingCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl AntigravityOnboardingCoordinator {
    pub fn new() -> Self {
        Self::with_config(
            onboarding_endpoint(),
            DEFAULT_MAX_ATTEMPTS,
            DEFAULT_POLL_INTERVAL,
        )
    }

    #[doc(hidden)]
    pub fn with_config(endpoint: String, max_attempts: usize, poll_interval: Duration) -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            accepting: AtomicBool::new(true),
            idle: Notify::new(),
            endpoint,
            max_attempts: max_attempts.max(1),
            poll_interval,
        }
    }

    /// Ensure one onboarding session for the observed configured generation.
    ///
    /// Same-generation callers share the active result. If configuration
    /// changes while an older session is active, that session is cancelled and
    /// the caller waits for cleanup before admitting the new generation.
    pub async fn ensure(
        self: &Arc<Self>,
        db: Arc<Db>,
        pool: Arc<ClientPool>,
        connection_id: &str,
        observed_generation: CredentialGeneration,
    ) -> SharedOnboardingResult {
        if !self.accepting.load(Ordering::Acquire) {
            return Err("Antigravity onboarding is shutting down".to_string());
        }

        loop {
            let existing = {
                let active = self.active.lock();
                active.get(connection_id).cloned()
            };
            if let Some(existing) = existing {
                if existing.generation == observed_generation {
                    return existing.wait().await;
                }
                existing.cancel();
                let _ = existing.wait().await;
                continue;
            }

            let operation = Arc::new(ActiveOnboarding::new(observed_generation));
            let inserted = {
                let mut active = self.active.lock();
                if active.contains_key(connection_id) {
                    false
                } else {
                    active.insert(connection_id.to_string(), operation.clone());
                    true
                }
            };
            if !inserted {
                continue;
            }

            let coordinator = Arc::clone(self);
            let operation_for_task = operation.clone();
            let connection_id_for_task = connection_id.to_string();
            tokio::spawn(async move {
                let result = coordinator
                    .run(
                        db,
                        pool,
                        &connection_id_for_task,
                        observed_generation,
                        &operation_for_task,
                    )
                    .await;
                operation_for_task.finish(result).await;
                let mut active = coordinator.active.lock();
                if active
                    .get(&connection_id_for_task)
                    .is_some_and(|current| Arc::ptr_eq(current, &operation_for_task))
                {
                    active.remove(&connection_id_for_task);
                }
                if active.is_empty() {
                    coordinator.idle.notify_waiters();
                }
            });

            return operation.wait().await;
        }
    }

    async fn run(
        &self,
        db: Arc<Db>,
        pool: Arc<ClientPool>,
        connection_id: &str,
        observed_generation: CredentialGeneration,
        operation: &ActiveOnboarding,
    ) -> SharedOnboardingResult {
        let canonical = canonical_connection(&db, connection_id)?;
        if connection_credential_generation(&canonical) != observed_generation {
            return Err("Antigravity connection changed before onboarding".to_string());
        }
        let access_token = canonical
            .access_token
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "Antigravity onboarding requires an access token".to_string())?
            .to_string();
        let project_id = antigravity_project_id(&canonical).ok_or_else(|| {
            "Antigravity onboarding requires discovered project metadata".to_string()
        })?;
        let tier_id = canonical
            .provider_specific_data
            .get("tierId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("legacy-tier")
            .to_string();
        let snapshot = db.snapshot();
        let proxy = resolve_proxy_target(&snapshot, &canonical, &snapshot.settings);
        let client = pool
            .get("antigravity-onboarding", proxy.as_ref())
            .map_err(|error| format!("Antigravity onboarding transport: {error}"))?;
        drop(snapshot);

        let metadata = agy_load_metadata();
        let body = json!({
            "tier_id": tier_id,
            "metadata": metadata,
        });

        let mut last_error = "Antigravity onboarding did not complete".to_string();
        for attempt in 0..self.max_attempts {
            ensure_current_generation(&db, connection_id, observed_generation)?;
            if operation.cancelled.load(Ordering::Acquire) {
                return Err("Antigravity onboarding cancelled".to_string());
            }

            let request = client
                .post(&self.endpoint)
                .header("Authorization", format!("Bearer {access_token}"))
                .header("Content-Type", "application/json")
                .header("User-Agent", agy_cli_user_agent())
                .json(&body);
            let cancelled = operation.cancel.notified();
            tokio::pin!(cancelled);
            let attempt_request = async {
                let response = request.send().await?;
                let status = response.status();
                let body = response.bytes().await?;
                Ok::<_, reqwest::Error>((status, body))
            };
            let response = tokio::select! {
                _ = &mut cancelled => return Err("Antigravity onboarding cancelled".to_string()),
                result = tokio::time::timeout(ATTEMPT_TIMEOUT, attempt_request) => result,
            };

            match response {
                Ok(Ok((status, body))) if status.is_success() => {
                    let payload = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
                    if payload.get("done").and_then(Value::as_bool) == Some(true) {
                        persist_readiness(&db, connection_id, observed_generation, None).await?;
                        return Ok(());
                    }
                    last_error = "Antigravity onboarding is still pending".to_string();
                }
                Ok(Ok((status, body))) => {
                    let detail = String::from_utf8_lossy(&body);
                    last_error = format!("Antigravity onboarding HTTP {status}: {detail}");
                    if status.is_client_error() && status.as_u16() != 429 {
                        break;
                    }
                }
                Ok(Err(error)) => {
                    last_error = format!("Antigravity onboarding request failed: {error}");
                }
                Err(_) => {
                    last_error = "Antigravity onboarding request timed out".to_string();
                }
            }

            if attempt + 1 < self.max_attempts {
                let cancelled = operation.cancel.notified();
                tokio::pin!(cancelled);
                // Bounded retry with jitter (donor: 3s + rand*4s).
                let jitter_ms = rand::random::<u64>() % 4000;
                let backoff = self.poll_interval + Duration::from_millis(jitter_ms);
                tokio::select! {
                    _ = &mut cancelled => return Err("Antigravity onboarding cancelled".to_string()),
                    _ = tokio::time::sleep(backoff) => {}
                }
            }
        }

        let error = truncate_error(&last_error);
        persist_readiness(&db, connection_id, observed_generation, Some(error.clone())).await?;
        Err(error)
    }

    pub fn cancel_connection(&self, connection_id: &str) {
        if let Some(operation) = self.active.lock().get(connection_id).cloned() {
            operation.cancel();
        }
    }

    pub fn active_count(&self) -> usize {
        self.active.lock().len()
    }

    /// Stop admission, cancel all active polling, and wait until workers have
    /// removed their in-flight entries. The caller owns any outer timeout.
    pub async fn shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        let active: Vec<_> = self.active.lock().values().cloned().collect();
        for operation in active {
            operation.cancel();
        }
        loop {
            let idle = self.idle.notified();
            if self.active.lock().is_empty() {
                return;
            }
            idle.await;
        }
    }
}

fn canonical_connection(
    db: &Db,
    connection_id: &str,
) -> Result<crate::types::ProviderConnection, String> {
    db.snapshot()
        .provider_connections
        .iter()
        .find(|connection| connection.id == connection_id && connection.provider == "antigravity")
        .cloned()
        .ok_or_else(|| "Antigravity connection is no longer configured".to_string())
}

fn ensure_current_generation(
    db: &Db,
    connection_id: &str,
    observed_generation: CredentialGeneration,
) -> Result<(), String> {
    let canonical = canonical_connection(db, connection_id)?;
    if connection_credential_generation(&canonical) == observed_generation {
        Ok(())
    } else {
        Err("Antigravity connection changed during onboarding".to_string())
    }
}

async fn persist_readiness(
    db: &Db,
    connection_id: &str,
    observed_generation: CredentialGeneration,
    error: Option<String>,
) -> Result<(), String> {
    let connection_id = connection_id.to_string();
    let mut applied = false;
    db.update(|db| {
        let Some(connection) = db.provider_connections.iter_mut().find(|connection| {
            connection.id == connection_id && connection.provider == "antigravity"
        }) else {
            return;
        };
        if connection_credential_generation(connection) != observed_generation {
            return;
        }
        applied = true;
        connection.test_status = Some(if error.is_some() { "error" } else { "active" }.to_string());
        connection.last_error = error.clone();
        connection.last_error_at = error.as_ref().map(|_| Utc::now().to_rfc3339());
    })
    .await
    .map_err(|error| format!("Failed to persist Antigravity onboarding state: {error}"))?;
    if applied {
        Ok(())
    } else {
        Err("Antigravity connection changed before onboarding state was saved".to_string())
    }
}

fn truncate_error(error: &str) -> String {
    error.chars().take(512).collect()
}

fn onboarding_endpoint() -> String {
    std::env::var("OPENPROXY_ANTIGRAVITY_ONBOARD_USER_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| cloud_code_api::ONBOARD_USER.to_string())
}

pub static ANTIGRAVITY_ONBOARDING_COORDINATOR: Lazy<Arc<AntigravityOnboardingCoordinator>> =
    Lazy::new(|| Arc::new(AntigravityOnboardingCoordinator::new()));
