mod common;

use std::io::Write;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use flate2::write::GzEncoder;
use flate2::Compression;
use openproxy::core::executor::{
    read_reqwest_body, read_reqwest_diagnostic, BoundedBodyError,
    DEFAULT_DIAGNOSTIC_BODY_LIMIT_BYTES, DIAGNOSTIC_TRANSPORT_MARKER, DIAGNOSTIC_TRUNCATION_MARKER,
};
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

async fn reqwest_response(upstream: &MockUpstream) -> reqwest::Response {
    reqwest::Client::new()
        .get(upstream.url("/body"))
        .send()
        .await
        .expect("loopback response")
}

async fn app_for(upstream: &MockUpstream, provider: &str) -> (axum::Router, TempTestDb) {
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
                id: format!("{provider}-account"),
                provider: provider.into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                priority: Some(1),
                api_key: Some("c29-placeholder-key".into()),
                default_model: Some("gpt-c29".into()),
                ..Default::default()
            }];
        })
        .await
        .expect("seed C29 app");
    (
        openproxy::build_app(AppState::new(test_db.db.clone())),
        test_db,
    )
}

async fn chat(app: axum::Router, provider: &str, stream: bool) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer test-key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": format!("{provider}/gpt-c29"),
                    "messages": [{"role": "user", "content": "bounded"}],
                    "stream": stream
                })
                .to_string(),
            ))
            .expect("C29 request"),
    )
    .await
    .expect("C29 response")
}

#[tokio::test]
async fn reqwest_reader_accepts_exact_limit_and_rejects_plus_one_without_length() {
    let exact = MockUpstream::start([ScriptedResponse::json(StatusCode::OK, "abcdefgh")]).await;
    assert_eq!(
        read_reqwest_body(reqwest_response(&exact).await, 8)
            .await
            .expect("exact limit"),
        "abcdefgh"
    );
    exact.shutdown().await;

    let oversized =
        MockUpstream::start([ScriptedResponse::json(StatusCode::OK, "abcdefghi")]).await;
    assert_eq!(
        read_reqwest_body(reqwest_response(&oversized).await, 8).await,
        Err(BoundedBodyError::TooLarge { limit: 8 })
    );
    oversized.shutdown().await;
}

#[tokio::test]
async fn identity_content_length_is_only_an_early_rejection_hint() {
    let upstream = MockUpstream::start([
        ScriptedResponse::json(StatusCode::OK, "tiny").with_header("content-length", "4096")
    ])
    .await;
    assert_eq!(
        read_reqwest_body(reqwest_response(&upstream).await, 1024).await,
        Err(BoundedBodyError::DeclaredTooLarge {
            declared: 4096,
            limit: 1024,
        })
    );
    upstream.shutdown().await;
}

#[tokio::test]
async fn actual_decompressed_chunked_bytes_override_compressed_content_length_hint() {
    let expanded = vec![b'x'; 4096];
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&expanded).expect("compress fixture");
    let compressed = encoder.finish().expect("finish fixture");
    assert!(compressed.len() < 1024);

    let upstream =
        MockUpstream::start([ScriptedResponse::json(StatusCode::OK, compressed.clone())
            .with_header("content-encoding", "gzip")
            .with_header("content-length", compressed.len().to_string())])
        .await;
    assert_eq!(
        read_reqwest_body(reqwest_response(&upstream).await, 1024).await,
        Err(BoundedBodyError::TooLarge { limit: 1024 })
    );
    upstream.shutdown().await;
}

#[tokio::test]
async fn unicode_split_across_chunks_is_preserved() {
    let upstream = MockUpstream::start([ScriptedResponse::sse([
        &b"{\"text\":\"\xe2"[..],
        &b"\x82"[..],
        &b"\xac\"}"[..],
    ])])
    .await;
    let body = read_reqwest_body(reqwest_response(&upstream).await, 14)
        .await
        .expect("split UTF-8 body");
    assert_eq!(std::str::from_utf8(&body).unwrap(), "{\"text\":\"€\"}");
    upstream.shutdown().await;
}

#[tokio::test]
async fn partial_success_transport_failure_becomes_bad_gateway_not_truncated_json() {
    let provider = "c29-partial";
    let upstream = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        r#"{"choices":[{"message":{"content":"partial"}}"#,
    )
    .failing_after_chunks()])
    .await;
    let (app, _db) = app_for(&upstream, provider).await;
    let response = chat(app, provider, false).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).expect("structured read failure");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("complete upstream response"));
    assert!(!String::from_utf8_lossy(&body).contains("partial"));
    upstream.shutdown().await;
}

#[tokio::test]
async fn partial_diagnostic_transport_failure_is_explicit() {
    let upstream = MockUpstream::start([
        ScriptedResponse::json(StatusCode::BAD_GATEWAY, "prefix").failing_after_chunks()
    ])
    .await;
    let diagnostic = read_reqwest_diagnostic(reqwest_response(&upstream).await, 128).await;
    assert!(!diagnostic.truncated);
    assert!(diagnostic.transport_failed);
    assert!(diagnostic.bytes.starts_with(b"prefix"));
    assert!(diagnostic.bytes.ends_with(DIAGNOSTIC_TRANSPORT_MARKER));
    assert!(diagnostic.bytes.len() <= 128);
    upstream.shutdown().await;
}

#[tokio::test]
async fn large_diagnostic_is_marked_and_preserves_status_and_retry_after() {
    let provider = "c29-diagnostic";
    let upstream = MockUpstream::start([ScriptedResponse::json(
        StatusCode::TOO_MANY_REQUESTS,
        vec![b'e'; DEFAULT_DIAGNOSTIC_BODY_LIMIT_BYTES + 1],
    )
    .with_header("retry-after", "120")])
    .await;
    let (app, _db) = app_for(&upstream, provider).await;
    let response = chat(app, provider, false).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().contains_key("retry-after"));
    let body = to_bytes(
        response.into_body(),
        DEFAULT_DIAGNOSTIC_BODY_LIMIT_BYTES + 1,
    )
    .await
    .unwrap();
    assert_eq!(body.len(), DEFAULT_DIAGNOSTIC_BODY_LIMIT_BYTES);
    assert!(body.ends_with(DIAGNOSTIC_TRUNCATION_MARKER));
    upstream.shutdown().await;
}

#[tokio::test]
async fn valid_nonstream_json_is_returned_without_truncation() {
    let provider = "c29-valid";
    let expected = json!({
        "id": "chatcmpl-c29",
        "object": "chat.completion",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "valid €"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
    });
    let upstream = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        serde_json::to_vec(&expected).unwrap(),
    )])
    .await;
    let (app, _db) = app_for(&upstream, provider).await;
    let response = chat(app, provider, false).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected);
    upstream.shutdown().await;
}

#[test]
fn generation_collector_census_and_native_stream_guard() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let executor_dir = root.join("src/core/executor");
    for entry in std::fs::read_dir(executor_dir).expect("executor source directory") {
        let path = entry.expect("executor source entry").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
            || path.file_name().and_then(|name| name.to_str()) == Some("bounded_body.rs")
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("executor source");
        for forbidden in [
            ".bytes()",
            ".text()",
            "BodyExt::collect",
            ".json().await",
            ".json::<",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} retains an unbounded whole-body collector pattern {forbidden}",
                path.display()
            );
        }
        if path.file_name().and_then(|name| name.to_str()) != Some("codex.rs") {
            assert!(
                !source.contains(".bytes_stream()"),
                "{} retains a direct response stream collector instead of the shared bounded reader",
                path.display()
            );
        }
    }

    let chat = include_str!("../src/server/api/chat.rs");
    assert!(!chat.contains("collect_upstream_response_bytes"));
    let native = chat
        .split("async fn proxy_response_with_pending_tracking")
        .nth(1)
        .and_then(|tail| tail.split("fn responses_stream_completed").next())
        .expect("native streaming function source");
    assert!(native.contains("response.bytes_stream()"));
    let live_match = native
        .split("let body = match response {")
        .nth(1)
        .expect("native transport match");
    assert!(!live_match.contains("read_upstream_body(response"));
    assert!(!live_match.contains("read_upstream_diagnostic(response"));

    let bounded = include_str!("../src/core/executor/bounded_body.rs");
    assert!(bounded.contains("checked_add(chunk.len())"));
    assert!(bounded.contains("declared_length_is_identity_encoded"));
    assert!(bounded.contains("UpstreamResponse::Reqwest"));
    assert!(bounded.contains("UpstreamResponse::Hyper"));
    assert!(bounded.contains("DEFAULT_SUCCESS_BODY_LIMIT_BYTES"));
    assert!(bounded.contains("DEFAULT_DIAGNOSTIC_BODY_LIMIT_BYTES"));
    assert!(bounded.contains("DIAGNOSTIC_TRANSPORT_MARKER"));
    assert!(!bounded.contains("extend_from_slice(&chunk)"));
    assert!(!DIAGNOSTIC_TRANSPORT_MARKER.is_empty());
}
