use std::sync::Arc;

use futures_util::stream;
use futures_util::StreamExt;
use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};

use crate::core::config::app_constants::{
    CODEX_CLIENT_VERSION, CODEX_ORIGINATOR, CODEX_USER_AGENT,
};
use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::image_helper::{
    ensure_final_request_size, fetch_image_as_base64, ImagePrefetchBudget, ImagePrefetchError,
};
use crate::core::translator::request::openai_responses::chat_to_openai_responses_request;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

/// Codex tool JSON Schema pattern strip: Codex's `/responses` validator
/// rejects Unicode property escapes (`\p{...}`) with HTTP 400 — even
/// though they're valid ECMAScript. The JS does a copy-on-write walk
/// removing only `pattern` values containing `\p{...}`; see
/// `open-sse/utils/codexToolSchema.js` (#3922).
///
/// Port of `stripCodexUnsupportedPatterns`: recursively walks tool
/// parameter schemas, removing `pattern` fields that contain Unicode
/// property escapes. Returns the (possibly mutated) schema node.
fn has_unicode_property_escape(pattern: &str) -> bool {
    // `\p{...}` / `\P{...}` with an odd number of preceding backslashes —
    // an even count means the backslash itself is escaped, so `\\p{Cc}`
    // is a literal "p". Mirrors UNICODE_PROPERTY_ESCAPE in codexToolSchema.js.
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        let mut backslashes = 0;
        while i < bytes.len() && bytes[i] == b'\\' {
            backslashes += 1;
            i += 1;
        }
        if backslashes % 2 == 1
            && i + 1 < bytes.len()
            && (bytes[i] == b'p' || bytes[i] == b'P')
            && bytes[i + 1] == b'{'
        {
            return true;
        }
    }
    false
}

fn strip_codex_tool_patterns(node: &mut Value) {
    match node {
        Value::Object(map) => {
            // `properties` is special-cased: its keys are arbitrary property
            // *names* (which may themselves be "pattern" or "properties") and
            // must never be read as schema keywords — but each property's
            // *value* is a schema node and must still be walked.
            // Mirrors stripNode in codexToolSchema.js.
            let mut property_names: Vec<String> = Vec::new();
            if let Some(Value::Object(props)) = map.get("properties") {
                property_names = props.keys().cloned().collect();
            }
            let other_keys: Vec<String> = map
                .keys()
                .filter(|k| k.as_str() != "properties" && k.as_str() != "pattern")
                .cloned()
                .collect();

            if node
                .get("pattern")
                .and_then(Value::as_str)
                .is_some_and(has_unicode_property_escape)
            {
                node.as_object_mut()
                    .expect("node is an object")
                    .remove("pattern");
            }

            for key in other_keys {
                if let Some(val) = node.get_mut(&key) {
                    strip_codex_tool_patterns(val);
                }
            }
            if let Some(Value::Object(props)) = node
                .as_object_mut()
                .and_then(|obj| obj.get_mut("properties"))
            {
                for name in property_names {
                    if let Some(prop_schema) = props.get_mut(&name) {
                        strip_codex_tool_patterns(prop_schema);
                    }
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                strip_codex_tool_patterns(item);
            }
        }
        _ => {}
    }
}

fn normalize_codex_tool(tool: &Value) -> Option<Value> {
    let mut normalized = tool.clone();
    let object = normalized.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("function") {
        strip_codex_tool_patterns(&mut normalized);
        return Some(normalized);
    }

    let function = object.get("function").and_then(Value::as_object);
    let name = object
        .get("name")
        .or_else(|| function.and_then(|value| value.get("name")))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?
        .trim()
        .to_string();
    let description = object
        .get("description")
        .or_else(|| function.and_then(|value| value.get("description")))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let parameters = object
        .get("parameters")
        .or_else(|| function.and_then(|value| value.get("parameters")))
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    let strict = object
        .get("strict")
        .or_else(|| function.and_then(|value| value.get("strict")))
        .and_then(Value::as_bool);

    normalized = json!({
        "type": "function",
        "name": name,
        "parameters": parameters,
    });
    if let Some(description) = description {
        normalized["description"] = Value::String(description);
    }
    if let Some(strict) = strict {
        normalized["strict"] = Value::Bool(strict);
    }
    strip_codex_tool_patterns(&mut normalized);
    Some(normalized)
}

fn normalize_codex_input_items(items: &mut [Value]) {
    for item in items {
        if item.get("role").and_then(Value::as_str) == Some("tool") {
            let call_id = item
                .get("tool_call_id")
                .or_else(|| item.get("call_id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let output = codex_tool_output(item.get("content"));
            *item = json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output,
            });
            continue;
        }

        if item.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }

        if let Some(text) = item
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            item["content"] = json!([{ "type": "output_text", "text": text }]);
            continue;
        }

        let Some(parts) = item.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in parts {
            let is_assistant_text = matches!(
                part.get("type").and_then(Value::as_str),
                Some("input_text" | "text")
            );
            if is_assistant_text {
                part["type"] = Value::String("output_text".to_string());
                if let Some(part) = part.as_object_mut() {
                    part.remove("annotations");
                    part.remove("logprobs");
                    part.remove("obfuscation");
                }
            }
        }
    }
}

fn codex_tool_output(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod codex_tool_pattern_tests {
    use super::strip_codex_tool_patterns;
    use serde_json::json;

    #[test]
    fn strips_unicode_property_pattern() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "pattern": "^[^\\p{Cc}]{1,200}$" },
                "safe": { "type": "string", "pattern": "^[a-z]+$" }
            }
        });
        strip_codex_tool_patterns(&mut schema);
        assert!(schema["properties"]["name"].get("pattern").is_none());
        assert_eq!(schema["properties"]["safe"]["pattern"], "^[a-z]+$");
    }

    #[test]
    fn keeps_escaped_literal_backslash_p() {
        // `\\p{Cc}` is a literal "p" — must not be stripped.
        let mut schema = json!({ "type": "string", "pattern": "^\\\\p{Cc}$" });
        strip_codex_tool_patterns(&mut schema);
        assert_eq!(schema["pattern"], "^\\\\p{Cc}$");
    }

    #[test]
    fn property_named_pattern_is_not_a_keyword() {
        // A field literally called "pattern" must not be read as the schema keyword:
        // its value does not get its own "pattern" stripped.
        let mut schema = json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string" }
            }
        });
        strip_codex_tool_patterns(&mut schema);
        assert!(schema["properties"]["pattern"].get("type").is_some());
    }
}

const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Maximum bytes inspected while identifying the first complete SSE event.
///
/// C11 keeps this preflight intentionally narrow: it can turn a structured
/// first-event protocol failure into an HTTP failure for the request-scoped
/// account planner, but it never waits for later output or a retry window.
const CODEX_FIRST_EVENT_MAX_BYTES: usize = 64 * 1024;

fn first_sse_event_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| position + 2)
        .or_else(|| {
            bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4)
        })
}

fn codex_first_event_failure_status(event: &[u8]) -> Option<reqwest::StatusCode> {
    let text = std::str::from_utf8(event).ok()?;
    let mut event_name = None;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }

    let payload = serde_json::from_str::<Value>(&data).ok();
    let payload_type = payload
        .as_ref()
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);
    let is_failure_event = matches!(event_name, Some("error" | "response.failed"))
        || matches!(payload_type, Some("error" | "response.failed"));
    if !is_failure_event {
        return None;
    }

    let error = payload.as_ref().and_then(|value| {
        value
            .get("error")
            .or_else(|| value.pointer("/response/error"))
    });
    let code = error
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str);
    let error_type = error
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);
    let is_kind = |expected: &str| code == Some(expected) || error_type == Some(expected);
    if is_kind("rate_limit_exceeded") || is_kind("usage_limit_reached") {
        Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
    } else if is_kind("server_is_overloaded")
        || is_kind("service_unavailable_error")
        || is_kind("model_at_capacity")
    {
        Some(reqwest::StatusCode::SERVICE_UNAVAILABLE)
    } else {
        None
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct CodexExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum CodexExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    InvalidUri(InvalidUri),
    InvalidRequest(http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    ImagePrefetch(ImagePrefetchError),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for CodexExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for CodexExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for CodexExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<http::Error> for CodexExecutorError {
    fn from(error: http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for CodexExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for CodexExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for CodexExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

pub struct CodexExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub web_search_context_size: Option<String>,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct CodexExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transport: TransportKind,
}

impl std::fmt::Debug for CodexExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transport", &self.transport)
            .finish()
    }
}

impl CodexExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, CodexExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// Parse Codex model string to extract actual OpenAI model name.
    ///
    /// Examples:
    /// - "codex/o4-mini" → "o4-mini"
    /// - "codex/o4-mini-high" → "o4-mini-high"
    /// - "codex/o3" → "o3"
    /// - "codex/o3-mini" → "o3-mini"
    /// - "o4-mini" → "o4-mini" (no prefix)
    pub fn parse_codex_model(model: &str) -> String {
        if let Some(stripped) = model.strip_prefix("codex/") {
            stripped.to_string()
        } else {
            model.to_string()
        }
    }

    /// Build the URL for Codex Responses API at chatgpt.com.
    ///
    /// When the model name ends with `_compact` or the `provider_node`
    /// carries a custom field `"_compact": true`, the `/compact` suffix
    /// is appended to reduce response size.
    fn build_url(&self, model: &str) -> String {
        let base = self
            .provider_node
            .as_ref()
            .and_then(|node| node.base_url.as_deref())
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .unwrap_or(CODEX_RESPONSES_URL)
            .trim_end_matches('/')
            .to_string();
        let is_compact_model = model.ends_with("_compact");
        let is_compact_node = self
            .provider_node
            .as_ref()
            .and_then(|n| n.extra.get("_compact"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_compact_model || is_compact_node {
            format!("{}/compact", base)
        } else {
            base
        }
    }

    /// Build request headers for Codex Responses API.
    fn build_headers(
        &self,
        api_key: &str,
        stream: bool,
        connection_id: Option<&str>,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, CodexExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(CodexExecutorError::InvalidHeader)?,
        );

        // 9router parity: session_id header for request session continuity.
        let session_id = connection_id
            .filter(|&cid| !cid.is_empty())
            .unwrap_or("default");
        headers.insert(
            "session_id",
            HeaderValue::from_str(session_id).map_err(CodexExecutorError::InvalidHeader)?,
        );

        // Identify the current Codex client version to unlock version-gated models.
        headers.insert("originator", HeaderValue::from_static(CODEX_ORIGINATOR));
        headers.insert("Version", HeaderValue::from_static(CODEX_CLIENT_VERSION));
        headers.insert(USER_AGENT, HeaderValue::from_static(CODEX_USER_AGENT));

        // 9router parity: workspace binding for account scope + cache affinity.
        {
            let ws_id = credentials
                .provider_specific_data
                .get("workspaceId")
                .or_else(|| credentials.provider_specific_data.get("chatgptAccountId"))
                .and_then(|v| v.as_str())
                .or(connection_id);
            if let Some(ws) = ws_id {
                headers.insert(
                    "chatgpt-account-id",
                    HeaderValue::from_str(ws).map_err(CodexExecutorError::InvalidHeader)?,
                );
            }
        }

        if stream {
            headers.insert("Accept", HeaderValue::from_static("text/event-stream"));
        }

        Ok(headers)
    }

    /// Transform the request body from Chat Completions format to Codex Responses API format.
    ///
    /// Handles both pre-translated bodies (input[] array from `chat_to_openai_responses_request`)
    /// and untranslated OpenAI bodies (messages[] array) — this avoids double-translation bugs
    /// when the pipeline already ran request translation before calling the executor.
    ///
    /// The Codex Responses API at chatgpt.com uses `input` as an array of message items.
    /// This function:
    /// - Converts messages[] to input[] with type "message", role, and content as input_text blocks
    /// - Converts "system" role to "developer"
    /// - Strips server-generated IDs (rs_, fc_, resp_, msg_ prefixes) to avoid 404s with store:false
    ///
    /// 9router codex.js parity:
    /// - Forces stream: true (Codex backend; client JSON via forceStream SSE→JSON)
    /// - Forces store: false
    /// - Strips effort suffixes from model (`-high`, `-medium`, …) into reasoning.effort
    /// - Keeps the required instructions field neutral when the client omits it
    fn transform_request_body(
        &self,
        body: &Value,
        actual_model: &str,
        _stream: bool,
        web_search_context_size: Option<&str>,
    ) -> Result<Value, CodexExecutorError> {
        let mut normalized_body = body.clone();
        if normalized_body.get("input").is_none() {
            chat_to_openai_responses_request(actual_model, &mut normalized_body, true, None);
        }
        let mut input_items = match normalized_body.get("input") {
            Some(Value::Array(input)) if !input.is_empty() => input.clone(),
            Some(Value::String(text)) => vec![json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": if text.is_empty() { "..." } else { text },
                }],
            })],
            _ => {
                return Err(CodexExecutorError::UnsupportedFormat(
                    "Missing or empty input array in request body".to_string(),
                ));
            }
        };
        normalize_codex_input_items(&mut input_items);

        let instructions = normalized_body
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or("");

        // Strip effort suffix from model name (9router: none/minimal/low/medium/high/xhigh)
        let effort_levels = ["none", "minimal", "low", "medium", "high", "xhigh"];
        let mut model_id = actual_model.to_string();
        let mut model_effort: Option<&str> = None;
        for level in effort_levels {
            let suffix = format!("-{level}");
            if let Some(stripped) = model_id.strip_suffix(&suffix) {
                model_id = stripped.to_string();
                model_effort = Some(level);
                break;
            }
            // Also support model(high) style
            let paren = format!("({level})");
            if let Some(idx) = model_id.rfind(&paren) {
                model_id = model_id[..idx].trim_end().to_string();
                model_effort = Some(level);
                break;
            }
        }

        // Priority: body.reasoning.effort > reasoning_effort > model suffix.
        let effort = normalized_body
            .pointer("/reasoning/effort")
            .and_then(Value::as_str)
            .or_else(|| body.get("reasoning_effort").and_then(Value::as_str))
            .or(model_effort);

        // Swarm-only `ultra` is not supported by the plain proxy router.
        if effort.is_some_and(|value| value.trim().eq_ignore_ascii_case("ultra")) {
            return Err(CodexExecutorError::UnsupportedFormat(
                "reasoning effort 'ultra' is not supported for codex".to_string(),
            ));
        }
        let model_lower = actual_model.trim().to_lowercase();
        if model_lower.ends_with("-ultra") || model_lower.contains("(ultra)") {
            return Err(CodexExecutorError::UnsupportedFormat(
                "reasoning effort 'ultra' is not supported for codex".to_string(),
            ));
        }

        let mut request_body = json!({
            "model": model_id,
            "input": input_items,
            "instructions": instructions,
            "stream": true, // 9router always forces stream
            "store": false,
        });

        if let Some(effort) = effort {
            request_body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
        }

        // Codex's /responses validator rejects tool schemas carrying Unicode
        // property escapes (\p{...}) with HTTP 400 — valid ECMA regex but not
        // supported by Codex's schema validator (#3922). Strip patterns before
        // dispatch; 9router applies the same strip in normalizeCodexTools.
        let mut tools = normalized_body
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(normalize_codex_tool)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tool_choice_none = body.get("tool_choice").and_then(Value::as_str) == Some("none");
        if !tool_choice_none {
            if let Some(context_size) = web_search_context_size {
                if !tools
                    .iter()
                    .any(|tool| tool.get("type").and_then(Value::as_str) == Some("web_search"))
                {
                    tools.push(json!({
                        "type": "web_search",
                        "external_web_access": true,
                        "search_context_size": context_size,
                    }));
                }
            }
        }
        if !tools.is_empty() {
            request_body["tools"] = Value::Array(tools);
        }
        if let Some(tool_choice) = body.get("tool_choice") {
            request_body["tool_choice"] = tool_choice.clone();
        }
        // JS keeps `stop` (not in the delete list, codex.js:462-479).
        if let Some(stop) = body.get("stop") {
            request_body["stop"] = stop.clone();
        }

        // Include reasoning encrypted content — Codex backend requires this for
        // reasoning models. JS: `if effort !== "none" → body.include =
        // ["reasoning.encrypted_content"]` (codex.js:457-459). Overwrite any
        // client-supplied include per JS.
        if effort.is_some_and(|effort| effort != "none") {
            request_body["include"] = json!(["reasoning.encrypted_content"]);
        }

        // Inject prompt_cache_key for stable Codex prompt caching when the
        // caller didn't supply one (JS codex.js:426-428).
        let cache_session = body
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                body.get("input")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|item| item.get("session_id"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_string())
            });
        if let Some(ck) = cache_session {
            request_body["prompt_cache_key"] = Value::String(ck);
        }

        // service_tier mapping: "fast" → "priority", delete other non-priority
        // (JS codex.js:480-481).
        if let Some(tier) = normalized_body.get("service_tier").and_then(Value::as_str) {
            if tier == "fast" {
                request_body["service_tier"] = Value::String("priority".to_string());
            } else if tier == "priority" {
                request_body["service_tier"] = Value::String("priority".to_string());
            }
            // else: dropped (not carried into request_body)
        }

        Ok(request_body)
    }

    /// Prefetch remote `image_url` content parts into `input_image` parts with
    /// inline base64 data URIs. Mirrors JS `prefetchImages` (codex.js:241-256):
    /// `data:` URLs pass through directly; remote URLs are fetched (15s timeout).
    pub async fn prefetch_images_in_request(body: &mut Value) -> Result<(), ImagePrefetchError> {
        let mut budget = ImagePrefetchBudget::new(body)?;
        let client = reqwest::Client::new();
        let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
            return Ok(());
        };
        for item in input.iter_mut() {
            let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            for part in content.iter_mut() {
                let Some(obj) = part.as_object_mut() else {
                    continue;
                };
                if obj.get("type").and_then(Value::as_str) != Some("image_url") {
                    continue;
                }
                let url = obj
                    .get("image_url")
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Object(o) => o.get("url").and_then(Value::as_str).map(String::from),
                        _ => None,
                    })
                    .ok_or(ImagePrefetchError::InvalidAttachment(
                        "Codex image_url must be a string or object with url",
                    ))?;
                if url.is_empty() {
                    return Err(ImagePrefetchError::InvalidAttachment(
                        "Codex image_url cannot be empty",
                    ));
                }
                let detail = obj
                    .get("image_url")
                    .and_then(|v| v.get("detail"))
                    .and_then(Value::as_str)
                    .unwrap_or("auto")
                    .to_string();
                let image_url = if url.starts_with("data:") {
                    budget.account_existing_data_url(&url)?;
                    url
                } else {
                    // Remote URL: fetch and inline as base64 data URI.
                    fetch_image_as_base64(&client, &url, &mut budget)
                        .await?
                        .data_url
                };
                let _ = obj.insert("type".into(), Value::String("input_image".to_string()));
                let _ = obj.insert("image_url".into(), Value::String(image_url));
                let _ = obj.insert("detail".into(), Value::String(detail));
            }
        }
        budget.ensure_final_request(body)
    }

    /// Parse a Codex upstream error, mapping `usage_limit_reached` to a
    /// resetsAtMs (JS `parseError`, codex.js:365-387).
    pub fn parse_error(status: u16, body_text: &str) -> crate::core::utils::error::UpstreamError {
        if status == 429 && !body_text.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(body_text) {
                let err = v.get("error");
                if err.and_then(|e| e.get("type")).and_then(Value::as_str)
                    == Some("usage_limit_reached")
                {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let mut resets_at_ms = None;
                    if let Some(secs) = err.and_then(|e| e.get("resets_at")).and_then(Value::as_u64)
                    {
                        let ms = secs * 1000;
                        if ms > now_ms {
                            resets_at_ms = Some(ms);
                        }
                    }
                    if resets_at_ms.is_none() {
                        if let Some(secs) = err
                            .and_then(|e| e.get("resets_in_seconds"))
                            .and_then(Value::as_u64)
                        {
                            resets_at_ms = Some(now_ms + secs * 1000);
                        }
                    }
                    let message = err
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or(body_text)
                        .to_string();
                    return crate::core::utils::error::UpstreamError {
                        status: 429,
                        message,
                        resets_at_ms,
                    };
                }
            }
        }
        crate::core::utils::error::UpstreamError {
            status,
            message: crate::core::utils::error::friendly_error_message(status, body_text),
            resets_at_ms: None,
        }
    }

    pub async fn execute(
        &self,
        request: CodexExecutionRequest,
    ) -> Result<CodexExecutorResponse, CodexExecutorError> {
        self.execute_inner(request, false).await
    }

    pub(crate) async fn execute_prefetched(
        &self,
        request: CodexExecutionRequest,
    ) -> Result<CodexExecutorResponse, CodexExecutorError> {
        self.execute_inner(request, true).await
    }

    async fn execute_inner(
        &self,
        mut request: CodexExecutionRequest,
        images_prefetched: bool,
    ) -> Result<CodexExecutorResponse, CodexExecutorError> {
        let actual_model = Self::parse_codex_model(&request.model);
        // JS codex.js:394-395 — a body-level `_compact: true` flag (set by the
        // Responses compat layer) routes to /compact and is stripped before
        // send; model-suffix and provider_node variants keep working.
        let body_compact = request
            .body
            .get("_compact")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let url = self.build_url(&actual_model);
        let url = if body_compact && !url.ends_with("/compact") {
            format!("{url}/compact")
        } else {
            url
        };

        // Get API key from credentials (try api_key first, then access_token for OAuth)
        let api_key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .ok_or_else(|| {
                CodexExecutorError::MissingCredentials("API key required".to_string())
            })?;

        let connection_id = request
            .credentials
            .email
            .as_deref()
            .or(request.credentials.id.as_str().into())
            .or(request.credentials.display_name.as_deref());
        // Always stream upstream (9router force stream); client JSON via chat sse_to_json
        let headers = self.build_headers(api_key, true, connection_id, &request.credentials)?;

        // Strip the `_compact` routing flag before send (JS codex.js:395
        // `delete body._compact` — the upstream never sees it).
        if let Some(obj) = request.body.as_object_mut() {
            obj.remove("_compact");
        }

        // Prefetch remote images into inline base64 data URIs (JS prefetchImages).
        if !images_prefetched
            && request
                .body
                .get("input")
                .and_then(Value::as_array)
                .is_some_and(|input| input.iter().any(|item| item.get("content").is_some()))
        {
            Self::prefetch_images_in_request(&mut request.body)
                .await
                .map_err(CodexExecutorError::ImagePrefetch)?;
        }

        let transformed_body = self.transform_request_body(
            &request.body,
            &actual_model,
            true,
            request.web_search_context_size.as_deref(),
        )?;
        ensure_final_request_size(&transformed_body).map_err(CodexExecutorError::ImagePrefetch)?;

        let client = self.pool.get("openai", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&transformed_body)
            .send()
            .await?;

        // Preserve non-success responses verbatim for the request-scoped
        // account/auth planner. Successful Codex responses are SSE; inspect at
        // most their first complete event so a structured failure can reach the
        // planner before downstream commitment. Normal streams are released as
        // soon as that first event arrives, without waiting for user output or
        // any temporal retry window.
        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
        if !response.status().is_success() || !is_sse {
            return Ok(CodexExecutorResponse {
                response: UpstreamResponse::Reqwest(response),
                url,
                headers,
                transport: TransportKind::Reqwest,
            });
        }

        let status = response.status();
        let response_headers = response.headers().clone();
        let mut upstream = response.bytes_stream();
        let mut prefix = Vec::new();
        let mut first_event = Vec::with_capacity(CODEX_FIRST_EVENT_MAX_BYTES);
        while first_event.len() < CODEX_FIRST_EVENT_MAX_BYTES {
            match upstream.next().await {
                Some(Ok(chunk)) => {
                    let remaining = CODEX_FIRST_EVENT_MAX_BYTES - first_event.len();
                    first_event.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    prefix.push(chunk);
                }
                Some(Err(error)) => return Err(CodexExecutorError::Request(error)),
                None => break,
            }
            if first_sse_event_end(&first_event).is_some()
                || first_event.len() >= CODEX_FIRST_EVENT_MAX_BYTES
            {
                break;
            }
        }

        if let Some(event_end) = first_sse_event_end(&first_event) {
            if let Some(failure_status) =
                codex_first_event_failure_status(&first_event[..event_end])
            {
                first_event.truncate(event_end);
                let mut failed = http::Response::new(ReqwestBody::from(first_event));
                *failed.status_mut() = failure_status;
                *failed.headers_mut() = response_headers;
                failed.headers_mut().remove(reqwest::header::CONTENT_LENGTH);
                return Ok(CodexExecutorResponse {
                    response: UpstreamResponse::Reqwest(reqwest::Response::from(failed)),
                    url,
                    headers,
                    transport: TransportKind::Reqwest,
                });
            }
        }

        let replay = stream::iter(prefix).map(Ok::<_, reqwest::Error>);
        let combined = replay.chain(upstream);
        let mut live = http::Response::new(ReqwestBody::wrap_stream(combined));
        *live.status_mut() = status;
        *live.headers_mut() = response_headers;
        Ok(CodexExecutorResponse {
            response: UpstreamResponse::Reqwest(reqwest::Response::from(live)),
            url,
            headers,
            transport: TransportKind::Reqwest,
        })
    }
}

/// Convert OpenAI Responses API SSE format to standard SSE format.
///
/// OpenAI Responses API returns events like:
/// - `event: response.done\ndata: {...}\n\n`
/// - `event: content.delta\ndata: {"type": "content.delta", "delta": {"type": "text_delta", "text": "Hello"}}\n\n`
///
/// We need to convert to standard format:
/// - `data: {"type": "content.delta", ...}\n\n`
pub fn convert_openai_sse_to_standard(input: &[u8]) -> Vec<u8> {
    if input.is_empty() {
        return Vec::new();
    }

    let input_str = String::from_utf8_lossy(input);
    let mut output = Vec::new();

    for line in input_str.lines() {
        // Skip the event: line, keep only data: lines
        if line.starts_with("data: ") {
            // Extract data content
            let data_content = line.trim_start_matches("data: ");
            // Output in standard SSE format
            output.extend_from_slice(b"data: ");
            output.extend_from_slice(data_content.as_bytes());
            output.extend_from_slice(b"\n\n");
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_codex_model_with_prefix() {
        assert_eq!(CodexExecutor::parse_codex_model("codex/o4-mini"), "o4-mini");
        assert_eq!(
            CodexExecutor::parse_codex_model("codex/o4-mini-high"),
            "o4-mini-high"
        );
        assert_eq!(CodexExecutor::parse_codex_model("codex/o3"), "o3");
        assert_eq!(CodexExecutor::parse_codex_model("codex/o3-mini"), "o3-mini");
    }

    #[test]
    fn test_parse_codex_model_without_prefix() {
        assert_eq!(CodexExecutor::parse_codex_model("o4-mini"), "o4-mini");
        assert_eq!(CodexExecutor::parse_codex_model("gpt-4"), "gpt-4");
    }

    #[test]
    fn test_codex_headers_advertise_current_client() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let headers = executor
            .build_headers(
                "token",
                true,
                Some("connection"),
                &ProviderConnection::default(),
            )
            .unwrap();

        assert_eq!(headers.get("Version").unwrap(), CODEX_CLIENT_VERSION);
        assert_eq!(headers.get(USER_AGENT).unwrap(), CODEX_USER_AGENT);
        assert_eq!(headers.get("originator").unwrap(), CODEX_ORIGINATOR);
    }

    #[tokio::test]
    async fn codex_preserves_data_urls_detail_and_image_order_without_fetching() {
        let first = "data:image/png;base64,iVBORw0KGgo=";
        let second = "data:image/jpeg;base64,/9j/";
        let mut body = json!({
            "input": [{
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": first, "detail": "high"}},
                    {"type": "input_text", "text": "between"},
                    {"type": "image_url", "image_url": second}
                ]
            }]
        });

        CodexExecutor::prefetch_images_in_request(&mut body)
            .await
            .unwrap();
        let content = body["input"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "input_image");
        assert_eq!(content[0]["image_url"], first);
        assert_eq!(content[0]["detail"], "high");
        assert_eq!(content[1]["text"], "between");
        assert_eq!(content[2]["type"], "input_image");
        assert_eq!(content[2]["image_url"], second);
        assert_eq!(content[2]["detail"], "auto");
    }

    #[tokio::test]
    async fn codex_rejects_malformed_remote_attachment_before_send() {
        let mut body = json!({
            "input": [{"role": "user", "content": [{
                "type": "image_url",
                "image_url": {"detail": "high"}
            }]}]
        });
        let error = CodexExecutor::prefetch_images_in_request(&mut body)
            .await
            .unwrap_err();
        assert!(matches!(error, ImagePrefetchError::InvalidAttachment(_)));
        assert_eq!(error.http_status(), 502);
    }

    #[test]
    fn test_codex_flattens_chat_function_tools() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let body = json!({
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "test_tool",
                    "description": "Test tool",
                    "parameters": {"type": "object", "properties": {}}
                }
            }]
        });

        let transformed = executor
            .transform_request_body(&body, "gpt-5.6-luna", false, None)
            .unwrap();
        assert_eq!(transformed["tools"][0]["name"], "test_tool");
        assert_eq!(transformed["tools"][0]["description"], "Test tool");
        assert!(transformed["tools"][0].get("function").is_none());
    }

    #[test]
    fn test_codex_web_search_injection_is_additive_and_idempotent() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let body = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "local_tool",
                    "parameters": {"type": "object", "properties": {}}
                }
            }]
        });

        let transformed = executor
            .transform_request_body(&body, "gpt-5.6-luna", true, Some("low"))
            .unwrap();
        let tools = transformed["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "local_tool");
        assert_eq!(tools[1]["type"], "web_search");
        assert_eq!(tools[1]["external_web_access"], true);
        assert_eq!(tools[1]["search_context_size"], "low");

        let existing = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "web_search", "external_web_access": false}]
        });
        let transformed = executor
            .transform_request_body(&existing, "gpt-5.6-luna", true, Some("high"))
            .unwrap();
        assert_eq!(transformed["tools"].as_array().unwrap().len(), 1);
        assert_eq!(transformed["tools"][0]["external_web_access"], false);
        assert!(transformed["tools"][0].get("search_context_size").is_none());

        let disabled = executor
            .transform_request_body(&body, "gpt-5.6-luna", true, None)
            .unwrap();
        assert_eq!(disabled["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_codex_web_search_respects_tool_choice_none() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let body = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tool_choice": "none"
        });

        let transformed = executor
            .transform_request_body(&body, "gpt-5.6-luna", true, Some("medium"))
            .unwrap();
        assert!(transformed.get("tools").is_none());
        assert_eq!(transformed["tool_choice"], "none");
    }

    #[test]
    fn test_codex_uses_output_text_for_assistant_history() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let body = json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "input_text", "text": "hello", "annotations": []}]}
            ]
        });

        let transformed = executor
            .transform_request_body(&body, "gpt-5.6-luna", false, None)
            .unwrap();
        assert_eq!(transformed["input"][1]["content"][0]["type"], "output_text");
        assert!(transformed["input"][1]["content"][0]
            .get("annotations")
            .is_none());
    }

    #[test]
    fn test_codex_converts_chat_tool_history() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let body = json!({
            "messages": [
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "test_tool", "arguments": "{}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "done"},
                {"role": "user", "content": "continue"}
            ]
        });

        let transformed = executor
            .transform_request_body(&body, "gpt-5.6-luna", false, None)
            .unwrap();
        assert_eq!(transformed["input"][0]["type"], "function_call");
        assert_eq!(transformed["input"][0]["call_id"], "call_1");
        assert_eq!(transformed["input"][0]["name"], "test_tool");
        assert_eq!(transformed["input"][0]["arguments"], "{}");
        assert_eq!(transformed["input"][1]["type"], "function_call_output");
        assert_eq!(transformed["input"][1]["call_id"], "call_1");
        assert_eq!(transformed["input"][2]["role"], "user");
        assert!(transformed["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item.get("role").and_then(Value::as_str) != Some("tool")));
    }

    #[test]
    fn test_codex_accepts_responses_string_input() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let body = json!({"input": "Hello"});

        let transformed = executor
            .transform_request_body(&body, "gpt-5.6-luna", false, None)
            .unwrap();

        assert_eq!(transformed["input"][0]["role"], "user");
        assert_eq!(transformed["input"][0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn test_codex_request_body_format() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "user", "content": "Hello, world!"}
            ],
            "stream": false,
            "temperature": 0.7
        });

        let result = executor
            .transform_request_body(&chat_body, "o4-mini-high", false, None)
            .unwrap();

        assert_eq!(result["model"], "o4-mini"); // suffix stripped
        assert_eq!(result["stream"], true); // forced
        assert_eq!(result["store"], false);
        assert_eq!(result["reasoning"]["effort"], "high");
        assert!(
            result.get("temperature").is_none(),
            "temperature should be stripped by allowlist"
        );

        // input should be an array of Response API items
        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Hello, world!");

        assert_eq!(result["instructions"], "");
    }

    #[test]
    fn test_codex_request_body_multiple_messages() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"},
                {"role": "user", "content": "How are you?"}
            ]
        });

        let result = executor
            .transform_request_body(&chat_body, "o4-mini", true, None)
            .unwrap();

        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "Hello");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["text"], "Hi there!");
        assert_eq!(input[2]["role"], "user");
        assert_eq!(input[2]["content"][0]["text"], "How are you?");
    }

    #[test]
    fn test_codex_request_body_converts_system_to_instructions() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello!"}
            ]
        });

        let result = executor
            .transform_request_body(&chat_body, "o4-mini", true, None)
            .unwrap();

        let input = result["input"].as_array().unwrap();
        assert_eq!(result["instructions"], "You are a helpful assistant.");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "Hello!");
        assert_eq!(input.len(), 1);
    }

    #[test]
    fn test_codex_sse_conversion() {
        let openai_sse = b"event: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\nevent: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}\n\nevent: response.done\ndata: {\"type\":\"response.done\"}\n";

        let result = convert_openai_sse_to_standard(openai_sse);
        let result_str = String::from_utf8(result).unwrap();

        assert!(result_str.contains("data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}"));
        assert!(result_str.contains("data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}"));
        assert!(result_str.contains("data: {\"type\":\"response.done\"}"));
    }

    #[test]
    fn test_codex_sse_conversion_empty() {
        let result = convert_openai_sse_to_standard(b"");
        assert!(result.is_empty());
    }

    #[test]
    fn test_codex_sse_conversion_standard_format_unchanged() {
        let standard_sse = b"data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n";
        let result = convert_openai_sse_to_standard(standard_sse);
        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.contains("data: {\"type\":\"content.delta\""));
    }

    #[test]
    fn test_build_url_base() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let url = executor.build_url("o4-mini");
        assert_eq!(url, "https://chatgpt.com/backend-api/codex/responses");
    }

    #[test]
    fn test_build_url_compact_suffix() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let url = executor.build_url("o4-mini_compact");
        assert_eq!(
            url,
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );
    }

    #[test]
    fn test_build_url_honors_configured_endpoint() {
        let node = ProviderNode {
            base_url: Some("http://127.0.0.1:1234/codex/responses/".into()),
            ..Default::default()
        };
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), Some(node)).unwrap();
        assert_eq!(
            executor.build_url("gpt-5.6-luna"),
            "http://127.0.0.1:1234/codex/responses"
        );
        assert_eq!(
            executor.build_url("gpt-5.6-luna_compact"),
            "http://127.0.0.1:1234/codex/responses/compact"
        );
    }

    #[test]
    fn structured_first_event_failure_is_classified_without_message_matching() {
        let event = br#"event: response.failed
data: {"type":"response.failed","response":{"error":{"code":"server_is_overloaded","message":"arbitrary localized text"}}}

"#;
        assert_eq!(
            codex_first_event_failure_status(event),
            Some(reqwest::StatusCode::SERVICE_UNAVAILABLE)
        );
        assert_eq!(
            codex_first_event_failure_status(
                br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"server_is_overloaded"}

"#
            ),
            None
        );
    }

    #[test]
    fn test_codex_omits_reasoning_defaults() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "model": "codex/o4-mini"
        });

        let out = executor
            .transform_request_body(&body, "o4-mini", false, None)
            .unwrap();

        assert_eq!(out["instructions"], "");
        assert!(out.get("reasoning").is_none());
        assert!(out.get("include").is_none());
    }

    #[test]
    fn test_codex_include_reasoning_when_effort() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        // effort high → include == ["reasoning.encrypted_content"]
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "reasoning": { "effort": "high" },
            "model": "codex/o4-mini"
        });
        let out = executor
            .transform_request_body(&body, "o4-mini", false, None)
            .unwrap();
        assert_eq!(out["include"], json!(["reasoning.encrypted_content"]));

        // effort none → NO include
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "reasoning": { "effort": "none" },
            "model": "codex/o4-mini"
        });
        let out = executor
            .transform_request_body(&body, "o4-mini", false, None)
            .unwrap();
        assert!(
            out.get("include").is_none(),
            "no include when effort is none"
        );
    }

    #[test]
    fn test_codex_rejects_swarm_only_ultra_effort() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "reasoning": { "effort": "ultra" },
            "model": "codex/o4-mini"
        });

        assert!(matches!(
            executor.transform_request_body(&body, "o4-mini", false, None),
            Err(CodexExecutorError::UnsupportedFormat(_))
        ));

        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "model": "codex/o4-mini"
        });
        assert!(matches!(
            executor.transform_request_body(&body, "o4-mini-ultra", false, None),
            Err(CodexExecutorError::UnsupportedFormat(_))
        ));
    }

    #[test]
    fn test_codex_service_tier_mapping() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        // fast → priority
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "service_tier": "fast",
            "model": "codex/o4-mini"
        });
        let out = executor
            .transform_request_body(&body, "o4-mini", false, None)
            .unwrap();
        assert_eq!(out["service_tier"], "priority");

        // non-priority, non-fast → dropped
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "service_tier": "flexible",
            "model": "codex/o4-mini"
        });
        let out = executor
            .transform_request_body(&body, "o4-mini", false, None)
            .unwrap();
        assert!(out.get("service_tier").is_none());
    }

    #[test]
    fn test_codex_parse_error_usage_limit_reached() {
        // resets_at is in seconds since epoch; use a far-future value.
        let body = json!({
            "error": { "type": "usage_limit_reached", "message": "Limit hit", "resets_at": 4_100_000_000u64 }
        });
        let err = CodexExecutor::parse_error(429, &body.to_string());
        assert_eq!(err.status, 429);
        assert!(
            err.resets_at_ms.is_some(),
            "resets_at_ms should be populated"
        );
        assert_eq!(err.message, "Limit hit");
    }
}
