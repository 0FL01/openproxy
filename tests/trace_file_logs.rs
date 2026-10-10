//! Real-main coverage for the independent, always-on stream TRACE file sink.
//! All provider traffic and persistent state belong to local, disposable fixtures.

mod common;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::core::tls::ensure_rustls_provider;
use openproxy::db::sqlite::repo::request_repo::{self, RequestDetailFilter, RequestDetailRow};
use openproxy::server::trace_logs::{accepts_stream_trace, trace_log_channel};
use openproxy::types::{ProviderConnection, ProviderNode};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tracing_subscriber::{filter::filter_fn, layer::SubscriberExt, Layer};

const STREAM_TARGET: &str = "openproxy::chat::stream";
const PROVIDER_KEY: &str = "claude-surfacing-key";
const PROMPT_MARKER: &str = "private-trace-fixture-prompt";
const HEADER_MARKER: &str = "private-trace-fixture-header";
const RESPONSE_MARKER: &str = "private-trace-fixture-output";
const THINKING_MARKER: &str = "private-trace-fixture-thinking";
const WAIT_BUDGET: Duration = Duration::from_secs(10);

fn complete_claude_stream() -> ScriptedResponse {
    ScriptedResponse::sse([
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_surf\",\"model\":\"claude-opus-5-5\",\"role\":\"assistant\",\"usage\":{\"input_tokens\":3}}}\n\n".to_owned(),
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n".to_owned(),
        format!("event: content_block_delta\ndata: {}\n\n", json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":THINKING_MARKER}})),
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_owned(),
        format!("event: content_block_delta\ndata: {}\n\n", json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":RESPONSE_MARKER}})),
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}\n\n".to_owned(),
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_owned(),
    ])
    .with_header("x-trace-fixture-private", HEADER_MARKER)
}

async fn claude_test_db(upstream: &MockUpstream) -> Arc<TempTestDb> {
    let test_db = Arc::new(TempTestDb::new().await);
    // Match claude_stream_error_surfacing::app_for: this node routes only to
    // the mock Messages endpoint and uses an unmistakably fake account key.
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![ProviderNode {
                id: "claude".into(),
                r#type: "anthropic-compatible".into(),
                name: "Claude surfacing upstream".into(),
                prefix: Some("claude".into()),
                api_type: Some("messages".into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            db.provider_connections = vec![ProviderConnection {
                id: "claude-surfacing-account".into(),
                provider: "claude".into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                priority: Some(1),
                api_key: Some(PROVIDER_KEY.into()),
                default_model: Some("claude-opus-5-5".into()),
                ..Default::default()
            }];
        })
        .await
        .expect("seed isolated Claude fixture");
    test_db
}

struct BinaryServer {
    child: Child,
    client: reqwest::Client,
    base_url: String,
    // Retain the directory until after child cleanup, including on panic.
    test_db: Arc<TempTestDb>,
}

impl BinaryServer {
    async fn start(test_db: Arc<TempTestDb>, console_filter: &str) -> Self {
        ensure_rustls_provider();
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve ephemeral port");
        let port = listener.local_addr().expect("ephemeral address").port();
        assert_ne!(port, 4623, "fixture must not use the production port");
        drop(listener);
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_millis(500))
            .timeout(WAIT_BUDGET)
            .build()
            .expect("fixture HTTP client");
        let child = Command::new(env!("CARGO_BIN_EXE_openproxy"))
            .arg("--no-open")
            .arg("--data-dir")
            .arg(test_db.path())
            .env_clear()
            .env("HOME", test_db.path())
            .env("HOSTNAME", "127.0.0.1")
            .env("PORT", port.to_string())
            .env("DATA_DIR", test_db.path())
            .env("RUST_LOG", console_filter)
            .env("TOKIO_WORKER_THREADS", "2")
            .env("DISABLE_AUTO_BACKUP", "1")
            .env("OPENPROXY_REQUEST_LOG_MODE", "durable")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn isolated openproxy binary");
        let mut server = Self {
            child,
            client,
            base_url: format!("http://127.0.0.1:{port}"),
            test_db,
        };
        let deadline = Instant::now() + WAIT_BUDGET;
        loop {
            server.assert_running();
            if let Ok(response) = server
                .client
                .get(format!("{}/health", server.base_url))
                .send()
                .await
            {
                if response.status() == StatusCode::OK {
                    return server;
                }
            }
            assert!(
                Instant::now() < deadline,
                "fixture server readiness timeout"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn assert_running(&mut self) {
        assert!(
            self.child
                .try_wait()
                .expect("check fixture process")
                .is_none(),
            "fixture server exited unexpectedly (stdio intentionally private)"
        );
    }

    fn active_trace_path(&self) -> PathBuf {
        self.test_db.path().join("logs/stream-trace/active.jsonl")
    }

    async fn complete_responses_request(&self) -> Vec<Value> {
        let response = self
            .client
            .post(format!("{}/v1/responses", self.base_url))
            .bearer_auth("test-key")
            .header("x-openproxy-claude-mask", "1")
            .header("x-trace-fixture-private", HEADER_MARKER)
            .json(&json!({
                "model": "claude/claude-opus-5-5",
                "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":PROMPT_MARKER}]}],
                "stream": true
            }))
            .send()
            .await
            .expect("fixture Responses request");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let body = response.text().await.expect("consume complete SSE body");
        let mut events = parse_responses_sse(&body);
        // Only wall-clock creation timestamps vary. All event names, sequence
        // numbers, item IDs, payloads and terminal placement remain pinned.
        for event in &mut events {
            if let Some(created_at) = event.pointer_mut("/data/response/created_at") {
                assert!(created_at.as_i64().is_some_and(|timestamp| timestamp > 0));
                *created_at = json!(0);
            }
        }
        assert_eq!(events, expected_responses_projection());
        events
    }

    async fn authenticated_json(&self, path: &str) -> Value {
        let response = self
            .client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth("test-key")
            .send()
            .await
            .expect("authenticated fixture GET");
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.expect("fixture JSON payload")
    }

    async fn stats(&self) -> Value {
        let stats = self.authenticated_json("/api/observability/stats").await;
        for key in [
            "logBufferLines",
            "requestLogDropped",
            "requestLogQueuedBytes",
            "sqliteWalBytes",
            "dataDirAvailBytes",
            "traceLogDropped",
            "traceLogIoErrors",
        ] {
            assert!(stats[key].as_u64().is_some(), "integer stats field {key}");
        }
        for level in ["info", "warn", "error", "debug", "trace"] {
            assert!(stats["levels"][level].as_u64().is_some());
        }
        assert_eq!(stats["requestLogDropped"], 0);
        assert_eq!(stats["requestLogQueuedBytes"], 0);
        stats
    }

    async fn assert_stats_auth_required(&self) {
        let response = self
            .client
            .get(format!("{}/api/observability/stats", self.base_url))
            .send()
            .await
            .expect("unauthenticated stats request");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    async fn assert_console_does_not_contain_trace(&self, console_filter: &str) {
        let payload = self.authenticated_json("/api/observability/logs").await;
        let lines = payload["logs"].as_array().expect("console log lines");
        if console_filter == "off" {
            assert!(
                lines.is_empty(),
                "off must remain effective on the console layer"
            );
        } else {
            assert!(
                !lines.is_empty(),
                "info console layer should remain enabled"
            );
        }
        assert!(lines.iter().all(|line| {
            let line = line.as_str().expect("console log string");
            !line.contains("stream_start") && !line.contains("stream_end")
        }));
    }

    async fn stop(&mut self) {
        if self
            .child
            .try_wait()
            .expect("check child before cleanup")
            .is_none()
        {
            self.child.kill().expect("kill fixture child");
            self.wait_for_exit().await;
        }
    }

    async fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + WAIT_BUDGET;
        while self.child.try_wait().expect("reap fixture child").is_none() {
            assert!(Instant::now() < deadline, "fixture child exit timeout");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[cfg(unix)]
    async fn terminate(&mut self) {
        // This checks append persistence of already-written records, not queue
        // draining on a signal or any change to server shutdown semantics.
        let pid = i32::try_from(self.child.id()).expect("fixture child PID");
        // SAFETY: the PID belongs to the live child retained by this handle.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        self.wait_for_exit().await;
    }
}

impl Drop for BinaryServer {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn parse_responses_sse(body: &str) -> Vec<Value> {
    assert!(
        body.ends_with("\n\n"),
        "SSE must end at a complete frame boundary"
    );
    body.split("\n\n")
        .filter(|frame| !frame.is_empty())
        .map(|frame| {
            let mut lines = frame.lines();
            let event = lines
                .next()
                .and_then(|line| line.strip_prefix("event: "))
                .expect("Responses event line");
            let data: Value = serde_json::from_str(
                lines
                    .next()
                    .and_then(|line| line.strip_prefix("data: "))
                    .expect("Responses data line"),
            )
            .expect("Responses event JSON");
            assert!(lines.next().is_none(), "unexpected SSE frame fields");
            assert_eq!(data["type"], event);
            json!({"event":event,"data":data})
        })
        .collect()
}

fn expected_responses_projection() -> Vec<Value> {
    let response_id = "resp_chatcmpl-msg_surf";
    let reasoning_id = "rs_resp_chatcmpl-msg_surf_0";
    let message_id = "msg_resp_chatcmpl-msg_surf_0";
    let summary = json!({"type":"summary_text","text":THINKING_MARKER});
    let text = json!({"type":"output_text","annotations":[],"logprobs":[],"text":RESPONSE_MARKER});
    let mut events = vec![
        json!({"type":"response.created","response":{"id":response_id,"object":"response","created_at":0,"model":"claude-opus-5-5","status":"in_progress","background":false,"error":null,"service_tier":null,"output":[]}}),
        json!({"type":"response.in_progress","response":{"id":response_id,"object":"response","created_at":0,"status":"in_progress"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"id":reasoning_id,"type":"reasoning","summary":[]}}),
        json!({"type":"response.reasoning_summary_part.added","item_id":reasoning_id,"output_index":0,"summary_index":0,"part":{"type":"summary_text","text":""}}),
        json!({"type":"response.reasoning_summary_text.delta","item_id":reasoning_id,"output_index":0,"summary_index":0,"delta":THINKING_MARKER}),
        json!({"type":"response.reasoning_summary_text.done","item_id":reasoning_id,"output_index":0,"summary_index":0,"text":THINKING_MARKER}),
        json!({"type":"response.reasoning_summary_part.done","item_id":reasoning_id,"output_index":0,"summary_index":0,"part":summary}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"id":reasoning_id,"type":"reasoning","summary":[summary]}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"id":message_id,"type":"message","content":[],"role":"assistant"}}),
        json!({"type":"response.content_part.added","item_id":message_id,"output_index":0,"content_index":0,"part":{"type":"output_text","annotations":[],"logprobs":[],"text":""}}),
        json!({"type":"response.output_text.delta","item_id":message_id,"output_index":0,"content_index":0,"delta":RESPONSE_MARKER,"logprobs":[]}),
        json!({"type":"response.output_text.done","item_id":message_id,"output_index":0,"content_index":0,"text":RESPONSE_MARKER,"logprobs":[]}),
        json!({"type":"response.content_part.done","item_id":message_id,"output_index":0,"content_index":0,"part":text}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"id":message_id,"type":"message","content":[text],"role":"assistant"}}),
        json!({"type":"response.completed","response":{"id":response_id,"object":"response","created_at":0,"status":"completed","background":false,"error":null,"incomplete_details":null,"service_tier":null,"usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":0},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":15}}}),
    ];
    for (index, event) in events.iter_mut().enumerate() {
        event["sequence_number"] = json!(index + 1);
    }
    events
        .into_iter()
        .map(|data| json!({"event":data["type"],"data":data}))
        .collect()
}

fn parse_trace_jsonl(bytes: &[u8]) -> Vec<Value> {
    assert_eq!(bytes.last(), Some(&b'\n'), "complete JSONL record boundary");
    std::str::from_utf8(bytes)
        .expect("UTF-8 trace log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("trace JSONL record"))
        .collect()
}

async fn wait_for_trace_streams(path: &Path, expected: usize) -> Vec<u8> {
    let deadline = Instant::now() + WAIT_BUDGET;
    loop {
        match std::fs::read(path) {
            Ok(bytes) if bytes.last() == Some(&b'\n') => {
                let events = parse_trace_jsonl(&bytes);
                let count = |message: &str| {
                    events
                        .iter()
                        .filter(|event| event["fields"]["message"] == message)
                        .count()
                };
                if count("stream_start") == expected && count("stream_end") == expected {
                    return bytes;
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("read fixture trace log: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "complete file trace events timeout"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn assert_private_markers_absent(bytes: &[u8]) {
    let output = std::str::from_utf8(bytes).expect("UTF-8 trace content");
    for marker in [
        PROMPT_MARKER,
        HEADER_MARKER,
        RESPONSE_MARKER,
        THINKING_MARKER,
        PROVIDER_KEY,
        "test-key",
        "authorization",
        "x-api-key",
        "x-trace-fixture-private",
        "fixture-span-private",
    ] {
        assert!(
            !output.contains(marker),
            "private fixture marker leaked to TRACE file"
        );
    }
}

fn assert_claude_file_trace(bytes: &[u8], expected_streams: usize) {
    assert_private_markers_absent(bytes);
    let events = parse_trace_jsonl(bytes);
    assert_eq!(
        events.len(),
        expected_streams * 2,
        "start/end only for translated Claude"
    );
    let mut stream_ids = std::collections::HashSet::new();
    for pair in events.chunks_exact(2) {
        for event in pair {
            assert_eq!(event["target"], STREAM_TARGET);
            assert_eq!(event["level"], "TRACE");
            assert!(event.get("span").is_none());
            assert!(event.get("spans").is_none());
            assert!(event["fields"].is_object());
            for name in ["headers", "body", "request", "response", "prompt"] {
                assert!(event.get(name).is_none());
                assert!(event["fields"].get(name).is_none());
            }
        }
        assert_eq!(pair[0]["fields"]["message"], "stream_start");
        assert_eq!(pair[0]["fields"]["provider"], "claude");
        assert_eq!(pair[0]["fields"]["model"], "claude-opus-5-5");
        assert_eq!(pair[1]["fields"]["message"], "stream_end");
        assert_eq!(pair[1]["fields"]["reason"], "eof");
        let id = pair[0]["fields"]["stream_id"]
            .as_str()
            .expect("start stream ID");
        assert!(!id.is_empty());
        assert!(
            stream_ids.insert(id),
            "restart must create a fresh stream ID"
        );
        assert_eq!(pair[1]["fields"]["stream_id"], id);
    }
}

async fn request_details(test_db: &TempTestDb) -> Vec<RequestDetailRow> {
    let sqlite = test_db.db.sqlite.clone();
    tokio::task::spawn_blocking(move || {
        sqlite.with_conn(|conn| request_repo::list(conn, &RequestDetailFilter::default(), 10, 0))
    })
    .await
    .expect("join fixture requestDetails read")
    .expect("read fixture requestDetails")
}

async fn assert_successful_attempts(server: &BinaryServer, expected: usize) {
    let rows = request_details(&server.test_db).await;
    assert_eq!(
        rows.len(),
        expected,
        "one requestDetails row per provider attempt"
    );
    for row in rows {
        assert_eq!(row.provider.as_deref(), Some("claude"));
        assert_eq!(row.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(
            row.connection_id.as_deref(),
            Some("claude-surfacing-account")
        );
        assert_eq!(row.status.as_deref(), Some("success"));
        assert_eq!(row.data["route"], "/v1/responses");
        assert_eq!(row.data["statusCode"], 200);
        assert_eq!(row.data["inputTokens"], 10);
        assert_eq!(row.data["outputTokens"], 5);
        assert!(
            row.data
                .as_object()
                .expect("requestDetails object")
                .keys()
                .all(|key| {
                    matches!(
                        key.as_str(),
                        "route"
                            | "statusCode"
                            | "durationMs"
                            | "inputTokens"
                            | "outputTokens"
                            | "cachedTokens"
                            | "chatSession"
                            | "streamTrace"
                            | "upstreamTps"
                    )
                }),
            "runtime TRACE frames/counters must not add SQLite metadata"
        );
        let trace = &row.data["streamTrace"];
        assert!(
            trace
                .as_object()
                .expect("existing Claude trace object")
                .keys()
                .all(|key| {
                    matches!(
                        key.as_str(),
                        "version"
                            | "upstreamEvents"
                            | "emittedEvents"
                            | "itemTypes"
                            | "stopReason"
                            | "finishReason"
                            | "completedCount"
                            | "errorCount"
                            | "framesAfterCompleted"
                            | "doneSent"
                            | "entries"
                    )
                }),
            "runtime TRACE must not extend the existing Claude SQLite trace"
        );
        assert_eq!(trace["version"], 1);
        assert_eq!(trace["upstreamEvents"]["message_start"], 1);
        assert_eq!(trace["upstreamEvents"]["message_stop"], 1);
        assert_eq!(trace["upstreamEvents"]["content_block_start:thinking"], 1);
        assert_eq!(trace["itemTypes"]["reasoning"], 1);
        assert_eq!(trace["itemTypes"]["message"], 1);
        assert_eq!(trace["stopReason"], "end_turn");
        assert_eq!(trace["completedCount"], 1);
        assert_eq!(trace["errorCount"], 0);
        assert_eq!(trace["framesAfterCompleted"], 0);
        let entries = trace["entries"]
            .as_array()
            .expect("existing bounded Claude trace");
        assert!(entries.len() <= 64);
        assert!(entries.iter().all(|entry| {
            let entry = entry.as_str().expect("metadata trace entry");
            entry.chars().count() <= 64
                && !entry.contains("stream_start")
                && !entry.contains("stream_end")
        }));
        assert_private_markers_absent(row.data.to_string().as_bytes());
    }
    let logs = server
        .authenticated_json("/api/request-logs?page=1&pageSize=10")
        .await;
    let requests = logs["requests"].as_array().expect("public request logs");
    assert_eq!(requests.len(), expected);
    for log in requests {
        assert_eq!(log["statusCode"], 200);
        assert!(log.get("errorCode").is_none());
        assert!(log.get("errorMessage").is_none());
        assert_eq!(log["streamTrace"]["completedCount"], 1);
        assert_eq!(log["streamTrace"]["framesAfterCompleted"], 0);
    }
}

async fn assert_local_provider_request(upstream: &MockUpstream, expected: usize) {
    let requests = upstream.requests().await;
    assert_eq!(
        requests.len(),
        expected,
        "one fixture request per stream, no fallback"
    );
    for request in requests {
        assert_eq!(request.path, "/v1/messages");
        assert!(request.headers["x-api-key"] == PROVIDER_KEY);
        assert!(std::str::from_utf8(&request.body)
            .unwrap()
            .contains(PROMPT_MARKER));
    }
}

async fn always_on_binary_case(console_filter: &str) {
    let upstream = MockUpstream::start([complete_claude_stream()]).await;
    let test_db = claude_test_db(&upstream).await;
    let mut server = BinaryServer::start(test_db, console_filter).await;
    server.assert_stats_auth_required().await;
    server.complete_responses_request().await;
    let bytes = wait_for_trace_streams(&server.active_trace_path(), 1).await;
    assert_claude_file_trace(&bytes, 1);
    assert_successful_attempts(&server, 1).await;
    assert_local_provider_request(&upstream, 1).await;
    let stats = server.stats().await;
    assert_eq!(stats["traceLogDropped"], 0);
    assert_eq!(stats["traceLogIoErrors"], 0);
    server
        .assert_console_does_not_contain_trace(console_filter)
        .await;
    server.stop().await;
    upstream.shutdown().await;
}

#[tokio::test]
async fn real_main_stream_trace_file_is_always_on_with_console_info() {
    always_on_binary_case("info").await;
}

#[tokio::test]
async fn real_main_stream_trace_file_is_always_on_with_console_off() {
    always_on_binary_case("off").await;
}

#[tokio::test]
async fn unavailable_trace_sink_keeps_stream_and_sqlite_attempt_successful() {
    let upstream = MockUpstream::start([complete_claude_stream()]).await;
    let test_db = claude_test_db(&upstream).await;
    // A non-directory path component fails even when this test runs as root;
    // chmod-based permission tests do not reliably exercise that condition.
    std::fs::write(test_db.path().join("logs"), b"fixture sink obstruction")
        .expect("seed unavailable trace directory");
    let mut server = BinaryServer::start(test_db, "off").await;
    server.assert_stats_auth_required().await;
    server.complete_responses_request().await;
    assert_successful_attempts(&server, 1).await;
    assert_local_provider_request(&upstream, 1).await;
    let deadline = Instant::now() + WAIT_BUDGET;
    loop {
        let stats = server.stats().await;
        if stats["traceLogDropped"].as_u64().unwrap() >= 1
            && stats["traceLogIoErrors"].as_u64().unwrap() >= 1
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "failed trace writes must increment additive counters"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!server.active_trace_path().exists());
    server.assert_running();
    server.stop().await;
    upstream.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn real_main_restart_appends_after_already_persisted_trace_prefix() {
    let upstream = MockUpstream::start([complete_claude_stream(), complete_claude_stream()]).await;
    let test_db = claude_test_db(&upstream).await;
    let mut first = BinaryServer::start(test_db.clone(), "info").await;
    let first_wire = first.complete_responses_request().await;
    let prefix = wait_for_trace_streams(&first.active_trace_path(), 1).await;
    assert_claude_file_trace(&prefix, 1);
    assert_successful_attempts(&first, 1).await;
    first.terminate().await;

    let mut second = BinaryServer::start(test_db, "off").await;
    let second_wire = second.complete_responses_request().await;
    assert_eq!(first_wire, second_wire);
    let appended = wait_for_trace_streams(&second.active_trace_path(), 2).await;
    assert!(
        appended.starts_with(&prefix),
        "restart overwrote persisted trace bytes"
    );
    assert!(appended.len() > prefix.len());
    assert_claude_file_trace(&appended, 2);
    assert_successful_attempts(&second, 2).await;
    assert_local_provider_request(&upstream, 2).await;
    let stats = second.stats().await;
    assert_eq!(stats["traceLogDropped"], 0);
    assert_eq!(stats["traceLogIoErrors"], 0);
    second.stop().await;
    upstream.shutdown().await;
}

#[test]
fn scoped_file_layer_accepts_exact_trace_target_for_claude_and_codex_without_spans() {
    let directory = tempfile::tempdir().expect("scoped trace fixture directory");
    let (writer, mut guard) = trace_log_channel();
    let counters = guard.counters();
    guard
        .start(directory.path())
        .expect("start scoped trace worker");
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::sink)
                .with_filter(tracing_subscriber::EnvFilter::new("off")),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_ansi(false)
                .with_current_span(false)
                .with_span_list(false)
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NONE)
                .with_writer(writer)
                .with_filter(filter_fn(accepts_stream_trace)),
        );
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::trace_span!(target: "openproxy::chat::stream", "fixture-span-private", authorization = "fixture-span-private");
        let _entered = span.enter();
        for provider in ["claude", "codex"] {
            assert!(tracing::enabled!(target: "openproxy::chat::stream", tracing::Level::TRACE));
            tracing::trace!(target: "openproxy::chat::stream", provider, "stream_start");
            tracing::trace!(target: "openproxy::chat::stream", provider, "stream_end");
        }
        tracing::debug!(target: "openproxy::chat::stream", "excluded-debug");
        tracing::info!(target: "openproxy::chat::stream", "excluded-info");
        tracing::warn!(target: "openproxy::chat::stream", "excluded-warn");
        tracing::error!(target: "openproxy::chat::stream", "excluded-error");
        tracing::trace!(target: "openproxy::chat::stream::child", "excluded-child-target");
        tracing::trace!(target: "openproxy::chat::stream_extra", "excluded-prefix-target");
        tracing::trace!(target: "other::stream", "excluded-unrelated-target");
    });
    assert!(
        guard.shutdown_with_budget(WAIT_BUDGET),
        "scoped guard did not drain"
    );
    assert_eq!(counters.dropped(), 0);
    assert_eq!(counters.io_errors(), 0);
    let bytes = std::fs::read(directory.path().join("logs/stream-trace/active.jsonl"))
        .expect("read drained scoped TRACE file");
    assert_private_markers_absent(&bytes);
    let events = parse_trace_jsonl(&bytes);
    assert_eq!(
        events.len(),
        4,
        "only exact-target TRACE events are accepted"
    );
    for (event, (provider, message)) in events.iter().zip([
        ("claude", "stream_start"),
        ("claude", "stream_end"),
        ("codex", "stream_start"),
        ("codex", "stream_end"),
    ]) {
        assert_eq!(event["target"], STREAM_TARGET);
        assert_eq!(event["level"], "TRACE");
        assert_eq!(event["fields"]["provider"], provider);
        assert_eq!(event["fields"]["message"], message);
        assert!(event.get("span").is_none());
        assert!(event.get("spans").is_none());
    }
}
