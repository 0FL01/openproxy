//! C36 early admission for LLM generation routes.
//!
//! A bounded semaphore caps concurrently active LLM generations *before* JSON
//! extraction and large translations. The permit is stored in the response
//! extensions so it is held for the whole response body until EOF/drop and
//! released on every error path, including client cancellation. Health,
//! admin, and non-POST routes never take a permit and cannot starve.
//!
//! Overload is a controlled 429 with `Retry-After` and an explicit
//! `admission_queue_full` code — reported separately, never counted as a
//! memory optimization. No total-duration timeout is imposed: ordinary long
//! SSE streams hold their permit until they finish.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Default cap on concurrently active LLM generations.
pub const DEFAULT_MAX_ACTIVE_GENERATIONS: usize = 64;
/// Default bounded wait for a permit (`0` = immediate controlled rejection).
pub const DEFAULT_ADMISSION_WAIT_MS: u64 = 0;
/// `Retry-After` seconds advertised on overload rejection.
pub const ADMISSION_RETRY_AFTER_SECS: u64 = 1;
/// Explicit overload code so clients/operators can separate admission
/// rejections from upstream rate limits.
pub const ADMISSION_REJECT_CODE: &str = "admission_queue_full";

fn parse_positive_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn parse_wait(name: &str, default_ms: u64) -> Duration {
    let ms = std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default_ms);
    Duration::from_millis(ms)
}

pub struct LlmAdmission {
    semaphore: Arc<Semaphore>,
    limit: usize,
    wait: Duration,
    admitted: AtomicU64,
    rejected: AtomicU64,
}

/// Permit guard kept in response extensions: the semaphore permit lives until
/// the full response (headers + streaming body) is sent, dropped, or
/// cancelled, so peak active buffers stay bounded by configuration.
/// (`http::Extensions` requires `Clone`, so the owned permit rides in an
/// `Arc`; the single copy dies with the response.)
#[derive(Clone)]
struct PermitGuard {
    _permit: Arc<OwnedSemaphorePermit>,
}

impl LlmAdmission {
    pub fn new(limit: usize, wait: Duration) -> Self {
        let limit = limit.max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(limit)),
            limit,
            wait,
            admitted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            parse_positive_usize(
                "OPENPROXY_MAX_ACTIVE_GENERATIONS",
                DEFAULT_MAX_ACTIVE_GENERATIONS,
            ),
            parse_wait("OPENPROXY_ADMISSION_WAIT_MS", DEFAULT_ADMISSION_WAIT_MS),
        )
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn wait(&self) -> Duration {
        self.wait
    }

    /// Currently held permits (admitted but not yet released).
    pub fn active(&self) -> usize {
        self.limit
            .saturating_sub(self.semaphore.available_permits())
    }

    pub fn admitted(&self) -> u64 {
        self.admitted.load(Ordering::Acquire)
    }

    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Acquire)
    }

    async fn acquire(&self) -> Option<OwnedSemaphorePermit> {
        if self.wait.is_zero() {
            match self.semaphore.clone().try_acquire_owned() {
                Ok(permit) => {
                    self.admitted.fetch_add(1, Ordering::AcqRel);
                    Some(permit)
                }
                Err(_) => {
                    self.rejected.fetch_add(1, Ordering::AcqRel);
                    None
                }
            }
        } else {
            match tokio::time::timeout(self.wait, self.semaphore.clone().acquire_owned()).await {
                Ok(Ok(permit)) => {
                    self.admitted.fetch_add(1, Ordering::AcqRel);
                    Some(permit)
                }
                _ => {
                    self.rejected.fetch_add(1, Ordering::AcqRel);
                    None
                }
            }
        }
    }
}

fn admission_rejection() -> Response {
    let mut body = crate::core::utils::error::build_error_body(
        StatusCode::TOO_MANY_REQUESTS.as_u16(),
        Some("Server is at generation capacity; retry shortly"),
    );
    if let Some(error) = body.get_mut("error") {
        error[ADMISSION_REJECT_CODE] = serde_json::Value::String(ADMISSION_REJECT_CODE.to_string());
        // Keep the OpenAI-shaped code field explicit for overload.
        error["code"] = serde_json::Value::String(ADMISSION_REJECT_CODE.to_string());
    }
    let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    response.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        HeaderValue::from_static("1"),
    );
    response.headers_mut().insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

/// Axum middleware: admit POST generation requests before body extraction.
/// Non-POST (CORS preflight, health, model listing) passes through without a
/// permit so control-plane traffic cannot starve.
pub async fn admit_llm_request(
    State(state): State<crate::server::state::AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.method() != axum::http::Method::POST {
        return next.run(request).await;
    }
    let Some(permit) = state.llm_admission.acquire().await else {
        return admission_rejection();
    };
    let mut response = next.run(request).await;
    response.extensions_mut().insert(PermitGuard {
        _permit: Arc::new(permit),
    });
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn immediate_rejection_counts_and_releases() {
        let admission = LlmAdmission::new(1, Duration::from_millis(0));
        let first = admission.acquire().await.expect("first permit");
        assert_eq!(admission.active(), 1);
        assert!(admission.acquire().await.is_none());
        assert_eq!(admission.rejected(), 1);
        drop(first);
        assert_eq!(admission.active(), 0);
        assert!(admission.acquire().await.is_some());
        assert_eq!(admission.admitted(), 2);
    }

    #[tokio::test]
    async fn bounded_wait_admits_after_release() {
        let admission = Arc::new(LlmAdmission::new(1, Duration::from_secs(5)));
        let first = admission.acquire().await.expect("first permit");
        let waiter = {
            let admission = admission.clone();
            tokio::spawn(async move { admission.acquire().await.is_some() })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(first);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .expect("join")
                .expect("waiter"),
            "bounded wait must admit after release"
        );
    }

    #[tokio::test]
    async fn bounded_wait_rejects_after_budget() {
        let admission = LlmAdmission::new(1, Duration::from_millis(50));
        let _first = admission.acquire().await.expect("first permit");
        let start = std::time::Instant::now();
        assert!(admission.acquire().await.is_none());
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "wait must stay bounded"
        );
        assert_eq!(admission.rejected(), 1);
    }
}
