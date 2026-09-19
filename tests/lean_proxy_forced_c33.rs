mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::core::chat::stream_to_json::{accumulate_sse_bytes, sse_stream_to_json};
use openproxy::core::stream_framing::SseFramer;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::util::ServiceExt;

async fn app_for(upstream: &MockUpstream) -> (axum::Router, TempTestDb) {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![ProviderNode {
                id: "openai".into(),
                r#type: "openai-compatible".into(),
                name: "C33 OpenAI".into(),
                prefix: Some("openai".into()),
                api_type: Some("chat".into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            db.provider_connections = vec![ProviderConnection {
                id: "c33-openai-account".into(),
                provider: "openai".into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                priority: Some(1),
                api_key: Some("c33-placeholder-key".into()),
                default_model: Some("gpt-c33".into()),
                ..Default::default()
            }];
        })
        .await
        .expect("seed C33 app");
    (
        openproxy::build_app(AppState::new(test_db.db.clone())),
        test_db,
    )
}

fn request() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": "openai/gpt-c33",
                "messages": [{"role": "user", "content": "forced"}],
                "stream": false
            })
            .to_string(),
        ))
        .unwrap()
}

fn chat_fixture() -> String {
    let chunks = [
        json!({"id":"chatcmpl-c33","object":"chat.completion.chunk","created":1712345678,"model":"gpt-c33","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}),
        json!({"id":"chatcmpl-c33","object":"chat.completion.chunk","created":1712345678,"model":"gpt-c33","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}),
        json!({"id":"chatcmpl-c33","object":"chat.completion.chunk","created":1712345678,"model":"gpt-c33","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
    ];
    chunks
        .iter()
        .map(|c| format!("data: {}\n\n", serde_json::to_string(c).unwrap()))
        .collect::<Vec<_>>()
        .join("")
        + "data: [DONE]\n\n"
}

fn responses_fixture() -> String {
    concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_c33\",\"created_at\":1712345678}}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}],\"role\":\"assistant\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_c33\",\"status\":\"completed\",\"usage\":{\"input_tokens\":15,\"output_tokens\":25,\"total_tokens\":40}}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string()
}

fn feed_every_split(
    input: &[u8],
    mut on_event: impl FnMut(&openproxy::core::stream_framing::SseEvent<'_>),
) {
    // Feed byte-by-byte to prove split invariance.
    let mut framer = SseFramer::new();
    for byte in input {
        framer
            .feed(std::slice::from_ref(byte), |event| on_event(&event))
            .expect("byte feed must not fail on valid fixture");
    }
    framer.finish(|event| on_event(&event)).unwrap();
}

#[test]
fn incremental_accumulator_matches_whole_body_goldens_at_every_split() {
    for fixture in [chat_fixture(), responses_fixture()] {
        let expected = sse_stream_to_json(fixture.as_bytes(), Some("gpt-c33"))
            .expect("whole-body golden must succeed")
            .expect("golden must yield JSON");
        // Whole incremental helper.
        let incremental = accumulate_sse_bytes(fixture.as_bytes(), Some("gpt-c33"))
            .expect("incremental must succeed")
            .expect("incremental must yield JSON");
        // Compare semantic JSON ignoring generated id/created.
        let mut expected_norm = expected.clone();
        let mut incremental_norm = incremental.clone();
        for value in [&mut expected_norm, &mut incremental_norm] {
            value.as_object_mut().unwrap().remove("id");
            value.as_object_mut().unwrap().remove("created");
        }
        assert_eq!(
            incremental_norm, expected_norm,
            "incremental accumulator diverged from whole-body golden"
        );
        // Byte-by-byte framing still yields convertible events.
        let mut count = 0usize;
        feed_every_split(fixture.as_bytes(), |_| count += 1);
        assert!(count >= 3, "expected at least 3 framed events, got {count}");
    }
}

#[test]
fn incremental_accumulator_matches_whole_body_errors() {
    // Sparse index must fail identically in both paths.
    let bad = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices": [{"index": 1_000_000_000, "delta": {"content": "x"}}]})
    );
    let whole = sse_stream_to_json(bad.as_bytes(), None).unwrap_err();
    let incremental = accumulate_sse_bytes(bad.as_bytes(), None).unwrap_err();
    assert_eq!(whole.code, incremental.code);
    assert_eq!(whole.code, "upstream_stream_index_limit");
}

#[tokio::test]
async fn forced_long_response_matches_semantic_golden_without_raw_history() {
    let fixture = chat_fixture();
    // Split the valid SSE across many small TCP chunks.
    let mid = fixture.len() / 3;
    let (a, rest) = fixture.split_at(mid);
    let (b, c) = rest.split_at(rest.len() / 2);
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        a.to_string(),
        b.to_string(),
        c.to_string(),
    ])])
    .await;
    let (app, _db) = app_for(&upstream).await;
    let response = app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["object"], "chat.completion");
    assert_eq!(value["choices"][0]["message"]["content"], "Hello world");
    assert_eq!(value["choices"][0]["finish_reason"], "stop");
    assert_eq!(value["usage"]["prompt_tokens"], 10);
    upstream.shutdown().await;
}

#[tokio::test]
async fn forced_responses_stream_matches_semantic_golden() {
    let fixture = responses_fixture();
    let upstream = MockUpstream::start([ScriptedResponse::sse([fixture])]).await;
    let (app, _db) = app_for(&upstream).await;
    let response = app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "Hello world");
    assert_eq!(value["usage"]["total_tokens"], 40);
    upstream.shutdown().await;
}

#[tokio::test]
async fn forced_bare_json_fallback_returns_single_json_representation() {
    let bare = json!({
        "id": "chatcmpl-c33-bare",
        "object": "chat.completion",
        "created": 1712345678,
        "model": "gpt-c33",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "bare"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    });
    let upstream = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        serde_json::to_vec(&bare).unwrap(),
    )])
    .await;
    let (app, _db) = app_for(&upstream).await;
    let response = app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value, bare);
    upstream.shutdown().await;
}

#[tokio::test]
async fn forced_truncated_stream_is_explicit_502_without_partial_json() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        "data: {\"id\":\"chatcmpl-c33\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
    ])
    .failing_after_chunks()])
    .await;
    let (app, _db) = app_for(&upstream).await;
    let response = app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        value.get("choices").is_none(),
        "partial JSON must not succeed"
    );
    assert!(
        value["error"]["code"]
            .as_str()
            .unwrap()
            .contains("upstream")
            || value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("complete upstream")
    );
    upstream.shutdown().await;
}

#[tokio::test]
async fn forced_oversized_frame_is_explicit_502() {
    let big = "x".repeat(1_050_000);
    let payload = format!(
        "data: {}\n\n",
        serde_json::to_string(&json!({
            "id": "chatcmpl-c33-big",
            "choices": [{"index": 0, "delta": {"content": big}}]
        }))
        .unwrap()
    );
    let upstream = MockUpstream::start([ScriptedResponse::sse([payload])]).await;
    let (app, _db) = app_for(&upstream).await;
    let response = app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], "upstream_sse_frame_too_large");
    upstream.shutdown().await;
}

#[tokio::test]
async fn cancelling_forced_incremental_collection_drops_held_upstream_body() {
    let release = Arc::new(Notify::new());
    let body_dropped = Arc::new(Notify::new());
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        "data: {\"id\":\"chatcmpl-c33\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"held\"}}]}\n\n",
    ])
    .holding_eof(release.clone())
    .notifying_on_body_drop(body_dropped.clone())])
    .await;
    let (app, _db) = app_for(&upstream).await;
    let task = tokio::spawn(app.oneshot(request()));
    upstream.wait_for_requests(1).await;
    task.abort();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), body_dropped.notified())
            .await
            .is_ok(),
        "cancelled incremental SSE-to-JSON retained the upstream body"
    );
    release.notify_waiters();
    upstream.shutdown().await;
}

#[test]
fn source_guards_prove_no_retained_raw_sse_history_on_forced_path() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let chat = std::fs::read_to_string(root.join("src/server/api/chat.rs")).unwrap();
    let forced_start = chat
        .find("struct ForcedSseCollection")
        .expect("shared forced collector must exist");
    let forced_end = chat[forced_start..]
        .find("\nasync fn proxy_response(")
        .map(|offset| forced_start + offset)
        .unwrap_or(chat.len());
    let forced = &chat[forced_start..forced_end];
    // No whole-body collector or full-body converter on the forced path.
    assert!(
        !forced.contains("read_upstream_body(response"),
        "forced SSE-to-JSON must not retain the whole upstream body"
    );
    assert!(
        !forced.contains("sse_stream_to_json(&body_bytes"),
        "forced path must feed C32 frames incrementally, not convert retained bytes"
    );
    assert!(
        !forced.contains("let body_bytes"),
        "forced path must not bind a full raw SSE buffer"
    );
    assert!(
        !forced.contains("serde_json::from_slice(&body_bytes)"),
        "forced fallback must use the bare-JSON prefix only"
    );
    // Incremental ownership is explicit.
    for required in [
        "ForcedSseAccumulator::new()",
        "SseFramer::new()",
        "accumulator.ingest(&event)",
        "accumulator.is_terminal()",
        "accumulator.finish(Some(model))",
        "wire_seen",
        "prefix_raw",
        "success_body_limit()",
    ] {
        assert!(
            forced.contains(required),
            "forced incremental path must contain {required}"
        );
    }
    // Other collected paths (dashboard/non-stream) intentionally still use the
    // shared C29 bounded collector.
    assert!(chat.contains("read_upstream_body(response, success_body_limit()).await"));

    let converter = std::fs::read_to_string(root.join("src/core/chat/stream_to_json.rs")).unwrap();
    for required in [
        "pub struct ForcedSseAccumulator",
        "pub fn ingest(&mut self, event: &SseEvent",
        "pub fn finish(self",
        "pub fn saw_any_event",
        "fn ingest_data_str",
        "fn ingest_event",
        "fn assemble_responses_summary",
    ] {
        assert!(
            converter.contains(required),
            "incremental accumulator must contain {required}"
        );
    }
}
