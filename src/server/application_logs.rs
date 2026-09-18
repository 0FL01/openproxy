//! Metadata-only logs for authenticated provider attempts.
//!
//! C35 logging contract: one bounded events-and-bytes pipeline feeds a single
//! background SQLite writer, so generation never spawns a task per log event.
//! `durable` mode (default) awaits the insert before upstream and awaits the
//! finish update afterwards — honest I/O/backpressure, no zero-wait claim.
//! `lean` mode (`OPENPROXY_REQUEST_LOG_MODE=lean`) only enqueues metadata
//! without waiting: the first upstream byte never waits for SQLite, a full
//! queue increments an explicit drop counter instead of growing, and the last
//! queued entries may be lost on crash/shutdown past the flush budget.
//! Config/credential persistence paths are untouched and always durable.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Lean logging bounds: one shared pipeline, bounded by events AND bytes.
pub const LEAN_LOG_QUEUE_EVENTS: usize = 512;
pub const LEAN_LOG_QUEUE_BYTES: usize = 512 * 1024;
pub const LEAN_LOG_MAX_EVENT_BYTES: usize = 8 * 1024;
const LEAN_LOG_FIELD_BYTES: usize = 256;

/// Request-log durability mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestLogMode {
    Lean,
    Durable,
}

/// Resolve the log mode. Defaults to `durable` so existing guarantees are
/// preserved unless lean is explicitly opted in; migration is never silent.
pub fn request_log_mode() -> RequestLogMode {
    match std::env::var("OPENPROXY_REQUEST_LOG_MODE") {
        Ok(value) if value.eq_ignore_ascii_case("lean") => RequestLogMode::Lean,
        _ => RequestLogMode::Durable,
    }
}

/// (queue events, queue bytes, max event bytes) for contracts/tests.
pub fn request_log_bounds() -> (usize, usize, usize) {
    (
        LEAN_LOG_QUEUE_EVENTS,
        LEAN_LOG_QUEUE_BYTES,
        LEAN_LOG_MAX_EVENT_BYTES,
    )
}

/// Number of lean log operations dropped on overflow since process start.
pub fn request_log_dropped() -> u64 {
    log_service().dropped.load(Ordering::Acquire)
}

/// Last epoch-millis a log-write-failure warning was emitted.
/// Throttles degraded-mode warns: under sustained ENOSPC an unthrottled
/// per-failure warn would spam the same full disk.
static LOG_WRITE_WARN_AT_MS: AtomicU64 = AtomicU64::new(0);
const LOG_WRITE_WARN_INTERVAL_MS: u64 = 60_000;

/// Count one lost log write (any pipeline path: lean overflow already
/// counts at the call site; durable insert, background write, and finish
/// failures call this). Serving continues unlogged (fail-open).
pub fn count_log_write_failed() {
    log_service().dropped.fetch_add(1, Ordering::AcqRel);
}

/// Throttled degraded-mode warning. Call after `count_log_write_failed`.
pub fn warn_log_write_failed(site: &'static str) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let last = LOG_WRITE_WARN_AT_MS.load(Ordering::Acquire);
    if now_ms.saturating_sub(last) < LOG_WRITE_WARN_INTERVAL_MS {
        return;
    }
    if LOG_WRITE_WARN_AT_MS
        .compare_exchange(last, now_ms, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    tracing::warn!(
        target: "openproxy::logs",
        site,
        dropped = request_log_dropped(),
        "request log writes are failing; serving requests unlogged"
    );
}

/// Count one lost log write and emit a throttled warning.
pub fn note_log_write_failed(site: &'static str) {
    count_log_write_failed();
    warn_log_write_failed(site);
}

/// WAL file size in bytes (0 when absent). On-read gauge for the
/// auth-gated observability stats endpoint; no background polling.
pub fn sqlite_wal_bytes(data_dir: &std::path::Path) -> u64 {
    std::fs::metadata(data_dir.join("openproxy.sqlite-wal"))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// Free bytes available to unprivileged writers on `data_dir`'s filesystem
/// (`None` when the query itself fails). Read-only `statvfs`, no new crates.
pub fn data_dir_avail_bytes(data_dir: &std::path::Path) -> Option<u64> {
    let path = std::ffi::CString::new(data_dir.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `statvfs` only reads the path; `stat` is a valid zeroed struct.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// One-line boot preflight for the data directory: an `ERROR` when free
/// space drops below 1 GiB. Non-fatal courtesy check — a disk can still
/// fill at runtime (see the stats gauges); never refuses to boot.
pub fn log_disk_preflight(data_dir: &std::path::Path) {
    const MIN_AVAIL_BYTES: u64 = 1024 * 1024 * 1024;
    match data_dir_avail_bytes(data_dir) {
        Some(avail) if avail < MIN_AVAIL_BYTES => {
            tracing::error!(
                target: "openproxy::logs",
                avail_bytes = avail,
                "data directory disk space critically low"
            );
        }
        _ => {}
    }
}

/// Currently queued (not yet written) lean log bytes.
pub fn request_log_queued_bytes() -> usize {
    log_service().queued_bytes.load(Ordering::Acquire)
}

/// Wait until the lean queue drains or `budget` elapses. Returns true when
/// drained. Crash/shutdown callers must pass an explicit budget and treat
/// `false` as honest loss, never as silent success.
pub async fn request_log_flush_with_budget(budget: Duration) -> bool {
    let service = log_service();
    let start = Instant::now();
    loop {
        if service.queued_bytes.load(Ordering::Acquire) == 0 {
            return true;
        }
        if start.elapsed() >= budget {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn truncate_field(value: &str) -> String {
    if value.len() <= LEAN_LOG_FIELD_BYTES {
        return value.to_string();
    }
    let mut end = LEAN_LOG_FIELD_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

enum LogOpKind {
    Insert {
        id: String,
        timestamp: String,
        provider: String,
        model: String,
        api_key_id: String,
        api_key_name: String,
        data: Value,
    },
    Finish {
        id: String,
        status: String,
        data: Value,
    },
}

struct LogOp {
    db: Arc<Db>,
    kind: LogOpKind,
    bytes: usize,
}

fn op_bytes(kind: &LogOpKind) -> usize {
    match kind {
        LogOpKind::Insert {
            id,
            timestamp,
            provider,
            model,
            api_key_id,
            api_key_name,
            data,
        } => {
            id.len()
                + timestamp.len()
                + provider.len()
                + model.len()
                + api_key_id.len()
                + api_key_name.len()
                + serde_json::to_string(data).map(|s| s.len()).unwrap_or(0)
        }
        LogOpKind::Finish { id, status, data } => {
            id.len() + status.len() + serde_json::to_string(data).map(|s| s.len()).unwrap_or(0)
        }
    }
}

struct LogService {
    tx: tokio::sync::mpsc::Sender<LogOp>,
    rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<LogOp>>>,
    writer_started: AtomicBool,
    dropped: AtomicU64,
    queued_bytes: AtomicUsize,
}

fn log_service() -> &'static LogService {
    static SERVICE: OnceLock<LogService> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let (tx, rx) = tokio::sync::mpsc::channel::<LogOp>(LEAN_LOG_QUEUE_EVENTS);
        LogService {
            tx,
            rx: std::sync::Mutex::new(Some(rx)),
            writer_started: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            queued_bytes: AtomicUsize::new(0),
        }
    })
}

/// Ensure exactly one background writer consumes the queue. The writer is a
/// dedicated OS thread (not a task on the caller's runtime), so it survives
/// short-lived runtimes — e.g. one `#[tokio::test]` current-thread runtime
/// per test — and the channel is never closed while the process lives.
/// Exactly-once spawn keeps start/finish order intact.
fn ensure_log_writer() {
    let service = log_service();
    if service.writer_started.load(Ordering::Acquire) {
        return;
    }
    // Only one caller wins the flag and takes the receiver, so exactly one
    // consumer ever exists and start/finish order is preserved.
    if service
        .writer_started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let rx = service.rx.lock().ok().and_then(|mut guard| guard.take());
    if let Some(rx) = rx {
        std::thread::Builder::new()
            .name("openproxy-request-log-writer".to_string())
            .spawn(move || log_writer_loop(rx))
            .expect("request log writer thread must spawn");
    }
}

fn log_writer_loop(mut rx: tokio::sync::mpsc::Receiver<LogOp>) {
    // `blocking_recv` parks this dedicated thread; it must never run on an
    // async worker. Senders live in the process-global service, so this loop
    // only ends at process exit.
    while let Some(op) = rx.blocking_recv() {
        let bytes = op.bytes;
        write_op_sync(op);
        log_service().queued_bytes.fetch_sub(
            bytes.min(LEAN_LOG_QUEUE_BYTES + LEAN_LOG_MAX_EVENT_BYTES),
            Ordering::AcqRel,
        );
    }
}

/// Synchronous SQLite write for the dedicated writer thread (which has no
/// async runtime, so no `spawn_blocking` here). `with_conn` serializes on the
/// connection mutex; a slow database only slows this single writer, while
/// lean producers keep their non-blocking enqueue contract.
fn write_op_sync(op: LogOp) {
    let result: rusqlite::Result<()> = match op.kind {
        LogOpKind::Insert {
            id,
            timestamp,
            provider,
            model,
            api_key_id,
            api_key_name,
            data,
        } => op.db.sqlite.with_conn(|conn| {
            request_repo::insert(
                conn,
                &NewRequestDetail {
                    id: &id,
                    timestamp: &timestamp,
                    provider: Some(&provider),
                    model: Some(&model),
                    connection_id: None,
                    status: "pending",
                    api_key_id: Some(&api_key_id),
                    api_key_name: Some(&api_key_name),
                    correlation_id: None,
                    data: &data,
                },
            )
        }),
        LogOpKind::Finish { id, status, data } => op
            .db
            .sqlite
            .with_conn(|conn| request_repo::finish(conn, &id, &status, &data))
            .map(|_| ()),
    };
    if let Err(error) = result {
        tracing::warn!(target: "openproxy::logs", %error, "background request log write failed");
        note_log_write_failed("background-write");
    }
}

/// Non-blocking bounded enqueue shared by every lean log path. Returns false
/// (and counts one drop) when the event exceeds the per-event cap or the
/// events/bytes pipeline is full. Never blocks, never spawns per-event tasks.
fn try_enqueue_lean(db: Arc<Db>, kind: LogOpKind) -> bool {
    ensure_log_writer();
    let bytes = op_bytes(&kind);
    if bytes > LEAN_LOG_MAX_EVENT_BYTES {
        log_service().dropped.fetch_add(1, Ordering::AcqRel);
        warn_log_write_failed("oversized-event");
        return false;
    }
    let service = log_service();
    // Reserve bytes first with a CAS loop so concurrent enqueuers cannot
    // jointly overshoot the byte cap.
    loop {
        let queued = service.queued_bytes.load(Ordering::Acquire);
        if queued.saturating_add(bytes) > LEAN_LOG_QUEUE_BYTES {
            service.dropped.fetch_add(1, Ordering::AcqRel);
            warn_log_write_failed("queue-bytes-full");
            return false;
        }
        if service
            .queued_bytes
            .compare_exchange(queued, queued + bytes, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            break;
        }
    }
    match service.tx.try_send(LogOp { db, kind, bytes }) {
        Ok(()) => true,
        Err(_) => {
            service.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
            service.dropped.fetch_add(1, Ordering::AcqRel);
            warn_log_write_failed("queue-events-full");
            false
        }
    }
}

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing, Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::db::sqlite::repo::request_repo::{self, NewRequestDetail};
use crate::db::Db;
use crate::server::state::AppState;
use crate::types::{ApiKey, TokenUsage};

#[derive(Clone)]
pub struct RequestLogContext {
    db: Arc<Db>,
    route: String,
    api_key_id: String,
    api_key_name: String,
    mode: RequestLogMode,
}

impl RequestLogContext {
    pub fn new(db: Arc<Db>, api_key: &ApiKey, route: &str) -> Self {
        Self::new_with_mode(db, api_key, route, request_log_mode())
    }

    pub fn new_with_mode(db: Arc<Db>, api_key: &ApiKey, route: &str, mode: RequestLogMode) -> Self {
        Self {
            db,
            route: truncate_field(route),
            api_key_id: truncate_field(&api_key.id),
            api_key_name: truncate_field(&api_key.name),
            mode,
        }
    }

    pub fn mode(&self) -> RequestLogMode {
        self.mode
    }

    fn start_data(&self) -> Value {
        json!({
            "route": self.route,
            "statusCode": Value::Null,
            "durationMs": 0,
            "inputTokens": Value::Null,
            "outputTokens": Value::Null,
            "cachedTokens": Value::Null,
        })
    }

    pub async fn start_attempt(&self, provider: &str, model: &str) -> Option<AttemptLog> {
        let provider = truncate_field(provider);
        let model = truncate_field(model);
        let id = uuid::Uuid::new_v4().to_string();
        let timestamp = Utc::now().to_rfc3339();
        let data = self.start_data();
        match self.mode {
            // Lean: enqueue only. Never waits for SQLite, so the first
            // upstream byte cannot be gated on audit I/O. Overflow drops with
            // an explicit counter instead of growing.
            RequestLogMode::Lean => {
                let enqueued = try_enqueue_lean(
                    self.db.clone(),
                    LogOpKind::Insert {
                        id: id.clone(),
                        timestamp,
                        provider,
                        model,
                        api_key_id: self.api_key_id.clone(),
                        api_key_name: self.api_key_name.clone(),
                        data: data.clone(),
                    },
                );
                if !enqueued {
                    return None;
                }
                Some(AttemptLog {
                    db: self.db.clone(),
                    id,
                    started: Instant::now(),
                    data,
                    finished: Arc::new(AtomicBool::new(false)),
                    lean: true,
                })
            }
            // Durable: synchronous insert before upstream with backpressure.
            // Honest I/O cost, never advertised as zero-wait.
            RequestLogMode::Durable => {
                let sqlite = self.db.sqlite.clone();
                let record_id = id.clone();
                let record_timestamp = timestamp.clone();
                let record_provider = provider.clone();
                let record_model = model.clone();
                let api_key_id = self.api_key_id.clone();
                let api_key_name = self.api_key_name.clone();
                let record_data = data.clone();
                let inserted = tokio::task::spawn_blocking(move || {
                    sqlite.with_conn(|conn| {
                        request_repo::insert(
                            conn,
                            &NewRequestDetail {
                                id: &record_id,
                                timestamp: &record_timestamp,
                                provider: Some(&record_provider),
                                model: Some(&record_model),
                                connection_id: None,
                                status: "pending",
                                api_key_id: Some(&api_key_id),
                                api_key_name: Some(&api_key_name),
                                correlation_id: None,
                                data: &record_data,
                            },
                        )
                    })
                })
                .await;

                match inserted {
                    Ok(Ok(())) => Some(AttemptLog {
                        db: self.db.clone(),
                        id,
                        started: Instant::now(),
                        data,
                        finished: Arc::new(AtomicBool::new(false)),
                        lean: false,
                    }),
                    Ok(Err(error)) => {
                        tracing::warn!(target: "openproxy::logs", %error, "failed to start request log");
                        note_log_write_failed("durable-insert");
                        None
                    }
                    Err(error) => {
                        tracing::warn!(target: "openproxy::logs", %error, "request log task failed");
                        note_log_write_failed("durable-insert-task");
                        None
                    }
                }
            }
        }
    }
}

/// Closed error-cause vocabulary for failed attempt rows (C43).
/// Metadata only: fixed enum strings, never free text, never bodies.
/// Passed as an explicit literal at each `finish("error")` branch —
/// never inferred from status codes.
pub mod error_kind {
    /// Upstream rejected credentials or the token is dead (401/403,
    /// refreshable OAuth failure observed on the attempt).
    pub const AUTH_FAILURE: &str = "auth_failure";
    /// Upstream or proxy rate limit (429).
    pub const RATE_LIMITED: &str = "rate_limited";
    /// Client/upstream rejected the request shape (400/413/422).
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// Upstream transport error, 5xx, stall, or non-SSE garbage.
    pub const UPSTREAM_FAILURE: &str = "upstream_failure";
    /// Local failure: translation, framing, body collection/stream limits.
    pub const LOCAL_FAILURE: &str = "local_failure";
}

pub struct AttemptLog {
    db: Arc<Db>,
    id: String,
    started: Instant,
    data: Value,
    finished: Arc<AtomicBool>,
    lean: bool,
}

impl AttemptLog {
    pub async fn finish(
        self,
        status: &'static str,
        status_code: Option<u16>,
        tokens: Option<&TokenUsage>,
        error_kind: Option<&'static str>,
    ) {
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        let data = self.finished_data(status_code, tokens, error_kind);
        if self.lean {
            // Lean: enqueue the finish without waiting for SQLite.
            try_enqueue_lean(
                self.db.clone(),
                LogOpKind::Finish {
                    id: self.id.clone(),
                    status: status.to_string(),
                    data,
                },
            );
        } else {
            persist_finish(self.db.clone(), self.id.clone(), status, data).await;
        }
        self.finished.store(true, Ordering::Release);
    }

    fn finished_data(
        &self,
        status_code: Option<u16>,
        tokens: Option<&TokenUsage>,
        error_kind: Option<&'static str>,
    ) -> Value {
        let mut data = self.data.as_object().cloned().unwrap_or_else(Map::new);
        data.insert("statusCode".into(), json!(status_code));
        if let Some(kind) = error_kind {
            data.insert("errorKind".into(), json!(kind));
        }
        data.insert(
            "durationMs".into(),
            json!(self.started.elapsed().as_millis()),
        );
        data.insert(
            "inputTokens".into(),
            json!(tokens.and_then(|tokens| tokens.prompt_tokens.or(tokens.input_tokens))),
        );
        data.insert(
            "outputTokens".into(),
            json!(tokens.and_then(|tokens| tokens.completion_tokens.or(tokens.output_tokens))),
        );
        data.insert(
            "cachedTokens".into(),
            json!(tokens.and_then(|tokens| tokens.cached_tokens.or(tokens.cache_read_input_tokens))),
        );
        Value::Object(data)
    }
}

impl Drop for AttemptLog {
    fn drop(&mut self) {
        if self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        // C35: cancellation/interruption never spawns an unbounded detached
        // task. Enqueue best-effort through the same bounded pipeline in both
        // modes; overflow is counted explicitly instead of growing. The writer
        // is a runtime-independent thread, so no runtime check is needed.
        let data = self.finished_data(None, None, None);
        try_enqueue_lean(
            self.db.clone(),
            LogOpKind::Finish {
                id: self.id.clone(),
                status: "interrupted".to_string(),
                data,
            },
        );
    }
}

async fn persist_finish(db: Arc<Db>, id: String, status: &'static str, data: Value) {
    let sqlite = db.sqlite.clone();
    let result = tokio::task::spawn_blocking(move || {
        sqlite.with_conn(|conn| request_repo::finish(conn, &id, status, &data))
    })
    .await;
    match result {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            tracing::warn!(target: "openproxy::logs", "request log disappeared before finish");
            note_log_write_failed("finish-missing");
        }
        Ok(Err(error)) => {
            tracing::warn!(target: "openproxy::logs", %error, "failed to finish request log");
            note_log_write_failed("finish-write");
        }
        Err(error) => {
            tracing::warn!(target: "openproxy::logs", %error, "request log finish task failed");
            note_log_write_failed("finish-task");
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/request-logs", routing::get(get_request_logs))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogsQuery {
    page: Option<usize>,
    page_size: Option<usize>,
    provider: Option<String>,
    model: Option<String>,
    status: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogsPayload {
    requests: Vec<RequestLogRecord>,
    pagination: RequestLogsPagination,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogsPagination {
    page: usize,
    page_size: usize,
    total_items: usize,
    total_pages: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogRecord {
    request_id: String,
    timestamp: String,
    route: String,
    provider: String,
    model: String,
    status: String,
    status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_kind: Option<String>,
    duration_ms: u64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    api_key_id: Option<String>,
    api_key_name: Option<String>,
}

async fn get_request_logs(
    State(state): State<AppState>,
    Query(query): Query<RequestLogsQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) =
        crate::server::api::require_dashboard_or_management_api_key(&headers, &state)
    {
        return response;
    }

    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);
    if page == 0 || !(1..=100).contains(&page_size) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "page must be >= 1 and pageSize must be between 1 and 100" })),
        )
            .into_response();
    }

    let clean = |value: Option<String>| {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let provider = clean(query.provider);
    let model = clean(query.model);
    let status = clean(query.status);
    let start_date = query.start_date.as_deref().and_then(parse_timestamp);
    let end_date = query.end_date.as_deref().and_then(parse_timestamp);
    let offset = (page - 1) * page_size;
    let sqlite = state.db.sqlite.clone();
    let result = tokio::task::spawn_blocking(move || {
        sqlite.with_conn(|conn| {
            let filter = request_repo::RequestDetailFilter {
                provider: provider.as_deref(),
                model: model.as_deref(),
                status: status.as_deref(),
                start_date: start_date.as_deref(),
                end_date: end_date.as_deref(),
                ..Default::default()
            };
            let total = request_repo::count(conn, &filter)?;
            let rows = request_repo::list(conn, &filter, page_size, offset)?;
            Ok((total, rows))
        })
    })
    .await;

    let (total_items, rows) = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            tracing::error!(target: "openproxy::logs", %error, "failed to query request logs");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(error) => {
            tracing::error!(target: "openproxy::logs", %error, "request log query task failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let total_pages = total_items.div_ceil(page_size);
    Json(RequestLogsPayload {
        requests: rows.into_iter().map(request_log_from_row).collect(),
        pagination: RequestLogsPagination {
            page,
            page_size,
            total_items,
            total_pages,
        },
    })
    .into_response()
}

fn parse_timestamp(value: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc).to_rfc3339())
}

fn request_log_from_row(row: request_repo::RequestDetailRow) -> RequestLogRecord {
    RequestLogRecord {
        request_id: row.id,
        timestamp: row.timestamp,
        route: row
            .data
            .get("route")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        provider: row.provider.unwrap_or_default(),
        model: row.model.unwrap_or_default(),
        status: row.status.unwrap_or_else(|| "interrupted".to_string()),
        status_code: row
            .data
            .get("statusCode")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok()),
        error_kind: row
            .data
            .get("errorKind")
            .and_then(Value::as_str)
            .map(str::to_string),
        duration_ms: row
            .data
            .get("durationMs")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        input_tokens: row.data.get("inputTokens").and_then(Value::as_u64),
        output_tokens: row.data.get("outputTokens").and_then(Value::as_u64),
        cached_tokens: row.data.get("cachedTokens").and_then(Value::as_u64),
        api_key_id: row.api_key_id,
        api_key_name: row.api_key_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_log_exposes_only_metadata() {
        let record = request_log_from_row(request_repo::RequestDetailRow {
            id: "request-1".into(),
            timestamp: "2026-09-15T12:00:00Z".into(),
            provider: Some("openai".into()),
            model: Some("gpt-5".into()),
            connection_id: Some("secret-connection".into()),
            status: Some("success".into()),
            api_key_id: Some("key-1".into()),
            api_key_name: Some("OpenCode".into()),
            correlation_id: Some("internal".into()),
            data: json!({
                "route": "work",
                "statusCode": 200,
                "durationMs": 42,
                "inputTokens": 10,
                "outputTokens": 20,
                "cachedTokens": 7,
                "request": "secret prompt",
                "response": "secret response"
            }),
        });

        let value = serde_json::to_value(record).unwrap();
        assert_eq!(value["requestId"], "request-1");
        assert_eq!(value["route"], "work");
        assert_eq!(value["inputTokens"], 10);
        assert_eq!(value["cachedTokens"], 7);
        assert_eq!(value["apiKeyId"], "key-1");
        assert_eq!(value["apiKeyName"], "OpenCode");
        let serialized = value.to_string();
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("internal"));
    }

    #[test]
    fn request_log_exposes_error_kind() {
        let with_kind = request_log_from_row(request_repo::RequestDetailRow {
            id: "request-2".into(),
            timestamp: "2026-09-18T12:00:00Z".into(),
            provider: Some("codex".into()),
            model: Some("gpt-5.6-luna".into()),
            connection_id: None,
            status: Some("error".into()),
            api_key_id: Some("key-1".into()),
            api_key_name: Some("OpenCode".into()),
            correlation_id: None,
            data: json!({
                "route": "cx/gpt-5.6-luna",
                "statusCode": 502,
                "errorKind": error_kind::UPSTREAM_FAILURE,
                "durationMs": 1200,
            }),
        });
        let value = serde_json::to_value(with_kind).unwrap();
        assert_eq!(value["errorKind"], error_kind::UPSTREAM_FAILURE);

        let without_kind = request_log_from_row(request_repo::RequestDetailRow {
            id: "request-3".into(),
            timestamp: "2026-09-18T12:00:00Z".into(),
            provider: Some("codex".into()),
            model: Some("gpt-5.6-luna".into()),
            connection_id: None,
            status: Some("success".into()),
            api_key_id: Some("key-1".into()),
            api_key_name: Some("OpenCode".into()),
            correlation_id: None,
            data: json!({
                "route": "cx/gpt-5.6-luna",
                "statusCode": 200,
                "durationMs": 800,
            }),
        });
        let value = serde_json::to_value(without_kind).unwrap();
        assert!(value.get("errorKind").is_none());
    }

    #[test]
    fn log_write_failure_counter_increments() {
        // Process-global counter: assert a lower bound, never exact equality.
        let before = request_log_dropped();
        note_log_write_failed("test");
        note_log_write_failed("test");
        assert!(request_log_dropped() >= before + 2);
    }
}
