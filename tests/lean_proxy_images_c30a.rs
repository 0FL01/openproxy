mod common;

use std::convert::Infallible;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body as AxumBody};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::stream;
use hyper::http;
use openproxy::core::executor::{
    ClientPool, CodexExecutionRequest, CodexExecutor, CodexExecutorError,
};
use openproxy::core::translator::helpers::image_helper::{
    process_image_response_for_test, ImageLimits, ImagePrefetchBudget, ImagePrefetchError,
};
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use reqwest::Body as ReqwestBody;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tower::util::ServiceExt;

fn limits(per_image: usize, aggregate: usize, encoded: usize) -> ImageLimits {
    ImageLimits {
        per_image_decoded: per_image,
        aggregate_decoded: aggregate,
        aggregate_encoded: encoded,
        final_request: 1024 * 1024,
    }
}

fn response(
    status: u16,
    content_type: Option<&str>,
    content_length: Option<usize>,
    body: ReqwestBody,
) -> reqwest::Response {
    let mut builder = http::Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(http::header::CONTENT_TYPE, content_type);
    }
    if let Some(content_length) = content_length {
        builder = builder.header(http::header::CONTENT_LENGTH, content_length);
    }
    reqwest::Response::from(builder.body(body).unwrap())
}

fn png(size: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(size, b'x');
    bytes
}

#[tokio::test]
async fn exact_per_image_limit_passes_and_plus_one_fails_while_streaming() {
    let body = json!({"messages": []});
    let exact = png(32);
    let chunks = stream::iter(vec![
        Ok::<_, Infallible>(exact[..8].to_vec()),
        Ok(exact[8..].to_vec()),
    ]);
    let mut budget = ImagePrefetchBudget::with_limits(&body, limits(32, 64, 256)).unwrap();
    let fetched = process_image_response_for_test(
        response(
            200,
            Some("image/png"),
            None,
            ReqwestBody::wrap_stream(chunks),
        ),
        &mut budget,
    )
    .await
    .unwrap();
    assert!(fetched.data_url.starts_with("data:image/png;base64,"));

    let mut budget = ImagePrefetchBudget::with_limits(&body, limits(32, 64, 256)).unwrap();
    let error = process_image_response_for_test(
        response(200, Some("image/png"), None, ReqwestBody::from(png(33))),
        &mut budget,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        ImagePrefetchError::ImageTooLarge { limit: 32 }
    ));
    assert_eq!(error.http_status(), 413);
}

#[tokio::test]
async fn identity_content_length_rejects_early_but_unknown_chunked_length_is_counted() {
    let body = json!({"messages": []});
    let mut budget = ImagePrefetchBudget::with_limits(&body, limits(16, 64, 256)).unwrap();
    let error = process_image_response_for_test(
        response(
            200,
            Some("image/png"),
            Some(17),
            ReqwestBody::from(Vec::new()),
        ),
        &mut budget,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        ImagePrefetchError::DeclaredImageTooLarge {
            declared: 17,
            limit: 16
        }
    ));

    let chunks = stream::iter(vec![Ok::<_, Infallible>(png(12)), Ok(vec![b'x'; 5])]);
    let mut budget = ImagePrefetchBudget::with_limits(&body, limits(16, 64, 256)).unwrap();
    let error = process_image_response_for_test(
        response(
            200,
            Some("image/png"),
            None,
            ReqwestBody::wrap_stream(chunks),
        ),
        &mut budget,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        ImagePrefetchError::ImageTooLarge { limit: 16 }
    ));
}

#[tokio::test]
async fn gzip_wire_length_cannot_hide_decoded_expansion() {
    let decoded = png(2048);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&decoded).unwrap();
    let compressed = encoder.finish().unwrap();
    assert!(compressed.len() < decoded.len());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            compressed.len()
        );
        socket.write_all(headers.as_bytes()).await.unwrap();
        socket.write_all(&compressed).await.unwrap();
    });

    let fetched_response = reqwest::Client::new()
        .get(format!("http://{address}/image"))
        .send()
        .await
        .unwrap();
    let body = json!({"messages": []});
    let mut budget = ImagePrefetchBudget::with_limits(&body, limits(128, 4096, 4096)).unwrap();
    let error = process_image_response_for_test(fetched_response, &mut budget)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ImagePrefetchError::ImageTooLarge { limit: 128 }
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn individually_valid_images_cannot_exceed_aggregate_decoded_budget() {
    let body = json!({"messages": []});
    let mut budget = ImagePrefetchBudget::with_limits(&body, limits(16, 20, 256)).unwrap();
    process_image_response_for_test(
        response(200, Some("image/png"), None, ReqwestBody::from(png(12))),
        &mut budget,
    )
    .await
    .unwrap();
    let error = process_image_response_for_test(
        response(200, Some("image/png"), None, ReqwestBody::from(png(12))),
        &mut budget,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        ImagePrefetchError::AggregateDecodedTooLarge { limit: 20 }
    ));
}

#[tokio::test]
async fn base64_expansion_obeys_exact_encoded_and_final_estimate_limits() {
    let body = json!({"messages": []});
    let image = png(12);
    let encoded_data_url_len = "data:image/png;base64,".len() + 16;
    let initial_len = serde_json::to_vec(&body).unwrap().len();

    let exact_limits = ImageLimits {
        per_image_decoded: 12,
        aggregate_decoded: 12,
        aggregate_encoded: encoded_data_url_len,
        final_request: initial_len + encoded_data_url_len,
    };
    let mut budget = ImagePrefetchBudget::with_limits(&body, exact_limits).unwrap();
    process_image_response_for_test(
        response(
            200,
            Some("image/png"),
            None,
            ReqwestBody::from(image.clone()),
        ),
        &mut budget,
    )
    .await
    .unwrap();

    let mut budget = ImagePrefetchBudget::with_limits(
        &body,
        ImageLimits {
            aggregate_encoded: encoded_data_url_len - 1,
            ..exact_limits
        },
    )
    .unwrap();
    assert!(matches!(
        process_image_response_for_test(
            response(
                200,
                Some("image/png"),
                None,
                ReqwestBody::from(image.clone())
            ),
            &mut budget,
        )
        .await
        .unwrap_err(),
        ImagePrefetchError::AggregateEncodedTooLarge { .. }
    ));

    let mut budget = ImagePrefetchBudget::with_limits(
        &body,
        ImageLimits {
            final_request: initial_len + encoded_data_url_len - 1,
            ..exact_limits
        },
    )
    .unwrap();
    assert!(matches!(
        process_image_response_for_test(
            response(200, Some("image/png"), None, ReqwestBody::from(image)),
            &mut budget,
        )
        .await
        .unwrap_err(),
        ImagePrefetchError::FinalRequestTooLarge { .. }
    ));
}

#[tokio::test]
async fn mime_magic_missing_status_and_midstream_failures_are_explicit_502s() {
    let body = json!({"messages": []});
    for (response, expected) in [
        (
            response(200, Some("text/plain"), None, ReqwestBody::from(png(12))),
            "mime",
        ),
        (
            response(
                200,
                Some("image/png"),
                None,
                ReqwestBody::from(b"not-image".to_vec()),
            ),
            "magic",
        ),
        (
            response(404, Some("image/png"), None, ReqwestBody::from(Vec::new())),
            "status",
        ),
    ] {
        let mut budget = ImagePrefetchBudget::with_limits(&body, limits(64, 64, 256)).unwrap();
        let error = process_image_response_for_test(response, &mut budget)
            .await
            .unwrap_err();
        assert_eq!(error.http_status(), 502, "{expected}: {error}");
    }

    let failed_stream = stream::iter(vec![
        Ok::<Vec<u8>, std::io::Error>(png(12)),
        Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "cut")),
    ]);
    let mut budget = ImagePrefetchBudget::with_limits(&body, limits(64, 64, 256)).unwrap();
    let error = process_image_response_for_test(
        response(
            200,
            Some("image/png"),
            None,
            ReqwestBody::wrap_stream(failed_stream),
        ),
        &mut budget,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ImagePrefetchError::Transport(_)));
    assert_eq!(error.http_status(), 502);
}

#[tokio::test]
async fn cancellation_drops_the_in_progress_response_stream() {
    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let stream = async_stream::stream! {
        let _signal = DropSignal(Some(dropped_tx));
        yield Ok::<Vec<u8>, std::io::Error>(png(12));
        futures_util::future::pending::<()>().await;
    };
    let body = json!({"messages": []});
    let mut budget = ImagePrefetchBudget::with_limits(&body, limits(64, 64, 256)).unwrap();
    let task = tokio::spawn(async move {
        process_image_response_for_test(
            response(
                200,
                Some("image/png"),
                None,
                ReqwestBody::wrap_stream(stream),
            ),
            &mut budget,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    tokio::time::timeout(Duration::from_secs(1), dropped_rx)
        .await
        .expect("cancelled image read must drop its response stream")
        .unwrap();
}

fn node(id: &str, kind: &str, prefix: &str, base_url: String, api_type: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: kind.into(),
        name: format!("C30A {kind}"),
        prefix: Some(prefix.into()),
        api_type: Some(api_type.into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

fn connection(id: &str, provider: &str, model: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        priority: Some(1),
        api_key: Some(format!("{id}-key")),
        default_model: Some(model.into()),
        ..Default::default()
    }
}

async fn post(app: &axum::Router, uri: &str, body: serde_json::Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(AxumBody::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn native_openai_and_claude_passthrough_perform_zero_image_fetches() {
    let openai = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        r#"{"id":"chatcmpl-c30a","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
    )])
    .await;
    let claude = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        r#"{"id":"msg_c30a","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#,
    )])
    .await;
    let db = TempTestDb::new().await;
    db.db
        .update(|state| {
            state.api_keys = vec![test_api_key()];
            state.provider_nodes = vec![
                node(
                    "openai-compatible-c30a",
                    "openai-compatible",
                    "oa30",
                    openai.url("/v1"),
                    "chat",
                ),
                node(
                    "anthropic-compatible-c30a",
                    "anthropic-compatible",
                    "ac30",
                    claude.url("/v1"),
                    "messages",
                ),
            ];
            state.provider_connections = vec![
                connection("oa-c30a", "openai-compatible-c30a", "model-a"),
                connection("ac-c30a", "anthropic-compatible-c30a", "claude-sonnet-4"),
            ];
        })
        .await
        .unwrap();
    let app = openproxy::build_app(AppState::new(db.db.clone()));
    let blocked_url = "http://127.0.0.1:1/must-not-fetch.png";

    let response = post(
        &app,
        "/v1/chat/completions",
        json!({
            "model": "oa30/model-a",
            "messages": [{"role": "user", "content": [{
                "type": "image_url", "image_url": {"url": blocked_url}
            }]}],
            "stream": false
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = post(
        &app,
        "/v1/messages",
        json!({
            "model": "ac30/claude-sonnet-4",
            "max_tokens": 32,
            "messages": [{"role": "user", "content": [{
                "type": "image", "source": {"type": "url", "url": blocked_url}
            }]}],
            "stream": false
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let openai_requests = openai.requests().await;
    let claude_requests = claude.requests().await;
    assert_eq!(openai_requests.len(), 1);
    assert_eq!(claude_requests.len(), 1);
    let openai_body: serde_json::Value = serde_json::from_slice(&openai_requests[0].body).unwrap();
    let claude_body: serde_json::Value = serde_json::from_slice(&claude_requests[0].body).unwrap();
    assert_eq!(
        openai_body["messages"][0]["content"][0]["image_url"]["url"],
        blocked_url
    );
    assert_eq!(
        claude_body["messages"][0]["content"][0]["source"]["url"],
        blocked_url
    );

    openai.shutdown().await;
    claude.shutdown().await;
}

#[tokio::test]
async fn malformed_inline_required_attachment_returns_502_before_generation() {
    let generation = MockUpstream::start([]).await;
    let db = TempTestDb::new().await;
    db.db
        .update(|state| {
            state.api_keys = vec![test_api_key()];
            state.provider_nodes = vec![node(
                "ollama",
                "ollama",
                "ol30",
                generation.url("/v1"),
                "chat",
            )];
            state.provider_connections = vec![connection("oa-c30a", "ollama", "model-a")];
        })
        .await
        .unwrap();
    let app = openproxy::build_app(AppState::new(db.db.clone()));

    let response = post(
        &app,
        "/v1/chat/completions",
        json!({
                "model": "ollama/model-a",
            "messages": [{"role": "user", "content": [{
                "type": "image_url", "image_url": {"detail": "high"}
            }]}],
            "stream": false
        }),
    )
    .await;
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(String::from_utf8_lossy(&body).contains("Image prefetch failed"));
    assert_eq!(generation.request_count().await, 0);

    generation.shutdown().await;
}

#[tokio::test]
async fn codex_typed_prefetch_failure_prevents_generation_send() {
    let generation = MockUpstream::start([]).await;
    let executor = CodexExecutor::new(
        Arc::new(ClientPool::new()),
        Some(ProviderNode {
            base_url: Some(generation.url("/codex/responses")),
            ..Default::default()
        }),
    )
    .unwrap();
    let error = executor
        .execute(CodexExecutionRequest {
            model: "gpt-5.6-luna".into(),
            body: json!({
                "input": [{"role": "user", "content": [{
                    "type": "image_url", "image_url": {"detail": "high"}
                }]}]
            }),
            stream: true,
            credentials: ProviderConnection {
                id: "codex-c30a".into(),
                provider: "codex".into(),
                api_key: Some("fixture-key".into()),
                ..Default::default()
            },
            proxy: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(error, CodexExecutorError::ImagePrefetch(_)));
    assert_eq!(generation.request_count().await, 0);
    generation.shutdown().await;
}
