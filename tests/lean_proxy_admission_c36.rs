//! C36: early admission with a permit held for the whole response body.
//!
//! The semaphore caps active LLM generations before JSON extraction; overload
//! is a controlled 429 (never counted as a memory win); health/admin and
//! CORS preflight never take permits; permits release on every path including
//! cancellation; ordinary long SSE streams are not killed by a fixed total
//! duration.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::server::api::admission::{ADMISSION_REJECT_CODE, DEFAULT_MAX_ACTIVE_GENERATIONS};
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::util::ServiceExt;

fn success_json() -> ScriptedResponse {
    ScriptedResponse::json(
        StatusCode::OK,
        json!({
            "id": "chatcmpl-c36",
            "object": "chat.completion",
            "created": 0,
            "model": "gpt-c36",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
        .to_string(),
    )
}

async fn app_with_limit(
    upstream: &MockUpstream,
    limit: usize,
) -> (
    axum::Router,
    TempTestDb,
    Arc<openproxy::server::api::admission::LlmAdmission>,
) {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![ProviderNode {
                id: "openai".into(),
                r#type: "openai-compatible".into(),
                name: "C36 OpenAI".into(),
                prefix: Some("openai".into()),
                api_type: Some("chat".into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            db.provider_connections = vec![ProviderConnection {
                id: "c36-openai-account".into(),
                provider: "openai".into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                priority: Some(1),
                api_key: Some("c36-placeholder-key".into()),
                default_model: Some("gpt-c36".into()),
                ..Default::default()
            }];
        })
        .await
        .expect("seed C36 app");
    let mut state = AppState::new(test_db.db.clone());
    state.llm_admission = Arc::new(openproxy::server::api::admission::LlmAdmission::new(
        limit,
        Duration::from_millis(0),
    ));
    let admission = state.llm_admission.clone();
    (openproxy::build_app(state), test_db, admission)
}

fn chat_request(stream: bool) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": "openai/gpt-c36",
                "messages": [{"role": "user", "content": "admission"}],
                "stream": stream
            })
            .to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn overload_rejected_before_body_with_retry_after() {
    let release = Arc::new(Notify::new());
    let upstream =
        MockUpstream::start([success_json().holding_eof(release.clone()), success_json()]).await;
    let (app, _db, admission) = app_with_limit(&upstream, 1).await;

    // First request takes the only permit and blocks at the gated upstream.
    let first = tokio::spawn(app.clone().oneshot(chat_request(false)));
    upstream.wait_for_requests(1).await;
    assert_eq!(admission.active(), 1);

    // Second request must be refused before its body is extracted/routed:
    // no second upstream request, explicit overload code, Retry-After.
    let rejected = tokio::time::timeout(Duration::from_secs(5), app.oneshot(chat_request(false)))
        .await
        .expect("rejection is immediate")
        .expect("rejection response");
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(rejected.headers()["retry-after"], "1");
    let body = to_bytes(rejected.into_body(), 16 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], ADMISSION_REJECT_CODE);
    assert_eq!(upstream.requests().await.len(), 1);
    assert_eq!(admission.rejected(), 1);

    // Releasing the gate lets the first generation finish normally.
    release.notify_waiters();
    let first = tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .expect("first completes")
        .expect("join")
        .expect("first response");
    assert_eq!(first.status(), StatusCode::OK);
    // The completed response still owns its permit (production hyper holds
    // it until the socket drains); drop it before asserting release.
    drop(first);
    assert_eq!(admission.active(), 0);
    upstream.shutdown().await;
}

#[tokio::test]
async fn cancelled_request_releases_its_permit() {
    let release = Arc::new(Notify::new());
    let upstream =
        MockUpstream::start([success_json().holding_eof(release.clone()), success_json()]).await;
    let (app, _db, admission) = app_with_limit(&upstream, 1).await;

    let held = tokio::spawn(app.clone().oneshot(chat_request(false)));
    upstream.wait_for_requests(1).await;
    assert_eq!(admission.active(), 1);

    // Cancel before the response (headers or body) completes.
    held.abort();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        admission.active(),
        0,
        "aborted generation must release its permit"
    );

    // A new generation is admitted after the cancellation.
    release.notify_waiters();
    let retry = tokio::time::timeout(Duration::from_secs(10), app.oneshot(chat_request(false)))
        .await
        .expect("retry admitted")
        .expect("retry response");
    assert_eq!(retry.status(), StatusCode::OK);
    drop(retry);
    assert_eq!(admission.active(), 0);
    upstream.shutdown().await;
}

#[tokio::test]
async fn health_and_preflight_bypass_admission() {
    let release = Arc::new(Notify::new());
    let upstream = MockUpstream::start([success_json().holding_eof(release.clone())]).await;
    let (app, _db, admission) = app_with_limit(&upstream, 1).await;

    let held = tokio::spawn(app.clone().oneshot(chat_request(false)));
    upstream.wait_for_requests(1).await;
    assert_eq!(admission.active(), 1);

    // Liveness never queues behind generations.
    let health = Request::builder()
        .method("GET")
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), app.clone().oneshot(health))
        .await
        .expect("health is fast")
        .expect("health response");
    assert_eq!(response.status(), StatusCode::OK);

    // CORS preflight on a generation route takes no permit either.
    let preflight = Request::builder()
        .method("OPTIONS")
        .uri("/v1/chat/completions")
        .body(Body::empty())
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), app.oneshot(preflight))
        .await
        .expect("preflight is fast")
        .expect("preflight response");
    assert_ne!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    held.abort();
    release.notify_waiters();
    upstream.shutdown().await;
}

#[tokio::test]
async fn long_sse_stream_is_not_killed_by_admission() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        "data: {\"id\":\"chatcmpl-c36\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl-c36\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"b\"}}]}\n\n",
        "data: [DONE]\n\n",
    ])
    .with_chunk_timing(Duration::from_millis(150), Duration::from_millis(150))])
    .await;
    let (app, _db, admission) = app_with_limit(&upstream, 4).await;

    let response = tokio::time::timeout(Duration::from_secs(15), app.oneshot(chat_request(true)))
        .await
        .expect("stream completes")
        .expect("stream response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let text = String::from_utf8(body.to_vec()).expect("sse text");
    assert!(text.contains("\"content\":\"a\"") || text.contains('a'));
    assert!(text.contains("[DONE]"));
    assert_eq!(admission.active(), 0);
    upstream.shutdown().await;
}

#[test]
fn source_guards_keep_admission_before_extraction() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let routes = std::fs::read_to_string(root.join("src/server/api/mod.rs")).unwrap();
    assert!(
        routes.contains("admission::admit_llm_request"),
        "LLM routes must run admission middleware"
    );
    let admission = std::fs::read_to_string(root.join("src/server/api/admission.rs")).unwrap();
    for required in [
        "OwnedSemaphorePermit",
        "extensions_mut",
        "TOO_MANY_REQUESTS",
        "admission_queue_full",
        "available_permits",
    ] {
        assert!(
            admission.contains(required),
            "admission implementation missing: {required}"
        );
    }
    assert!(
        !admission.contains("timeout(Duration::from_secs(30))")
            && !admission.contains("TOTAL_TIMEOUT"),
        "admission must not impose a fixed total stream timeout"
    );
    const {
        assert!(DEFAULT_MAX_ACTIVE_GENERATIONS >= 32);
    }
}
