mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::core::executor::{
    ClientPool, DefaultExecutor, TransportKind, MAX_PREPARED_UPSTREAM_BODY_BYTES,
};
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode, RuntimeTransport};
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn compatible_node(id: &str, base_url: String) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: id.into(),
        prefix: Some(id.into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

fn connection(
    id: &str,
    provider: &str,
    priority: u32,
    key: &str,
    endpoint: Option<String>,
) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        priority: Some(priority),
        api_key: Some(key.into()),
        default_model: Some("gpt-c27".into()),
        runtime_transport: endpoint.map(|base_url| RuntimeTransport {
            base_url: Some(base_url),
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn one_prepared_body_is_shared_across_account_headers_and_both_transports() {
    let upstream = MockUpstream::start([
        ScriptedResponse::json(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"try another account"}}"#,
        ),
        ScriptedResponse::json(StatusCode::OK, r#"{"id":"ok","choices":[]}"#),
    ])
    .await;
    let provider = "c27-direct";
    let node = compatible_node(provider, upstream.url("/v1"));
    let executor = DefaultExecutor::new(provider, Arc::new(ClientPool::new()), Some(node))
        .expect("C27 executor");
    let body = json!({
        "model": "gpt-c27",
        "messages": [{"role": "user", "content": "Привет 🌍"}],
        "tools": [{"type": "function", "function": {
            "name": "lookup",
            "parameters": {"type": "object", "properties": {"ключ": {"type": "string"}}}
        }}],
        "unknownExtension": {"nested": [1, true, "值"]},
        "stream": false
    });
    let prepared = executor
        .prepare_upstream_body(&body, "gpt-c27")
        .expect("bounded prepared body");
    assert!(prepared.serialized_bytes().len() < MAX_PREPARED_UPSTREAM_BODY_BYTES);
    let shared_ptr = prepared.serialized_bytes().as_ptr();

    let hyper_connection = connection(
        "account-hyper",
        provider,
        1,
        "key-hyper",
        Some(upstream.url("/v1/chat/completions")),
    );
    let first = executor
        .execute_prepared(
            "gpt-c27",
            false,
            &hyper_connection,
            None,
            &BTreeMap::new(),
            &prepared,
        )
        .await
        .expect("hyper attempt");
    assert_eq!(first.transport, TransportKind::Hyper);
    assert_eq!(first.response.status(), StatusCode::TOO_MANY_REQUESTS);

    let reqwest_connection = connection(
        "account-reqwest",
        provider,
        2,
        "key-reqwest",
        Some(upstream.url("/v1/responses")),
    );
    let second = executor
        .execute_prepared(
            "gpt-c27",
            false,
            &reqwest_connection,
            None,
            &BTreeMap::new(),
            &prepared,
        )
        .await
        .expect("reqwest attempt");
    assert_eq!(second.transport, TransportKind::Reqwest);
    assert_eq!(second.response.status(), StatusCode::OK);
    assert_eq!(prepared.serialized_bytes().as_ptr(), shared_ptr);

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(
        requests[0].body.as_ref(),
        prepared.serialized_bytes().as_ref()
    );
    for request in &requests {
        assert_eq!(
            request.headers.get("content-type").unwrap(),
            "application/json"
        );
        assert_eq!(
            request
                .headers
                .get("content-length")
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            prepared.serialized_bytes().len()
        );
        let captured: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(captured["messages"][0]["content"], "Привет 🌍");
        assert_eq!(captured["unknownExtension"]["nested"][2], "值");
    }
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer key-hyper"
    );
    assert_eq!(
        requests[1].headers.get("authorization").unwrap(),
        "Bearer key-reqwest"
    );

    let changed_body = json!({
        "model": "gpt-c27-next",
        "messages": [{"role": "user", "content": "changed"}],
        "stream": false
    });
    let changed = executor
        .prepare_upstream_body(&changed_body, "gpt-c27-next")
        .expect("changed prepared body");
    assert!(!executor.can_reuse_prepared_body(&prepared, "gpt-c27-next"));
    assert_ne!(prepared.serialized_bytes(), changed.serialized_bytes());

    upstream.shutdown().await;
}

#[tokio::test]
async fn request_scoped_fallback_uses_identical_prepared_bytes_once_per_account() {
    let upstream = MockUpstream::start([
        ScriptedResponse::json(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"c27 fallback"}}"#,
        )
        .with_header("retry-after", "1"),
        ScriptedResponse::json(
            StatusCode::OK,
            r#"{"id":"c27","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
        ),
    ])
    .await;
    let provider = "c27-fallback";
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![compatible_node(provider, upstream.url("/v1"))];
            db.provider_connections = vec![
                connection("account-a", provider, 1, "key-a", None),
                connection("account-b", provider, 2, "key-b", None),
            ];
        })
        .await
        .expect("seed C27 app");
    let app = openproxy::build_app(AppState::new(test_db.db.clone()));
    let request_body = json!({
        "model": format!("{provider}/gpt-c27"),
        "messages": [{"role": "user", "content": "fallback 🧪"}],
        "unknownExtension": {"keep": "完整"},
        "stream": false
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(request_body.to_string()))
                .unwrap(),
        )
        .await
        .expect("C27 response");
    assert_eq!(response.status(), StatusCode::OK);
    let response_body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&response_body).contains("ok"));

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), 2, "C13 permits one attempt per account");
    assert_eq!(requests[0].body, requests[1].body);
    let captured: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(captured["model"], "gpt-c27");
    assert_eq!(captured["messages"][0]["content"], "fallback 🧪");
    assert_eq!(captured["unknownExtension"]["keep"], "完整");
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer key-a"
    );
    assert_eq!(
        requests[1].headers.get("authorization").unwrap(),
        "Bearer key-b"
    );

    upstream.shutdown().await;
}

#[test]
fn source_has_one_bounded_prepare_and_no_per_send_serialization() {
    let executor = include_str!("../src/core/executor/default.rs");
    let chat = include_str!("../src/server/api/chat.rs");
    let send_one = executor
        .split("async fn send_one(")
        .nth(1)
        .and_then(|tail| tail.split("pub fn pool(").next())
        .expect("DefaultExecutor::send_one source");

    assert!(executor.contains("struct BoundedJsonWriter"));
    assert!(executor.contains("MAX_PREPARED_UPSTREAM_BODY_BYTES"));
    assert!(executor.contains("serde_json::to_writer(&mut writer, value)"));
    assert!(!send_one.contains("serde_json::to_vec"));
    assert!(!send_one.contains(".json("));
    assert!(send_one.contains(".body(body.clone())"));
    assert!(chat.contains("let mut default_prepared_body: Option<PreparedUpstreamBody> = None"));
    assert_eq!(
        chat.matches("prepare_upstream_body(&request_body, model)")
            .count(),
        1
    );
    assert!(chat.contains("can_reuse_prepared_body(prepared, model)"));
    assert!(
        executor.contains("pub transformed_body: Value"),
        "C28 owns response lifetime"
    );
    assert!(
        !executor.contains("static PREPARED"),
        "prepared bytes must be request-scoped"
    );
}
