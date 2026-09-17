mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::lean_harness::{MockUpstream, ScriptedResponse};
use openproxy::core::executor::{ClientPool, DefaultExecutor, ExecutionRequest};
use openproxy::types::{ProviderConnection, ProviderNode, RuntimeTransport};
use serde_json::json;

fn compatible_node(id: &str, base_url: String) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: "C10 loopback".into(),
        prefix: Some(id.into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

fn connection(provider: &str, base_url: String) -> ProviderConnection {
    ProviderConnection {
        id: format!("{provider}-c10"),
        provider: provider.into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        api_key: Some("upstream-c10-key".into()),
        runtime_transport: Some(RuntimeTransport {
            base_url: Some(base_url),
        }),
        ..Default::default()
    }
}

async fn assert_single_immediate_attempt(provider: &str, model: &str, status: StatusCode) {
    let body = format!(
        r#"{{"error":{{"message":"c10-{status}"}},"status":{}}}"#,
        status.as_u16()
    );
    let upstream = MockUpstream::start([
        ScriptedResponse::json(status, body.clone())
            .with_header("retry-after", "17")
            .with_header("x-c10", "preserved"),
        ScriptedResponse::json(
            StatusCode::OK,
            r#"{"id":"must-not-be-requested","choices":[]}"#,
        ),
    ])
    .await;
    let base_url = upstream.url("/v1");
    let node = compatible_node(provider, base_url.clone());
    let executor = DefaultExecutor::new(provider, Arc::new(ClientPool::new()), Some(node))
        .expect("C10 compatible executor");
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        executor.execute(ExecutionRequest {
            model: model.into(),
            body: json!({
                "model": model,
                "messages": [{"role": "user", "content": "one attempt"}],
                "stream": false
            }),
            stream: false,
            credentials: connection(provider, base_url),
            proxy: None,
            client_headers: BTreeMap::new(),
        }),
    )
    .await
    .expect("executor must not wait for a temporal retry delay")
    .expect("C10 raw upstream response");
    assert_eq!(result.response.status(), status);
    assert_eq!(result.response.headers().get("retry-after").unwrap(), "17");
    assert_eq!(result.response.headers().get("x-c10").unwrap(), "preserved");
    assert_eq!(result.response.text().await, body);
    assert_eq!(
        upstream.request_count().await,
        1,
        "executor repeated a generation request for {status}"
    );
    upstream.shutdown().await;
}

#[tokio::test]
async fn temporal_failures_return_raw_without_sleep_or_same_url_retry() {
    // The 429 case uses the former tokenrouter-free-model exception. The
    // semaphore remains, but its old 1s/2s temporal retry loop does not.
    assert_single_immediate_attempt(
        "tokenrouter",
        "qwen/qwen3.8-max-free",
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    assert_single_immediate_attempt("c10-502", "gpt-c10", StatusCode::BAD_GATEWAY).await;
    assert_single_immediate_attempt("c10-503", "gpt-c10", StatusCode::SERVICE_UNAVAILABLE).await;
    assert_single_immediate_attempt("c10-504", "gpt-c10", StatusCode::GATEWAY_TIMEOUT).await;
}

#[test]
fn default_executor_contains_no_temporal_generation_scheduler() {
    let source = include_str!("../src/core/executor/default.rs");
    let execute = source
        .split("pub async fn execute(")
        .nth(1)
        .and_then(|tail| {
            tail.split("/// Send a single request without retries")
                .next()
        })
        .expect("DefaultExecutor::execute source");

    for removed in [
        "for retry in",
        "tokio::time::sleep",
        "delay_secs",
        "2u64.pow",
    ] {
        assert!(
            !execute.contains(removed),
            "DefaultExecutor still owns temporal generation scheduling: {removed}"
        );
    }
    assert!(
        execute.contains("try_refresh_credentials"),
        "transitional credential recovery must remain explicit until C13"
    );
    assert!(
        source.contains("self.pool.get(&self.provider")
            && source.contains("self.pool.get_hyper_direct(&self.provider)"),
        "C10 must preserve pooled reqwest and hyper transports"
    );
}
