mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::core::translator::limits::{
    StreamLimitError, MAX_STREAM_ACCUMULATED_BYTES, MAX_STREAM_CHOICES,
    MAX_STREAM_TOOL_ARGUMENT_BYTES, MAX_STREAM_TOOL_CALLS, MAX_STREAM_WIRE_INDEX,
};
use openproxy::core::translator::registry::{global_registry, Format, ResponseTransformState};
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
                name: "C31 OpenAI".into(),
                prefix: Some("openai".into()),
                api_type: Some("chat".into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            db.provider_connections = vec![ProviderConnection {
                id: "c31-openai-account".into(),
                provider: "openai".into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                priority: Some(1),
                api_key: Some("c31-placeholder-key".into()),
                default_model: Some("gpt-c31".into()),
                ..Default::default()
            }];
        })
        .await
        .expect("seed C31 app");
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
                "model": "openai/gpt-c31",
                "messages": [{"role": "user", "content": "bounded"}],
                "stream": false
            })
            .to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn forced_sse_limit_failure_is_explicit_json_before_commit() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        "data: {\"id\":\"chatcmpl-c31\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1000000000,\"id\":\"call_bad\",\"function\":{\"name\":\"bad\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    ])])
    .await;
    let (app, _db) = app_for(&upstream).await;

    let response = app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(response.headers()["content-type"], "application/json");
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], "upstream_stream_index_limit");
    assert_ne!(value["error"]["code"], "sse_to_json_failed");
    upstream.shutdown().await;
}

#[tokio::test]
async fn cancelling_forced_sse_collection_drops_held_upstream_body() {
    let release = Arc::new(Notify::new());
    let body_dropped = Arc::new(Notify::new());
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        "data: {\"id\":\"chatcmpl-c31\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"held\"}}]}\n\n",
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
        "cancelled SSE-to-JSON request retained the upstream body"
    );
    release.notify_waiters();
    upstream.shutdown().await;
}

#[test]
fn source_guards_keep_c31_separate_from_c32_c33_and_sparse_vectors() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let converter = std::fs::read_to_string(root.join("src/core/chat/stream_to_json.rs")).unwrap();
    for forbidden in [
        "unwrap_or(0) as usize",
        "output.resize(",
        "while entry.tool_calls.len()",
        "tc_idx as usize",
    ] {
        assert!(
            !converter.contains(forbidden),
            "SSE-to-JSON retained forbidden sparse/index pattern {forbidden}"
        );
    }
    assert!(converter.contains("BTreeMap<u64, ToolCallAccum>"));
    assert!(converter.contains("checked_append("));

    let caller = std::fs::read_to_string(root.join("src/server/api/chat.rs")).unwrap();
    assert!(caller.contains("Err(error) =>"));
    assert!(caller.contains("upstream_error"));
    assert!(
        !caller.contains("sse_stream_to_json(&body_bytes, Some(model))\n        .unwrap_or_else")
    );
    assert!(caller.contains("read_upstream_body(response, success_body_limit()).await"));
    // C33: forced SSE-to-JSON no longer converts retained bytes; it feeds C32
    // frames incrementally into a C31-bounded accumulator.
    assert!(!caller.contains("sse_stream_to_json(&body_bytes"));
    assert!(caller.contains("ForcedSseAccumulator"));

    let compat = std::fs::read_to_string(root.join("src/server/api/compat.rs")).unwrap();
    assert!(!compat.contains("ensure_toolcall_idx"));
    assert!(!compat.contains("toolcall_active: Vec"));
    assert!(!compat.contains("unwrap_or(0) as usize"));
    assert!(!compat.contains("HashMap<usize"));
    assert!(compat.contains("pending_tool_args: BTreeMap<u64, String>"));

    let ollama = std::fs::read_to_string(root.join("src/server/api/v1_api_chat.rs")).unwrap();
    assert!(!ollama.contains("unwrap_or(self.pending_tool_calls.len() as u64) as usize"));
    assert!(ollama.contains("BTreeMap<u64, PendingToolCall>"));

    for relative in [
        "src/core/translator/response/claude_to_openai.rs",
        "src/core/translator/response/commandcode_to_openai.rs",
        "src/core/translator/response/gemini_to_openai.rs",
        "src/core/translator/response/openai_responses.rs",
        "src/core/translator/response/openai_to_antigravity.rs",
        "src/core/translator/response/openai_to_claude.rs",
        "src/core/translator/response/openai_to_gemini.rs",
        "src/core/translator/response/ollama_to_openai.rs",
    ] {
        let source = std::fs::read_to_string(root.join(relative)).unwrap();
        assert!(!source.contains("unwrap_or(0) as usize"), "{relative}");
        assert!(!source.contains(" += 1;"), "{relative}");
    }

    let limits = std::fs::read_to_string(root.join("src/core/translator/limits.rs")).unwrap();
    assert!(limits.contains("checked_add(fragment.len())"));
    assert!(limits.contains("try_reserve(fragment.len())"));

    let cursor = std::fs::read_to_string(root.join("src/core/executor/cursor.rs")).unwrap();
    assert!(cursor.contains("Vec<CursorToolCallAccum>"));
    assert!(!cursor.contains("tool_call_map: HashMap"));
    assert!(cursor.contains("checked_append("));

    assert!(!converter.contains("let mut final_retained_bytes = 0usize"));
    let registry = std::fs::read_to_string(root.join("src/core/translator/registry.rs")).unwrap();
    assert!(registry.contains("if state.failure.is_some()"));
}

fn openai_tool_chunk(index: Value, id: &str, name: &str, arguments: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "chatcmpl-c31-translator",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [{
                "index": index,
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": arguments}
            }]},
            "finish_reason": null
        }]
    }))
    .unwrap()
}

#[test]
fn active_translators_bound_indices_tool_count_arguments_and_text_state() {
    let registry = global_registry();

    let exact = "x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES);
    let mut state = ResponseTransformState::default();
    let output = registry
        .translate_response(
            Format::OpenAi,
            Format::Claude,
            &openai_tool_chunk(json!(7), "call_exact", "write", &exact),
            &mut state,
        )
        .expect("exact translator argument limit");
    assert!(output
        .iter()
        .any(|frame| frame.contains("content_block_start")));

    let mut state = ResponseTransformState::default();
    let oversized = "x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES + 1);
    let error = registry
        .translate_response(
            Format::OpenAi,
            Format::Claude,
            &openai_tool_chunk(json!(7), "call_large", "write", &oversized),
            &mut state,
        )
        .unwrap_err();
    assert_eq!(error.code, "upstream_stream_state_limit");

    for index in [json!(u64::MAX), json!(1_000_000_000u64), json!(-1)] {
        let mut state = ResponseTransformState::default();
        let error = registry
            .translate_response(
                Format::OpenAi,
                Format::Gemini,
                &openai_tool_chunk(index, "call_bad", "bad", "{}"),
                &mut state,
            )
            .unwrap_err();
        assert!(matches!(
            error.code,
            "upstream_stream_index_limit" | "upstream_stream_invalid_index"
        ));
    }

    let calls = (0..=MAX_STREAM_TOOL_CALLS)
        .map(|index| {
            json!({
                "index": index,
                "id": format!("call_{index}"),
                "function": {"name": "f", "arguments": "{}"}
            })
        })
        .collect::<Vec<_>>();
    let many = serde_json::to_vec(&json!({
        "choices": [{"index": 0, "delta": {"tool_calls": calls}}]
    }))
    .unwrap();
    let mut state = ResponseTransformState::default();
    assert_eq!(
        registry
            .translate_response(Format::OpenAi, Format::Antigravity, &many, &mut state)
            .unwrap_err()
            .code,
        "upstream_stream_state_limit"
    );

    let half = "雪".repeat(MAX_STREAM_ACCUMULATED_BYTES / 6);
    let text_chunk = |content: &str| {
        serde_json::to_vec(&json!({
            "choices": [{"index": 0, "delta": {"content": content}}]
        }))
        .unwrap()
    };
    let mut state = ResponseTransformState::default();
    for _ in 0..2 {
        registry
            .translate_response(
                Format::OpenAi,
                Format::OpenAiResponses,
                &text_chunk(&half),
                &mut state,
            )
            .expect("in-budget Unicode text state");
    }
    let error = registry
        .translate_response(
            Format::OpenAi,
            Format::OpenAiResponses,
            &text_chunk("雪雪"),
            &mut state,
        )
        .unwrap_err();
    assert_eq!(error.code, "upstream_stream_state_limit");
}

#[test]
fn registry_choice_and_wire_index_boundaries_and_failed_finish_are_explicit() {
    let registry = global_registry();
    let chunk = |index: u64| {
        serde_json::to_vec(&json!({
            "choices": [{"index": index, "delta": {"content": "ok"}}]
        }))
        .unwrap()
    };
    let mut state = ResponseTransformState::default();
    registry
        .translate_response(
            Format::OpenAi,
            Format::Gemini,
            &chunk(MAX_STREAM_WIRE_INDEX),
            &mut state,
        )
        .unwrap();
    let mut state = ResponseTransformState::default();
    assert_eq!(
        registry
            .translate_response(
                Format::OpenAi,
                Format::Gemini,
                &chunk(MAX_STREAM_WIRE_INDEX + 1),
                &mut state,
            )
            .unwrap_err()
            .code,
        "upstream_stream_index_limit"
    );

    let choices = (0..MAX_STREAM_CHOICES)
        .map(|index| json!({"index": index, "delta": {}}))
        .collect::<Vec<_>>();
    let exact = serde_json::to_vec(&json!({"choices": choices})).unwrap();
    let mut state = ResponseTransformState::default();
    registry
        .translate_response(Format::OpenAi, Format::Gemini, &exact, &mut state)
        .unwrap();

    let choices = (0..=MAX_STREAM_CHOICES)
        .map(|index| json!({"index": index, "delta": {}}))
        .collect::<Vec<_>>();
    let oversized = serde_json::to_vec(&json!({"choices": choices})).unwrap();
    let mut state = ResponseTransformState::default();
    assert_eq!(
        registry
            .translate_response(Format::OpenAi, Format::Gemini, &oversized, &mut state,)
            .unwrap_err()
            .code,
        "upstream_stream_state_limit"
    );

    let mut state = ResponseTransformState {
        failure: Some(StreamLimitError::bytes("test", 1)),
        ..Default::default()
    };
    assert!(registry
        .finish_stream(Format::OpenAi, Format::OpenAi, &mut state)
        .is_empty());
}
