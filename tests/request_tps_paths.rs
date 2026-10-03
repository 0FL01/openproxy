//! Original provider TPS through real attempt logging, both transports and
//! native/translated/forced/dashboard paths. No translated usage is the oracle.
mod common;

use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::db::sqlite::repo::request_repo::{self, RequestDetailFilter};
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

async fn app(upstream: &MockUpstream, provider: &str, claude: bool) -> (axum::Router, TempTestDb) {
    let db = TempTestDb::new().await;
    db.db
        .update(|data| {
            data.api_keys = vec![test_api_key()];
            data.settings.require_api_key = true;
            data.settings.require_login = false;
            data.provider_nodes = vec![ProviderNode {
                id: provider.into(),
                name: provider.into(),
                prefix: Some(provider.into()),
                r#type: if claude {
                    "anthropic-compatible"
                } else {
                    "openai-compatible"
                }
                .into(),
                api_type: Some(
                    if claude {
                        "messages"
                    } else if provider.contains("responses") {
                        "responses"
                    } else {
                        "chat"
                    }
                    .into(),
                ),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            data.provider_connections = vec![ProviderConnection {
                id: "tps-fixture-account".into(),
                provider: provider.into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                api_key: Some("placeholder-tps-key".into()),
                default_model: Some("fixture".into()),
                ..Default::default()
            }];
        })
        .await
        .unwrap();
    (openproxy::build_app(AppState::new(db.db.clone())), db)
}

fn request(provider: &str, route: &str, stream: bool) -> Request<Body> {
    let body = if route == "/v1/messages" {
        json!({"model":format!("{provider}/fixture"),"messages":[{"role":"user","content":"hello"}],"max_tokens":32,"stream":stream})
    } else if route == "/v1/responses" {
        json!({"model":format!("{provider}/fixture"),"input":"hello","stream":stream})
    } else {
        json!({"model":format!("{provider}/fixture"),"messages":[{"role":"user","content":"hello"}],"stream":stream})
    };
    Request::builder()
        .method("POST")
        .uri(route)
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn observation(db: &TempTestDb) -> Value {
    let rows = db
        .db
        .sqlite
        .with_conn(|conn| request_repo::list(conn, &RequestDetailFilter::default(), 10, 0))
        .unwrap();
    assert_eq!(rows.len(), 1);
    rows[0]
        .data
        .get("upstreamTps")
        .cloned()
        .unwrap_or(Value::Null)
}

fn chat_sse() -> String {
    concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}],\"usage\":{\"completion_tokens\":20}}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"completion_tokens\":7,\"completion_tokens_details\":{\"reasoning_tokens\":3}}}\n\n",
        "data: [DONE]\n\n"
    ).into()
}

fn claude_sse() -> String {
    concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fixture\",\"model\":\"fixture\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":2,\"output_tokens\":99}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    ).into()
}

#[tokio::test]
async fn original_tps_native_translated_forced_and_dashboard_transport_parity() {
    for (provider, claude, route, stream) in [
        ("openai-compatible-tps", false, "/v1/chat/completions", true),
        ("openai-compatible-tps", false, "/v1/responses", true),
        ("openai", false, "/v1/chat/completions", false),
        (
            "openai-compatible-tps",
            false,
            "/api/dashboard/chat/completions",
            true,
        ),
        ("anthropic-compatible-tps", true, "/v1/messages", true),
        (
            "anthropic-compatible-tps",
            true,
            "/v1/chat/completions",
            true,
        ),
        (
            "anthropic-compatible-tps",
            true,
            "/api/dashboard/chat/completions",
            true,
        ),
    ] {
        let fixture = if claude { claude_sse() } else { chat_sse() };
        let upstream = MockUpstream::start([ScriptedResponse::sse([fixture])
            .with_chunk_timing(Duration::from_millis(15), Duration::ZERO)])
        .await;
        let (app, db) = app(&upstream, provider, claude).await;
        let response = app.oneshot(request(provider, route, stream)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{provider} {route}");
        let _body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let value = observation(&db).await;
        assert_eq!(
            value["generatedOutputTokens"], 7,
            "{provider} {route}: {value}"
        );
        assert_eq!(value["endKind"], "protocol_terminal");
        assert!(value["elapsedMicros"].as_u64().unwrap() >= 10_000);
        let requests = upstream.requests().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path.ends_with("/messages"), claude);
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn original_tps_json_before_translation_and_explicit_zero() {
    for (provider, claude, body, count) in [
        (
            "cline",
            false,
            json!({"success":true,"data":{"choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"completion_tokens":7}}}),
            7,
        ),
        (
            "openai-compatible-tps",
            false,
            json!({"choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"completion_tokens":0}}),
            0,
        ),
        (
            "anthropic-compatible-tps",
            true,
            json!({"id":"msg_fixture","type":"message","role":"assistant","model":"fixture","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","usage":{"input_tokens":2,"output_tokens":7}}),
            7,
        ),
        (
            "openai-compatible-responses-tps",
            false,
            json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","status":"completed","content":[{"type":"output_text","text":"hello"}]}],"usage":{"output_tokens":8,"output_tokens_details":{"reasoning_tokens":3}}}}),
            8,
        ),
    ] {
        let upstream =
            MockUpstream::start([ScriptedResponse::json(StatusCode::OK, body.to_string())
                .with_chunk_timing(Duration::from_millis(15), Duration::ZERO)])
            .await;
        let (app, db) = app(&upstream, provider, claude).await;
        let response = app
            .oneshot(request(provider, "/v1/chat/completions", false))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let value = observation(&db).await;
        assert_eq!(value["generatedOutputTokens"], count, "{provider}: {value}");
        assert_eq!(value["endKind"], "json_body");
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn original_tps_http200_errors_and_unfinished_usage_remain_null() {
    for fixture in [
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}],\"usage\":{\"completion_tokens\":7}}\n\n".to_string(),
        chat_sse() + "data: {\"error\":{\"message\":\"late protocol failure\"}}\n\n",
        "data: {\"type\":\"error\",\"error\":{\"message\":\"protocol failure\"}}\n\n".to_string(),
    ] {
        let upstream = MockUpstream::start([ScriptedResponse::sse([fixture])]).await;
        let (app, db) = app(&upstream, "openai-compatible-tps", false).await;
        let response = app.oneshot(request("openai-compatible-tps", "/v1/chat/completions", true)).await.unwrap();
        to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        assert!(observation(&db).await.is_null());
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn original_tps_provisional_counters_do_not_become_final_on_any_chat_path() {
    let chat = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}],\"usage\":{\"completion_tokens\":9}}\n\n",
        "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"completion_tokens\":null}}\n\n",
        "data: [DONE]\n\n"
    );
    let claude = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fixture\",\"model\":\"fixture\",\"role\":\"assistant\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":9}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":null}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    for (provider, anthropic, route, stream) in [
        ("openai-compatible-tps", false, "/v1/chat/completions", true),
        ("openai-compatible-tps", false, "/v1/responses", true),
        ("openai", false, "/v1/chat/completions", false),
        (
            "openai-compatible-tps",
            false,
            "/api/dashboard/chat/completions",
            true,
        ),
        ("anthropic-compatible-tps", true, "/v1/messages", true),
        (
            "anthropic-compatible-tps",
            true,
            "/v1/chat/completions",
            true,
        ),
        (
            "anthropic-compatible-tps",
            true,
            "/api/dashboard/chat/completions",
            true,
        ),
    ] {
        let upstream = MockUpstream::start([ScriptedResponse::sse([if anthropic {
            claude
        } else {
            chat
        }])])
        .await;
        let (app, db) = app(&upstream, provider, anthropic).await;
        let response = app.oneshot(request(provider, route, stream)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{provider} {route}");
        to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        assert!(observation(&db).await.is_null(), "{provider} {route}");
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn original_tps_responses_requires_final_counter_and_consistent_event() {
    for terminal in [
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[],\"usage\":{\"output_tokens\":null}}}\n\n",
        "event: response.created\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[],\"usage\":{\"output_tokens\":9}}}\n\n",
    ] {
        for route in ["/v1/responses", "/v1/chat/completions", "/api/dashboard/chat/completions"] {
            let fixture = "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\",\"usage\":{\"output_tokens\":9}}}\n\n".to_string() + terminal;
            let upstream = MockUpstream::start([ScriptedResponse::sse([fixture])]).await;
            let (app, db) = app(&upstream, "openai-compatible-responses-tps", false).await;
            let response = app.oneshot(request("openai-compatible-responses-tps", route, true)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{route}");
            to_bytes(response.into_body(), 256 * 1024).await.unwrap();
            assert!(observation(&db).await.is_null(), "{route}: {terminal}");
            upstream.shutdown().await;
        }
    }
}

#[tokio::test]
async fn original_tps_collected_dashboard_stops_at_terminal_read_before_delayed_tail() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        chat_sse(),
        ": trailing heartbeat\n\n".to_string(),
    ])
    .with_chunk_timing(Duration::from_millis(15), Duration::from_millis(250))])
    .await;
    let (app, db) = app(&upstream, "openai-compatible-tps", false).await;
    let response = app
        .oneshot(request(
            "openai-compatible-tps",
            "/api/dashboard/chat/completions",
            true,
        ))
        .await
        .unwrap();
    to_bytes(response.into_body(), 256 * 1024).await.unwrap();
    let value = observation(&db).await;
    assert_eq!(value["generatedOutputTokens"], 7);
    assert!(
        value["elapsedMicros"].as_u64().unwrap() < 200_000,
        "terminal timing must exclude the delayed tail: {value}"
    );
    upstream.shutdown().await;
}

#[tokio::test]
async fn original_tps_forced_terminal_plus_malformed_tail_keeps_existing_error() {
    let fixture = chat_sse() + &"x".repeat(1_050_000);
    let upstream = MockUpstream::start([ScriptedResponse::sse([fixture])]).await;
    let (app, db) = app(&upstream, "openai", false).await;
    let response = app
        .oneshot(request("openai", "/v1/chat/completions", false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    to_bytes(response.into_body(), 256 * 1024).await.unwrap();
    assert!(observation(&db).await.is_null());
    upstream.shutdown().await;
}
