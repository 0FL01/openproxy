use std::collections::VecDeque;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use bytes::Bytes;
use openproxy::db::Db;
use tempfile::TempDir;
use tokio::sync::{oneshot, Mutex, Notify};
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub headers: HeaderMap,
    pub body: Bytes,
}

#[derive(Clone, Debug)]
pub struct ScriptedResponse {
    pub status: StatusCode,
    pub headers: Vec<(String, String)>,
    pub chunks: Vec<Bytes>,
    pub wait_before_first: Option<Arc<Notify>>,
    pub hold_eof: Option<Arc<Notify>>,
    pub fail_after_chunks: bool,
}

impl ScriptedResponse {
    pub fn json(status: StatusCode, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            chunks: vec![body.into()],
            wait_before_first: None,
            hold_eof: None,
            fail_after_chunks: false,
        }
    }

    pub fn sse(chunks: impl IntoIterator<Item = impl Into<Bytes>>) -> Self {
        Self {
            status: StatusCode::OK,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            chunks: chunks.into_iter().map(Into::into).collect(),
            wait_before_first: None,
            hold_eof: None,
            fail_after_chunks: false,
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn waiting_for(mut self, release: Arc<Notify>) -> Self {
        self.wait_before_first = Some(release);
        self
    }

    pub fn holding_eof(mut self, release: Arc<Notify>) -> Self {
        self.hold_eof = Some(release);
        self
    }

    pub fn failing_after_chunks(mut self) -> Self {
        self.fail_after_chunks = true;
        self
    }
}

#[derive(Clone)]
struct MockState {
    scripts: Arc<Mutex<VecDeque<ScriptedResponse>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    request_arrived: Arc<Notify>,
}

pub struct MockUpstream {
    base_url: String,
    state: MockState,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl MockUpstream {
    pub async fn start(scripts: impl IntoIterator<Item = ScriptedResponse>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind deterministic mock upstream");
        let address = listener.local_addr().expect("mock upstream address");
        let state = MockState {
            scripts: Arc::new(Mutex::new(scripts.into_iter().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
            request_arrived: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .fallback(any(mock_handler))
            .with_state(state.clone());
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("serve deterministic mock upstream");
        });

        Self {
            base_url: format!("http://{address}"),
            state,
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub async fn wait_for_requests(&self, expected: usize) {
        loop {
            if self.state.requests.lock().await.len() >= expected {
                return;
            }
            self.state.request_arrived.notified().await;
        }
    }

    pub async fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.lock().await.clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.expect("join deterministic mock upstream");
        }
    }
}

impl Drop for MockUpstream {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn mock_handler(State(state): State<MockState>, request: Request<Body>) -> Response<Body> {
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 32 * 1024 * 1024)
        .await
        .expect("read mock request body");
    state
        .requests
        .lock()
        .await
        .push(RecordedRequest { headers, body });
    state.request_arrived.notify_waiters();

    let Some(script) = state.scripts.lock().await.pop_front() else {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from("mock response script exhausted"))
            .expect("script-exhausted response");
    };

    let stream = async_stream::stream! {
        if let Some(release) = script.wait_before_first {
            release.notified().await;
        }
        for chunk in script.chunks {
            yield Ok::<Bytes, std::io::Error>(chunk);
            tokio::task::yield_now().await;
        }
        if script.fail_after_chunks {
            yield Err(std::io::Error::other("scripted upstream body failure"));
        }
        if let Some(release) = script.hold_eof {
            release.notified().await;
        }
    };

    let mut response = Response::builder().status(script.status);
    for (name, value) in script.headers {
        response = response.header(name, value);
    }
    response
        .body(Body::from_stream(stream))
        .expect("scripted response")
}

pub struct TempTestDb {
    pub db: Arc<Db>,
    _directory: TempDir,
}

impl TempTestDb {
    pub async fn new() -> Self {
        let directory = tempfile::tempdir().expect("lean harness tempdir");
        let db = Arc::new(
            Db::load_from(directory.path())
                .await
                .expect("lean harness db"),
        );
        Self {
            db,
            _directory: directory,
        }
    }
}
