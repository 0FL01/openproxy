use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use axum::body::Body;
use axum::http::header;
use axum::response::Response;
use chrono::Local;
use http_body_util::{BodyExt, StreamBody};

use crate::server::console_logs::shared_console_log_buffer;

/// Structured request/response logging matching 9router's logger.js.
///
/// Logs are printed to stderr with emoji icons and timing:
///   `[HH:MM:SS] 📥 POST /v1/messages model=...`
///   `[HH:MM:SS] 📤 200 (1234ms) POST /v1/messages`
///   `[HH:MM:SS] 💥 404 (42ms) POST /v1/messages`
///   `[HH:MM:SS] 🌊 [STREAM] event.type ...`
///
/// Logs are also broadcast to the SSE console-log stream so the dashboard
/// at `/dashboard/console-log` shows them in real time.
///
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Response header carrying the server-side request id on errors.
/// Lets a client reporting an HTTP 500 correlate it to one stderr line
/// via grep. The id is never persisted (C35: no correlation internals).
pub const REQUEST_ID_HEADER: &str = "x-openproxy-request-id";

/// Short non-unique request id for log correlation within a time window
/// (32 bits — a grep hint, not a global identifier).
pub fn new_request_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    id[..8].to_string()
}

/// Attach the request id to an error response. Success responses stay
/// header-free (quiet by default).
pub fn attach_request_id(
    mut response: axum::response::Response,
    request_id: &str,
) -> axum::response::Response {
    if response.status().as_u16() >= 400 {
        if let Ok(value) = axum::http::HeaderValue::from_str(request_id) {
            response.headers_mut().insert(REQUEST_ID_HEADER, value);
        }
    }
    response
}
fn log_both(terminal_line: &str) {
    eprintln!("{}", terminal_line);
    let clean = strip_ansi(terminal_line);
    shared_console_log_buffer().append_line_blocking(clean);
}

pub struct RequestLog {
    method: &'static str,
    path: String,
    start: Instant,
    request_id: Option<String>,
    finished: AtomicBool,
}

impl RequestLog {
    /// Silent constructor: no entry line. Exactly one terminal line is
    /// emitted later by `finish` (errors only), `watch` (abort only), or
    /// `Drop` (aborted before any outcome).
    pub fn start(
        method: &'static str,
        path: &str,
        model: Option<&str>,
        request_id: Option<String>,
    ) -> Self {
        let _ = model;
        Self {
            method,
            path: path.to_owned(),
            start: Instant::now(),
            request_id,
            finished: AtomicBool::new(false),
        }
    }

    /// Header-time outcome. Logs only failures (`status >= 400`).
    /// Idempotent: only the first terminal call emits.
    pub fn finish(&self, status: u16) {
        if status < 400 || self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let elapsed = self.start.elapsed().as_millis() as u64;
        let time = Local::now().format("%H:%M:%S");
        match &self.request_id {
            Some(id) => log_both(&format!(
                "[{}] 💥 {} ({}ms) {} {} id={}",
                time, status, elapsed, self.method, self.path, id
            )),
            None => log_both(&format!(
                "[{}] 💥 {} ({}ms) {} {}",
                time, status, elapsed, self.method, self.path
            )),
        }
    }

    /// Silent terminal mark for completed non-stream responses.
    pub fn complete(&self) {
        self.finished.store(true, Ordering::Release);
    }

    /// Abort outcome with a cause. Idempotent: only the first terminal
    /// call emits. Logging is sync and infallible (safe in `Drop`).
    pub fn abort(&self, cause: &'static str) {
        if self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let elapsed = self.start.elapsed().as_millis() as u64;
        let time = Local::now().format("%H:%M:%S");
        match &self.request_id {
            Some(id) => log_both(&format!(
                "[{}] 💥 aborted cause={} ({}ms) {} {} id={}",
                time, cause, elapsed, self.method, self.path, id
            )),
            None => log_both(&format!(
                "[{}] 💥 aborted cause={} ({}ms) {} {}",
                time, cause, elapsed, self.method, self.path
            )),
        }
    }

    /// Terminal handling for a handler response. Errors log immediately;
    /// successful SSE streams get a pass-through abort guard (clean EOF is
    /// silent, interruption emits one abort line); anything else completes
    /// silently. Consumes the log so `Drop` stays quiet afterwards.
    pub fn watch(mut self, response: Response) -> Response {
        let status = response.status().as_u16();
        if status >= 400 {
            self.finish(status);
            return response;
        }
        let is_sse = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|content_type| content_type.starts_with("text/event-stream"));
        if !is_sse {
            self.complete();
            return response;
        }
        // Silence the outer Drop: from here the stream guard owns the outcome.
        self.complete();
        let (parts, body) = response.into_parts();
        let mut watch = AbortWatch {
            method: self.method,
            path: std::mem::take(&mut self.path),
            start: self.start,
            request_id: self.request_id.take(),
            done: false,
        };
        let mut data_stream = body.into_data_stream();
        let stream = async_stream::stream! {
            use futures_util::StreamExt;
            while let Some(item) = data_stream.next().await {
                match item {
                    Ok(bytes) => {
                        yield Ok(hyper::body::Frame::data(bytes));
                    }
                    Err(error) => {
                        watch.fail("upstream_error");
                        yield Err(std::io::Error::other(error));
                        return;
                    }
                }
            }
            watch.complete();
        };
        Response::from_parts(parts, Body::new(StreamBody::new(stream)))
    }
}

impl Drop for RequestLog {
    fn drop(&mut self) {
        // Panic, cancellation, or disconnect before any outcome:
        // never go silent, emit one generic abort line.
        if !self.finished.swap(true, Ordering::AcqRel) {
            let elapsed = self.start.elapsed().as_millis() as u64;
            let time = Local::now().format("%H:%M:%S");
            match &self.request_id {
                Some(id) => log_both(&format!(
                    "[{}] 💥 aborted cause=cancelled ({}ms) {} {} id={}",
                    time, elapsed, self.method, self.path, id
                )),
                None => log_both(&format!(
                    "[{}] 💥 aborted cause=cancelled ({}ms) {} {}",
                    time, elapsed, self.method, self.path
                )),
            }
        }
    }
}

/// Stream-owned abort guard: clean EOF completes silently, upstream
/// transport death fails loudly, early drop (client disconnect, cancel,
/// unwind) reports `client_disconnect`. Sync/infallible only.
struct AbortWatch {
    method: &'static str,
    path: String,
    start: Instant,
    request_id: Option<String>,
    done: bool,
}

impl AbortWatch {
    fn fail(&mut self, cause: &'static str) {
        if self.done {
            return;
        }
        self.done = true;
        let elapsed = self.start.elapsed().as_millis() as u64;
        let time = Local::now().format("%H:%M:%S");
        match &self.request_id {
            Some(id) => log_both(&format!(
                "[{}] 💥 aborted cause={} ({}ms) {} {} id={}",
                time, cause, elapsed, self.method, self.path, id
            )),
            None => log_both(&format!(
                "[{}] 💥 aborted cause={} ({}ms) {} {}",
                time, cause, elapsed, self.method, self.path
            )),
        }
    }

    fn complete(&mut self) {
        self.done = true;
    }
}

impl Drop for AbortWatch {
    fn drop(&mut self) {
        if !self.done {
            self.fail("client_disconnect");
        }
    }
}
