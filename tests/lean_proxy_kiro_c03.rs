mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use futures_util::StreamExt;
use openproxy::core::executor::{ClientPool, KiroExecutionRequest, KiroExecutor, UpstreamResponse};
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::util::ServiceExt;

fn eventstream_frame(event_type: &str, payload: &Value) -> Bytes {
    let name = b":event-type";
    let value = event_type.as_bytes();
    let mut headers = Vec::new();
    headers.push(name.len() as u8);
    headers.extend_from_slice(name);
    headers.push(7u8);
    headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
    headers.extend_from_slice(value);

    let payload = serde_json::to_vec(payload).expect("serialize EventStream payload");
    let total = 12 + headers.len() + payload.len() + 4;
    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&(total as u32).to_be_bytes());
    frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    let prelude_crc = crc32fast::hash(&frame[..8]);
    frame.extend_from_slice(&prelude_crc.to_be_bytes());
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(&payload);
    let message_crc = crc32fast::hash(&frame);
    frame.extend_from_slice(&message_crc.to_be_bytes());
    Bytes::from(frame)
}

fn kiro_node(endpoint: String) -> ProviderNode {
    ProviderNode {
        id: "kiro".into(),
        r#type: "kiro".into(),
        name: "Kiro".into(),
        prefix: Some("kr".into()),
        api_type: Some("chat".into()),
        base_url: Some(endpoint),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn kiro_connection(id: &str, repair_setting: Option<bool>) -> ProviderConnection {
    let mut connection = common::test_connection("kiro");
    connection.id = id.to_string();
    connection.auth_type = "oauth".into();
    connection.api_key = None;
    connection.access_token = Some("fixture-kiro-token".into());
    connection
        .provider_specific_data
        .insert("authMethod".into(), json!("oauth"));
    if let Some(value) = repair_setting {
        connection
            .provider_specific_data
            .insert("kiroToolCallRepair".into(), json!(value));
    }
    connection
}

async fn kiro_app(endpoint: String, repair_setting: Option<bool>) -> (axum::Router, TempTestDb) {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![common::test_api_key()];
            db.provider_nodes = vec![kiro_node(endpoint)];
            db.provider_connections = vec![kiro_connection("kiro-c03", repair_setting)];
            db.settings.require_login = false;
        })
        .await
        .expect("seed Kiro C03 fixture");
    let state = openproxy::server::state::AppState::new(test_db.db.clone());
    (openproxy::build_app(state), test_db)
}

async fn post_kiro_chat(app: axum::Router) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer test-key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "kr/claude-sonnet-4.5",
                    "messages": [{"role": "user", "content": "keep this prompt unchanged"}],
                    "stream": true
                })
                .to_string(),
            ))
            .expect("Kiro request"),
    )
    .await
    .expect("Kiro app response")
}

#[tokio::test]
async fn valid_first_kiro_event_reaches_client_before_held_eof() {
    let release_eof = Arc::new(Notify::new());
    let upstream = MockUpstream::start([ScriptedResponse::sse([eventstream_frame(
        "assistantResponseEvent",
        &json!({"content": "first Kiro event"}),
    )])
    .with_header("content-type", "application/vnd.amazon.eventstream")
    .holding_eof(release_eof.clone())])
    .await;
    let (app, _test_db) = kiro_app(upstream.url("/generateAssistantResponse"), Some(true)).await;

    let response = post_kiro_chat(app).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(1), body.next())
        .await
        .expect("first translated event must not wait for EOF")
        .expect("first downstream chunk")
        .expect("valid downstream chunk");
    let first = String::from_utf8_lossy(&first);
    assert!(
        first.contains("first Kiro event"),
        "unexpected frame: {first}"
    );
    assert_eq!(upstream.request_count().await, 1);

    release_eof.notify_waiters();
    let mut tail = String::new();
    while let Some(chunk) = body.next().await {
        tail.push_str(&String::from_utf8_lossy(&chunk.expect("downstream tail")));
    }
    assert!(tail.contains("data: [DONE]"));
    assert!(!tail.contains("retry_failed"));
    upstream.shutdown().await;
}

#[tokio::test]
async fn semantic_response_shapes_and_legacy_setting_never_trigger_a_second_generation() {
    let contents = [
        "short valid answer",
        "...",
        "I'll verify the deployment now",
    ];
    let settings = [None, Some(false), Some(true)];
    let scripts = contents.iter().flat_map(|content| {
        settings.iter().map(move |_| {
            ScriptedResponse::sse([eventstream_frame(
                "assistantResponseEvent",
                &json!({"content": content}),
            )])
            .with_header("content-type", "application/vnd.amazon.eventstream")
        })
    });
    let upstream = MockUpstream::start(scripts).await;
    let executor = KiroExecutor::new(
        Arc::new(ClientPool::new()),
        Some(kiro_node(upstream.url("/generateAssistantResponse"))),
    )
    .expect("Kiro executor");

    let mut expected_requests = 0;
    for content in contents {
        for setting in settings {
            let body = json!({
                "conversationState": {
                    "currentMessage": {
                        "userInputMessage": {"content": "original prompt"}
                    }
                }
            });
            let result = executor
                .execute_request(KiroExecutionRequest {
                    model: "claude-sonnet-4.5".into(),
                    body: body.clone(),
                    stream: true,
                    credentials: kiro_connection("direct-c03", setting),
                    proxy: None,
                })
                .await
                .expect("single Kiro request");
            match result.response {
                UpstreamResponse::Reqwest(response) => {
                    let bytes = response.bytes().await.expect("read Kiro response");
                    assert!(bytes
                        .windows(content.len())
                        .any(|part| part == content.as_bytes()));
                }
                UpstreamResponse::Hyper(_) => panic!("Kiro must use reqwest transport"),
            }
            expected_requests += 1;
            assert_eq!(upstream.request_count().await, expected_requests);
        }
    }

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), contents.len() * settings.len());
    for request in requests {
        let body: Value = serde_json::from_slice(&request.body).expect("recorded request JSON");
        assert_eq!(
            body["conversationState"]["currentMessage"]["userInputMessage"]["content"],
            "original prompt"
        );
        assert!(!request
            .body
            .windows(18)
            .any(|part| part == b"Retry the previous"));
    }
    upstream.shutdown().await;
}

#[tokio::test]
async fn malformed_eventstream_frame_surfaces_error_without_retry() {
    let mut malformed = eventstream_frame(
        "assistantResponseEvent",
        &json!({"content": "must not decode"}),
    )
    .to_vec();
    malformed[8] ^= 0xff;
    let upstream = MockUpstream::start([ScriptedResponse::sse([Bytes::from(malformed)])
        .with_header("content-type", "application/vnd.amazon.eventstream")])
    .await;
    let (app, _test_db) = kiro_app(upstream.url("/generateAssistantResponse"), Some(true)).await;

    let response = post_kiro_chat(app).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("collect error response");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("kiro_eventstream_decode_error"), "{text}");
    assert!(text.contains("data: [DONE]"), "{text}");
    assert!(!text.contains("must not decode"), "{text}");
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}

#[tokio::test]
async fn dropping_client_body_cancels_the_held_upstream_stream() {
    let hold_eof = Arc::new(Notify::new());
    let upstream_body_dropped = Arc::new(Notify::new());
    let upstream = MockUpstream::start([ScriptedResponse::sse([eventstream_frame(
        "assistantResponseEvent",
        &json!({"content": "cancel after this"}),
    )])
    .with_header("content-type", "application/vnd.amazon.eventstream")
    .holding_eof(hold_eof)
    .notifying_on_body_drop(upstream_body_dropped.clone())])
    .await;
    let (app, _test_db) = kiro_app(upstream.url("/generateAssistantResponse"), None).await;

    let response = post_kiro_chat(app).await;
    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(1), body.next())
        .await
        .expect("first event before cancellation")
        .expect("first event")
        .expect("valid first event");
    assert!(String::from_utf8_lossy(&first).contains("cancel after this"));
    drop(body);

    tokio::time::timeout(Duration::from_secs(1), upstream_body_dropped.notified())
        .await
        .expect("dropping the downstream body must abort the upstream body");
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}

#[tokio::test]
async fn deprecated_kiro_repair_setting_round_trips_as_inert_data() {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.provider_connections = vec![
                kiro_connection("repair-true", Some(true)),
                kiro_connection("repair-false", Some(false)),
                kiro_connection("repair-absent", None),
            ];
        })
        .await
        .expect("persist deprecated settings");

    let snapshot = test_db.db.snapshot();
    let by_id = |id: &str| {
        snapshot
            .provider_connections
            .iter()
            .find(|connection| connection.id == id)
            .expect("persisted Kiro connection")
    };
    assert_eq!(
        by_id("repair-true")
            .provider_specific_data
            .get("kiroToolCallRepair"),
        Some(&json!(true))
    );
    assert_eq!(
        by_id("repair-false")
            .provider_specific_data
            .get("kiroToolCallRepair"),
        Some(&json!(false))
    );
    assert!(!by_id("repair-absent")
        .provider_specific_data
        .contains_key("kiroToolCallRepair"));
}
