//! Antigravity executor.
//!
//! Port of `open-sse/executors/antigravity.js`. Forwards Gemini-shaped
//! requests to Google's Antigravity Cloud Code endpoint
//! (`/v1internal:streamGenerateContent` or `/v1internal:generateContent`).
//!
//! Notes:
//! - Antigravity uses Gemini's request shape (`request.contents/tools/...`),
//!   not OpenAI's. This executor expects the body to already be in that
//!   shape (the request translator pipeline does the conversion).
//! - A fresh CLI session id is generated per request (donor parity).
//! - Tool function names are sanitised to Gemini's regex
//!   `[a-zA-Z_][a-zA-Z0-9_.:\-]{0,63}`.
//! - The `cleanJSONSchemaForAntigravity` schema-cleaning step from 9router
//!   is **NOT** ported here yet — it lives in `geminiHelper.js` and
//!   should be added once the gemini translator helper is ported. For
//!   now we forward tool parameters verbatim.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use rand::RngCore;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Map, Value};

use crate::core::config::app_constants::agy_cli_user_agent;
use crate::core::proxy::ProxyTarget;
use crate::core::utils::antigravity_project::antigravity_project_id;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

/// Default base URL for Antigravity's Cloud Code endpoint.
/// Chat traffic uses the daily host (bypasses prod 429); discovery
/// (loadCodeAssist/onboardUser) stays on PROD — see the OAuth control plane.
/// Ported from 9router v0.5.45 (fix(gemini): daily-cloudcode host switch).
pub const ANTIGRAVITY_BASE_URL: &str = "https://daily-cloudcode-pa.googleapis.com";

/// Antigravity caps maxOutputTokens at 64k regardless of what the caller
/// asks for; matches the upstream JS `MAX_ANTIGRAVITY_OUTPUT_TOKENS` (antigravity.js:21).
const MAX_ANTIGRAVITY_OUTPUT_TOKENS: u64 = 64_000;

/// Build the `requestId` for the Antigravity CLI request envelope.
/// Donor: `generateAntigravityRequestId` — always fresh `agent/{ms}/{rand4hex}`.
fn build_cli_request_id() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut bytes = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("agent/{now_ms}/{}", hex::encode(bytes))
}

/// Build a fresh CLI `sessionId`. Donor: random `-{0..9e18}` per request.
fn build_cli_session_id() -> String {
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut bytes);
    let value = u64::from_be_bytes(bytes) % 9_000_000_000_000_000_000u64;
    format!("-{value}")
}

#[derive(Clone)]
pub struct AntigravityExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum AntigravityExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    MissingCredentials(String),
}

impl From<reqwest::Error> for AntigravityExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for AntigravityExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for AntigravityExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for AntigravityExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for AntigravityExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct AntigravityExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct AntigravityExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transport: TransportKind,
}

impl AntigravityExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, AntigravityExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// Build the Antigravity CLI URL (always streaming — donor parity:
    /// unary `generateContent` 400s on some models, chatCore converts SSE→JSON).
    pub fn build_url(_stream: bool) -> String {
        Self::build_url_from_base(ANTIGRAVITY_BASE_URL, true)
    }

    fn build_url_from_base(base_url: &str, _stream: bool) -> String {
        format!(
            "{}/v1internal:streamGenerateContent?alt=sse",
            base_url.trim_end_matches('/')
        )
    }

    fn request_url(&self, stream: bool) -> String {
        let _ = stream;
        let configured_base = self
            .provider_node
            .as_ref()
            .and_then(|node| node.base_url.as_deref())
            .map(str::trim)
            .filter(|base_url| !base_url.is_empty());
        Self::build_url_from_base(configured_base.unwrap_or(ANTIGRAVITY_BASE_URL), true)
    }

    fn build_headers(
        access_token: &str,
        project_id: &str,
    ) -> Result<HeaderMap, AntigravityExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let auth = format!("Bearer {access_token}");
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&auth)?);

        // CLI identity only: pinned darwin/arm64, no X-Goog-Api-Client,
        // no Client-Metadata, no proxy/fingerprint headers upstream.
        let ua = agy_cli_user_agent();
        headers.insert("User-Agent", HeaderValue::from_str(&ua)?);
        if !project_id.trim().is_empty() {
            headers.insert(
                "x-goog-user-project",
                HeaderValue::from_str(project_id.trim())?,
            );
        }
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(
            "Accept-Encoding",
            HeaderValue::from_static("gzip, deflate, br"),
        );

        Ok(headers)
    }

    #[cfg(test)]
    fn build_headers_for_test(access_token: &str, project_id: &str) -> HeaderMap {
        Self::build_headers(access_token, project_id).expect("test headers")
    }

    /// Sanitize a tool function name so it matches Gemini's allowed
    /// pattern: `[a-zA-Z_][a-zA-Z0-9_.:\-]{0,63}`. Returns `_unknown`
    /// for empty input.
    fn sanitize_function_name(name: &str) -> String {
        if name.is_empty() {
            return "_unknown".to_string();
        }
        let mut s: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == ':' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if !s
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        {
            s.insert(0, '_');
        }
        s.chars().take(64).collect()
    }

    /// Perform the inbound-request transformation matching `transformRequest`
    /// in 9router's antigravity.js. Mutates `body` in place and returns the
    /// derived session id so caller can pass it through to headers.
    fn transform_request(
        body: &mut Value,
        credentials: &ProviderConnection,
    ) -> Result<String, AntigravityExecutorError> {
        // OpenAI clients may include stream_options even for non-streaming calls.
        // Google generateContent rejects that combination before processing the request.
        // Ported from 9router v0.5.45 (fix(antigravity): strip stream_options from
        // non-stream requests).
        let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        if !stream {
            if let Value::Object(map) = body {
                map.remove("stream_options");
            }
        }

        // Pull out request.contents and rewrite role/parts as needed.
        if let Some(request_obj) = body.get_mut("request").and_then(|v| v.as_object_mut()) {
            // Rewrite contents.
            if let Some(contents) = request_obj
                .get_mut("contents")
                .and_then(|v| v.as_array_mut())
            {
                for content in contents.iter_mut() {
                    let Some(co) = content.as_object_mut() else {
                        continue;
                    };
                    let parts_owned = co.get("parts").cloned();
                    let Some(parts_array) = parts_owned.as_ref().and_then(|p| p.as_array()) else {
                        continue;
                    };

                    let has_function_response = parts_array.iter().any(|p| {
                        p.as_object()
                            .map(|o| o.contains_key("functionResponse"))
                            .unwrap_or(false)
                    });
                    if has_function_response {
                        co.insert("role".into(), Value::String("user".to_string()));
                    }

                    // Strip thought-only parts.
                    let cleaned_parts: Vec<Value> = parts_array
                        .iter()
                        .filter(|p| {
                            let Some(o) = p.as_object() else {
                                return true;
                            };
                            let has_thought =
                                o.get("thought").map(|v| !v.is_null()).unwrap_or(false);
                            let has_function_call = o.contains_key("functionCall");
                            let has_thought_signature = o.contains_key("thoughtSignature");
                            let has_text = o.contains_key("text");
                            // Drop pure thought parts but keep thoughtSignature
                            // when paired with functionCall (Gemini 3+ requires it).
                            if has_thought && !has_function_call {
                                return false;
                            }
                            if has_thought_signature && !has_function_call && !has_text {
                                return false;
                            }
                            true
                        })
                        .cloned()
                        .collect();
                    // Ported from 9router's antigravity.js transformRequest:
                    // Gemini 3+ rejects functionCall parts without thoughtSignature.
                    // Clients (Claude Code, IDE) don't persist thoughtSignature in
                    // their history, so backfill the default signature on any
                    // functionCall part that arrives without one.
                    let needs_backfill = cleaned_parts.iter().any(|p| {
                        p.as_object()
                            .map(|o| {
                                o.contains_key("functionCall")
                                    && !o.contains_key("thoughtSignature")
                            })
                            .unwrap_or(false)
                    });
                    let final_parts: Vec<Value> = if needs_backfill {
                        cleaned_parts
                            .into_iter()
                            .map(|p| {
                                let Some(o) = p.as_object() else {
                                    return p;
                                };
                                if o.contains_key("functionCall") && !o.contains_key("thoughtSignature") {
                                    let mut backfilled = p.clone();
                                    if let Some(obj) = backfilled.as_object_mut() {
                                        obj.insert(
                                            "thoughtSignature".into(),
                                            Value::String(
                                                crate::core::translator::request::openai_to_gemini::DEFAULT_THINKING_AG_SIGNATURE.to_string(),
                                            ),
                                        );
                                    }
                                    backfilled
                                } else {
                                    p
                                }
                            })
                            .collect()
                    } else {
                        cleaned_parts
                    };
                    co.insert("parts".into(), Value::Array(final_parts));
                }
            }

            // Sanitize and merge tool function declarations into a single group.
            // Note: 9router's transformRequest does NOT cloak tool names — it
            // only merges, sanitizes function names, and cleans schemas. The
            // `_ide` suffixing / decoy injection lives in the (disabled)
            // cloakTools path, so it must NOT be applied here (parity .37).
            let merged_tools: Option<Vec<Value>> = if let Some(tools) =
                request_obj.get("tools").and_then(|v| v.as_array()).cloned()
            {
                let mut all_decls: Vec<Value> = Vec::new();
                for group in tools {
                    let Some(decls) = group.get("functionDeclarations").and_then(|v| v.as_array())
                    else {
                        continue;
                    };
                    for decl in decls {
                        let mut new_decl = decl.clone();
                        if let Some(obj) = new_decl.as_object_mut() {
                            let raw_name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            obj.insert(
                                "name".into(),
                                Value::String(Self::sanitize_function_name(raw_name)),
                            );
                            // Clean JSON schema for Antigravity API compatibility.
                            // Ported from 9router's antigravity.js transformRequest:
                            // `fn.parameters ? cleanJSONSchemaForAntigravity(structuredClone(fn.parameters)) : ...`
                            if let Some(params) = obj.get_mut("parameters") {
                                let cleaned = crate::core::translator::request::openai_to_gemini::clean_json_schema(params);
                                obj.insert("parameters".into(), cleaned);
                            } else {
                                // Provide an empty-but-valid schema if missing.
                                obj.insert(
                                    "parameters".into(),
                                    json!({
                                        "type": "object",
                                        "properties": {
                                            "reason": {"type": "string", "description": "Brief explanation"}
                                        },
                                        "required": ["reason"]
                                    }),
                                );
                            }
                        }
                        all_decls.push(new_decl);
                    }
                }
                if all_decls.is_empty() {
                    Some(Vec::new())
                } else {
                    Some(vec![json!({"functionDeclarations": all_decls})])
                }
            } else {
                None
            };

            if let Some(tools) = merged_tools {
                if tools.is_empty() {
                    request_obj.remove("tools");
                    request_obj.remove("toolConfig");
                } else {
                    request_obj.insert("tools".into(), Value::Array(tools));
                    request_obj.insert(
                        "toolConfig".into(),
                        json!({"functionCallingConfig": {"mode": "VALIDATED"}}),
                    );
                }
            }

            // Cap maxOutputTokens.
            let r#gen = request_obj
                .entry("generationConfig".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(gen_obj) = r#gen.as_object_mut() {
                let cap = MAX_ANTIGRAVITY_OUTPUT_TOKENS;
                if let Some(max_out) = gen_obj.get("maxOutputTokens").and_then(|v| v.as_u64()) {
                    if max_out > cap {
                        gen_obj.insert("maxOutputTokens".into(), Value::from(cap));
                    }
                }
            }

            // Drop safetySettings (Antigravity ignores them anyway and
            // some values cause 400s).
            request_obj.remove("safetySettings");

            // Resolve session id: fresh CLI random per request (donor parity).
            let session_id = build_cli_session_id();
            request_obj.insert("sessionId".into(), Value::String(session_id.clone()));
            return Ok(session_id);
        }

        // No `request` envelope → fresh CLI session id.
        Ok(build_cli_session_id())
    }

    pub async fn execute_request(
        &self,
        mut request: AntigravityExecutionRequest,
    ) -> Result<AntigravityExecutorResponse, AntigravityExecutorError> {
        let access_token = request
            .credentials
            .access_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AntigravityExecutorError::MissingCredentials("access_token required".to_string())
            })?
            .to_string();

        // The translator pipeline (OpenAi -> openai_to_antigravity_request) produces a flat
        // body {contents, tools, ...}.  Antigravity's Cloud Code endpoint requires the
        // Gemini-like body wrapped in a {"request": body} envelope.  If the body doesn't
        // already have a "request" key, wrap it here.
        if request.body.get("request").is_none() {
            let inner = std::mem::replace(&mut request.body, Value::Null);
            request.body = json!({"request": inner});
        }

        // C21: project metadata belongs to the canonical selected connection.
        // Generation is a pure local read; discovery is restricted to
        // setup/control-plane operations and never adds a warm-path RTT.
        let project_id = antigravity_project_id(&request.credentials).unwrap_or_default();

        let _session_id = Self::transform_request(&mut request.body, &request.credentials)?;

        // Add the top-level request envelope: project, model, userAgent,
        // requestType, requestId around the `request` sub-object (CLI profile).
        let request_type = "agent";
        let model_for_envelope = request
            .body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&request.model)
            .to_string();
        let request_id = build_cli_request_id();
        if let Some(obj) = request.body.as_object_mut() {
            obj.insert("project".into(), Value::String(project_id.clone()));
            obj.insert("model".into(), Value::String(model_for_envelope));
            obj.insert("userAgent".into(), Value::String("antigravity".to_string()));
            obj.insert(
                "requestType".into(),
                Value::String(request_type.to_string()),
            );
            obj.insert("requestId".into(), Value::String(request_id));
        }

        let url = self.request_url(request.stream);
        let headers = Self::build_headers(&access_token, &project_id)?;

        let client = self.pool.get("antigravity", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&request.body)
            .send()
            .await?;

        // C12: this executor owns protocol mapping, not temporal scheduling.
        // Preserve every HTTP response as a live, unconsumed body so the
        // request-scoped planner can inspect status, body, and Retry-After.
        Ok(AntigravityExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transport: TransportKind::Reqwest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_always_uses_streaming_path() {
        assert!(AntigravityExecutor::build_url(true).ends_with("streamGenerateContent?alt=sse"));
        assert!(AntigravityExecutor::build_url(false).ends_with("streamGenerateContent?alt=sse"));
    }

    #[test]
    fn sanitize_function_name_replaces_invalid_chars() {
        assert_eq!(
            AntigravityExecutor::sanitize_function_name("my:tool/name with space"),
            "my:tool_name_with_space"
        );
    }

    #[test]
    fn sanitize_function_name_prepends_underscore_if_starts_with_digit() {
        assert_eq!(AntigravityExecutor::sanitize_function_name("3foo"), "_3foo");
    }

    #[test]
    fn sanitize_function_name_truncates_to_64() {
        let long = "a".repeat(100);
        assert_eq!(AntigravityExecutor::sanitize_function_name(&long).len(), 64);
    }

    #[test]
    fn sanitize_function_name_handles_empty() {
        assert_eq!(AntigravityExecutor::sanitize_function_name(""), "_unknown");
    }

    #[test]
    fn transform_request_caps_max_output_tokens() {
        let mut body = json!({
            "request": {
                "contents": [],
                "generationConfig": {"maxOutputTokens": 1_000_000}
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert_eq!(
            body["request"]["generationConfig"]["maxOutputTokens"],
            64_000
        );
    }

    #[test]
    fn test_max_output_tokens_cap_64000() {
        // Guard test: values under the cap pass through; above are capped at 64k.
        let mut body = json!({
            "request": {
                "contents": [],
                "generationConfig": {"maxOutputTokens": 60_000}
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert_eq!(
            body["request"]["generationConfig"]["maxOutputTokens"],
            60_000
        );

        let mut body = json!({
            "request": {
                "contents": [],
                "generationConfig": {"maxOutputTokens": 200_000}
            }
        });
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert_eq!(
            body["request"]["generationConfig"]["maxOutputTokens"],
            64_000
        );
    }

    #[test]
    fn transform_request_strips_thought_only_parts() {
        let mut body = json!({
            "request": {
                "contents": [{
                    "role": "model",
                    "parts": [
                        {"thought": true, "text": ""},
                        {"text": "real content"}
                    ]
                }]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        let parts = body["request"]["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "real content");
    }

    #[test]
    fn transform_request_keeps_thought_signature_when_paired_with_function_call() {
        let mut body = json!({
            "request": {
                "contents": [{
                    "role": "model",
                    "parts": [
                        {"thoughtSignature": "abc", "functionCall": {"name": "x", "args": {}}}
                    ]
                }]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert_eq!(
            body["request"]["contents"][0]["parts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn transform_request_rewrites_role_for_function_response() {
        let mut body = json!({
            "request": {
                "contents": [{
                    "role": "tool",
                    "parts": [
                        {"functionResponse": {"name": "x", "response": {"result": "ok"}}}
                    ]
                }]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert_eq!(body["request"]["contents"][0]["role"], "user");
    }

    #[test]
    fn transform_request_merges_tools_into_single_group() {
        let mut body = json!({
            "request": {
                "contents": [],
                "tools": [
                    {"functionDeclarations": [{"name": "a", "parameters": {"type": "object"}}]},
                    {"functionDeclarations": [{"name": "b!?", "parameters": {"type": "object"}}]}
                ]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        let groups = body["request"]["tools"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        let decls = groups[0]["functionDeclarations"].as_array().unwrap();
        // JS transformRequest does NOT cloak: exactly the 2 original tools,
        // sanitized, with no renamed names or injected decoys.
        assert_eq!(decls.len(), 2, "no decoys should be injected");

        let names: Vec<&str> = decls
            .as_slice()
            .iter()
            .filter_map(|d| d.get("name").and_then(|v| v.as_str()))
            .collect();
        assert!(names.contains(&"a"), "expected a to be in merged decls");
        assert!(names.contains(&"b__"), "expected b__ to be in merged decls");
        assert!(
            !names.iter().any(|n| n.ends_with("_ide")),
            "no _ide suffix should be applied: {names:?}"
        );
        // toolConfig set when tools are present.
        assert_eq!(
            body["request"]["toolConfig"]["functionCallingConfig"]["mode"],
            "VALIDATED"
        );
    }

    #[test]
    fn transform_request_drops_safety_settings() {
        let mut body = json!({
            "request": {
                "contents": [],
                "safetySettings": [{"category": "x", "threshold": "BLOCK_NONE"}]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();
        assert!(body["request"].get("safetySettings").is_none());
    }

    #[test]
    fn flat_body_gets_wrapped_in_request_envelope() {
        let mut body = json!({
            "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
            "tools": [{"functionDeclarations": [{"name": "my_tool", "parameters": {"type": "object"}}]}]
        });

        if body.get("request").is_none() {
            let inner = std::mem::replace(&mut body, Value::Null);
            body = json!({"request": inner});
        }

        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();

        assert!(
            body.get("request").is_some(),
            "body should have request envelope"
        );

        let tools = body["request"]["tools"].as_array().expect("tools array");
        let decls = tools[0]["functionDeclarations"]
            .as_array()
            .expect("functionDeclarations");
        let names: Vec<&str> = decls
            .iter()
            .filter_map(|d| d.get("name").and_then(|v| v.as_str()))
            .collect();

        assert!(
            names.contains(&"my_tool"),
            "client tool should keep its bare name (no _ide suffix)"
        );
        assert!(
            !names.iter().any(|n| n.ends_with("_ide")),
            "no _ide suffix should be applied: {names:?}"
        );

        assert_eq!(
            body["request"]["contents"][0]["role"], "user",
            "contents should be preserved after wrapping"
        );
    }

    #[test]
    fn build_headers_uses_cli_identity() {
        let headers = super::AntigravityExecutor::build_headers_for_test("tok", "proj");
        let ua = headers
            .get("User-Agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ua.starts_with("antigravity/cli/"),
            "CLI UA expected, got {ua}"
        );
        assert!(headers.get("X-Goog-Api-Client").is_none());
        assert!(headers.get("Client-Metadata").is_none());
        assert!(headers.get("X-Machine-Session-Id").is_none());
        assert_eq!(
            headers
                .get("x-goog-user-project")
                .and_then(|v| v.to_str().ok()),
            Some("proj")
        );
    }

    #[test]
    fn cli_request_id_has_agent_shape() {
        let id = super::build_cli_request_id();
        assert!(
            id.starts_with("agent/"),
            "CLI id must start with agent/: {id}"
        );
        let parts: Vec<&str> = id.split('/').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[1].chars().all(|c| c.is_ascii_digit()));
        assert_eq!(parts[2].len(), 8);
    }

    #[test]
    fn cli_session_id_is_negative_number() {
        let id = super::build_cli_session_id();
        assert!(id.starts_with('-'));
        assert!(id[1..].chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn transform_request_preserves_system_prompt() {
        let system_text =
            "You are a Claude agent built for OpenCode. Preserve this client instruction.";
        let mut body = json!({
            "request": {
                "systemInstruction": {
                    "parts": [{"text": system_text}]
                },
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
            }
        });
        let creds = ProviderConnection::default();
        AntigravityExecutor::transform_request(&mut body, &creds).unwrap();

        let parts = body["request"]["systemInstruction"]["parts"]
            .as_array()
            .expect("systemInstruction.parts should be an array");
        let text = parts[0]["text"].as_str().expect("text should be a string");
        assert_eq!(text, system_text);
    }
}
