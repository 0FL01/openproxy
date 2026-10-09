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

fn responses_tool_request() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .header("x-openproxy-claude-mask", "1")
        .body(Body::from(
            json!({
                "model": "claude/claude-opus-5-5",
                "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Read the file"}]}],
                "tools": [{
                    "type": "function",
                    "name": "read",
                    "description": "Read a file",
                    "parameters": {"type": "object", "properties": {"file_path": {"type": "string"}}, "required": ["file_path"]}
                }],
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
const THINKING_START: &str = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n";
const THINKING_DELTA: &str = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"let me check\"}}\n\n";
const THINKING_STOP: &str =
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n";
const TOOL_USE_START: &str = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_read_1\",\"name\":\"read\",\"input\":{}}}\n\n";
const TOOL_USE_ARGS_FIRST: &str = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"file_path\\\":\"}}\n\n";
const TOOL_USE_ARGS_SECOND: &str = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"src/lib.rs\\\"}\"}}\n\n";
const TOOL_USE_STOP: &str =
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n";
const TOOL_USE_FINISH: &str = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}\n\n";
const END_TURN_FINISH: &str = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}\n\n";

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

async fn latest_public_request_log(app: &axum::Router) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/request-logs?page=1&pageSize=10")
                .header("authorization", "Bearer test-key")
                .body(Body::empty())
                .expect("request logs request"),
        )
        .await
        .expect("request logs response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("request logs body");
    let payload: Value = serde_json::from_slice(&bytes).expect("request logs JSON");
    payload["requests"][0].clone()
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

    let response = app
        .clone()
        .oneshot(responses_request())
        .await
        .expect("response");
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
    let log = latest_public_request_log(&app).await;
    assert_eq!(log["route"], "/v1/responses");
    assert_eq!(log["statusCode"], 502);
    assert_eq!(log["errorCode"], "upstream_error_event");
    assert!(log["errorMessage"]
        .as_str()
        .unwrap()
        .contains("overloaded_error"));
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

    let response = app
        .clone()
        .oneshot(responses_request())
        .await
        .expect("response");
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
    let log = latest_public_request_log(&app).await;
    assert_eq!(log["route"], "/v1/responses");
    assert_eq!(log["statusCode"], 502);
    assert_eq!(log["errorCode"], "upstream_stream_truncated");
    assert!(log["errorMessage"]
        .as_str()
        .unwrap()
        .contains("Claude stream ended"));
    upstream.shutdown().await;
}

#[tokio::test]
async fn complete_claude_stream_still_completes_the_responses_projection() {
    // Includes a thinking block: the `</think>` chunk used to trigger a
    // premature response.completed (null finish_reason was treated as
    // terminal by the shared helper). Completion must stay at the end.
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        THINKING_START,
        THINKING_DELTA,
        THINKING_STOP,
        TEXT_DELTA,
        MESSAGE_STOP,
    ])])
    .await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(responses_request())
        .await
        .expect("response");
    let body = read_body(response).await;
    assert!(body.contains("response.completed"), "{body}");
    assert!(body.contains("partial"), "{body}");
    assert!(!body.contains("upstream_stream_truncated"), "{body}");
    // Completion must be the LAST terminal event, after all text content.
    let completed_at = body.rfind("response.completed").expect("completed index");
    let text_done_at = body
        .rfind("response.output_text.delta")
        .expect("text delta index");
    assert!(
        completed_at > text_done_at,
        "response.completed must come after text deltas: {body}"
    );

    let details = latest_request_details(&test_db).await;
    assert_eq!(details["statusCode"], 200);
    assert!(
        details.get("errorKind").is_none(),
        "complete stream must log success: {details}"
    );
    // Pure-text turn with thinking: reasoning + message items, no tools,
    // exactly one completion, nothing emitted after it.
    let trace = &details["streamTrace"];
    assert_eq!(trace["upstreamEvents"]["message_start"], 1, "{trace}");
    assert_eq!(
        trace["upstreamEvents"]["content_block_start:thinking"], 1,
        "{trace}"
    );
    assert_eq!(trace["itemTypes"]["reasoning"], 1, "{trace}");
    assert_eq!(trace["itemTypes"]["message"], 1, "{trace}");
    assert!(trace["itemTypes"].get("function_call").is_none(), "{trace}");
    assert_eq!(trace["completedCount"], 1, "{trace}");
    assert_eq!(trace["errorCount"], 0, "{trace}");
    assert_eq!(trace["framesAfterCompleted"], 0, "{trace}");
    let log = latest_public_request_log(&app).await;
    assert_eq!(log["route"], "/v1/responses");
    assert_eq!(log["statusCode"], 200);
    assert!(log.get("errorCode").is_none());
    assert!(log.get("errorMessage").is_none());
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_tool_use_argument_deltas_complete_responses_tool_call() {
    // Thinking block first: the tool call used to be emitted AFTER a
    // premature response.completed triggered by the `</think>` chunk.
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        THINKING_START,
        THINKING_DELTA,
        THINKING_STOP,
        TOOL_USE_START,
        TOOL_USE_ARGS_FIRST,
        TOOL_USE_ARGS_SECOND,
        TOOL_USE_STOP,
        TOOL_USE_FINISH,
        MESSAGE_STOP,
    ])])
    .await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(responses_tool_request())
        .await
        .expect("response");
    let body = read_body(response).await;
    assert!(body.contains("response.output_item.added"), "{body}");
    assert!(body.contains("\"type\":\"function_call\""), "{body}");
    assert!(body.contains("\"call_id\":\"toolu_read_1\""), "{body}");
    assert!(body.contains("\"name\":\"read\""), "{body}");
    assert!(
        body.contains("response.function_call_arguments.delta"),
        "{body}"
    );
    assert!(body.contains("\"delta\":\"{\\\"file_path\\\":\""), "{body}");
    assert!(body.contains("src/lib.rs"), "{body}");
    assert!(
        body.contains("response.function_call_arguments.done"),
        "{body}"
    );
    assert!(
        body.contains("\"arguments\":\"{\\\"file_path\\\":\\\"src/lib.rs\\\"}\""),
        "{body}"
    );
    assert!(body.contains("response.completed"), "{body}");
    assert!(
        !body.contains("upstream_stream_invalid_tool_call"),
        "{body}"
    );
    // Completion must come after the full tool call, not before it.
    let completed_at = body.rfind("response.completed").expect("completed index");
    let args_done_at = body
        .rfind("response.function_call_arguments.done")
        .expect("args done index");
    assert!(
        completed_at > args_done_at,
        "response.completed must come after function_call_arguments.done: {body}"
    );

    upstream.wait_for_requests(1).await;
    let details = latest_request_details(&test_db).await;
    assert_eq!(details["statusCode"], 200);
    assert!(details.get("errorCode").is_none(), "{details}");
    assert!(details.get("errorKind").is_none(), "{details}");
    // Stream trace: upstream saw a thinking + tool_use turn; the Responses
    // projection emitted the function_call item and one terminal completion.
    let trace = &details["streamTrace"];
    assert_eq!(trace["upstreamEvents"]["message_start"], 1, "{trace}");
    assert_eq!(
        trace["upstreamEvents"]["content_block_start:thinking"], 1,
        "{trace}"
    );
    assert_eq!(
        trace["upstreamEvents"]["content_block_start:tool_use"], 1,
        "{trace}"
    );
    assert_eq!(trace["stopReason"], "tool_use", "{trace}");
    assert_eq!(trace["itemTypes"]["reasoning"], 1, "{trace}");
    assert_eq!(trace["itemTypes"]["function_call"], 1, "{trace}");
    assert_eq!(trace["toolNames"][0], "read", "{trace}");
    assert_eq!(trace["completedCount"], 1, "{trace}");
    assert_eq!(trace["errorCount"], 0, "{trace}");
    assert_eq!(trace["framesAfterCompleted"], 0, "{trace}");
    let log = latest_public_request_log(&app).await;
    assert_eq!(log["statusCode"], 200);
    assert!(log.get("errorCode").is_none(), "{log}");
    assert!(log.get("errorMessage").is_none(), "{log}");
    let projected_trace = &log["streamTrace"];
    assert_eq!(projected_trace["completedCount"], 1, "{projected_trace}");
    assert_eq!(
        projected_trace["itemTypes"]["function_call"], 1,
        "{projected_trace}"
    );
    assert_eq!(
        projected_trace["stopReason"], "tool_use",
        "{projected_trace}"
    );
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_events_after_message_stop_are_visible_in_stream_trace() {
    // Anomaly detector: upstream text after message_stop would produce a
    // downstream delta AFTER response.completed — the class of defect that
    // makes an agent client restart its turn. The trace must surface it.
    // Usage rides message_delta like a real Anthropic stream so the terminal
    // lands on message_stop, before the stray post-stop delta.
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        TEXT_DELTA,
        END_TURN_FINISH,
        MESSAGE_STOP,
        TEXT_DELTA,
    ])])
    .await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(responses_request())
        .await
        .expect("response");
    let body = read_body(response).await;
    assert!(body.contains("response.completed"), "{body}");

    let details = latest_request_details(&test_db).await;
    let trace = &details["streamTrace"];
    assert_eq!(trace["completedCount"], 1, "{trace}");
    assert!(
        trace["framesAfterCompleted"].as_u64().unwrap_or(0) > 0,
        "post-completed frame must be recorded: {trace}"
    );
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_http_error_diagnostic_is_logged_and_redacted_from_request_logs() {
    let upstream_body = json!({
        "type": "error",
        "error": {
            "type": "invalid_request_error",
            "message": "Invalid tool declaration. Authorization: Bearer bearer-secret x-api-key: header-secret sk-ant-body-secret",
            "details": {"error_code": "tool_schema_invalid"}
        }
    })
    .to_string();
    let upstream = MockUpstream::start([ScriptedResponse::json(
        StatusCode::BAD_REQUEST,
        upstream_body.clone(),
    )])
    .await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(responses_request())
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let downstream_body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("downstream body");
    assert_eq!(downstream_body.as_ref(), upstream_body.as_bytes());
    upstream.wait_for_requests(1).await;

    let details = latest_request_details(&test_db).await;
    assert_eq!(details["route"], "/v1/responses");
    assert_eq!(details["statusCode"], 400);
    assert_eq!(details["errorKind"], "invalid_request");
    assert_eq!(details["errorCode"], "tool_schema_invalid");
    assert!(!details.to_string().contains("bearer-secret"));
    assert!(!details.to_string().contains("header-secret"));
    assert!(!details.to_string().contains("sk-ant-body-secret"));

    let log = latest_public_request_log(&app).await;
    assert_eq!(log["route"], "/v1/responses");
    assert_eq!(log["statusCode"], 400);
    assert_eq!(log["errorKind"], "invalid_request");
    assert_eq!(log["errorCode"], "tool_schema_invalid");
    let message = log["errorMessage"].as_str().unwrap();
    assert!(message.starts_with("Invalid tool declaration."));
    assert!(message.contains("Authorization: [REDACTED]"));
    assert!(message.contains("x-api-key: [REDACTED]"));
    assert!(!log.to_string().contains("bearer-secret"));
    assert!(!log.to_string().contains("header-secret"));
    assert!(!log.to_string().contains("sk-ant-body-secret"));
    assert!(!log.to_string().contains("error body"));

    upstream.shutdown().await;
}
