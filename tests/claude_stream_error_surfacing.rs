//! Claude→Responses stream failure surfacing (production incident follow-up).
//!
//! A masked OpenCode session delegating to a subagent retried forever on
//! "Server error": the proxy swallowed Anthropic mid-stream `error` events
//! (200 headers, in-band failure) and logged truncated streams as success.
//! These tests pin the three contract points: upstream error events surface
//! as `upstream_error_event` with an upstream error kind, truncated Claude
//! streams are flagged, and complete streams keep their success shape.

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

async fn app_for(upstream: &MockUpstream) -> (axum::Router, TempTestDb) {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![ProviderNode {
                id: "claude".into(),
                r#type: "anthropic-compatible".into(),
                name: "Claude surfacing upstream".into(),
                prefix: Some("claude".into()),
                api_type: Some("messages".into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            db.provider_connections = vec![ProviderConnection {
                id: "claude-surfacing-account".into(),
                provider: "claude".into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                priority: Some(1),
                api_key: Some("claude-surfacing-key".into()),
                default_model: Some("claude-opus-5-5".into()),
                ..Default::default()
            }];
        })
        .await
        .expect("seed claude surfacing app");
    (
        openproxy::build_app(AppState::new(test_db.db.clone())),
        test_db,
    )
}

fn responses_request() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .header("x-openproxy-claude-mask", "1")
        .body(Body::from(
            json!({
                "model": "claude/claude-opus-5-5",
                "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
                "stream": true
            })
            .to_string(),
        ))
        .unwrap()
}

async fn read_body(response: axum::response::Response) -> String {
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("stream body");
    String::from_utf8_lossy(&bytes).into_owned()
}

const MESSAGE_START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_surf\",\"model\":\"claude-opus-5-5\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":3}}}\n\n";
const TEXT_DELTA: &str = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n";
const MESSAGE_STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

async fn latest_request_details(test_db: &TempTestDb) -> Value {
    use rusqlite::Connection;
    let db = test_db.db.sqlite.clone();
    tokio::task::spawn_blocking(move || {
        db.with_conn(|conn: &mut Connection| {
            conn.query_row(
                "SELECT data FROM requestDetails ORDER BY timestamp DESC LIMIT 1",
                [],
                |row| {
                    let data: String = row.get(0)?;
                    Ok(serde_json::from_str::<Value>(&data).expect("details JSON"))
                },
            )
        })
        .expect("read latest requestDetails")
    })
    .await
    .expect("join details read")
}

#[tokio::test]
async fn claude_upstream_error_event_surfaces_as_streaming_error() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        TEXT_DELTA,
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
    ])])
    .await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app.oneshot(responses_request()).await.expect("response");
    let body = read_body(response).await;
    assert!(
        body.contains("\"type\":\"error\"") || body.contains("event: error"),
        "client must see an error event: {body}"
    );
    assert!(body.contains("upstream_error_event"), "{body}");

    let details = latest_request_details(&test_db).await;
    assert_eq!(details["errorKind"], "upstream_failure");
    assert_eq!(details["errorCode"], "upstream_error_event");
    assert_eq!(details["statusCode"], 502);
    assert!(
        details["errorMessage"]
            .as_str()
            .is_some_and(|message| message.contains("overloaded_error")),
        "persisted message: {}",
        details["errorMessage"]
    );
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_truncated_stream_is_flagged_not_logged_success() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        TEXT_DELTA,
        // EOF without message_stop: the Responses projection started but
        // never reached response.completed.
    ])])
    .await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app.oneshot(responses_request()).await.expect("response");
    let body = read_body(response).await;
    assert!(body.contains("upstream_stream_truncated"), "{body}");
    assert!(
        !body.contains("response.completed"),
        "a truncated stream must not claim completion: {body}"
    );

    let details = latest_request_details(&test_db).await;
    assert_eq!(details["errorKind"], "local_failure");
    assert_eq!(details["errorCode"], "upstream_stream_truncated");
    assert_eq!(details["statusCode"], 502);
    upstream.shutdown().await;
}

#[tokio::test]
async fn complete_claude_stream_still_completes_the_responses_projection() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        TEXT_DELTA,
        MESSAGE_STOP,
    ])])
    .await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app.oneshot(responses_request()).await.expect("response");
    let body = read_body(response).await;
    assert!(body.contains("response.completed"), "{body}");
    assert!(body.contains("partial"), "{body}");
    assert!(!body.contains("upstream_stream_truncated"), "{body}");

    let details = latest_request_details(&test_db).await;
    assert_eq!(details["statusCode"], 200);
    assert!(
        details.get("errorKind").is_none(),
        "complete stream must log success: {details}"
    );
    upstream.shutdown().await;
}
