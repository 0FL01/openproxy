mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn compatible_node(base_url: String) -> ProviderNode {
    ProviderNode {
        id: "c08-node".into(),
        r#type: "openai-compatible".into(),
        name: "C08 loopback".into(),
        prefix: Some("c08".into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

fn compatible_connection() -> ProviderConnection {
    ProviderConnection {
        id: "glm-c08".into(),
        provider: "c08-node".into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        priority: Some(1),
        api_key: Some("upstream-c08-key".into()),
        default_model: Some("glm-5.1".into()),
        ..Default::default()
    }
}

async fn post_chat(app: &axum::Router, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("C08 request"),
        )
        .await
        .expect("C08 response")
}

#[tokio::test]
async fn ordinary_chat_forwards_large_structured_input_without_local_context_rejection() {
    let upstream_error = br#"{"error":{"message":"maximum context length exceeded","code":"context_length_exceeded"},"provider":"glm"}"#;
    let upstream = MockUpstream::start([
        ScriptedResponse::json(
            StatusCode::OK,
            r#"{"id":"chatcmpl-c08","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
        ),
        ScriptedResponse::json(StatusCode::PAYLOAD_TOO_LARGE, upstream_error.as_slice()),
    ])
    .await;
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![compatible_node(upstream.url("/v1"))];
            db.provider_connections = vec![compatible_connection()];
        })
        .await
        .expect("seed C08 database");
    let app = openproxy::build_app(AppState::new(test_db.db.clone()));

    // This prompt-bearing JSON exceeds the deleted one-byte-per-four-token
    // heuristic while remaining comfortably below the independent 32 MiB body
    // limit. Both inline-binary shapes are included to guard against restoring
    // the old recursive clone-and-strip traversal.
    let unicode = "界".repeat(500_000);
    let tool_description = "schema".repeat(120_000);
    let data_url = format!("data:image/png;base64,{}", "A".repeat(150_000));
    let base64_data = "B".repeat(150_000);
    let large_request = json!({
        "model": "c08/glm-5.1",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": unicode},
                {"type": "image_url", "image_url": {"url": data_url}}
            ]
        }],
        "tools": [{
            "type": "function",
            "function": {
                "name": "inspect_payload",
                "description": tool_description,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "blob": {
                            "type": "base64",
                            "data": base64_data
                        }
                    }
                }
            }
        }],
        "stream": false
    });
    assert!(large_request.to_string().len() > 2_000_000);

    let response = post_chat(&app, large_request).await;
    let status = response.status();
    let response_body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read success response");
    let observed_after_large = upstream.requests().await;
    assert_eq!(
        status,
        StatusCode::OK,
        "large request failed after {} upstream requests (paths {:?}): {}",
        observed_after_large.len(),
        observed_after_large
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        String::from_utf8_lossy(&response_body),
    );
    assert!(String::from_utf8_lossy(&response_body).contains("chatcmpl-c08"));

    let context_error_request = json!({
        "model": "c08/glm-5.1",
        "messages": [{"role": "user", "content": "provider decides the real limit"}],
        "stream": false
    });
    let response = post_chat(&app, context_error_request).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let response_body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read upstream context error");
    assert_eq!(response_body.as_ref(), upstream_error);

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), 2, "one upstream attempt per client request");
    let forwarded: Value = serde_json::from_slice(&requests[0].body).expect("forwarded JSON");
    assert_eq!(
        forwarded["messages"][0]["content"][0]["text"]
            .as_str()
            .map(str::len),
        Some(1_500_000),
        "large Unicode content must not be truncated"
    );
    assert_eq!(
        forwarded["tools"][0]["function"]["description"]
            .as_str()
            .map(str::len),
        Some(720_000),
        "tool schemas must not be truncated"
    );
    assert!(forwarded["messages"][0]["content"][1]["image_url"]["url"]
        .as_str()
        .is_some_and(|value| value.len() > 150_000));
    assert_eq!(
        forwarded["tools"][0]["function"]["parameters"]["properties"]["blob"]["data"]
            .as_str()
            .map(str::len),
        Some(150_000)
    );

    upstream.shutdown().await;
}

#[test]
fn ordinary_chat_has_no_local_token_estimator_or_synthetic_context_error() {
    let chat = include_str!("../src/server/api/chat.rs");
    let context = include_str!("../src/core/context_limit.rs");

    for removed in [
        "estimate_input_tokens",
        "context_limit_error",
        "context_limit_attempt_error",
        "legacy_proxy_rejection_limit",
        "legacy_proxy_input_limit",
    ] {
        assert!(
            !chat.contains(removed),
            "chat path still references {removed}"
        );
        assert!(
            !context.contains(removed),
            "deleted context policy still defines {removed}"
        );
    }
    assert!(
        include_str!("../src/server/api/compat.rs").contains("pub async fn count_tokens"),
        "explicit count_tokens contract must remain independent"
    );
}
