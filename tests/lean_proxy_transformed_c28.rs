mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use futures_util::StreamExt;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::util::ServiceExt;

#[test]
fn executor_response_graphs_do_not_own_transformed_request_json() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let executor_dir = root.join("src/core/executor");
    for entry in std::fs::read_dir(executor_dir).expect("executor source directory") {
        let path = entry.expect("executor source entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("executor source");
        for forbidden in [
            "pub transformed_body",
            "transformed_body:",
            ".field(\"transformed_body\"",
            "Arc<Value>",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} retains forbidden response ownership pattern {forbidden}",
                path.display()
            );
        }
    }

    let default = include_str!("../src/core/executor/default.rs");
    let prepared = default
        .split("pub struct PreparedUpstreamBody {")
        .nth(1)
        .and_then(|tail| tail.split("impl PreparedUpstreamBody").next())
        .expect("PreparedUpstreamBody source");
    assert!(prepared.contains("bytes: Bytes"));
    assert!(!prepared.contains("Value"));

    let chat = include_str!("../src/server/api/chat.rs");
    assert!(!chat.contains("result.transformed_body"));
    assert!(!chat.contains("response.transformed_body"));

    let cli = include_str!("../src/cli/mod.rs");
    assert!(!cli.contains("response.transformed_body"));
    assert!(cli.contains("executor.transform_request(&request_body, &resolved.model)"));
}

#[tokio::test]
async fn large_request_keeps_tool_and_usage_streaming_before_held_eof_then_cancels() {
    const LARGE_PROMPT_BYTES: usize = 4 * 1024 * 1024;
    let tool_event = concat!(
        "data: {\"id\":\"chatcmpl-c28\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
        "\"tool_calls\":[{\"index\":0,\"id\":\"call_c28\",\"type\":\"function\",",
        "\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":\\\"rust\\\"}\"}}]}}]}\n\n"
    );
    let usage_event = concat!(
        "data: {\"id\":\"chatcmpl-c28\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],",
        "\"usage\":{\"prompt_tokens\":2097152,\"completion_tokens\":7,",
        "\"total_tokens\":2097159}}\n\n"
    );
    let hold_eof = Arc::new(Notify::new());
    let body_dropped = Arc::new(Notify::new());
    let upstream = MockUpstream::start([ScriptedResponse::sse([tool_event, usage_event])
        .holding_eof(hold_eof)
        .notifying_on_body_drop(body_dropped.clone())])
    .await;

    let provider = "c28-held";
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![ProviderNode {
                id: provider.into(),
                r#type: "openai-compatible".into(),
                name: provider.into(),
                prefix: Some(provider.into()),
                api_type: Some("chat".into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            db.provider_connections = vec![ProviderConnection {
                id: "c28-account".into(),
                provider: provider.into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                priority: Some(1),
                api_key: Some("c28-placeholder-key".into()),
                default_model: Some("gpt-c28".into()),
                ..Default::default()
            }];
        })
        .await
        .expect("seed C28 app");
    let app = openproxy::build_app(AppState::new(test_db.db.clone()));
    let request_body = json!({
        "model": format!("{provider}/gpt-c28"),
        "messages": [{"role": "user", "content": "x".repeat(LARGE_PROMPT_BYTES)}],
        "tools": [{"type": "function", "function": {
            "name": "lookup",
            "description": "preserve tool mapping",
            "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
        }}],
        "stream": true,
        "stream_options": {"include_usage": true}
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(request_body.to_string()))
                .expect("C28 request"),
        )
        .await
        .expect("C28 response");
    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body().into_data_stream();
    let observed = tokio::time::timeout(Duration::from_secs(2), async {
        let mut observed = Vec::new();
        while !String::from_utf8_lossy(&observed).contains("total_tokens") {
            let chunk = body
                .next()
                .await
                .expect("held response remains open")
                .expect("valid SSE chunk");
            observed.extend_from_slice(&chunk);
        }
        observed
    })
    .await
    .expect("tool and usage events arrive before held EOF");
    let observed = String::from_utf8(observed).expect("UTF-8 SSE");
    assert!(observed.contains("call_c28"));
    assert!(observed.contains("lookup"));
    assert!(observed.contains("prompt_tokens\":2097152"));
    assert!(observed.contains("total_tokens\":2097159"));

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), 1);
    let forwarded: Value = serde_json::from_slice(&requests[0].body).expect("forwarded JSON");
    assert_eq!(
        forwarded["messages"][0]["content"]
            .as_str()
            .expect("large prompt")
            .len(),
        LARGE_PROMPT_BYTES
    );
    assert_eq!(forwarded["tools"][0]["function"]["name"], "lookup");

    drop(body);
    tokio::time::timeout(Duration::from_secs(1), body_dropped.notified())
        .await
        .expect("downstream cancellation drops held upstream body");
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}
