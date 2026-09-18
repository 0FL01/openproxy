use std::time::Instant;

use chrono::Local;

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
}

impl RequestLog {
    pub fn start(
        method: &'static str,
        path: &str,
        model: Option<&str>,
        request_id: Option<String>,
    ) -> Self {
        let time = Local::now().format("%H:%M:%S");
        match model {
            Some(m) => log_both(&format!(
                "\x1b[36m[{}] 📥 {} {} model={}\x1b[0m",
                time, method, path, m
            )),
            None => log_both(&format!("\x1b[36m[{}] 📥 {} {}\x1b[0m", time, method, path)),
        }
        Self {
            method,
            path: path.to_owned(),
            start: Instant::now(),
            request_id,
        }
    }

    pub fn finish(self, status: u16) {
        let elapsed = self.start.elapsed().as_millis() as u64;
        let icon = if status < 400 { "📤" } else { "💥" };
        let time = Local::now().format("%H:%M:%S");
        match &self.request_id {
            Some(id) => log_both(&format!(
                "[{}] {} {} ({}ms) {} {} id={}",
                time, icon, status, elapsed, self.method, self.path, id
            )),
            None => log_both(&format!(
                "[{}] {} {} ({}ms) {} {}",
                time, icon, status, elapsed, self.method, self.path
            )),
        }
    }
}

pub fn stream(event: &str, data: Option<&str>) {
    let time = Local::now().format("%H:%M:%S");
    match data {
        Some(d) => log_both(&format!("[{}] 🌊 [STREAM] {} {}", time, event, d)),
        None => log_both(&format!("[{}] 🌊 [STREAM] {}", time, event)),
    }
}
