mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use common::lean_harness::{MockUpstream, ScriptedResponse};
use openproxy::core::executor::{ClientPool, DefaultExecutor, ExecutionRequest};
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;
use tokio::sync::Barrier;

fn anthropic_node(base_url: String) -> ProviderNode {
    ProviderNode {
        id: "claude".into(),
        r#type: "anthropic-compatible".into(),
        name: "Claude test upstream".into(),
        prefix: Some("claude".into()),
        api_type: Some("messages".into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

fn connection(id: &str, key: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "claude".into(),
        auth_type: "apikey".into(),
        api_key: Some(key.into()),
        is_active: Some(true),
        ..Default::default()
    }
}

fn headers(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(name, value)| ((*name).into(), (*value).into()))
        .collect()
}

fn request(
    credentials: ProviderConnection,
    client_headers: BTreeMap<String, String>,
) -> ExecutionRequest {
    ExecutionRequest {
        model: "claude-sonnet-4.5".into(),
        body: json!({
            "model": "claude-sonnet-4.5",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 32,
            "stream": false
        }),
        stream: false,
        credentials,
        proxy: None,
        client_headers,
    }
}

fn ok_response() -> ScriptedResponse {
    ScriptedResponse::json(
        StatusCode::OK,
        r#"{"id":"msg_test","type":"message","role":"assistant","content":[]}"#,
    )
}

#[tokio::test]
async fn alternating_claude_and_opencode_clients_never_inherit_headers() {
    let upstream = MockUpstream::start([ok_response(), ok_response(), ok_response()]).await;
    let executor = DefaultExecutor::new(
        "claude",
        Arc::new(ClientPool::new()),
        Some(anthropic_node(upstream.url("/v1"))),
    )
    .expect("Claude executor");

    let claude_cli = headers(&[
        ("user-agent", "claude-cli/2.1.282"),
        ("x-app", "cli"),
        ("x-claude-code-session-id", "session-a"),
        ("x-stainless-retry-count", "7"),
        ("anthropic-version", "2024-01-01"),
        ("anthropic-beta", "current-request-beta"),
        ("authorization", "Bearer must-not-forward"),
        ("cookie", "must-not-forward"),
        ("x-forwarded-for", "203.0.113.7"),
    ]);
    executor
        .execute(request(connection("account-a", "key-a"), claude_cli))
        .await
        .expect("Claude CLI request");

    let opencode = headers(&[("user-agent", "opencode/1.18.31")]);
    executor
        .execute(request(connection("account-b", "key-b"), opencode))
        .await
        .expect("OpenCode request");

    executor
        .execute(request(connection("account-c", "key-c"), BTreeMap::new()))
        .await
        .expect("headerless request");

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), 3);

    let first = &requests[0].headers;
    assert_eq!(first["x-api-key"], "key-a");
    assert_eq!(first["x-claude-code-session-id"], "session-a");
    assert_eq!(first["x-stainless-retry-count"], "7");
    assert_eq!(first["anthropic-version"], "2023-06-01");
    assert_eq!(first["anthropic-beta"], "current-request-beta");
    assert_eq!(first["authorization"], "Bearer key-a");
    assert!(first.get("cookie").is_none());
    assert!(first.get("x-forwarded-for").is_none());

    let second = &requests[1].headers;
    assert_eq!(second["x-api-key"], "key-b");
    assert_eq!(second["user-agent"], "opencode/1.18.31");
    assert_eq!(second["anthropic-version"], "2023-06-01");
    assert!(second.get("x-claude-code-session-id").is_none());
    assert!(second.get("x-stainless-retry-count").is_none());
    assert!(second.get("anthropic-beta").is_none());

    let third = &requests[2].headers;
    assert_eq!(third["x-api-key"], "key-c");
    assert_eq!(third["anthropic-version"], "2023-06-01");
    assert!(third.get("x-claude-code-session-id").is_none());
    assert_ne!(
        third
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some("claude-cli/2.1.282")
    );
    assert_ne!(
        third
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some("opencode/1.18.31")
    );

    upstream.shutdown().await;
}

#[tokio::test]
async fn concurrent_accounts_keep_claude_session_headers_request_scoped() {
    const REQUESTS: usize = 24;

    let upstream = MockUpstream::start((0..REQUESTS).map(|_| ok_response())).await;
    let executor = Arc::new(
        DefaultExecutor::new(
            "claude",
            Arc::new(ClientPool::new()),
            Some(anthropic_node(upstream.url("/v1"))),
        )
        .expect("Claude executor"),
    );
    let barrier = Arc::new(Barrier::new(REQUESTS));
    let mut tasks = Vec::with_capacity(REQUESTS);

    for index in 0..REQUESTS {
        let executor = executor.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            let key = format!("key-{index}");
            let session = format!("session-{index}");
            barrier.wait().await;
            executor
                .execute(request(
                    connection(&format!("account-{index}"), &key),
                    headers(&[
                        ("user-agent", "claude-code/2.1.92"),
                        ("x-claude-code-session-id", &session),
                    ]),
                ))
                .await
                .expect("concurrent Claude request");
        }));
    }

    for task in tasks {
        task.await.expect("join concurrent request");
    }

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), REQUESTS);
    for recorded in requests {
        let key = recorded.headers["x-api-key"]
            .to_str()
            .expect("ASCII account key");
        let index = key.strip_prefix("key-").expect("key prefix");
        assert_eq!(
            recorded.headers["x-claude-code-session-id"],
            format!("session-{index}")
        );
    }

    upstream.shutdown().await;
}
