//! Verify real inbound h2c and safe version observations at the application
//! boundary, alongside authenticated live tool/usage SSE and cancellation.
mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{StatusCode, Version};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use futures_util::StreamExt;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;
use tokio::sync::Notify;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogCapture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[tokio::test]
async fn actual_h2c_ingress_keeps_auth_live_sse_and_private_transport_logs() {
    let logs = LogCapture::default();
    tracing_subscriber::fmt()
        .with_env_filter("openproxy::transport=info")
        .with_ansi(false)
        .without_time()
        .with_writer(logs.clone())
        .try_init()
        .expect("isolated integration-test transport subscriber");

    let dropped = Arc::new(Notify::new());
    let upstream = MockUpstream::start([
        ScriptedResponse::sse([
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_h2\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
        ]).holding_eof(Arc::new(Notify::new())).notifying_on_body_drop(dropped.clone()),
    ]).await;
    let db = TempTestDb::new().await;
    db.db
        .update(|data| {
            data.api_keys = vec![test_api_key()];
            data.settings.require_api_key = true;
            data.provider_nodes = vec![ProviderNode {
                id: "transport-fixture".into(),
                r#type: "openai-compatible".into(),
                name: "private-provider-name-marker".into(),
                prefix: Some("transport-fixture".into()),
                api_type: Some("chat".into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            data.provider_connections = vec![ProviderConnection {
                id: "private-account-name-marker".into(),
                provider: "transport-fixture".into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                api_key: Some("private-credential-marker".into()),
                default_model: Some("private-model-marker".into()),
                ..Default::default()
            }];
        })
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = openproxy::build_app(AppState::new(db.db.clone()));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    // Prior knowledge models nginx's backend h2c connection only.
    let h2 = reqwest::Client::builder()
        .http2_prior_knowledge()
        .build()
        .unwrap();
    let endpoint = format!("http://{address}/v1/chat/completions?private-query-marker=1");
    let body = json!({
        "model": "transport-fixture/private-model-marker",
        "messages": [{"role":"user", "content":"private-body-marker"}],
        "stream": true,
        "stream_options": {"include_usage":true}
    });
    let unauthorized = h2.post(&endpoint).json(&body).send().await.unwrap();
    assert_eq!(unauthorized.version(), Version::HTTP_2);
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(upstream.request_count().await, 0);

    let response = h2
        .post(endpoint)
        .header("authorization", "Bearer test-key")
        .header("x-forwarded-proto", "HTTP/1.1-private-header-marker")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.version(), Version::HTTP_2);
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.bytes_stream();
    let bytes = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        while !String::from_utf8_lossy(&bytes).contains("total_tokens") {
            bytes.extend_from_slice(&stream.next().await.unwrap().unwrap());
        }
        bytes
    })
    .await
    .expect("tool and usage SSE arrive over h2c before held upstream EOF");
    assert!(String::from_utf8_lossy(&bytes).contains("call_h2"));
    drop(stream);
    tokio::time::timeout(Duration::from_secs(3), dropped.notified())
        .await
        .expect("downstream H2 reset propagates cancellation to upstream body");
    assert_eq!(
        upstream.request_count().await,
        1,
        "no generation repeated after cancellation"
    );

    let h1 = reqwest::Client::builder().http1_only().build().unwrap();
    let health = h1
        .get(format!("http://{address}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.version(), Version::HTTP_11);
    assert_eq!(health.status(), StatusCode::OK);
    let observed = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        observed.contains("leg=\"inbound\" http_version=HTTP/2.0"),
        "{observed}"
    );
    assert!(
        observed.contains("leg=\"inbound\" http_version=HTTP/1.1"),
        "{observed}"
    );
    assert!(observed.contains("leg=\"upstream\" executor=\"default\" transport=\"hyper\" http_version=HTTP/1.1 status=200"), "{observed}");
    for forbidden in ["private-", "test-key", "authorization", "/v1/", "127.0.0.1"] {
        assert!(
            !observed.contains(forbidden),
            "transport logs must contain metadata only: {observed}"
        );
    }
    server.abort();
    upstream.shutdown().await;
}
