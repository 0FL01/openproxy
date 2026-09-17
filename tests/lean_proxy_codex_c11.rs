mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::lean_harness::{MockUpstream, ScriptedResponse};
use futures_util::StreamExt;
use openproxy::core::executor::{
    ClientPool, CodexExecutionRequest, CodexExecutor, UpstreamResponse,
};
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;
use tokio::sync::Notify;

fn codex_node(endpoint: String) -> ProviderNode {
    ProviderNode {
        id: "codex".into(),
        r#type: "codex".into(),
        name: "Codex C11 loopback".into(),
        prefix: Some("cx".into()),
        api_type: Some("responses".into()),
        base_url: Some(endpoint),
        ..Default::default()
    }
}

fn codex_connection() -> ProviderConnection {
    ProviderConnection {
        id: "codex-c11".into(),
        provider: "codex".into(),
        auth_type: "oauth".into(),
        is_active: Some(true),
        access_token: Some("fixture-c11-token".into()),
        ..Default::default()
    }
}

fn codex_request() -> CodexExecutionRequest {
    CodexExecutionRequest {
        model: "codex/gpt-5.6-luna".into(),
        body: json!({
            "model": "codex/gpt-5.6-luna",
            "messages": [{"role": "user", "content": "one Codex attempt"}],
            "stream": true
        }),
        stream: true,
        web_search_context_size: None,
        credentials: codex_connection(),
        proxy: None,
    }
}

async fn execute_against(
    upstream: &MockUpstream,
) -> openproxy::core::executor::CodexExecutorResponse {
    let executor = CodexExecutor::new(
        Arc::new(ClientPool::new()),
        Some(codex_node(upstream.url("/backend-api/codex/responses"))),
    )
    .expect("Codex C11 executor");
    tokio::time::timeout(
        Duration::from_millis(500),
        executor.execute(codex_request()),
    )
    .await
    .expect("Codex preflight must not wait for a temporal retry delay")
    .expect("Codex C11 response")
}

#[tokio::test]
async fn structured_first_error_reaches_planner_without_retry_or_sleep() {
    let first_error = concat!(
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{",
        "\"code\":\"server_is_overloaded\",\"message\":\"localized overload\"}}}\n\n"
    );
    let split = first_error.len() / 2;
    let upstream = MockUpstream::start([
        ScriptedResponse::sse([
            first_error[..split].to_string(),
            first_error[split..].to_string(),
        ])
        .with_header("retry-after", "19")
        .with_header("x-c11", "preserved"),
        ScriptedResponse::sse([concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"must not run\"}\n\n"
        )]),
    ])
    .await;

    let result = execute_against(&upstream).await;
    assert_eq!(result.response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(result.response.headers().get("retry-after").unwrap(), "19");
    assert_eq!(result.response.headers().get("x-c11").unwrap(), "preserved");
    assert_eq!(result.response.text().await, first_error);
    assert_eq!(
        upstream.request_count().await,
        1,
        "Codex executor repeated the generation request"
    );
    upstream.shutdown().await;
}

#[tokio::test]
async fn normal_first_event_is_available_before_eof_and_cancellation_propagates() {
    let first_delta = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"first\"}\n\n"
    );
    let hold_eof = Arc::new(Notify::new());
    let upstream_body_dropped = Arc::new(Notify::new());
    let upstream = MockUpstream::start([ScriptedResponse::sse([first_delta])
        .holding_eof(hold_eof)
        .notifying_on_body_drop(upstream_body_dropped.clone())])
    .await;

    let result = execute_against(&upstream).await;
    assert_eq!(result.response.status(), StatusCode::OK);
    let UpstreamResponse::Reqwest(response) = result.response else {
        panic!("Codex must use reqwest transport");
    };
    let mut body = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_millis(500), body.next())
        .await
        .expect("normal first event must not wait for EOF")
        .expect("first Codex chunk")
        .expect("valid first Codex chunk");
    assert_eq!(first.as_ref(), first_delta.as_bytes());
    drop(body);

    tokio::time::timeout(Duration::from_secs(1), upstream_body_dropped.notified())
        .await
        .expect("downstream cancellation must drop the held Codex body");
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}

#[tokio::test]
async fn error_after_meaningful_delta_is_streamed_once_without_new_generation() {
    let delta = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"committed\"}\n\n"
    );
    let later_error = concat!(
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{",
        "\"code\":\"service_unavailable_error\",\"message\":\"late\"}}}\n\n"
    );
    let upstream = MockUpstream::start([
        ScriptedResponse::sse([delta, later_error])
            .with_chunk_timing(Duration::ZERO, Duration::from_millis(20)),
        ScriptedResponse::sse(["data: must-not-be-requested\n\n"]),
    ])
    .await;

    let result = execute_against(&upstream).await;
    assert_eq!(result.response.status(), StatusCode::OK);
    let body = result.response.text().await;
    assert!(body.contains("committed"), "{body}");
    assert!(body.contains("service_unavailable_error"), "{body}");
    assert_eq!(body.matches("committed").count(), 1, "{body}");
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}

#[test]
fn codex_executor_has_no_independent_temporal_retry_scheduler() {
    let source = include_str!("../src/core/executor/codex.rs");
    let execute = source
        .split("pub async fn execute(")
        .nth(1)
        .and_then(|tail| tail.split("/// Convert OpenAI Responses API SSE").next())
        .expect("CodexExecutor::execute source");

    for removed in [
        "MAX_RETRIES",
        "for attempt in",
        "tokio::time::sleep",
        "CODEX_SSE_PEEK_BYTES",
        "CODEX_SSE_RETRY_PATTERNS",
        "CODEX_SSE_ACCOUNT_FALLBACK_PATTERNS",
        "CODEX_MODEL_CAPACITY_MESSAGE",
        "codex_sse_has_user_output",
    ] {
        assert!(
            !execute.contains(removed) && !source.contains(removed),
            "Codex still owns temporal generation scheduling: {removed}"
        );
    }
    assert!(execute.contains("CODEX_FIRST_EVENT_MAX_BYTES"));
    assert!(execute.contains("codex_first_event_failure_status"));
    assert!(source.contains("self.pool.get(\"openai\""));
}
