//! Claude→Responses stream failure surfacing (production incident follow-up).
//!
//! A masked OpenCode session delegating to a subagent retried forever on
//! "Server error": the proxy swallowed Anthropic mid-stream `error` events
//! (200 headers, in-band failure) and logged truncated streams as success.
//! These tests pin the three contract points: upstream error events surface
//! as `upstream_error_event` with an upstream error kind, truncated Claude
//! streams are flagged, and complete streams keep their success shape. Claude
//! refusals are explicit, non-retried failures on translated routes; native
//! Messages JSON/SSE retain their byte-exact successful HTTP transport shape.

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
const REFUSAL_DELTA: &str = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"},\"usage\":{\"input_tokens\":3902,\"output_tokens\":0}}\n\n";
const REFUSAL_MESSAGE_BODY: &str = "{\"type\":\"message\",\"id\":\"msg_refusal\",\"content\":[],\"stop_reason\":\"refusal\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}";
const REFUSAL_MESSAGE: &str = "Claude refused to respond (stop_reason=refusal).";

fn claude_request(route: &str, stream: bool) -> Request<Body> {
    let body = if route == "/v1/responses" {
        json!({
            "model": "claude/claude-opus-5-5",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "stream": stream
        })
    } else {
        json!({
            "model": "claude/claude-opus-5-5",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 64,
            "stream": stream
        })
    };
    Request::builder()
        .method("POST")
        .uri(route)
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .header("x-openproxy-claude-mask", "1")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[derive(Debug)]
struct SseEvent {
    event: String,
    data: Value,
}

fn assert_refusal_stream(body: &str) -> Vec<SseEvent> {
    assert!(
        body.ends_with("\n\n"),
        "unterminated downstream SSE: {body}"
    );
    assert!(
        !body.contains("[DONE]"),
        "refusal must not send DONE: {body}"
    );
    let events: Vec<_> = body
        .split("\n\n")
        .filter(|frame| !frame.trim().is_empty())
        .map(|frame| {
            let mut event = None;
            let mut data = Vec::new();
            for line in frame.lines() {
                if let Some(name) = line.strip_prefix("event:") {
                    assert!(event.is_none(), "duplicate event field: {frame}");
                    event = Some(name.trim().to_string());
                } else if let Some(payload) = line.strip_prefix("data:") {
                    data.push(payload.trim_start());
                } else {
                    assert!(line.starts_with(':'), "unexpected SSE field: {frame}");
                }
            }
            SseEvent {
                event: event.expect("projected SSE event name"),
                data: serde_json::from_str(&data.join("\n")).expect("projected SSE JSON"),
            }
        })
        .collect();
    assert!(
        events.len() >= 2,
        "refusal must follow committed SSE: {body}"
    );
    assert_eq!(
        events.iter().filter(|event| event.event == "error").count(),
        1,
        "exactly one terminal error: {body}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.data["type"] == "error")
            .count(),
        1,
        "exactly one error payload: {body}"
    );
    assert!(
        events
            .iter()
            .all(|event| event.event != "response.completed"
                && event.data["type"] != "response.completed"),
        "refusal must not claim completion: {body}"
    );
    // Every emitted event is sequenced, including the final error: no reset,
    // duplicate, skipped number, or trailing frame is acceptable.
    for pair in events.windows(2) {
        let previous = pair[0].data["sequence_number"]
            .as_u64()
            .expect("prior sequence");
        let next = pair[1].data["sequence_number"]
            .as_u64()
            .expect("next sequence");
        assert_eq!(next, previous + 1, "non-contiguous SSE sequence: {body}");
    }
    let terminal = events.last().expect("terminal error");
    assert_eq!(
        terminal.event, "error",
        "error must be strictly last: {body}"
    );
    assert_eq!(terminal.data["type"], "error");
    assert_eq!(terminal.data["code"], "invalid_prompt");
    assert_eq!(terminal.data["message"], REFUSAL_MESSAGE);
    assert!(terminal.data.get("param").is_some_and(Value::is_null));
    let nested = &terminal.data["error"];
    assert_eq!(nested["type"], "invalid_request_error");
    assert_eq!(nested["code"], terminal.data["code"]);
    assert_eq!(nested["message"], terminal.data["message"]);
    assert!(nested.get("param").is_some_and(Value::is_null));
    events
}

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

async fn assert_refusal_log(
    app: &axum::Router,
    test_db: &TempTestDb,
    upstream: &MockUpstream,
    route: &str,
    status_code: u16,
    input_tokens: u64,
) -> (Value, Value) {
    assert_eq!(
        upstream.request_count().await,
        1,
        "refusal must not retry generation"
    );
    let details = latest_request_details(test_db).await;
    let log = latest_public_request_log(app).await;
    for record in [&details, &log] {
        assert_eq!(record["route"], route, "{record}");
        assert_eq!(record["statusCode"], status_code, "{record}");
        assert_eq!(record["errorKind"], "upstream_failure", "{record}");
        assert_eq!(record["errorCode"], "invalid_prompt", "{record}");
        assert_eq!(record["errorMessage"], REFUSAL_MESSAGE, "{record}");
        assert_eq!(record["inputTokens"], input_tokens, "{record}");
        assert_eq!(
            record["outputTokens"], 0,
            "known zero must survive: {record}"
        );
    }
    assert_eq!(log["status"], "error", "{log}");
    assert!(
        details.get("upstreamTps").is_none(),
        "failed upstream TPS: {details}"
    );
    for field in [
        "tokensPerSecond",
        "generatedOutputTokens",
        "upstreamDurationMs",
    ] {
        assert!(
            log[field].is_null(),
            "failed upstream TPS field {field}: {log}"
        );
    }
    (details, log)
}

async fn assert_refusal_stream_log(
    app: &axum::Router,
    test_db: &TempTestDb,
    upstream: &MockUpstream,
) {
    let (details, log) =
        assert_refusal_log(app, test_db, upstream, "/v1/responses", 502, 3902).await;
    let trace = &details["streamTrace"];
    assert_eq!(trace["stopReason"], "refusal", "{trace}");
    assert_eq!(trace["errorCount"], 1, "{trace}");
    assert_eq!(trace["completedCount"], 0, "{trace}");
    assert_eq!(trace["framesAfterCompleted"], 0, "{trace}");
    assert_eq!(trace["doneSent"], false, "{trace}");
    let entries = trace["entries"].as_array().expect("bounded trace entries");
    assert!(entries.len() <= 64, "{trace}");
    assert!(
        entries
            .iter()
            .all(|entry| entry.as_str().unwrap().chars().count() <= 64),
        "{trace}"
    );
    for field in ["upstreamEvents", "emittedEvents", "itemTypes"] {
        assert!(trace[field].as_object().unwrap().len() <= 48, "{trace}");
    }
    if let Some(names) = trace["toolNames"].as_array() {
        assert!(names.len() <= 8, "{trace}");
    }
    assert_eq!(
        log["streamTrace"], *trace,
        "public trace must retain terminal: {log}"
    );
}

async fn assert_non_stream_refusal(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("error body");
    let payload: Value = serde_json::from_slice(&bytes).expect("error JSON");
    assert_eq!(payload["error"]["code"], "invalid_prompt");
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(payload["error"]["message"], REFUSAL_MESSAGE);
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

#[tokio::test]
async fn claude_refusal_surfaces_as_invalid_prompt_error() {
    // Incident shape: message_start, refusal delta with zero output tokens,
    // message_stop. Must not complete as an empty success.
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        REFUSAL_DELTA,
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
    assert_refusal_stream(&body);
    assert_refusal_stream_log(&app, &test_db, &upstream).await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_refusal_after_partial_text_keeps_text_and_fails() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        TEXT_DELTA,
        REFUSAL_DELTA,
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
    // Already-committed text stays; the terminal is still an explicit error.
    let events = assert_refusal_stream(&body);
    let text_deltas: Vec<_> = events[..events.len() - 1]
        .iter()
        .filter(|event| event.event == "response.output_text.delta")
        .map(|event| event.data["delta"].as_str().expect("text delta"))
        .collect();
    assert_eq!(
        text_deltas,
        ["partial"],
        "committed text before error: {body}"
    );
    assert_refusal_stream_log(&app, &test_db, &upstream).await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_refusal_after_partial_tool_args_does_not_invent_completion() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        TOOL_USE_START,
        TOOL_USE_ARGS_FIRST,
        REFUSAL_DELTA,
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
    let events = assert_refusal_stream(&body);
    let committed = &events[..events.len() - 1];
    let (item_at, item) = committed
        .iter()
        .enumerate()
        .find(|(_, event)| {
            event.event == "response.output_item.added"
                && event.data["item"]["type"] == "function_call"
        })
        .expect("committed tool call before refusal");
    assert_eq!(item.data["item"]["call_id"], "toolu_read_1");
    assert_eq!(item.data["item"]["name"], "read");
    let argument_deltas: Vec<_> = committed
        .iter()
        .enumerate()
        .filter(|(_, event)| event.event == "response.function_call_arguments.delta")
        .collect();
    assert_eq!(
        argument_deltas.len(),
        1,
        "committed partial arguments: {body}"
    );
    let (args_at, args) = argument_deltas[0];
    assert!(
        args_at > item_at,
        "arguments follow real tool identity: {body}"
    );
    assert_eq!(args.data["delta"], "{\"file_path\":");
    assert_eq!(args.data["item_id"], item.data["item"]["id"]);
    assert_eq!(args.data["output_index"], item.data["output_index"]);
    assert!(
        events.iter().all(
            |event| event.event != "response.function_call_arguments.done"
                && event.event != "response.output_item.done"
        ),
        "refusal must not invent argument completion: {body}"
    );
    assert_refusal_stream_log(&app, &test_db, &upstream).await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_refusal_and_stop_in_one_chunk_fails_once() {
    // One transport chunk carrying refusal + message_stop: single terminal.
    let combined = format!("{REFUSAL_DELTA}{MESSAGE_STOP}");
    let upstream =
        MockUpstream::start([ScriptedResponse::sse([MESSAGE_START.to_string(), combined])]).await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(responses_request())
        .await
        .expect("response");
    let body = read_body(response).await;
    assert_refusal_stream(&body);
    assert_refusal_stream_log(&app, &test_db, &upstream).await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_refusal_split_across_chunks_fails_once() {
    // The refusal frame split mid-JSON across transport chunks.
    let half = REFUSAL_DELTA.len() / 2;
    let (first, second) = REFUSAL_DELTA.split_at(half);
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        first,
        second,
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
    assert_refusal_stream(&body);
    assert_refusal_stream_log(&app, &test_db, &upstream).await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_data_only_refusal_uses_json_type_and_fails_once() {
    // Claude-compatible upstreams may omit event: and carry the event kind
    // only in JSON. Refusal detection and the journal must still agree.
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START.split_once('\n').unwrap().1,
        REFUSAL_DELTA.split_once('\n').unwrap().1,
        MESSAGE_STOP.split_once('\n').unwrap().1,
    ])])
    .await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(responses_request())
        .await
        .expect("response");
    let body = read_body(response).await;
    assert_refusal_stream(&body);
    assert_refusal_stream_log(&app, &test_db, &upstream).await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_refusal_in_delimiterless_eof_tail_is_logged_as_error() {
    // No frame delimiter and no message_stop: finish() must recognize the
    // refusal tail before its generic truncation guard or success logger.
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        TEXT_DELTA,
        REFUSAL_DELTA.trim_end_matches('\n'),
    ])])
    .await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(responses_request())
        .await
        .expect("response");
    let body = read_body(response).await;
    let events = assert_refusal_stream(&body);
    assert!(
        events[..events.len() - 1]
            .iter()
            .any(|event| event.event == "response.output_text.delta"
                && event.data["delta"] == "partial"),
        "committed text before EOF refusal: {body}"
    );
    assert_refusal_stream_log(&app, &test_db, &upstream).await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_refusal_non_stream_returns_400_invalid_prompt() {
    let upstream =
        MockUpstream::start([ScriptedResponse::json(StatusCode::OK, REFUSAL_MESSAGE_BODY)]).await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(claude_request("/v1/responses", false))
        .await
        .expect("response");
    assert_non_stream_refusal(response).await;
    assert_refusal_log(&app, &test_db, &upstream, "/v1/responses", 400, 10).await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn claude_refusal_non_stream_chat_returns_400_invalid_prompt() {
    let upstream =
        MockUpstream::start([ScriptedResponse::json(StatusCode::OK, REFUSAL_MESSAGE_BODY)]).await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(claude_request("/v1/chat/completions", false))
        .await
        .expect("response");
    assert_non_stream_refusal(response).await;
    assert_refusal_log(&app, &test_db, &upstream, "/v1/chat/completions", 400, 10).await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn dashboard_collected_claude_refusal_returns_400_and_logs_error() {
    // The dashboard requests SSE, but collects a non-stream upstream JSON
    // message first. It must reject refusal before constructing success SSE.
    let route = "/api/dashboard/chat/completions";
    let upstream =
        MockUpstream::start([ScriptedResponse::json(StatusCode::OK, REFUSAL_MESSAGE_BODY)]).await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(claude_request(route, true))
        .await
        .expect("dashboard response");
    assert_non_stream_refusal(response).await;
    assert_refusal_log(&app, &test_db, &upstream, route, 400, 10).await;
    let requests = upstream.requests().await;
    assert_eq!(requests[0].path, "/v1/messages");
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("collected upstream request");
    assert_eq!(body["stream"], false, "dashboard collection path: {body}");
    upstream.shutdown().await;
}

#[tokio::test]
async fn native_claude_refusal_passes_through_unchanged() {
    // Native /v1/messages: the refusal detector must not rewrite bytes.
    let upstream =
        MockUpstream::start([ScriptedResponse::json(StatusCode::OK, REFUSAL_MESSAGE_BODY)]).await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(claude_request("/v1/messages", false))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("downstream body");
    assert_eq!(bytes.as_ref(), REFUSAL_MESSAGE_BODY.as_bytes());

    assert_eq!(upstream.request_count().await, 1);
    let details = latest_request_details(&test_db).await;
    assert_eq!(details["statusCode"], 200);
    assert!(details.get("errorCode").is_none(), "{details}");
    assert!(details.get("errorKind").is_none(), "{details}");
    let log = latest_public_request_log(&app).await;
    assert_eq!(log["statusCode"], 200);
    assert_eq!(log["status"], "success");
    assert!(log.get("errorCode").is_none(), "{log}");
    upstream.shutdown().await;
}

#[tokio::test]
async fn native_claude_sse_refusal_passes_through_byte_exact_with_200() {
    let chunks = [MESSAGE_START, TEXT_DELTA, REFUSAL_DELTA, MESSAGE_STOP];
    let upstream = MockUpstream::start([ScriptedResponse::sse(chunks)]).await;
    let (app, test_db) = app_for(&upstream).await;

    let response = app
        .clone()
        .oneshot(claude_request("/v1/messages", true))
        .await
        .expect("native SSE response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("native SSE body");
    assert_eq!(bytes.as_ref(), chunks.concat().as_bytes());
    assert_eq!(upstream.request_count().await, 1);
    let details = latest_request_details(&test_db).await;
    assert_eq!(details["statusCode"], 200);
    assert!(details.get("errorCode").is_none(), "{details}");
    assert!(details.get("errorKind").is_none(), "{details}");
    let log = latest_public_request_log(&app).await;
    assert_eq!(log["statusCode"], 200);
    assert_eq!(log["status"], "success");
    assert!(log.get("errorCode").is_none(), "{log}");
    upstream.shutdown().await;
}

#[tokio::test]
async fn ordinary_empty_end_turn_still_completes() {
    // Guard against over-triggering: an empty end_turn turn keeps its
    // long-standing successful completion shape.
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        MESSAGE_START,
        END_TURN_FINISH,
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
    assert_eq!(
        body.matches("event: response.completed").count(),
        1,
        "{body}"
    );
    assert!(!body.contains("event: error"), "{body}");
    assert!(!body.contains("\"type\":\"error\""), "{body}");

    assert_eq!(upstream.request_count().await, 1);
    let details = latest_request_details(&test_db).await;
    assert_eq!(details["statusCode"], 200);
    assert!(details.get("errorCode").is_none(), "{details}");
    assert!(details.get("errorKind").is_none(), "{details}");
    assert_eq!(details["streamTrace"]["completedCount"], 1, "{details}");
    assert_eq!(details["streamTrace"]["errorCount"], 0, "{details}");
    upstream.shutdown().await;
}
