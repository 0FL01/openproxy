//! Real TLS/ALPN fixtures exercise ClientPool::get, including production
//! timeouts and pooling. Only the fixture's trusted root is test-injected.

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, Response, Version};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use super::*;

const TOOL: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_transport\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{}\"}}]}}]}\n\n";
const USAGE: &str = "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n";

#[derive(Default)]
struct Observations {
    connections: AtomicUsize,
    requests: Mutex<Vec<(usize, Version, String)>>,
    alpn: Mutex<Vec<Vec<u8>>>,
    cancel_dropped: Arc<Notify>,
    neighbor_release: Arc<Notify>,
}

struct Fixture {
    address: std::net::SocketAddr,
    root: reqwest::Certificate,
    observations: Arc<Observations>,
    task: JoinHandle<()>,
}

impl Fixture {
    async fn start(h2: bool) -> Self {
        ensure_rustls_provider();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .expect("local fixture certificate");
        let root = reqwest::Certificate::from_der(certificate.cert.der())
            .expect("trusted local fixture root");
        let key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate.cert.der().clone()], key.into())
            .expect("fixture TLS configuration");
        tls.alpn_protocols = if h2 {
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        } else {
            vec![b"http/1.1".to_vec()]
        };
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let observations = Arc::new(Observations::default());
        let observed = observations.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        let acceptor = acceptor.clone();
                        let observed = observed.clone();
                        connections.spawn(async move {
                            // Failed handshakes are expected in the trust/name tests.
                            let Ok(tls) = acceptor.accept(socket).await else { return };
                            let alpn = tls.get_ref().1.alpn_protocol().unwrap_or_default().to_vec();
                            let is_h2 = alpn == b"h2";
                            observed.alpn.lock().await.push(alpn);
                            let connection = observed.connections.fetch_add(1, Ordering::SeqCst);
                            let service = service_fn(move |request| {
                                respond(request, observed.clone(), connection)
                            });
                            if is_h2 {
                                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                                    .serve_connection(TokioIo::new(tls), service).await;
                            } else {
                                let _ = hyper::server::conn::http1::Builder::new()
                                    .serve_connection(TokioIo::new(tls), service).await;
                            }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            address,
            root,
            observations,
            task,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("https://localhost:{}{path}", self.address.port())
    }

    fn pool(&self, timeout: ClientTimeout) -> ClientPool {
        let mut pool = ClientPool::with_timeout(timeout);
        pool.test_roots.push(self.root.clone());
        pool
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Dropped(Arc<Notify>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

async fn respond(
    request: Request<hyper::body::Incoming>,
    observed: Arc<Observations>,
    connection: usize,
) -> Result<Response<Body>, Infallible> {
    let version = request.version();
    let path = request.uri().path().to_owned();
    let body = request.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["stream"],
        true
    );
    observed
        .requests
        .lock()
        .await
        .push((connection, version, path.clone()));
    if path == "/json" {
        return Ok(Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(r#"{"ok":true,"usage":{"total_tokens":5}}"#))
            .unwrap());
    }
    let stream = async_stream::stream! {
        let _dropped = (path == "/cancel").then(|| Dropped(observed.cancel_dropped.clone()));
        yield Ok::<_, Infallible>(Bytes::from_static(TOOL.as_bytes()));
        // Separate writes exercise incremental frame delivery.
        tokio::task::yield_now().await;
        yield Ok(Bytes::from_static(USAGE.as_bytes()));
        if path == "/cancel" || path == "/stall" {
            std::future::pending::<()>().await;
        } else {
            observed.neighbor_release.notified().await;
            yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
        }
    };
    Ok(Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap())
}

async fn send(client: &reqwest::Client, fixture: &Fixture, path: &str) -> reqwest::Response {
    tokio::time::timeout(
        Duration::from_secs(3),
        client
            .post(fixture.url(path))
            .body(r#"{"stream":true}"#)
            .send(),
    )
    .await
    .expect("response headers before deadline")
    .expect("verified TLS response")
}

async fn prefix<S>(stream: &mut S) -> Vec<u8>
where
    S: futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        while !String::from_utf8_lossy(&bytes).contains("total_tokens") {
            bytes.extend_from_slice(&stream.next().await.expect("stream open").unwrap());
        }
        assert_eq!(bytes, [TOOL.as_bytes(), USAGE.as_bytes()].concat());
        bytes
    })
    .await
    .expect("tool and usage frames before held EOF")
}

#[tokio::test]
async fn negotiated_h2_multiplexes_and_cancel_resets_only_its_stream() {
    let fixture = Fixture::start(true).await;
    let pool = fixture.pool(ClientTimeout::default());
    let client = pool.get("transport", None).unwrap();
    assert!(Arc::ptr_eq(&client, &pool.get("transport", None).unwrap()));
    let json = send(&client, &fixture, "/json").await;
    assert_eq!(json.version(), Version::HTTP_2);
    assert_eq!(json.json::<serde_json::Value>().await.unwrap()["ok"], true);
    let cancel = send(&client, &fixture, "/cancel").await;
    let neighbor = send(&client, &fixture, "/neighbor").await;
    assert_eq!(cancel.version(), Version::HTTP_2);
    assert_eq!(neighbor.version(), Version::HTTP_2);
    let mut cancel = cancel.bytes_stream();
    let mut neighbor = neighbor.bytes_stream();
    prefix(&mut cancel).await;
    prefix(&mut neighbor).await;
    let requests = fixture.observations.requests.lock().await.clone();
    assert_eq!(requests.len(), 3);
    assert!(requests
        .iter()
        .all(|(id, version, _)| *id == 0 && *version == Version::HTTP_2));
    assert_eq!(fixture.observations.connections.load(Ordering::SeqCst), 1);
    assert_eq!(
        *fixture.observations.alpn.lock().await,
        vec![b"h2".to_vec()]
    );
    drop(cancel);
    tokio::time::timeout(
        Duration::from_secs(3),
        fixture.observations.cancel_dropped.notified(),
    )
    .await
    .expect("H2 reset drops the cancelled upstream body without EOF release");
    fixture.observations.neighbor_release.notify_one();
    let tail = tokio::time::timeout(Duration::from_secs(3), async {
        let mut tail = Vec::new();
        while let Some(chunk) = neighbor.next().await {
            tail.extend_from_slice(&chunk.unwrap());
        }
        tail
    })
    .await
    .expect("neighbor completes after cancellation");
    assert_eq!(tail, b"data: [DONE]\n\n");
    // A later request still uses the same socket. Neither cancellation nor H2
    // pooling causes another generation or a replacement connection.
    assert_eq!(
        send(&client, &fixture, "/json").await.version(),
        Version::HTTP_2
    );
    assert_eq!(fixture.observations.requests.lock().await.len(), 4);
    assert_eq!(fixture.observations.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn h1_only_tls_peer_keeps_json_and_live_tool_usage_streaming() {
    let fixture = Fixture::start(false).await;
    let pool = fixture.pool(ClientTimeout::default());
    let client = pool.get("transport", None).unwrap();
    let json = send(&client, &fixture, "/json").await;
    assert_eq!(json.version(), Version::HTTP_11);
    assert_eq!(
        json.json::<serde_json::Value>().await.unwrap()["usage"]["total_tokens"],
        5
    );
    let response = send(&client, &fixture, "/neighbor").await;
    assert_eq!(response.version(), Version::HTTP_11);
    let mut stream = response.bytes_stream();
    prefix(&mut stream).await;
    fixture.observations.neighbor_release.notify_one();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
    assert_eq!(fixture.observations.connections.load(Ordering::SeqCst), 1);
    assert_eq!(
        *fixture.observations.alpn.lock().await,
        vec![b"http/1.1".to_vec()]
    );
}

#[tokio::test]
async fn negotiated_h2_preserves_read_stall_timeout() {
    let fixture = Fixture::start(true).await;
    let pool = fixture.pool(ClientTimeout {
        stream: Duration::from_millis(200),
        ..Default::default()
    });
    let client = pool.get("transport", None).unwrap();
    let mut stream = send(&client, &fixture, "/stall").await.bytes_stream();
    prefix(&mut stream).await;
    let error = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .expect("configured read timeout is still active")
        .expect("timeout error")
        .unwrap_err();
    assert!(error.is_timeout());
    assert_eq!(fixture.observations.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn negotiated_tls_still_requires_trusted_root_and_matching_hostname() {
    let fixture = Fixture::start(true).await;
    let untrusted = ClientPool::new().get("transport", None).unwrap();
    assert!(untrusted
        .post(fixture.url("/json"))
        .body(r#"{"stream":true}"#)
        .send()
        .await
        .is_err());
    let pool = fixture.pool(ClientTimeout::default());
    let trusted = pool.get("transport", None).unwrap();
    let wrong_name = format!("https://{}/json", fixture.address);
    assert!(trusted
        .post(wrong_name)
        .body(r#"{"stream":true}"#)
        .send()
        .await
        .is_err());
    assert!(fixture.observations.requests.lock().await.is_empty());
}

#[tokio::test]
async fn codex_preflight_preserves_negotiated_version_and_cancellation() {
    use crate::core::executor::generation_timing::GenerationTiming;
    use crate::core::executor::{CodexExecutionRequest, CodexExecutor, UpstreamResponse};
    use crate::types::{ProviderConnection, ProviderNode};

    for h2 in [true, false] {
        let fixture = Fixture::start(h2).await;
        let executor = CodexExecutor::new(
            Arc::new(fixture.pool(ClientTimeout::default())),
            Some(ProviderNode {
                base_url: Some(fixture.url("/cancel")),
                ..Default::default()
            }),
        )
        .unwrap();
        let timing = GenerationTiming::default();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            timing.scope(executor.execute(CodexExecutionRequest {
                model: "codex/gpt-fixture".into(),
                body: serde_json::json!({"stream":true,"input":[{"role":"user","content":[{"type":"input_text","text":"fixture"}]}]}),
                stream: true,
                credentials: ProviderConnection {
                    access_token: Some("fixture-token".into()),
                    ..Default::default()
                },
                proxy: None,
            })),
        )
        .await
        .expect("Codex first-event preflight must not await EOF")
        .unwrap();
        let prefix_timing = timing.prefix().unwrap();
        assert!(timing.started().unwrap() <= prefix_timing.last_read_at);
        assert!(prefix_timing.last_read_at <= std::time::Instant::now());
        assert!(prefix_timing.total_bytes >= TOOL.len() as u64);
        assert!(prefix_timing.total_bytes <= (TOOL.len() + USAGE.len()) as u64);
        let UpstreamResponse::Reqwest(response) = result.response else {
            panic!("Codex transport")
        };
        assert_eq!(
            response.version(),
            if h2 {
                Version::HTTP_2
            } else {
                Version::HTTP_11
            }
        );
        let mut stream = response.bytes_stream();
        prefix(&mut stream).await;
        assert_eq!(
            timing.prefix().unwrap().last_read_at,
            prefix_timing.last_read_at
        );
        drop(stream);
        tokio::time::timeout(
            Duration::from_secs(3),
            fixture.observations.cancel_dropped.notified(),
        )
        .await
        .expect("preflight replay chain propagates downstream cancellation");
        assert_eq!(fixture.observations.requests.lock().await.len(), 1);
    }
}
