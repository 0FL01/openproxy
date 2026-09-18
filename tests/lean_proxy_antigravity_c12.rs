mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::lean_harness::{MockUpstream, ScriptedResponse};
use futures_util::StreamExt;
use openproxy::core::executor::{
    read_upstream_body, AntigravityExecutionRequest, AntigravityExecutor, ClientPool,
    UpstreamResponse,
};
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;
use tokio::sync::Notify;

fn antigravity_node(base_url: String) -> ProviderNode {
    ProviderNode {
        id: "antigravity".into(),
        r#type: "antigravity".into(),
        name: "Antigravity C12 loopback".into(),
        prefix: Some("ag".into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

fn antigravity_connection(id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "antigravity".into(),
        auth_type: "oauth".into(),
        is_active: Some(true),
        access_token: Some("fixture-c12-token".into()),
        ..Default::default()
    }
}

fn antigravity_request(connection_id: &str, stream: bool) -> AntigravityExecutionRequest {
    AntigravityExecutionRequest {
        model: "gemini-2.5-pro".into(),
        body: json!({
            "model": "gemini-2.5-pro",
            "contents": [{"role": "user", "parts": [{"text": "one attempt"}]}]
        }),
        stream,
        credentials: antigravity_connection(connection_id),
        proxy: None,
    }
}

async fn execute_against(
    upstream: &MockUpstream,
    connection_id: &str,
    stream: bool,
) -> openproxy::core::executor::AntigravityExecutorResponse {
    let executor = AntigravityExecutor::new(
        Arc::new(ClientPool::new()),
        Some(antigravity_node(upstream.url("/alternate"))),
    )
    .expect("Antigravity C12 executor");
    tokio::time::timeout(
        Duration::from_millis(500),
        executor.execute_request(antigravity_request(connection_id, stream)),
    )
    .await
    .expect("Antigravity generation must not wait for a temporal retry delay")
    .expect("Antigravity C12 response")
}

#[tokio::test]
async fn errors_preserve_body_headers_endpoint_and_one_attempt() {
    let cases = [
        (
            "retry-after",
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"rate limited"}}"#,
            Some("19"),
        ),
        (
            "malformed",
            StatusCode::INTERNAL_SERVER_ERROR,
            "not-json{{{ quota_exhausted",
            Some("17"),
        ),
        (
            "unauthorized",
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"expired"}}"#,
            None,
        ),
    ];

    for (name, status, body, retry_after) in cases {
        let mut first = ScriptedResponse::json(status, body).with_header("x-c12", name);
        if let Some(value) = retry_after {
            first = first.with_header("retry-after", value);
        }
        let upstream = MockUpstream::start([
            first,
            ScriptedResponse::json(StatusCode::OK, r#"{"must":"not run"}"#),
        ])
        .await;
        let connection_id = format!("c12-{name}");

        let result = execute_against(&upstream, &connection_id, false).await;
        assert_eq!(result.response.status(), status, "case {name}");
        assert_eq!(result.response.headers().get("x-c12").unwrap(), name);
        if let Some(value) = retry_after {
            assert_eq!(result.response.headers().get("retry-after").unwrap(), value);
        }
        let response_body = read_upstream_body(result.response, 64 * 1024)
            .await
            .expect("bounded C12 response body");
        assert_eq!(response_body, body, "case {name}");
        assert_eq!(
            upstream.request_count().await,
            1,
            "Antigravity repeated case {name}"
        );
        let requests = upstream.requests().await;
        assert_eq!(
            requests[0].path, "/alternate/v1internal:generateContent",
            "configured endpoint was not used for case {name}"
        );
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn successful_sse_is_live_and_cancellation_drops_upstream() {
    let first_delta =
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"first\"}]}}]}\n\n";
    let hold_eof = Arc::new(Notify::new());
    let upstream_body_dropped = Arc::new(Notify::new());
    let upstream = MockUpstream::start([ScriptedResponse::sse([first_delta])
        .holding_eof(hold_eof)
        .notifying_on_body_drop(upstream_body_dropped.clone())])
    .await;

    let result = execute_against(&upstream, "c12-success-sse", true).await;
    assert_eq!(result.response.status(), StatusCode::OK);
    assert_eq!(
        result.url,
        upstream.url("/alternate/v1internal:streamGenerateContent?alt=sse")
    );
    let UpstreamResponse::Reqwest(response) = result.response else {
        panic!("Antigravity must use pooled reqwest transport");
    };
    let mut body = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_millis(500), body.next())
        .await
        .expect("first SSE chunk must arrive before EOF")
        .expect("first Antigravity chunk")
        .expect("valid Antigravity chunk");
    assert_eq!(first.as_ref(), first_delta.as_bytes());
    drop(body);

    tokio::time::timeout(Duration::from_secs(1), upstream_body_dropped.notified())
        .await
        .expect("downstream cancellation must drop the held upstream body");
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}

#[test]
fn antigravity_generation_has_no_independent_temporal_retry_scheduler() {
    let source = include_str!("../src/core/executor/antigravity.rs");
    let execute = source
        .split("pub async fn execute_request(")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n\n#[cfg(test)]").next())
        .expect("AntigravityExecutor::execute_request source");

    for removed in [
        "MAX_RETRIES",
        "for attempt in",
        "parse_retry_after",
        "is_transient_antigravity_error",
        "response.bytes()",
        "rand::random",
        "tokio::time::sleep",
    ] {
        assert!(
            !execute.contains(removed),
            "Antigravity generation still owns temporal scheduling: {removed}"
        );
    }
    assert!(!source.contains("RetryExhausted"));
    assert!(execute.contains("self.pool.get(\"antigravity\""));
    assert!(execute.contains("self.request_url(request.stream)"));
    assert!(execute.contains("UpstreamResponse::Reqwest(response)"));
    assert!(!execute.contains("on_user_onboard"));
    assert!(!execute.contains("tokio::spawn"));
}
