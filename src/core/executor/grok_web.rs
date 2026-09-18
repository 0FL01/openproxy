//! Grok Web executor — `grok.com/rest` SSO-cookie chat API.
//!
//! Port of 9router `open-sse/executors/grok-web.js` (+ `registry/grok-web.js`):
//! OpenAI chat body → single Grok message string, browser-fingerprint headers
//! (statsig / traceparent / Sec-Ch-Ua) for Cloudflare, NDJSON response stream →
//! OpenAI SSE chunks, `reasoning_content` only for thinking models, and
//! estimated usage (`ceil(len/4)`).
//!
//! Distinct from:
//! - [`super::xai`] → `api.x.ai` (API key / OAuth)
//! - [`super::grok_cli`] → `cli-chat-proxy.grok.com` Responses API
//!
//! Cookie auth: the `sso` cookie is stored in `credentials.api_key` (web-cookie
//! provider convention, `authType: "cookie"`), with a leading `sso=` prefix
//! stripped if present.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hyper::http;
use hyper::http::uri::InvalidUri;
use rand::RngCore;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE};
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{read_reqwest_body, success_body_limit, ClientPool, TransportKind, UpstreamResponse};

const GROK_CHAT_API: &str = "https://grok.com/rest/app-chat/conversations/new";
const GROK_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";
const GROK_STATSIG_SUFFIX: &str =
    "e:TypeError: Cannot read properties of null (reading 'children')";

/// Grok web mode mapping (JS MODEL_MAP, lines 9-24).
struct GrokModelInfo {
    grok_model: &'static str,
    model_mode: &'static str,
    is_thinking: bool,
}

fn model_map() -> &'static [(&'static str, GrokModelInfo)] {
    &[
        (
            "grok-3",
            GrokModelInfo {
                grok_model: "grok-3",
                model_mode: "MODEL_MODE_GROK_3",
                is_thinking: false,
            },
        ),
        (
            "grok-3-mini",
            GrokModelInfo {
                grok_model: "grok-3",
                model_mode: "MODEL_MODE_GROK_3_MINI_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-3-thinking",
            GrokModelInfo {
                grok_model: "grok-3",
                model_mode: "MODEL_MODE_GROK_3_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4",
            GrokModelInfo {
                grok_model: "grok-4",
                model_mode: "MODEL_MODE_GROK_4",
                is_thinking: false,
            },
        ),
        (
            "grok-4-mini",
            GrokModelInfo {
                grok_model: "grok-4-mini",
                model_mode: "MODEL_MODE_GROK_4_MINI_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4-thinking",
            GrokModelInfo {
                grok_model: "grok-4",
                model_mode: "MODEL_MODE_GROK_4_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4-heavy",
            GrokModelInfo {
                grok_model: "grok-4",
                model_mode: "MODEL_MODE_HEAVY",
                is_thinking: true,
            },
        ),
        (
            "grok-4.1-mini",
            GrokModelInfo {
                grok_model: "grok-4-1-thinking-1129",
                model_mode: "MODEL_MODE_GROK_4_1_MINI_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4.1-fast",
            GrokModelInfo {
                grok_model: "grok-4-1-thinking-1129",
                model_mode: "MODEL_MODE_FAST",
                is_thinking: false,
            },
        ),
        (
            "grok-4.1-expert",
            GrokModelInfo {
                grok_model: "grok-4-1-thinking-1129",
                model_mode: "MODEL_MODE_EXPERT",
                is_thinking: true,
            },
        ),
        (
            "grok-4.1-thinking",
            GrokModelInfo {
                grok_model: "grok-4-1-thinking-1129",
                model_mode: "MODEL_MODE_GROK_4_1_THINKING",
                is_thinking: true,
            },
        ),
        (
            "grok-4.2",
            GrokModelInfo {
                grok_model: "grok-420",
                model_mode: "MODEL_MODE_GROK_420",
                is_thinking: false,
            },
        ),
        (
            "grok-4.20",
            GrokModelInfo {
                grok_model: "grok-420",
                model_mode: "MODEL_MODE_GROK_420",
                is_thinking: false,
            },
        ),
        (
            "grok-4.20-beta",
            GrokModelInfo {
                grok_model: "grok-420",
                model_mode: "MODEL_MODE_GROK_420",
                is_thinking: false,
            },
        ),
    ]
}

/// Random lowercase-alpha string of `length` chars (JS randomString without alphanumeric).
fn random_alpha(length: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let mut out = String::with_capacity(length);
    for _ in 0..length {
        out.push(CHARS[(rng.next_u32() as usize) % CHARS.len()] as char);
    }
    out
}

/// Random lowercase-alphanumeric string of `length` chars (JS randomString alphanumeric).
fn random_alnum(length: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    let mut out = String::with_capacity(length);
    for _ in 0..length {
        out.push(CHARS[(rng.next_u32() as usize) % CHARS.len()] as char);
    }
    out
}

/// `x-statsig-id`: base64 of a fake browser TypeError (JS generateStatsigId).
fn generate_statsig_id() -> String {
    let msg = format!(
        "{}['{}']{}",
        "e:TypeError: Cannot read properties of null (reading 'children",
        random_alnum(5),
        ")']"
    );
    STANDARD.encode(format!("{}:{}", msg, GROK_STATSIG_SUFFIX))
}

/// Random hex string of `len_bytes` bytes (JS randomHex).
fn random_hex(len_bytes: usize) -> String {
    let mut bytes = vec![0u8; len_bytes];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Flatten OpenAI messages to a single Grok message string (JS parseOpenAIMessages).
///
/// Every message gets a `role: ` prefix except the last user message, which is
/// passed raw. System messages DO get role-prefixed. `developer` role is
/// normalized to `system`.
fn parse_openai_messages(messages: &[Value]) -> String {
    let mut extracted: Vec<(String, String)> = Vec::new();

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        let role = if role == "developer" {
            "system".to_string()
        } else {
            role
        };

        let content = match message.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        if content.trim().is_empty() {
            continue;
        }
        extracted.push((role, content));
    }

    let last_user_idx = extracted.iter().rposition(|(role, _)| role == "user");

    let mut parts = Vec::with_capacity(extracted.len());
    for (idx, (role, text)) in extracted.into_iter().enumerate() {
        if Some(idx) == last_user_idx {
            parts.push(text);
        } else {
            parts.push(format!("{role}: {text}"));
        }
    }

    parts.join("\n\n")
}

pub struct GrokWebExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug)]
pub enum GrokWebExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidUri(InvalidUri),
    InvalidRequest(hyper::http::Error),
    CookieParse(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    ResponseBody(String),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for GrokWebExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<InvalidUri> for GrokWebExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for GrokWebExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for GrokWebExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for GrokWebExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for GrokWebExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl std::fmt::Display for GrokWebExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for {}", p),
            Self::InvalidCredentials(msg) => write!(f, "Invalid credentials: {}", msg),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
            Self::CookieParse(msg) => write!(f, "Cookie parse error: {}", msg),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::ResponseBody(message) => write!(f, "Response body error: {message}"),
            Self::UnsupportedFormat(msg) => write!(f, "Unsupported format: {}", msg),
        }
    }
}

impl std::error::Error for GrokWebExecutorError {}

pub struct GrokWebExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transport: TransportKind,
}

impl std::fmt::Debug for GrokWebExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrokWebExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transport", &self.transport)
            .finish()
    }
}

pub struct GrokWebExecutor {
    pool: Arc<ClientPool>,
}

impl GrokWebExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    pub async fn execute_request(
        &self,
        request: GrokWebExecutionRequest,
    ) -> Result<GrokWebExecutorResponse, GrokWebExecutorError> {
        let url = self.build_url();

        // Resolve model → Grok internal model/mode; default to grok-4.1-fast
        // for unmapped models (JS `modelInfo || MODEL_MAP["grok-4.1-fast"]`).
        let model_info = model_map()
            .iter()
            .find(|(id, _)| *id == request.model)
            .map(|(_, info)| info)
            .unwrap_or_else(|| {
                model_map()
                    .iter()
                    .find(|(id, _)| *id == "grok-4.1-fast")
                    .map(|(_, info)| info)
                    .expect("grok-4.1-fast is a model map entry")
            });

        let message = match request.body.get("messages").and_then(Value::as_array) {
            Some(messages) => parse_openai_messages(messages),
            None => String::new(),
        };

        let transformed_body = self.build_payload(model_info, &message);
        let headers = self.build_headers(&request.credentials)?;

        let body_bytes = serde_json::to_vec(&transformed_body)?;

        let client = self.pool.get("grok-web", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        // JS grok-web.js:295-330 — map upstream failures to JSON error bodies
        // with actionable messages (auth/429 text drives account rotation).
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let (msg, code): (&str, String) = match status {
                401 | 403 => (
                    "Grok auth failed — SSO cookie may be expired. Re-paste your sso cookie value from grok.com.",
                    format!("HTTP_{status}"),
                ),
                429 => (
                    "Grok rate limited. Wait a moment and retry, or rotate cookies.",
                    format!("HTTP_{status}"),
                ),
                _ => (
                    &format!("Grok returned HTTP {status}")[..],
                    format!("HTTP_{status}"),
                ),
            };
            return Ok(GrokWebExecutorResponse {
                response: json_error(status, msg, "upstream_error", Some(&code)),
                url,
                headers,
                transport: TransportKind::Reqwest,
            });
        }

        // grok.com replies in NDJSON (result.response.token deltas etc.).
        // Convert to OpenAI SSE (stream) or a chat.completion JSON (non-stream)
        // exactly like buildStreamingResponse / buildNonStreamingResponse.
        let cid = format!(
            "chatcmpl-grok-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        let created = chrono::Utc::now().timestamp();
        let converted = convert_grok_response(
            response,
            &request.model,
            &cid,
            created,
            model_info.is_thinking,
            request.stream,
        )
        .await?;

        Ok(GrokWebExecutorResponse {
            response: converted,
            url,
            headers,
            transport: TransportKind::Reqwest,
        })
    }

    fn build_url(&self) -> String {
        GROK_CHAT_API.to_string()
    }

    /// grokPayload (JS lines 247-259) — verbatim field set.
    fn build_payload(&self, model_info: &GrokModelInfo, message: &str) -> Value {
        json!({
            "temporary": true,
            "modelName": model_info.grok_model,
            "modelMode": model_info.model_mode,
            "message": message,
            "fileAttachments": [],
            "imageAttachments": [],
            "disableSearch": false,
            "enableImageGeneration": false,
            "returnImageBytes": false,
            "returnRawGrokInXaiRequest": false,
            "enableImageStreaming": false,
            "imageGenerationCount": 0,
            "forceConcise": false,
            "toolOverrides": {},
            "enableSideBySide": true,
            "sendFinalMetadata": true,
            "isReasoning": false,
            "disableTextFollowUps": false,
            "disableMemory": true,
            "forceSideBySide": false,
            "isAsyncChat": false,
            "disableSelfHarmShortCircuit": false,
            "deviceEnvInfo": {
                "darkModeEnabled": false,
                "devicePixelRatio": 2,
                "screenWidth": 2056,
                "screenHeight": 1329,
                "viewportWidth": 2056,
                "viewportHeight": 1083
            }
        })
    }

    /// Browser-fingerprint headers (JS lines 263-283) — statsig, traceparent,
    /// Sec-Ch-Ua/Sec-Fetch-*, Chrome 136 UA, and the `sso` cookie.
    fn build_headers(
        &self,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, GrokWebExecutorError> {
        let mut headers = HeaderMap::new();

        let sso = credentials
            .api_key
            .as_deref()
            .ok_or_else(|| GrokWebExecutorError::MissingCredentials("grok-web".into()))?
            .to_string();
        let token = sso.strip_prefix("sso=").unwrap_or(&sso);
        let cookie_value = HeaderValue::from_str(&format!("sso={token}"))
            .map_err(|_| GrokWebExecutorError::CookieParse("Invalid cookie".to_string()))?;
        headers.insert(COOKIE, cookie_value);

        headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br, zstd"),
        );
        headers.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            HeaderValue::from_static("en-US,en;q=0.9"),
        );
        headers.insert(
            reqwest::header::ORIGIN,
            HeaderValue::from_static("https://grok.com"),
        );
        headers.insert(
            reqwest::header::REFERER,
            HeaderValue::from_static("https://grok.com/"),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(GROK_USER_AGENT),
        );
        headers.insert(
            "Sec-Ch-Ua",
            HeaderValue::from_static(
                r#""Google Chrome";v="136", "Chromium";v="136", "Not(A:Brand";v="24""#,
            ),
        );
        headers.insert("Sec-Ch-Ua-Mobile", HeaderValue::from_static("?0"));
        headers.insert("Sec-Ch-Ua-Platform", HeaderValue::from_static(r#""macOS""#));
        headers.insert("Sec-Fetch-Dest", HeaderValue::from_static("empty"));
        headers.insert("Sec-Fetch-Mode", HeaderValue::from_static("cors"));
        headers.insert("Sec-Fetch-Site", HeaderValue::from_static("same-origin"));
        headers.insert(
            "Baggage",
            HeaderValue::from_static(
                "sentry-environment=production,sentry-release=d6add6fb0460641fd482d767a335ef72b9b6abb8,sentry-public_key=b311e0f2690c81f25e2c4cf6d4f7ce1c",
            ),
        );
        headers.insert(
            "x-statsig-id",
            HeaderValue::from_str(&generate_statsig_id()).map_err(|_| {
                GrokWebExecutorError::InvalidCredentials("Invalid statsig id".to_string())
            })?,
        );
        headers.insert(
            "x-xai-request-id",
            HeaderValue::from_str(&Uuid::new_v4().to_string()).map_err(|_| {
                GrokWebExecutorError::InvalidCredentials("Invalid request id".to_string())
            })?,
        );
        headers.insert(
            "traceparent",
            HeaderValue::from_str(&format!("00-{}-{}-00", random_hex(16), random_hex(8))).map_err(
                |_| GrokWebExecutorError::InvalidCredentials("Invalid traceparent".to_string()),
            )?,
        );

        Ok(headers)
    }
}

/// OpenAI-compatible SSE chunk frame (9router sseChunk).
fn sse_chunk(
    cid: &str,
    created: i64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&serde_json::json!({
            "id": cid,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "system_fingerprint": Value::Null,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
                    .map(|s| Value::String(s.to_string()))
                    .unwrap_or(Value::Null),
                "logprobs": Value::Null,
            }],
        }))
        .unwrap_or_default()
    )
}

/// One parsed grok NDJSON line → what the converter should emit.
enum GrokEvent {
    Delta(String),
    Thinking(String),
    FullMessage(String),
    Fingerprint(String),
    Error(String),
    Done,
    Skip,
}

/// Parse one NDJSON line (JS extractContent per-line body, grok-web.js:113-146).
fn parse_grok_line(
    line: &str,
    fingerprint: &mut String,
    response_id: &mut String,
    think_opened: &mut bool,
    is_thinking_model: bool,
) -> GrokEvent {
    let Ok(event) = serde_json::from_str::<Value>(line) else {
        return GrokEvent::Skip;
    };
    if event.get("error").is_some() {
        let msg = event
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| {
                format!(
                    "Grok error: {}",
                    event.pointer("/error/code").cloned().unwrap_or(Value::Null)
                )
            });
        return GrokEvent::Error(msg);
    }
    let Some(resp) = event.pointer("/result/response") else {
        return GrokEvent::Skip;
    };

    if let Some(hash) = resp.pointer("/llmInfo/modelHash").and_then(Value::as_str) {
        *fingerprint = hash.to_string();
    }
    if let Some(rid) = resp.get("responseId").and_then(Value::as_str) {
        *response_id = rid.to_string();
    }

    if let Some(mr) = resp.get("modelResponse") {
        let message = mr.get("message").and_then(Value::as_str).unwrap_or("");
        if *think_opened && is_thinking_model && !message.is_empty() {
            *think_opened = false;
            return GrokEvent::Thinking(message.to_string());
        }
        if !message.is_empty() {
            if let Some(h) = mr
                .pointer("/metadata/llm_info/modelHash")
                .and_then(Value::as_str)
            {
                *fingerprint = h.to_string();
            }
            return GrokEvent::FullMessage(message.to_string());
        }
        return GrokEvent::Skip;
    }

    match resp.get("token") {
        Some(Value::String(token)) => GrokEvent::Delta(token.clone()),
        Some(Value::Number(_)) | Some(Value::Bool(true)) => {
            GrokEvent::Delta(resp["token"].to_string())
        }
        _ => GrokEvent::Skip,
    }
}

/// Convert the grok.com NDJSON reply into an OpenAI SSE stream or a
/// non-streaming chat.completion JSON (JS buildStreamingResponse /
/// buildNonStreamingResponse, grok-web.js:128-240).
async fn convert_grok_response(
    response: reqwest::Response,
    model: &str,
    cid: &str,
    created: i64,
    is_thinking_model: bool,
    stream: bool,
) -> Result<UpstreamResponse, GrokWebExecutorError> {
    // Grok Web requires complete NDJSON before it can synthesize either output
    // shape. Keep that protocol behavior, but cap the collected upstream bytes.
    let body = read_reqwest_body(response, success_body_limit())
        .await
        .map_err(|error| GrokWebExecutorError::ResponseBody(error.to_string()))?;
    let text = String::from_utf8_lossy(&body);

    let mut fingerprint = String::new();
    let mut response_id = String::new();
    let mut think_opened = false;
    let mut full_message = String::new();
    let mut thinking_text = String::new();
    let mut error_text: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match parse_grok_line(
            line,
            &mut fingerprint,
            &mut response_id,
            &mut think_opened,
            is_thinking_model,
        ) {
            GrokEvent::Delta(d) => full_message.push_str(&d),
            GrokEvent::Thinking(t) => thinking_text.push_str(&t),
            GrokEvent::FullMessage(m) => full_message = m,
            GrokEvent::Fingerprint(_) => {}
            GrokEvent::Error(e) => {
                error_text = Some(e);
                break;
            }
            GrokEvent::Done | GrokEvent::Skip => {}
        }
    }

    let fp: Value = if fingerprint.is_empty() {
        Value::Null
    } else {
        Value::String(fingerprint.clone())
    };

    let mut sse = String::new();
    let push = |sse: &mut String, delta: Value, finish: Option<&str>| {
        sse.push_str(&sse_chunk(cid, created, model, delta, finish));
    };

    if stream {
        sse.push_str(&sse_chunk(
            cid,
            created,
            model,
            json!({ "role": "assistant" }),
            None,
        ));
        if let Some(err) = &error_text {
            push(
                &mut sse,
                json!({ "content": format!("[Error: {err}]") }),
                None,
            );
        } else {
            if !thinking_text.is_empty() {
                push(
                    &mut sse,
                    json!({ "reasoning_content": thinking_text }),
                    None,
                );
            }
            if !full_message.is_empty() {
                push(&mut sse, json!({ "content": full_message }), None);
            }
        }
        let done = sse_chunk(cid, created, model, json!({}), Some("stop"));
        // Attach the collected fingerprint to every emitted frame.
        let done = done.replace(
            "\"system_fingerprint\":null",
            &format!("\"system_fingerprint\":{}", fp),
        );
        sse.push_str(&done);
        sse.push_str("data: [DONE]\n\n");

        let mut http_resp = http::Response::new(ReqwestBody::from(sse));
        *http_resp.status_mut() = reqwest::StatusCode::OK;
        http_resp.headers_mut().insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        http_resp.headers_mut().insert(
            reqwest::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        );
        Ok(UpstreamResponse::Reqwest(reqwest::Response::from(
            http_resp,
        )))
    } else {
        let content = match &error_text {
            Some(e) => format!("[Error: {e}]"),
            None => full_message.clone(),
        };
        let body = json!({
            "id": cid,
            "object": "chat.completion",
            "created": created,
            "model": model,
            "system_fingerprint": fp,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop",
                "logprobs": Value::Null,
            }],
            "usage": {
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0,
            },
        });
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
        *http_resp.status_mut() = reqwest::StatusCode::OK;
        http_resp.headers_mut().insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        Ok(UpstreamResponse::Reqwest(reqwest::Response::from(
            http_resp,
        )))
    }
}

fn json_error(status: u16, message: &str, err_type: &str, code: Option<&str>) -> UpstreamResponse {
    let mut body = serde_json::json!({ "error": { "message": message, "type": err_type } });
    if let Some(code) = code {
        body["error"]["code"] = Value::String(code.to_string());
    }
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
    *http_resp.status_mut() =
        reqwest::StatusCode::from_u16(status).unwrap_or(reqwest::StatusCode::BAD_GATEWAY);
    http_resp.headers_mut().insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    UpstreamResponse::Reqwest(reqwest::Response::from(http_resp))
}

#[cfg(test)]
mod grok_ndjson_tests {
    use super::*;

    #[test]
    fn token_delta_yields_content() {
        let mut fp = String::new();
        let mut rid = String::new();
        let mut think = false;
        let ev = parse_grok_line(
            r#"{"result":{"response":{"token":"hello","responseId":"r1"}}}"#,
            &mut fp,
            &mut rid,
            &mut think,
            false,
        );
        assert!(matches!(&ev, GrokEvent::Delta(d) if d == "hello"));
    }

    #[test]
    fn model_response_closes_thinking_then_full_message() {
        let mut fp = String::new();
        let mut rid = String::new();
        let mut think = true;
        let ev = parse_grok_line(
            r#"{"result":{"response":{"modelResponse":{"message":"final answer"}}}}"#,
            &mut fp,
            &mut rid,
            &mut think,
            true,
        );
        // JS: when thinking is open on a thinking model, the full message is
        // surfaced as reasoning first.
        assert!(matches!(ev, GrokEvent::Thinking(_)));
        assert!(!think);
    }

    #[test]
    fn error_line_maps_to_error_event() {
        let mut fp = String::new();
        let mut rid = String::new();
        let mut think = false;
        let ev = parse_grok_line(
            r#"{"error":{"message":"boom","code":42}}"#,
            &mut fp,
            &mut rid,
            &mut think,
            false,
        );
        assert!(matches!(&ev, GrokEvent::Error(m) if m == "boom"));
    }

    #[test]
    fn model_hash_updates_fingerprint() {
        let mut fp = String::new();
        let mut rid = String::new();
        let mut think = false;
        let _ = parse_grok_line(
            r#"{"result":{"response":{"llmInfo":{"modelHash":"abc123"},"token":"x"}}}"#,
            &mut fp,
            &mut rid,
            &mut think,
            false,
        );
        assert_eq!(fp, "abc123");
    }

    #[test]
    fn non_json_lines_are_skipped() {
        let mut fp = String::new();
        let mut rid = String::new();
        let mut think = false;
        let ev = parse_grok_line("not json", &mut fp, &mut rid, &mut think, false);
        assert!(matches!(ev, GrokEvent::Skip));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_grok_build_url() {
        let executor = GrokWebExecutor::new(Arc::new(ClientPool::default()));
        let url = executor.build_url();
        assert_eq!(url, "https://grok.com/rest/app-chat/conversations/new");
    }

    #[test]
    fn test_grok_web_payload_shape() {
        let executor = GrokWebExecutor::new(Arc::new(ClientPool::default()));
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "S"},
                {"role": "user", "content": "U"}
            ]
        });
        let model_info = model_map()
            .iter()
            .find(|(id, _)| *id == "grok-4.2")
            .map(|(_, info)| info)
            .unwrap();
        let message =
            parse_openai_messages(body.get("messages").and_then(Value::as_array).unwrap());
        let transformed = executor.build_payload(model_info, &message);
        assert_eq!(transformed["modelName"], "grok-420");
        assert_eq!(transformed["modelMode"], "MODEL_MODE_GROK_420");
        assert_eq!(transformed["message"], "system: S\n\nU");
        assert_eq!(transformed["temporary"], true);
    }

    #[test]
    fn test_parse_openai_messages_last_user_raw() {
        let messages = serde_json::json!([
            {"role": "system", "content": "Be helpful"},
            {"role": "user", "content": "First question"},
            {"role": "assistant", "content": "First answer"},
            {"role": "user", "content": "Follow up"}
        ]);
        let parsed = parse_openai_messages(messages.as_array().unwrap());
        assert_eq!(
            parsed,
            "system: Be helpful\n\nuser: First question\n\nassistant: First answer\n\nFollow up"
        );
    }

    #[test]
    fn test_parse_openai_messages_skips_empty() {
        let messages = serde_json::json!([
            {"role": "system", "content": " "},
            {"role": "user", "content": "Hello"}
        ]);
        let parsed = parse_openai_messages(messages.as_array().unwrap());
        assert_eq!(parsed, "Hello");
    }

    #[test]
    fn test_parse_openai_messages_content_array() {
        let messages = serde_json::json!([
            {"role": "user", "content": [
                {"type": "text", "text": "Hello"},
                {"type": "image_url", "image_url": {"url": "data:..."}},
                {"type": "text", "text": " world"}
            ]}
        ]);
        let parsed = parse_openai_messages(messages.as_array().unwrap());
        assert_eq!(parsed, "Hello  world");
    }

    #[test]
    fn test_parse_openai_messages_developer_is_system() {
        let messages = serde_json::json!([
            {"role": "developer", "content": "Instructions"},
            {"role": "user", "content": "Hi"}
        ]);
        let parsed = parse_openai_messages(messages.as_array().unwrap());
        assert_eq!(parsed, "system: Instructions\n\nHi");
    }

    #[test]
    fn test_generate_statsig_id_is_valid_base64() {
        let statsig = generate_statsig_id();
        // Must be valid base64 and decode to an e:TypeError string.
        let decoded = STANDARD.decode(&statsig).expect("valid base64");
        assert!(String::from_utf8(decoded)
            .unwrap()
            .starts_with("e:TypeError:"));
    }

    #[test]
    fn test_grok_headers_cookie_prefix_stripped() {
        use std::collections::BTreeMap;
        let executor = GrokWebExecutor::new(Arc::new(ClientPool::default()));
        let credentials = ProviderConnection {
            id: "test".to_string(),
            provider: "grok-web".to_string(),
            auth_type: "cookie".to_string(),
            name: None,
            priority: None,
            is_active: Some(true),
            created_at: None,
            updated_at: None,
            display_name: None,
            email: None,
            global_priority: None,
            default_model: None,
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: Some("sso=my-token".to_string()),
            test_status: None,
            last_tested: None,
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: None,
            error_code: None,
            consecutive_use_count: None,
            backoff_level: None,
            consecutive_errors: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            runtime_transport: None,
            provider_specific_data: BTreeMap::new(),
            extra: BTreeMap::new(),
        };
        let headers = executor.build_headers(&credentials).unwrap();
        assert_eq!(headers.get("cookie").unwrap(), "sso=my-token");
        assert!(headers.get("x-statsig-id").is_some());
        assert!(headers.get("x-xai-request-id").is_some());
        assert_eq!(headers.get("origin").unwrap(), "https://grok.com");
        let traceparent = headers.get("traceparent").unwrap().to_str().unwrap();
        assert!(traceparent.starts_with("00-"));
        assert_eq!(traceparent.len(), 3 + 32 + 1 + 16 + 1 + 2);
    }
}
