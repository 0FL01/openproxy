use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use tokio::time::Instant;

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::TryStreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};

use crate::core::account_fallback::{GenerationAttemptBudget, ProviderAttemptError};
use crate::core::chat::stream_to_json::ForcedSseAccumulator;
use crate::core::chat::RequestPlan;
use crate::core::executor::{
    diagnostic_body_limit, read_upstream_body, read_upstream_diagnostic, success_body_limit,
    BoundedBodyError, PreparedUpstreamBody, UpstreamResponse,
};
use crate::core::model::get_model_info;
use crate::core::model::models_dev::OpenCodeModelMetadata;
use crate::core::proxy::resolve_proxy_target;
use crate::core::stream_framing::{FrameError, SseFramer, TextStreamFrame, TextStreamFramer};
use crate::core::translator::helpers::image_helper::{
    ensure_final_request_size, fetch_image_as_base64, ImagePrefetchBudget, ImagePrefetchError,
};
use crate::core::translator::helpers::modality_helper::{
    capabilities_for_format, strip_unsupported_modalities, ModalityCapabilities,
};
use crate::core::translator::limits::StreamLimitError;
use crate::core::translator::registry::{self, Format};
use crate::core::translator::response_transform::{transform_sse_stream, transformer_for_provider};
use crate::core::utils::client_detector::{detect_client_tool, ClientTool};
use crate::core::utils::stream_flags::resolve_stream_flags;
use crate::oauth::token_refresh::{
    connection_credential_generation, CONNECTION_REFRESH_COORDINATOR,
};
use crate::server::application_logs::{error_kind, AttemptLog, RequestLogContext};
use crate::server::auth::{extract_api_key, require_api_key, require_api_key_with_reload};
use crate::server::state::AppState;
use crate::server::upstream_tps::UpstreamTpsObservation;
use crate::types::{ApiKey, AppDb, ProviderConnection, TokenUsage};

use super::auth_error_response;
use super::models_metadata::{Modality, ModelMetadataFacts, OpenCodeModelConfig};

/// Check whether the process should trust reverse-proxy forwarding headers
/// (`X-Forwarded-For`, `X-Real-IP`, `X-Forwarded-Proto`, etc.).
///
/// Set `TRUST_PROXY=true` in the environment to enable. **Default is `false`**
/// — when disabled, all forwarding headers are stripped from the incoming
/// request so that spoofed IPs / protocols from untrusted intermediaries
/// are never propagated upstream or used for rate-limiting decisions.
///
/// # Examples
///
/// ```ignore
/// TRUST_PROXY=true            # trust reverse-proxy headers
/// TRUST_PROXY=false           # strip them (default)
///                             # not set → same as false
/// ```
fn trust_proxy_enabled() -> bool {
    matches!(
        std::env::var("TRUST_PROXY").as_deref(),
        Ok("true") | Ok("1") | Ok("yes")
    )
}

/// Remove reverse-proxy forwarding headers from `headers` when
/// [`trust_proxy_enabled`] returns `false`.
///
/// This runs at the top of every chat-completions handler so that:
///   - `X-Forwarded-For` / `X-Real-IP` are not forwarded upstream.
///   - `X-Forwarded-Proto` is not used to infer TLS state.
///   - `X-Forwarded-Host` is not used to infer the target host.
///
/// When deployed directly (not behind nginx/Caddy/Traefik), stripping
/// these headers also prevents malicious clients from injecting them.
fn strip_forwarding_headers(headers: &mut HeaderMap) {
    if trust_proxy_enabled() {
        return;
    }
    // Common headers set by reverse proxies (nginx, Caddy, Traefik, HAProxy,
    // Cloudflare, AWS ALB, …) that should not be trusted when TRUST_PROXY
    // is not explicitly enabled.
    static FORWARDING_HEADERS: &[&str] = &[
        "x-forwarded-for",
        "x-forwarded-proto",
        "x-forwarded-host",
        "x-forwarded-server",
        "x-real-ip",
    ];
    for &name in FORWARDING_HEADERS {
        headers.remove(name);
    }
}

/// Maximum time we'll wait for the next byte from an upstream SSE stream before
/// considering the connection stalled. 3 minutes matches what most providers
/// use for their keep-alive heartbeats (OpenAI sends a comment every ~30s,
/// Anthropic every ~60s, Gemini every ~30s — 180s is well past any of them).
const SSE_STALL_TIMEOUT: Duration = Duration::from_secs(180);

/// Persist a passively observed Claude quota snapshot for a connection.
///
/// Stored under `settings.extra["claudeQuotaSnapshot:<connectionId>"]` and
/// read by the usage endpoint as the claude quota source (no upstream call).
/// Fire-and-forget on the generation path: a persistence failure is logged
/// and otherwise ignored — quota display must never affect generation.
async fn observe_claude_quota_snapshot(
    state: &AppState,
    connection: &ProviderConnection,
    snapshot: Value,
) {
    let key = format!("claudeQuotaSnapshot:{}", connection.id);
    let was_new = {
        let current = state.db.snapshot();
        let unchanged = current.settings.extra.get(&key).is_some_and(|existing| {
            // Skip the write when only the observation timestamp moved.
            existing.get("quotas") == snapshot.get("quotas")
        });
        !unchanged
    };
    if !was_new {
        return;
    }
    let db = state.db.clone();
    let result = db
        .update_settings(move |settings| {
            settings.extra.insert(key, snapshot);
        })
        .await;
    if let Err(error) = result {
        tracing::debug!("claude quota snapshot persist failed: {error}");
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct RoutedResponseFormats {
    pub client: Format,
    pub upstream: Format,
    pub native_passthrough: bool,
}

fn mark_routed_response_formats(mut response: Response, plan: &RequestPlan) -> Response {
    response.extensions_mut().insert(RoutedResponseFormats {
        client: plan.source_format,
        upstream: plan.target_format,
        native_passthrough: plan.passthrough,
    });
    response
}

fn has_native_codex_web_search(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("type").and_then(Value::as_str) == Some("web_search"))
        })
}

fn codex_web_search_requires_mcp_error() -> ProviderAttemptError {
    ProviderAttemptError {
        status: StatusCode::BAD_REQUEST.as_u16(),
        message: "Codex web search is available only through /v1/mcp".to_string(),
        retry_after: None,
        upstream_body: serde_json::to_vec(&json!({
            "error": {
                "message": "Codex web search is available only through /v1/mcp",
                "type": "invalid_request_error",
                "code": "codex_web_search_requires_mcp"
            }
        }))
        .ok(),
    }
}

pub async fn cors_options() -> Response {
    cors_preflight_response("GET, POST, OPTIONS")
}

pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let model = body
        .as_ref()
        .ok()
        .and_then(|b| b.get("model").and_then(|m| m.as_str()));
    let request_id = crate::server::request_logger::new_request_id();
    let _log = crate::server::request_logger::RequestLog::start(
        "POST",
        "/v1/chat/completions",
        model,
        Some(request_id.clone()),
    );
    let response = with_cors_response(
        chat_completions_for_endpoint(state, headers, body, Some("/v1/chat/completions")).await,
    );
    let response = crate::server::request_logger::attach_request_id(response, &request_id);
    _log.watch(response)
}

pub async fn dashboard_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let body = normalize_dashboard_chat_request_body(&state, body);

    // Error-only terminal logging, same one-line policy as public routes.
    let request_id = crate::server::request_logger::new_request_id();
    let _log = crate::server::request_logger::RequestLog::start(
        "POST",
        "/api/dashboard/chat/completions",
        None,
        Some(request_id.clone()),
    );
    let response = chat_completions_impl(
        state,
        headers,
        body,
        Some("/api/dashboard/chat/completions"),
        false,
    )
    .await;
    let response = crate::server::request_logger::attach_request_id(response, &request_id);
    _log.watch(response)
}

fn normalize_dashboard_chat_request_body(
    state: &AppState,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, JsonRejection> {
    let Ok(Json(mut value)) = body else {
        return body;
    };

    let dashboard_stream = value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if dashboard_stream {
        if let Some(fields) = value.as_object_mut() {
            fields.insert("stream".into(), Value::Bool(false));
            fields.insert("__dashboard_stream".into(), Value::Bool(true));
        }
    }

    let Some(model) = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return Ok(Json(value));
    };

    if model.contains('/') {
        return Ok(Json(value));
    }

    let snapshot = state.db.snapshot();
    if snapshot.model_aliases.contains_key(model) {
        return Ok(Json(value));
    }

    let mut matches = snapshot
        .provider_connections
        .iter()
        .filter(|connection| connection.is_active.unwrap_or(true))
        .filter(|connection| provider_connection_supports_model(connection, model))
        .map(|connection| format!("{}/{}", connection.provider, model));

    let Some(rewritten_model) = matches.next() else {
        return Ok(Json(value));
    };
    if matches.next().is_some() {
        return Ok(Json(value));
    }

    if let Some(fields) = value.as_object_mut() {
        fields.insert("model".into(), Value::String(rewritten_model));
    }

    Ok(Json(value))
}

fn provider_connection_supports_model(connection: &ProviderConnection, model: &str) -> bool {
    if connection.provider == "a6api" {
        return crate::core::model::a6api_connection_supports_model(connection, model);
    }
    if connection.default_model.as_deref() == Some(model) {
        return true;
    }

    connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(Value::as_array)
        .is_some_and(|models| {
            models
                .iter()
                .filter_map(Value::as_str)
                .any(|item| item == model)
        })
}

/// Strip ASCII control characters (except tab, LF, CR) from every
/// `messages[].content` string in a chat request body before forwarding.
///
/// Raw NUL/DEL bytes break strict upstreams and exact-body mocks; tab and
/// newline are legitimate prompt formatting and must survive.
fn strip_message_control_chars(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages {
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        if let Value::String(text) = content {
            text.retain(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'));
        }
    }
}

fn inject_glm_stream_usage(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    if obj.get("stream").and_then(Value::as_bool) != Some(true)
        || obj.contains_key("stream_options")
    {
        return;
    }
    obj.insert(
        "stream_options".to_string(),
        serde_json::json!({"include_usage": true}),
    );
}

pub async fn chat_completions_for_endpoint(
    state: AppState,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
    endpoint: Option<&'static str>,
) -> Response {
    chat_completions_impl(state, headers, body, endpoint, true).await
}

async fn chat_completions_impl(
    state: AppState,
    mut headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
    endpoint: Option<&'static str>,
    require_api_key_auth: bool,
) -> Response {
    // Security: strip reverse-proxy forwarding headers unless TRUST_PROXY=true.
    // When running without a trusted reverse proxy (default), headers like
    // X-Forwarded-For / X-Real-IP / X-Forwarded-Proto are spoofable by any
    // client and must not be used for rate limiting, IP logging, or TLS
    // inference decisions downstream.
    strip_forwarding_headers(&mut headers);

    let presented_api_key = extract_api_key(&headers);
    let authenticated_api_key =
        if require_api_key_auth && state.db.snapshot().settings.require_api_key {
            match require_api_key_with_reload(&headers, &state.db).await {
                Ok(api_key) => Some(api_key),
                Err(error) => return auth_error_response(error),
            }
        } else {
            presented_api_key.as_deref().and_then(|key| {
                state
                    .db
                    .snapshot()
                    .api_key_map
                    .get(key)
                    .filter(|api_key| api_key.is_active())
                    .cloned()
            })
        };

    let Json(mut body) = match body {
        Ok(body) => body,
        Err(error) if error.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return json_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                super::LLM_BODY_TOO_LARGE_MESSAGE,
            )
        }
        Err(_) => return json_error_response(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    // Claude Code marks a 1M-context request as `<model>[1m]`. The marker is a
    // client-side annotation that matches no alias or `provider/model`
    // pair, so it must not reach model resolution or the request dies with an
    // invalid-model error. The actual 1M capability travels in the
    // `anthropic-beta` header, which is forwarded untouched.
    // Mirrors `stripModelContextMarker` in 9router's
    // `open-sse/utils/modelMarkers.js`, called at the top of
    // `src/sse/handlers/chat.js`.
    if let Some(model) = body.get("model").and_then(Value::as_str) {
        let (stripped, marker) =
            crate::core::translator::request::claude_format::strip_model_context_marker(
                model.trim(),
            );
        if marker.is_some() {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("model".to_string(), Value::String(stripped));
            }
        }
    }

    // Sanitize message content before model resolution/forwarding (see
    // `strip_message_control_chars`).
    strip_message_control_chars(&mut body);

    let Some(model_str) = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return json_error_response(StatusCode::BAD_REQUEST, "Missing model");
    };
    let model_str = model_str.as_str();
    if model_str.starts_with("combo:") {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "Combo routes are no longer supported",
        );
    }
    let request_log_context = authenticated_api_key.as_ref().map(|api_key| {
        RequestLogContext::new(
            state.db.clone(),
            api_key,
            endpoint.unwrap_or("/v1/chat/completions"),
        )
        .with_chat_session(&headers)
    });

    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model_str, &snapshot);

    // Kiro backend retired: fail loudly instead of silently misrouting.
    if resolved.provider.as_deref() == Some("kiro") {
        return json_error_response(StatusCode::GONE, "provider kiro retired");
    }
    // Cursor / Windsurf / Grok backends retired: fail loudly instead of silently misrouting.
    if matches!(
        resolved.provider.as_deref(),
        Some(
            "cursor"
                | "cu"
                | "windsurf"
                | "ws"
                | "grok-web"
                | "gw"
                | "grok-cli"
                | "gcli"
                | "gb"
                | "grok-build"
        )
    ) {
        return json_error_response(StatusCode::GONE, "provider retired");
    }

    // Claude/Anthropic subscription accounts only serve traffic that already
    // presents itself as Claude Code: either a genuine claude-cli client or
    // an OpenCode client running the openproxy-claude-mask plugin (which
    // pins the UA and sends the marker header). Unmasked clients must not
    // burn OAuth quota with harness fingerprints scanners flag.
    if matches!(
        resolved.provider.as_deref(),
        Some("claude") | Some("anthropic")
    ) && !claude_mask_client_allowed(&headers)
    {
        return json_error_response(
            StatusCode::FORBIDDEN,
            "claude models require the OpenCode mask plugin (openproxy-claude-mask.js) or a claude-cli client",
        );
    }

    // Convert headers once for client-tool detection and provider dispatch.
    let headers_map: std::collections::HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    let client_tool = detect_client_tool(&headers_map, &body);
    // Accept/stream preference is applied via resolve_stream_flags on the plan
    // (does NOT mutate body.stream when client set stream:true — 9router parity).
    let accept_header = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let mut plan = RequestPlan::new(
        endpoint,
        &body,
        resolved.provider.as_deref().unwrap_or(model_str),
        &resolved.model,
    );
    apply_stream_plan(&mut plan, &body, accept_header.as_deref(), client_tool);
    match execute_single_model(
        &state,
        body,
        model_str,
        presented_api_key.as_deref(),
        request_log_context.as_ref(),
        endpoint,
        &plan,
        client_tool,
        Some(&headers_map),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => attempt_error_response(error),
    }
}

/// Prefetch remote images in OpenAI/Claude message content arrays.
fn image_prefetch_attempt_error(error: ImagePrefetchError) -> ProviderAttemptError {
    ProviderAttemptError::new(
        error.http_status(),
        format!("Image prefetch failed: {error}"),
    )
}

fn should_prefetch_message_images(plan: &RequestPlan) -> bool {
    !plan.passthrough && plan.target_format.needs_image_prefetch()
}

/// Use the /v1/models metadata precedence. `None` means unknown or conflicting
/// capabilities; only an explicit text-only declaration justifies telling the
/// model that it cannot see images.
fn tool_image_support(
    snapshot: &AppDb,
    provider: &str,
    model: &str,
    published: Option<&OpenCodeModelMetadata>,
) -> Option<bool> {
    let catalog = crate::core::model::catalog::provider_catalog();
    let entry = catalog.find_model(provider, model);
    let mut metadata = OpenCodeModelConfig::from_facts(ModelMetadataFacts {
        name: None,
        context: None,
        input: None,
        output: None,
        capabilities: entry.and_then(|m| m.capabilities.as_deref()).unwrap_or(&[]),
        modalities: None,
        attachment: None,
        reasoning: None,
        tool_call: None,
        efforts: None,
    });
    if let Some(published) = published {
        metadata.overlay(OpenCodeModelConfig::from_facts(ModelMetadataFacts {
            name: None,
            context: None,
            input: None,
            output: None,
            capabilities: &published.capabilities,
            modalities: published
                .input_modalities
                .as_deref()
                .zip(published.output_modalities.as_deref()),
            attachment: published.attachment,
            reasoning: None,
            tool_call: None,
            efforts: None,
        }));
    }
    let static_alias = catalog
        .static_alias_for_provider(provider)
        .unwrap_or(provider);
    if let Some(custom) = snapshot.custom_models.iter().find(|custom| {
        custom.id.trim() == model
            && [provider, static_alias].contains(&custom.provider_alias.trim())
            && matches!(custom.r#type.as_str(), "" | "llm" | "chat")
    }) {
        if let Some(overrides) = custom
            .extra
            .get("opencode")
            .and_then(|value| serde_json::from_value::<OpenCodeModelConfig>(value.clone()).ok())
        {
            metadata.overlay(overrides);
        }
    }
    let has_image = metadata
        .modalities
        .as_ref()?
        .input
        .iter()
        .any(|modality| matches!(modality, Modality::Image));
    if metadata
        .attachment
        .is_some_and(|attachment| attachment != has_image)
    {
        return None;
    }
    Some(has_image)
}

async fn prefetch_images_in_messages(body: &mut Value) -> Result<(), ImagePrefetchError> {
    let mut budget = ImagePrefetchBudget::new(body)?;
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return Ok(());
    };
    let client = reqwest::Client::new();
    for msg in messages.iter_mut() {
        let content_array = match msg.get_mut("content") {
            Some(Value::Array(arr)) => arr,
            _ => continue,
        };
        for part in content_array.iter_mut() {
            if part.get("image_url").is_some() {
                let url = part
                    .get("image_url")
                    .and_then(|image_url| image_url.get("url"))
                    .and_then(Value::as_str)
                    .ok_or(ImagePrefetchError::InvalidAttachment(
                        "image_url.url must be a string",
                    ))?
                    .to_string();
                if url.starts_with("data:") {
                    budget.account_existing_data_url(&url)?;
                } else {
                    let fetched = fetch_image_as_base64(&client, &url, &mut budget).await?;
                    let image = part
                        .get_mut("image_url")
                        .and_then(Value::as_object_mut)
                        .ok_or(ImagePrefetchError::InvalidAttachment(
                            "image_url must be an object",
                        ))?;
                    image.insert("url".into(), Value::String(fetched.data_url));
                }
            }
            let direct_claude_source = part.get("type").and_then(Value::as_str) == Some("image");
            let source = if direct_claude_source {
                part.get("source")
            } else {
                part.get("image").and_then(|image| image.get("source"))
            };
            if direct_claude_source && source.is_none() {
                return Err(ImagePrefetchError::InvalidAttachment(
                    "Claude image.source must be an object",
                ));
            }
            if let Some(source) = source {
                match source.get("type").and_then(Value::as_str) {
                    Some("url") => {
                        let url = source
                            .get("url")
                            .and_then(Value::as_str)
                            .ok_or(ImagePrefetchError::InvalidAttachment(
                                "image.source.url must be a string",
                            ))?
                            .to_string();
                        let fetched = fetch_image_as_base64(&client, &url, &mut budget).await?;
                        let (mime_type, data) = fetched.into_claude_source();
                        let source = if direct_claude_source {
                            part.get_mut("source")
                        } else {
                            part.get_mut("image")
                                .and_then(|image| image.get_mut("source"))
                        }
                        .and_then(Value::as_object_mut)
                        .ok_or(ImagePrefetchError::InvalidAttachment(
                            "image.source must be an object",
                        ))?;
                        source.remove("url");
                        source.insert("type".into(), Value::String("base64".into()));
                        source.insert("media_type".into(), Value::String(mime_type));
                        source.insert("data".into(), Value::String(data));
                    }
                    Some("base64") => {
                        let data = source.get("data").and_then(Value::as_str).ok_or(
                            ImagePrefetchError::InvalidAttachment(
                                "image.source.data must be a string",
                            ),
                        )?;
                        let mime = source.get("media_type").and_then(Value::as_str).ok_or(
                            ImagePrefetchError::InvalidAttachment(
                                "image.source.media_type must be a string",
                            ),
                        )?;
                        let encoded_len = 5usize
                            .checked_add(mime.len())
                            .and_then(|value| value.checked_add(8))
                            .and_then(|value| value.checked_add(data.len()))
                            .ok_or(ImagePrefetchError::AggregateEncodedTooLarge {
                                limit: usize::MAX,
                            })?;
                        budget.account_existing_encoded_len(encoded_len)?;
                    }
                    Some(_) | None => {}
                }
            }
        }
    }
    budget.ensure_final_request(body)
}

/// Apply 9router stream decision to a RequestPlan (mutates stream + sse_to_json).
fn apply_stream_plan(
    plan: &mut RequestPlan,
    body: &Value,
    accept: Option<&str>,
    client_tool: Option<ClientTool>,
) {
    let body_stream = body.get("stream").and_then(Value::as_bool);
    let sp = resolve_stream_flags(
        body_stream,
        accept,
        &plan.provider,
        plan.source_format,
        client_tool,
    );
    plan.stream = sp.stream;
    plan.sse_to_json = sp.sse_to_json;
    tracing::debug!(
        target: "openproxy::chat",
        "STREAM provider={} stream={} client_requested={} force={} sse_to_json={}",
        plan.provider,
        sp.stream,
        sp.client_requested_streaming,
        sp.provider_forced,
        sp.sse_to_json,
    );
}

async fn execute_single_model(
    state: &AppState,
    request_body: Value,
    model_str: &str,
    api_key: Option<&str>,
    log_context: Option<&RequestLogContext>,
    endpoint: Option<&'static str>,
    base_plan: &RequestPlan,
    client_tool: Option<ClientTool>,
    client_headers: Option<&std::collections::HashMap<String, String>>,
) -> Result<Response, ProviderAttemptError> {
    let snapshot = state.db.snapshot();
    let mut plan = base_plan.clone();
    let mut published_tool_images = None;
    if crate::core::model::models_dev::is_opencode_provider(&plan.provider) {
        // C19: generation only reads the atomically published local snapshot.
        // Refresh and remote HTTP belong to bounded control-plane paths.
        let models = state.models_dev.load();
        let metadata = models
            .find(&plan.provider, plan.dispatch_model())
            .ok_or_else(|| ProviderAttemptError {
                status: 400,
                message: format!(
                    "Model {} is not present in the published models.dev snapshot for {}",
                    plan.dispatch_model(),
                    plan.provider
                ),
                retry_after: None,
                upstream_body: None,
            })?;
        published_tool_images =
            tool_image_support(&snapshot, &plan.provider, &plan.model, Some(metadata));
        plan.apply_opencode_metadata(metadata);
    }
    if plan.provider == "commandcode" {
        let catalog = state.commandcode_models.load();
        let metadata = catalog
            .find(plan.dispatch_model())
            .ok_or_else(|| ProviderAttemptError {
                status: 400,
                message: format!(
                    "Model {} is not present in the published Command Code catalog",
                    plan.dispatch_model()
                ),
                retry_after: None,
                upstream_body: None,
            })?;
        plan.apply_commandcode_metadata(metadata);
    }

    // C26: this is the single-consumer boundary after request planning. Keep
    // one immutable request-scoped source only inside the account-fallback
    // planner, where each eligible attempt still needs an independent body.
    let mut body = request_body;
    if let Some(fields) = body.as_object_mut() {
        fields.insert("model".into(), Value::String(plan.model.clone()));
    } else {
        return Err(ProviderAttemptError {
            status: 400,
            message: "Request body must be a JSON object".into(),
            retry_after: None,
            upstream_body: None,
        });
    }

    if plan.provider == "codex" && has_native_codex_web_search(&body) {
        return Err(codex_web_search_requires_mcp_error());
    }

    // Catalog stripList (image/audio) before modality strip — 9router translateRequest stripList
    if !plan.strip_list.is_empty() {
        let refs: Vec<&str> = plan.strip_list.iter().map(String::as_str).collect();
        registry::strip_content_types(&mut body, &refs);
    }

    // 1–2. Modality strip + image prefetch only when NOT passthrough (9router)
    let mut images_prefetched = false;
    if !plan.passthrough {
        let caps = capabilities_for_format(plan.source_format);
        strip_unsupported_modalities(&mut body, plan.source_format, &caps);

        if should_prefetch_message_images(&plan) {
            prefetch_images_in_messages(&mut body)
                .await
                .map_err(image_prefetch_attempt_error)?;
            images_prefetched = true;
        }
    }

    // Dispatch uses catalog upstreamModelId when set
    let dispatch_model = plan.dispatch_model().to_string();
    if let Some(fields) = body.as_object_mut() {
        fields.insert("model".into(), Value::String(dispatch_model.clone()));
    }

    // 3. Translate incompatible protocols or apply only documented native
    // adapter requirements. Passthrough depends on protocol capability, not
    // a recognized User-Agent.
    if plan.passthrough {
        tracing::debug!(
            target: "openproxy::chat",
            "PASSTHROUGH protocol={:?} provider={} client_hint={:?}",
            plan.source_format,
            plan.provider,
            client_tool
        );
        if plan.target_format == Format::Claude {
            crate::core::translator::request::claude_format::normalize_native_claude_request(
                &mut body,
                &dispatch_model,
            );
        }
    } else if plan.needs_translation() {
        // Include rawHeaders so protocol adapters can resolve request-scoped
        // session/continuation identifiers from client headers.
        let mut creds = json!({
            "provider": plan.provider,
        });
        if plan.source_format == Format::OpenAiResponses && plan.target_format == Format::OpenAi {
            let image_support =
                if crate::core::model::models_dev::is_opencode_provider(&plan.provider) {
                    published_tool_images
                } else {
                    tool_image_support(&snapshot, &plan.provider, &plan.model, None)
                };
            creds["acceptsToolImages"] = Value::Bool(
                !plan.strip_list.iter().any(|part| part == "image") && image_support == Some(true),
            );
            creds["modelLacksVision"] = Value::Bool(image_support == Some(false));
        }
        if let Some(headers) = client_headers {
            if let Some(obj) = creds.as_object_mut() {
                let raw: serde_json::Map<String, Value> = headers
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect();
                obj.insert("rawHeaders".into(), Value::Object(raw));
            }
        }
        let strip_refs: Vec<&str> = plan.strip_list.iter().map(String::as_str).collect();
        // Snapshot _customToolNames BEFORE translate_request_with_strip
        // strips it (translator-only metadata for the response path).
        let custom_tool_names = body
            .get("_customToolNames")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        registry::global_registry().translate_request_with_strip(
            plan.source_format,
            plan.target_format,
            &dispatch_model,
            &mut body,
            plan.stream,
            Some(&creds),
            if strip_refs.is_empty() {
                None
            } else {
                Some(&strip_refs)
            },
        );
        // Thread custom-tool names through to the response path (9router
        // chatCore.js:198 + streamingHandler customToolNames).
        if !custom_tool_names.is_empty() {
            let joined = custom_tool_names.join(",");
            if let Some(obj) = body.as_object_mut() {
                obj.insert("_customToolNames".into(), Value::String(joined));
            }
        }
    }

    // 3b. Re-apply an explicit model(level) suffix onto provider-native fields.
    crate::core::utils::thinking_suffix::reapply_thinking_after_translate(
        plan.target_format,
        &plan.provider,
        &dispatch_model,
        &mut body,
        plan.thinking_level.as_deref(),
        plan.stream,
    );

    // Sync stream flag onto body for executors that read body.stream
    if let Some(obj) = body.as_object_mut() {
        obj.insert("stream".into(), Value::Bool(plan.stream));
    }

    // Z.ai's and A6API's OpenAI-compatible streaming endpoints report the
    // final token usage chunk when `stream_options.include_usage` is enabled.
    // The target format is known here, so the flag cannot leak onto GLM's
    // Claude transport, which does not accept this field.
    if plan.target_format == Format::OpenAi
        && matches!(plan.provider.as_str(), "glm" | "glm-cn" | "a6api")
    {
        inject_glm_stream_usage(&mut body);
    }

    // Codex has one additional existing remote-image shape under `input`.
    // Resolve it once at the request boundary, never once per account attempt.
    if plan.provider == "codex" {
        crate::core::executor::CodexExecutor::prefetch_images_in_request(&mut body)
            .await
            .map_err(image_prefetch_attempt_error)?;
        images_prefetched = true;
    }

    if images_prefetched {
        ensure_final_request_size(&body).map_err(image_prefetch_attempt_error)?;
    }

    tracing::debug!(
        target: "openproxy::chat",
        "PLAN provider={} model={} upstream={} source={:?} target={:?} stream={} translate={} transport={:?} strip={:?}",
        plan.provider,
        plan.model,
        dispatch_model,
        plan.source_format,
        plan.target_format,
        plan.stream,
        plan.needs_translation(),
        plan.transport_base_url,
        plan.strip_list,
    );

    forward_with_provider_fallback(
        state,
        &plan.provider,
        &dispatch_model,
        body,
        api_key,
        log_context,
        endpoint,
        &plan,
        client_tool,
        client_headers,
    )
    .await
}

async fn forward_with_provider_fallback(
    state: &AppState,
    provider: &str,
    model: &str,
    mut request_body: Value,
    _api_key: Option<&str>,
    log_context: Option<&RequestLogContext>,
    endpoint: Option<&'static str>,
    plan: &RequestPlan,
    client_tool: Option<ClientTool>,
    client_headers: Option<&std::collections::HashMap<String, String>>,
) -> Result<Response, ProviderAttemptError> {
    let mut excluded = HashSet::new();
    // C46 reverse tool-name map (Claude Code → client), set when the
    // harness spoof renamed tool declarations; threaded to the streaming
    // response path so tool_calls return under the client's names.
    let mut harness_tool_name_map: Option<serde_json::Map<String, serde_json::Value>> = None;
    let mut last_error: Option<ProviderAttemptError> = None;
    let mut reloaded = false;
    let mut auth_recovery_used = false;
    // C27: the post-planning body becomes immutable after the stream/dashboard
    // fields below are normalized. DefaultExecutor transforms and serializes it
    // once, then shares the bounded bytes across eligible account attempts.
    let mut default_prepared_body: Option<PreparedUpstreamBody> = None;
    // C46 harness session: one per chat request, shared by all attempts.
    // Deterministic per (api key, UTC date) — live CC keeps one session id
    // for a whole CLI run (census: 3 unique ids across 628 requests); a
    // fresh UUID per request was itself a fingerprint. Prompt-id is stable
    // per hour (live: one per agent task, reused up to 167 requests), and
    // client-request-id is unique per chat request like the live CLI.
    // Without an authenticated key (require_api_key=false) each id falls
    // back to a fresh v4 UUID — the pre-change behavior.
    let harness_key_id =
        log_context.map(crate::server::application_logs::RequestLogContext::api_key_id);
    let claude_harness_session = match &harness_key_id {
        Some(key_id) => derive_harness_uuid(&format!(
            "openproxy:claude-harness:session:{key_id}:{}",
            chrono::Utc::now().format("%Y-%m-%d")
        )),
        None => uuid::Uuid::new_v4().to_string(),
    };
    let claude_harness_prompt_id = match &harness_key_id {
        Some(key_id) => derive_harness_uuid(&format!(
            "openproxy:claude-harness:prompt:{key_id}:{}",
            chrono::Utc::now().format("%Y-%m-%dT%H")
        )),
        None => uuid::Uuid::new_v4().to_string(),
    };
    let claude_harness_request_id = uuid::Uuid::new_v4().to_string();
    let codex_supporters = if provider == "codex" {
        Some(codex_supporters_after_cold_wait(state, model).await?)
    } else {
        None
    };
    let initial_snapshot = state.db.snapshot();
    let eligible_accounts = eligible_connection_count_with_supporters(
        &initial_snapshot,
        provider,
        model,
        codex_supporters.as_ref(),
    )
    .max(1);
    let endpoint_attempts_per_account = 1;
    // Every eligible account gets a bounded set of protocol endpoint surfaces.
    // One additional generation attempt is reserved for the sole request-scoped
    // 401/403 credential recovery. There is no cross-request state or sleep.
    let attempt_budget = GenerationAttemptBudget::new(
        eligible_accounts
            .saturating_mul(endpoint_attempts_per_account)
            .saturating_add(1),
    );

    // Extract custom-tool names (OpenAI Responses translator metadata).
    // Kept for the streaming response path; stripped from the body below.
    let custom_tool_names: Option<String> = request_body
        .as_object_mut()
        .and_then(|obj| obj.remove("_customToolNames"))
        .and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s),
            Value::Array(a) => {
                let names: Vec<String> = a
                    .iter()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect();
                if names.is_empty() {
                    None
                } else {
                    Some(names.join(","))
                }
            }
            _ => None,
        });

    loop {
        let snapshot = state.db.snapshot();
        let Some(mut connection) = select_connection_with_supporters(
            &snapshot,
            provider,
            model,
            &excluded,
            codex_supporters.as_ref(),
        ) else {
            if let Some(error) = last_error {
                return Err(error);
            }

            // Stale-snapshot recovery: if the CLI added a provider
            // connection while the server was running, the in-memory
            // snapshot won't have it. Reload from SQLite once and retry.
            if !reloaded {
                reloaded = true;
                if state.db.reload_snapshot().await.is_ok() {
                    continue;
                }
            }

            return Err(ProviderAttemptError {
                status: 400,
                message: format!("No credentials for provider: {provider}"),
                retry_after: None,
                upstream_body: None,
            });
        };

        if provider == "codex" {
            connection = crate::oauth::token_refresh::codex_connection_for_request(
                state.db.clone(),
                connection,
            )
            .await;
        }

        // 9router resolveTransport: pin multi-endpoint base URL for this request
        if let Some(ref base) = plan.transport_base_url {
            connection.runtime_transport = Some(crate::types::RuntimeTransport {
                base_url: Some(base.clone()),
            });
        }

        // get_model_info resolves openai-compatible/anthropic-compatible
        // nodes to their node NAME as the provider — match on name OR prefix
        // so the node-aware DefaultExecutor path is taken for both.
        let provider_node = snapshot
            .provider_nodes
            .iter()
            .find(|node| {
                node.id == provider
                    || node.prefix.as_deref() == Some(provider)
                    || (node.r#type.ends_with("-compatible") && node.name == provider)
            })
            .cloned();
        let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);

        let dashboard_stream = request_body
            .get("__dashboard_stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(fields) = request_body.as_object_mut() {
            fields.remove("__dashboard_stream");
        }

        // Stream flag already resolved on plan via resolve_stream_flags
        // (DeepSeek-TUI, forceStream, and Accept preference).
        let stream = plan.stream;
        if let Some(obj) = request_body.as_object_mut() {
            obj.insert("stream".into(), Value::Bool(stream));
        }

        if !attempt_budget.try_acquire() {
            return Err(last_error.unwrap_or_else(|| {
                ProviderAttemptError::new(
                    502,
                    format!(
                        "Generation attempt budget exhausted ({}/{})",
                        attempt_budget.used(),
                        attempt_budget.max()
                    ),
                )
            }));
        }

        let attempt_log = match log_context {
            Some(context) => {
                context
                    .start_connected_attempt(provider, model, Some(&connection.id))
                    .await
            }
            None => None,
        };

        use crate::core::executor::{
            AntigravityExecutionRequest, AntigravityExecutor, AzureExecutionRequest, AzureExecutor,
            CodexExecutionRequest, CodexExecutor, DefaultExecutor, DevinCliExecutor,
            DevinExecutionRequest, GithubExecutionRequest, GithubExecutor, KimchiExecutor,
            OpenCodeExecutionRequest, OpenCodeExecutor, OpenCodeTier, ProviderExecutionRequest,
            ProviderExecutionResponse, ProviderExecutor, TraeExecutionRequest, TraeExecutor,
            VertexExecutionRequest, VertexExecutor,
        };

        let is_codex_model = provider == "codex";
        let tps = attempt_log
            .as_ref()
            .map(AttemptLog::tps)
            .unwrap_or_default();
        exclude_synthetic_tps(provider, &tps);
        let executor_result: Result<ProviderExecutionResponse, ProviderAttemptError> = tps.timing().scope(async {
            if provider == "vertex" || provider == "vertex-partner" || provider == "vxp" {
                let executor = VertexExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Vertex executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(VertexExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Vertex execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if is_codex_model {
                let executor = CodexExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Codex executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_prefetched(
                        CodexExecutionRequest {
                            model: model.to_string(),
                            body: request_body.clone(),
                            stream,
                            credentials: connection.clone(),
                            proxy,
                        },
                        attempt_log.as_ref().map(AttemptLog::codex_cache),
                    )
                    .await
                    .map_err(|e| {
                        let status = match &e {
                            crate::core::executor::CodexExecutorError::ImagePrefetch(error) => {
                                error.http_status()
                            }
                            crate::core::executor::CodexExecutorError::UnsupportedFormat(_) => 400,
                            _ => 500,
                        };
                        ProviderAttemptError {
                            status,
                            message: format!("Codex execution failed: {:?}", e),
                            retry_after: None,
                            upstream_body: None,
                        }
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "github" {
                let executor = GithubExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Github executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(GithubExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Github execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "azure" {
                let executor = AzureExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Azure executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(AzureExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Azure execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if let Some(tier) = OpenCodeTier::from_provider(provider) {
                let executor = OpenCodeExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("OpenCode executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(OpenCodeExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                        raw_headers: client_headers
                            .into_iter()
                            .flatten()
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect(),
                        tier,
                        format: plan.target_format,
                        family: plan.model_family.clone(),
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("OpenCode execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "antigravity" {
                let executor = AntigravityExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                    status: 500,
                    message: format!("Antigravity executor creation failed: {:?}", e),
                    retry_after: None,
                    upstream_body: None,
                })?;
                let result = executor
                    .execute_request(AntigravityExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Antigravity execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "zed" {
                use crate::core::executor::{ZedExecutionRequest, ZedExecutor};
                let executor = ZedExecutor::new(state.client_pool.clone())
                    .unwrap_or_else(|e: std::convert::Infallible| match e {});
                let result = executor
                    .execute_request(ZedExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Zed execution failed: {}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "trae" {
                let executor = TraeExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(TraeExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Trae execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "devin-cli" || provider == "dv" {
                // ACP stdio executor — spawns `devin acp` (noAuth; the CLI
                // carries its own credentials) and bridges session/update
                // notifications to OpenAI SSE.
                let executor = DevinCliExecutor::new(state.client_pool.clone()).map_err(|e| {
                    ProviderAttemptError {
                        status: 500,
                        message: format!("Devin executor init failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    }
                })?;
                let result = executor
                    .execute_request(DevinExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Devin execution failed: {}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url.clone(),
                    headers: HeaderMap::new(),
                    transport: result.transport,
                })
            } else if provider == "kimchi" {
                let executor = KimchiExecutor::new(state.client_pool.clone(), provider_node);
                let result = executor
                    .execute(ProviderExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                        signal: None,
                        log: None,
                        proxy_options: None,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Kimchi execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "codebuddy-cn" || provider == "cbcn" {
                use crate::core::executor::CodeBuddyCNExecutor;
                let executor =
                    CodeBuddyCNExecutor::new(state.client_pool.clone(), provider_node.clone());
                let result = executor
                    .execute(ProviderExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream: true, // force stream (9router)
                        credentials: connection.clone(),
                        proxy,
                        signal: None,
                        log: None,
                        proxy_options: None,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("CodeBuddy CN execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "codebuddy-intl" || provider == "cbai" {
                use crate::core::executor::CodeBuddyIntlExecutor;
                let executor =
                    CodeBuddyIntlExecutor::new(state.client_pool.clone(), provider_node.clone());
                let result = executor
                    .execute(ProviderExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream: true, // registry forceStream (JS #11101 fix)
                        credentials: connection.clone(),
                        proxy,
                        signal: None,
                        log: None,
                        proxy_options: None,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("CodeBuddy intl execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "ollama" {
                use crate::core::executor::{OllamaExecutionRequest, OllamaExecutor};
                let executor = OllamaExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(OllamaExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Ollama execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else if provider == "mimo-free" || provider == "mmf" {
                use crate::core::executor::{MimoFreeExecutionRequest, MimoFreeExecutor};
                let executor = MimoFreeExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(MimoFreeExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("MimoFree execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            } else {
                let gateway_node = provider_node
                    .as_ref()
                    .is_some_and(|node| node.r#type == "anthropic-compatible");
                let executor = DefaultExecutor::new(
                    provider.to_string(),
                    state.client_pool.clone(),
                    provider_node,
                )
                .map_err(|e| ProviderAttemptError {
                    status: 500,
                    message: format!("Default executor creation failed: {:?}", e),
                    retry_after: None,
                    upstream_body: None,
                })?;
                let mut request_headers: BTreeMap<String, String> = client_headers
                    .into_iter()
                    .flatten()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                // C46: harness identity is connection-gated with a per-request
                // session id. The gate is evaluated on every account attempt
                // (cheap pure fn) and hoisted above the prepared-body reuse
                // check, so attempts 2..N keep the session header even when
                // the spoofed body bytes are shared. Live Claude Code always
                // pairs the header with `metadata.user_id.session_id`.
                let harness =
                    if crate::core::translator::request::claude_format::claude_harness_gated(
                        provider,
                        connection.auth_type.as_str(),
                        connection
                            .runtime_transport
                            .as_ref()
                            .and_then(|transport| transport.base_url.as_deref()),
                        gateway_node,
                    ) {
                        Some(
                            crate::core::translator::request::claude_format::ClaudeHarnessIdentity {
                                session_id: claude_harness_session.clone(),
                                client_ua: request_headers
                                    .iter()
                                    .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
                                    .map(|(_, value)| value.clone()),
                                trusted_mask: claude_mask_marker_present(&request_headers),
                            },
                        )
                    } else {
                        None
                    };
                apply_harness_session_header(
                    &mut request_headers,
                    harness.is_some(),
                    &claude_harness_session,
                );
                apply_harness_prompt_id(
                    &mut request_headers,
                    harness.is_some(),
                    &claude_harness_prompt_id,
                );
                apply_harness_request_id(
                    &mut request_headers,
                    harness.is_some(),
                    &claude_harness_request_id,
                );
                if default_prepared_body
                    .as_ref()
                    .is_none_or(|prepared| !executor.can_reuse_prepared_body(prepared, model))
                {
                    // The harness identity (if any) applies to the
                    // planning-fresh body clone before the executor prepares
                    // (and shares) upstream bytes. Account-independent by
                    // construction (fixed device id, per-request session), so
                    // no per-account re-prepare.
                    let scoped_body;
                    let body_for_prepare = match harness.as_ref() {
                        Some(identity) => {
                            let mut spoofed = request_body.clone();
                            crate::core::translator::request::claude_format::apply_claude_harness(
                                &mut spoofed,
                                identity,
                            );
                            // C46 tool aliasing: rename client tool names to
                            // their Claude Code forms on the spoofed body and
                            // remember the reverse map for the response path
                            // (CC name → client name), so tool loops survive
                            // the round trip. Unknown names pass through.
                            harness_tool_name_map = crate::core::translator::request::claude_format::apply_claude_code_tool_aliases(&mut spoofed);
                            scoped_body = spoofed;
                            &scoped_body
                        }
                        None => &request_body,
                    };
                    default_prepared_body = Some(
                        executor
                            .prepare_upstream_body(body_for_prepare, model)
                            .map_err(|err| err.into_provider_attempt_error())?,
                    );
                }
                let prepared = default_prepared_body
                    .as_ref()
                    .expect("prepared body was initialized");
                let result = executor
                    .execute_prepared(
                        model,
                        stream,
                        &connection,
                        proxy.as_ref(),
                        &request_headers,
                        prepared,
                    )
                    .await
                    .map_err(|err| err.into_provider_attempt_error())?;
                Ok(ProviderExecutionResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transport: result.transport,
                })
            }
        })
        .await;

        let execution = executor_result;

        match execution {
            Ok(result) => {
                let status = result.response.status();
                // Passive Claude quota observation (donor parity): every
                // upstream /v1/messages response carries
                // `anthropic-ratelimit-unified-*` utilization watermarks.
                // Riding the existing traffic replaces usage-endpoint
                // polling entirely — scope-limited grants (setup tokens)
                // can never authorize the usage endpoint anyway. Runs on
                // success and error statuses alike (429s still carry the
                // windows); failures never break generation.
                if matches!(provider, "claude" | "anthropic") {
                    if let Some(snapshot) =
                        crate::core::usage::quota_headers::claude_quota_snapshot_from_headers(
                            result.response.headers(),
                        )
                    {
                        observe_claude_quota_snapshot(state, &connection, snapshot).await;
                    }
                }
                if status.is_success() {
                    if dashboard_stream {
                        let response =
                            proxy_dashboard_sse(result.response, plan, attempt_log).await;
                        return Ok(response);
                    }
                    // forceStream + client non-stream → collect SSE → JSON (9router)
                    if plan.sse_to_json {
                        tracing::debug!(
                            target: "openproxy::chat",
                            "FORCE_STREAM sse_to_json provider={} model={}",
                            provider,
                            model
                        );
                        let response =
                            proxy_sse_to_json_response(result.response, model, plan, attempt_log)
                                .await;
                        return Ok(response);
                    }
                    if !stream {
                        let response =
                            proxy_response(result.response, provider, plan, attempt_log).await;
                        let response = mark_routed_response_formats(response, plan);
                        return Ok(response);
                    }
                    let normalize_for_dashboard =
                        endpoint == Some("/api/dashboard/chat/completions");
                    let response = proxy_response_with_pending_tracking(
                        result.response,
                        provider.to_string(),
                        model.to_string(),
                        normalize_for_dashboard,
                        plan,
                        custom_tool_names.clone(),
                        harness_tool_name_map.clone(),
                        attempt_log,
                    )
                    .await;
                    let response = mark_routed_response_formats(response, plan);
                    return Ok(response);
                }

                // 9router parity: retryAfter may come from the Retry-After header
                // OR the error JSON body (errorBody.retryAfter). Header wins; the
                // body is the fallback when a provider returns it only in JSON.
                // Anthropic unified rate-limit windows (Claude OAuth traffic): the
                // reset instant rides `anthropic-ratelimit-unified-*-reset` headers
                // that exist on every /v1/messages response, but a reset only means
                // "the client must wait" when the corresponding window is actually
                // rejected — allowed/overage windows on 200s must not synthesize a
                // Retry-After. Mirrors the donor's ParseClaudeRateLimitReset gate,
                // reduced to the decision we actually make (advisory client header
                // only; the planner never sleeps — C13).
                let header_retry_after = retry_after_from_headers(result.response.headers())
                    .or_else(|| {
                        if status.as_u16() == 429 {
                            unified_reset_retry_after(result.response.headers())
                        } else {
                            None
                        }
                    });
                let (message, upstream_body) =
                    extract_upstream_error_with_body(result.response).await;
                let body_retry_after = upstream_body
                    .as_deref()
                    .and_then(crate::core::account_fallback::parse_retry_after_from_body);
                let retry_after = header_retry_after.or(body_retry_after);
                let (diagnostic_code, diagnostic_message) =
                    upstream_error_diagnostic(status.as_u16(), upstream_body.as_deref());
                let policy_rejection =
                    crate::core::translator::request::claude_format::is_claude_oauth_policy_rejection(
                        status.as_u16(),
                        upstream_body.as_deref(),
                    );
                let refreshable_auth_failure =
                    is_refreshable_auth_failure(status, upstream_body.as_deref());
                let oauth_error_code =
                    crate::core::translator::request::claude_format::claude_oauth_error_code(
                        upstream_body.as_deref(),
                    );
                last_error = Some(ProviderAttemptError {
                    status: status.as_u16(),
                    message: message.clone(),
                    retry_after,
                    upstream_body,
                });
                if let Some(mut attempt_log) = attempt_log {
                    // C43: explicit error-kind literal per branch — never
                    // inferred from the status code. Policy rejections stay
                    // AUTH_FAILURE: classification is by body text, not status.
                    let error_kind = if policy_rejection || refreshable_auth_failure {
                        error_kind::AUTH_FAILURE
                    } else if matches!(status.as_u16(), 429) {
                        error_kind::RATE_LIMITED
                    } else if matches!(status.as_u16(), 400 | 413 | 422) {
                        error_kind::INVALID_REQUEST
                    } else {
                        error_kind::UPSTREAM_FAILURE
                    };
                    attempt_log
                        .record_error_details(diagnostic_code.as_deref(), &diagnostic_message);
                    attempt_log
                        .finish("error", Some(status.as_u16()), None, Some(error_kind))
                        .await;
                }

                // A body-invalid request is account-independent. Replaying it
                // across every configured credential only multiplies an
                // already-known client failure. Carve-out (C13 amend): a 400
                // carrying a Claude OAuth policy-rejection text is an
                // auth-failure, not a body failure — advance, don't terminate.
                if matches!(status.as_u16(), 400 | 413 | 422)
                    && !(status.as_u16() == 400 && policy_rejection)
                {
                    return Err(last_error.expect("upstream error recorded"));
                }
                // F3: persist policy rejections to the existing diagnostic
                // fields only. No new columns, no routing suppression (C14).
                if policy_rejection {
                    let policy_db = state.db.clone();
                    let policy_connection_id = connection.id.clone();
                    let policy_provider = connection.provider.clone();
                    let now = Utc::now().to_rfc3339();
                    let _ = policy_db
                        .update(move |snapshot| {
                            let Some(stored) = snapshot
                                .provider_connections
                                .iter_mut()
                                .find(|candidate| {
                                    candidate.id == policy_connection_id
                                        && candidate.provider == policy_provider
                                })
                            else {
                                return;
                            };
                            stored.last_error = Some(
                                "Claude OAuth credential was rejected by upstream policy. Please re-authorize the connection."
                                    .to_string(),
                            );
                            stored.last_error_at = Some(now);
                            stored.error_code =
                                Some("claude_oauth_policy_rejection".to_string());
                        })
                        .await;
                }
                // Structured `details.error_code` rejections (live fixture:
                // 403 oauth_scope_insufficient) cannot be fixed by a token
                // refresh; record them on the credential so the dashboard
                // shows why. Routing is untouched (C14).
                if status == StatusCode::FORBIDDEN {
                    if let Some(code) = oauth_error_code
                        .as_deref()
                        .filter(|code| code.starts_with("oauth_"))
                    {
                        let scope_db = state.db.clone();
                        let scope_connection_id = connection.id.clone();
                        let scope_provider = connection.provider.clone();
                        let scope_code = code.to_string();
                        let now = Utc::now().to_rfc3339();
                        let _ = scope_db
                            .update(move |snapshot| {
                                let Some(stored) = snapshot
                                    .provider_connections
                                    .iter_mut()
                                    .find(|candidate| {
                                        candidate.id == scope_connection_id
                                            && candidate.provider == scope_provider
                                    })
                                else {
                                    return;
                                };
                                stored.last_error = Some(format!(
                                    "Claude OAuth credential was rejected upstream: {scope_code}. Re-authorize the connection with the required scopes."
                                ));
                                stored.last_error_at = Some(now);
                                stored.error_code = Some(scope_code);
                            })
                            .await;
                    }
                }

                // C17A: the request-scoped planner is the sole foreground
                // recovery owner, and every caller joins the connection-scoped
                // coordinator. A plain 403 is not enough evidence to rotate a
                // credential: only structured token/authentication codes are
                // refreshable. The follow-up generation still consumes C13's
                // shared request budget at the top of the loop.
                if refreshable_auth_failure
                    && connection.refresh_token.is_some()
                    && !auth_recovery_used
                {
                    auth_recovery_used = true;
                    let observed_generation = connection_credential_generation(&connection);
                    match CONNECTION_REFRESH_COORDINATOR
                        .refresh_connection(
                            state.db.clone(),
                            &connection.provider,
                            &connection.id,
                            observed_generation,
                        )
                        .await
                    {
                        Ok(_) => {
                            // Re-select from a fresh canonical snapshot rather
                            // than retaining the stale request clone.
                            continue;
                        }
                        Err(error) => {
                            tracing::warn!(
                                provider = %connection.provider,
                                connection_id = %connection.id,
                                error = %error,
                                "foreground OAuth credential recovery failed"
                            );
                        }
                    }
                }

                excluded.insert(connection.id.clone());
                continue;
            }
            Err(error) => {
                if let Some(attempt_log) = attempt_log {
                    attempt_log.tps().invalidate();
                    let error_kind = match error.status {
                        401 | 403 => error_kind::AUTH_FAILURE,
                        429 => error_kind::RATE_LIMITED,
                        400 | 413 | 422 => error_kind::INVALID_REQUEST,
                        _ => error_kind::UPSTREAM_FAILURE,
                    };
                    attempt_log
                        .finish("error", Some(error.status), None, Some(error_kind))
                        .await;
                }
                last_error = Some(error);
                // Local/upstream payload-limit failures are body-dependent, not
                // credential-dependent. Replaying the same prepared body on
                // every account cannot succeed and would repeat serialization.
                if last_error.as_ref().is_some_and(|error| error.status == 413) {
                    return Err(last_error.expect("payload-limit error recorded"));
                }
                excluded.insert(connection.id.clone());
                continue;
            }
        }
    }
}

/// Give the background Codex discovery a bounded chance to publish a cold
/// account's models before declaring a previously selected model absent.
async fn codex_supporters_after_cold_wait(
    state: &AppState,
    model: &str,
) -> Result<HashSet<String>, ProviderAttemptError> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = state.db.snapshot();
        let supporters = state.codex_models.cached_supporters(model, &snapshot);
        if !supporters.is_empty() {
            return Ok(supporters);
        }
        let unpublished = snapshot.provider_connections.iter().any(|connection| {
            connection.provider == "codex"
                && connection.is_active()
                && connection_has_credentials(connection)
                && state
                    .codex_models
                    .published_for_connection(&snapshot, connection)
                    .is_none()
        });
        if !unpublished {
            return Err(ProviderAttemptError::new(
                400,
                format!(
                    "Codex model {model} is not present in the published catalog or explicit configuration"
                ),
            ));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(ProviderAttemptError::new(
                503,
                "Codex model catalog is still loading",
            ));
        }
        tokio::time::sleep_until((now + Duration::from_millis(200)).min(deadline)).await;
    }
}

/// C46 harness session header: live Claude Code always pairs
/// `x-claude-code-session-id` with `metadata.user_id.session_id`. When the
/// harness transform is active for this connection and the client sent no
/// session header of its own, inject the request-scoped session id so the
/// header matches the spoofed body metadata on every account attempt.
/// Client value wins; no-op for non-harness traffic.
fn apply_harness_session_header(
    request_headers: &mut BTreeMap<String, String>,
    harness_active: bool,
    session: &str,
) {
    if harness_active && !request_headers.contains_key("x-claude-code-session-id") {
        request_headers.insert("x-claude-code-session-id".to_string(), session.to_string());
    }
}

/// Deterministic, v4-shaped UUID for harness identities. Live Claude Code
/// session/prompt ids are UUIDv4 (census 625/625); a plain UUIDv5 would show
/// a version-5 nibble on every spoofed request, so the derived bytes are
/// re-stamped to the v4 version/variant layout.
fn derive_harness_uuid(seed: &str) -> String {
    let mut bytes = *uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, seed.as_bytes()).as_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// C46 harness prompt-id: live CC pairs a stable per-task prompt UUID with
/// its requests (612/628, reused up to 167 times); insert-if-missing keeps
/// a genuine client's own value.
fn apply_harness_prompt_id(
    request_headers: &mut BTreeMap<String, String>,
    harness_active: bool,
    prompt_id: &str,
) {
    if harness_active && !request_headers.contains_key("x-claude-code-prompt-id") {
        request_headers.insert("x-claude-code-prompt-id".to_string(), prompt_id.to_string());
    }
}

/// C46 harness client-request-id: unique per chat request (live 628/628),
/// stable across account-fallback attempts; insert-if-missing.
fn apply_harness_request_id(
    request_headers: &mut BTreeMap<String, String>,
    harness_active: bool,
    request_id: &str,
) {
    if harness_active && !request_headers.contains_key("x-client-request-id") {
        request_headers.insert("x-client-request-id".to_string(), request_id.to_string());
    }
}

/// A 401 is an explicit authentication failure. A 403 can also be returned for
/// authorization policy and quota failures, so it triggers credential rotation
/// only when the provider supplies a structured token/authentication code.
/// Free-text messages are intentionally not classified — except for the narrow
/// Claude OAuth policy-rejection predicate, where a refresh provably cannot
/// help (policy/transport/ban, not expiry).
fn is_refreshable_auth_failure(status: StatusCode, body: Option<&[u8]>) -> bool {
    if crate::core::translator::request::claude_format::is_claude_oauth_policy_rejection(
        status.as_u16(),
        body,
    ) {
        return false;
    }
    if status == StatusCode::UNAUTHORIZED {
        return true;
    }
    if status != StatusCode::FORBIDDEN {
        return false;
    }

    let Ok(payload) = body
        .and_then(|body| serde_json::from_slice::<Value>(body).ok())
        .ok_or(())
    else {
        return false;
    };
    let candidates = [
        payload.pointer("/error/code"),
        payload.pointer("/error/type"),
        payload.pointer("/error/status"),
        payload.pointer("/error/details/error_code"),
        payload.get("code"),
        payload.get("type"),
        payload.get("status"),
    ];
    candidates.into_iter().flatten().any(|value| {
        value.as_str().is_some_and(|code| {
            matches!(
                code.trim().to_ascii_lowercase().as_str(),
                "invalid_token"
                    | "token_expired"
                    | "expired_token"
                    | "authentication_error"
                    | "unauthenticated"
            )
        })
    })
}

// Collected paths observe originals during reads, rather than assigning the
// end-of-collection timestamp to a terminal that arrived earlier.
/// These executors return adapter-built OpenAI bodies, not source frames.
/// Devin's counters are local estimates; Zed/Trae have already translated and
/// collected the provider stream before this boundary can observe it.
fn exclude_synthetic_tps(provider: &str, tps: &UpstreamTpsObservation) {
    if matches!(provider, "zed" | "trae" | "devin-cli" | "dv") {
        tps.invalidate();
    }
}

async fn read_original_body(
    response: UpstreamResponse,
    format: Format,
    provider: &str,
    log: Option<&AttemptLog>,
) -> Result<Bytes, BoundedBodyError> {
    let tps = log.map(AttemptLog::tps);
    let cache = log
        .map(AttemptLog::codex_cache)
        .filter(|observation| observation.snapshot().is_some());
    let headers = response.headers();
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let streaming = ct.contains("text/event-stream") || ct.contains("application/x-ndjson");
    if !streaming || (tps.is_none() && cache.is_none()) {
        let bytes = read_upstream_body(response, success_body_limit()).await?;
        let read_at = std::time::Instant::now();
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(tps) = &tps {
                // A documented native envelope is still original provider
                // evidence. Borrow its payload before the later legacy unwrap.
                let original = if matches!(provider, "cline" | "clinepass")
                    && value.get("success").and_then(Value::as_bool) == Some(true)
                    && value.get("error").is_none_or(Value::is_null)
                {
                    value
                        .get("data")
                        .filter(|data| data.is_object())
                        .unwrap_or(&value)
                } else {
                    &value
                };
                tps.observe_json(format, original, read_at);
            }
            if let Some(cache) = &cache {
                cache.observe(None, Some(&value));
            }
        }
        return Ok(bytes);
    }
    let limit = success_body_limit();
    if is_identity_encoded(headers) {
        if let Some(declared) =
            declared_content_length(headers).filter(|length| *length > limit as u64)
        {
            return Err(BoundedBodyError::DeclaredTooLarge { declared, limit });
        }
    }
    let mut framer = format.text_stream_mode(Some(ct)).map(TextStreamFramer::new);
    let mut collected = Vec::new();
    let mut last_read = None;
    let mut framing_failed = false;
    let mut accept = |chunk: &[u8], read_at: std::time::Instant| -> Result<(), BoundedBodyError> {
        collected
            .len()
            .checked_add(chunk.len())
            .filter(|next| *next <= limit)
            .ok_or(BoundedBodyError::TooLarge { limit })?;
        collected
            .try_reserve(chunk.len())
            .map_err(|_| BoundedBodyError::Capacity { limit })?;
        collected.extend_from_slice(chunk);
        if framing_failed {
            return Ok(());
        }
        if let Some(framer) = framer.as_mut() {
            let mut feed = |segment: &[u8], at| {
                last_read = at;
                framer.feed(segment, |frame| {
                    if let Some(tps) = &tps {
                        tps.observe_payload(format, frame.event(), frame.payload(), at);
                    }
                    if let Some(cache) = &cache {
                        let value = frame
                            .payload()
                            .and_then(|payload| serde_json::from_str::<Value>(payload).ok());
                        cache.observe(frame.event(), value.as_ref());
                    }
                })
            };
            let result = match &tps {
                Some(tps) => tps.feed_segments(chunk, read_at, &mut feed),
                None => feed(chunk, Some(read_at)),
            };
            if result.is_err() {
                if let Some(tps) = &tps {
                    tps.invalidate();
                }
                // Framing remains observational on collected native paths.
                framing_failed = true;
            }
        }
        Ok(())
    };
    match response {
        UpstreamResponse::Reqwest(response) => {
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream
                .try_next()
                .await
                .map_err(|error| BoundedBodyError::Transport(error.to_string()))?
            {
                let read_at = std::time::Instant::now();
                accept(&chunk, read_at)?;
            }
        }
        UpstreamResponse::Hyper(response) => {
            let mut body = response.into_body();
            while let Some(frame) = body.frame().await {
                let read_at = std::time::Instant::now();
                let frame =
                    frame.map_err(|error| BoundedBodyError::Transport(error.to_string()))?;
                if let Ok(chunk) = frame.into_data() {
                    accept(&chunk, read_at)?;
                }
            }
        }
    }
    let eof_at = std::time::Instant::now();
    if let Some(framer) = framer.as_mut().filter(|_| !framing_failed) {
        let result = framer.finish(|frame| {
            if let Some(tps) = &tps {
                tps.observe_payload(format, frame.event(), frame.payload(), last_read);
            }
            if let Some(cache) = &cache {
                let value = frame
                    .payload()
                    .and_then(|payload| serde_json::from_str::<Value>(payload).ok());
                cache.observe(frame.event(), value.as_ref());
            }
        });
        if result.is_err() {
            if let Some(tps) = &tps {
                tps.invalidate();
            }
        }
    }
    if let Some(tps) = &tps {
        tps.clean_eof(format, eof_at);
    }
    Ok(Bytes::from(collected))
}

async fn proxy_dashboard_sse(
    response: UpstreamResponse,
    plan: &RequestPlan,
    attempt_log: Option<AttemptLog>,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let body_bytes = match read_original_body(
        response,
        plan.target_format,
        &plan.provider,
        attempt_log.as_ref(),
    )
    .await
    {
        Ok(body) => body,
        Err(error) => return collected_body_failure_response(error, attempt_log).await,
    };

    let token_usage = extract_token_usage_from_bytes(&body_bytes);
    if let Some(attempt_log) = attempt_log {
        attempt_log
            .finish("success", Some(status.as_u16()), token_usage.as_ref(), None)
            .await;
    }

    let text = extract_dashboard_assistant_text_from_bytes(&body_bytes);
    let sse_body = build_dashboard_sse_body(text.as_deref(), token_usage.as_ref());
    build_dashboard_sse_response(status, &headers, sse_body)
}

#[cfg(test)]
fn select_connection(
    snapshot: &AppDb,
    provider: &str,
    model: &str,
    excluded: &HashSet<String>,
) -> Option<ProviderConnection> {
    select_connection_with_supporters(snapshot, provider, model, excluded, None)
}

fn select_connection_with_supporters(
    snapshot: &AppDb,
    provider: &str,
    model: &str,
    excluded: &HashSet<String>,
    discovered_supporters: Option<&HashSet<String>>,
) -> Option<ProviderConnection> {
    let mut candidates: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection.provider == provider
                && connection.is_active()
                && connection_has_credentials(connection)
                && !excluded.contains(&connection.id)
                && discovered_supporters.map_or_else(
                    || connection_supports_model(connection, model),
                    |supporters| supporters.contains(&connection.id),
                )
        })
        .cloned()
        .collect();

    if candidates.is_empty() {
        // No stored connection. Inject a virtual one for noAuth free providers
        // (matches 9router's getProviderCredentials behavior).
        if is_no_auth_provider(provider) && !excluded.contains("noauth") {
            return Some(virtual_no_auth_connection(provider));
        }
        return None;
    }

    candidates.sort_by(|left, right| {
        (left.priority.unwrap_or(u32::MAX), left.id.as_str())
            .cmp(&(right.priority.unwrap_or(u32::MAX), right.id.as_str()))
    });
    candidates.into_iter().next()
}

fn eligible_connection_count_with_supporters(
    snapshot: &AppDb,
    provider: &str,
    model: &str,
    discovered_supporters: Option<&HashSet<String>>,
) -> usize {
    let count = snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection.provider == provider
                && connection.is_active()
                && connection_has_credentials(connection)
                && discovered_supporters.map_or_else(
                    || connection_supports_model(connection, model),
                    |supporters| supporters.contains(&connection.id),
                )
        })
        .count();
    if count == 0 && is_no_auth_provider(provider) {
        1
    } else {
        count
    }
}

fn is_no_auth_provider(provider: &str) -> bool {
    matches!(provider, "opencode" | "opencode-zen")
}

fn virtual_no_auth_connection(provider: &str) -> ProviderConnection {
    let mut connection = ProviderConnection::default();
    connection.id = "noauth".to_string();
    connection.provider = provider.to_string();
    connection.auth_type = "none".to_string();
    connection.name = Some("Public".to_string());
    connection.is_active = Some(true);
    connection.access_token = Some("public".to_string());
    connection
}

fn connection_has_credentials(connection: &ProviderConnection) -> bool {
    connection
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        || connection
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
}

fn connection_supports_model(connection: &ProviderConnection, model: &str) -> bool {
    if connection.provider == "a6api" {
        return crate::core::model::a6api_connection_supports_model(connection, model);
    }
    let enabled_models: Vec<_> = connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();

    if !enabled_models.is_empty() {
        return enabled_models
            .iter()
            .any(|value| model_ids_match(value, model));
    }

    connection
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none_or(|value| model_ids_match(value, model))
}

fn model_ids_match(advertised: &str, requested: &str) -> bool {
    let advertised = advertised.trim();
    let requested = requested.trim();

    advertised == requested || advertised.ends_with(&format!("/{requested}"))
}

struct ForcedSseCollection {
    status: StatusCode,
    accumulator: ForcedSseAccumulator,
    prefix_raw: Vec<u8>,
}

enum ForcedSseCollectionError {
    Body(BoundedBodyError),
    Stream(StreamLimitError),
}

fn feed_forced_sse_chunk(
    framer: &mut SseFramer,
    accumulator: &mut ForcedSseAccumulator,
    prefix_raw: &mut Vec<u8>,
    chunk: &[u8],
    wire_seen: &mut usize,
    wire_limit: usize,
    codex_cache: Option<&crate::core::executor::codex_cache::CodexCacheObservation>,
    tps: Option<(&UpstreamTpsObservation, Format, std::time::Instant)>,
) -> Result<(), ForcedSseCollectionError> {
    *wire_seen = wire_seen
        .checked_add(chunk.len())
        .filter(|next| *next <= wire_limit)
        .ok_or(ForcedSseCollectionError::Body(BoundedBodyError::TooLarge {
            limit: wire_limit,
        }))?;
    if !accumulator.saw_any_event() {
        prefix_raw.try_reserve(chunk.len()).map_err(|_| {
            ForcedSseCollectionError::Body(BoundedBodyError::Capacity { limit: wire_limit })
        })?;
        prefix_raw.extend_from_slice(chunk);
    }
    let mut ingest_error = None;
    let mut feed = |segment: &[u8], at| {
        framer.feed(segment, |event| {
            if let Some((tps, format, _)) = tps {
                tps.observe_payload(format, event.event(), event.data(), at);
            }
            if let Some(observation) = codex_cache {
                let value = event
                    .data()
                    .and_then(|data| serde_json::from_str::<Value>(data).ok());
                observation.observe(event.event(), value.as_ref());
            }
            if ingest_error.is_none() {
                ingest_error = accumulator.ingest(&event).err();
            }
        })
    };
    let result = match tps {
        Some((tps, _, read_at)) => tps.feed_segments(chunk, read_at, &mut feed),
        None => feed(chunk, None),
    };
    if result.is_err() || ingest_error.is_some() {
        if let Some((tps, _, _)) = tps {
            tps.invalidate();
        }
    }
    result.map_err(|error| ForcedSseCollectionError::Stream(frame_error_to_stream(error)))?;
    if let Some(error) = ingest_error {
        return Err(ForcedSseCollectionError::Stream(error));
    }
    if accumulator.saw_any_event() && !prefix_raw.is_empty() {
        prefix_raw.clear();
        prefix_raw.shrink_to_fit();
    }
    Ok(())
}

#[cfg(test)]
async fn collect_forced_sse(
    response: UpstreamResponse,
    codex_cache: Option<crate::core::executor::codex_cache::CodexCacheObservation>,
) -> Result<ForcedSseCollection, ForcedSseCollectionError> {
    collect_forced_sse_observed(response, codex_cache, None, Format::Codex).await
}

async fn collect_forced_sse_observed(
    response: UpstreamResponse,
    codex_cache: Option<crate::core::executor::codex_cache::CodexCacheObservation>,
    tps: Option<UpstreamTpsObservation>,
    format: Format,
) -> Result<ForcedSseCollection, ForcedSseCollectionError> {
    let status = response.status();
    let headers = response.headers().clone();
    let wire_limit = success_body_limit();
    if is_identity_encoded(&headers) {
        if let Some(declared) = declared_content_length(&headers) {
            if declared > wire_limit as u64 {
                return Err(ForcedSseCollectionError::Body(
                    BoundedBodyError::DeclaredTooLarge {
                        declared,
                        limit: wire_limit,
                    },
                ));
            }
        }
    }

    let mut framer = SseFramer::new();
    let mut accumulator = ForcedSseAccumulator::new();
    let mut prefix_raw = Vec::new();
    let mut wire_seen = 0;

    match response {
        UpstreamResponse::Reqwest(response) => {
            let mut stream = response.bytes_stream();
            loop {
                let chunk = tokio::time::timeout(
                    SSE_STALL_TIMEOUT,
                    futures_util::TryStreamExt::try_next(&mut stream),
                )
                .await
                .map_err(|_| {
                    ForcedSseCollectionError::Body(BoundedBodyError::Transport(
                        "upstream SSE stream stalled".to_string(),
                    ))
                })?
                .map_err(|error| {
                    ForcedSseCollectionError::Body(BoundedBodyError::Transport(error.to_string()))
                })?;
                let Some(chunk) = chunk else {
                    break;
                };
                let read_at = std::time::Instant::now();
                feed_forced_sse_chunk(
                    &mut framer,
                    &mut accumulator,
                    &mut prefix_raw,
                    &chunk,
                    &mut wire_seen,
                    wire_limit,
                    codex_cache.as_ref(),
                    tps.as_ref().map(|tps| (tps, format, read_at)),
                )?;
                if accumulator.is_terminal() {
                    break;
                }
            }
        }
        UpstreamResponse::Hyper(response) => {
            let mut body = response.into_body();
            loop {
                let frame = tokio::time::timeout(SSE_STALL_TIMEOUT, body.frame())
                    .await
                    .map_err(|_| {
                        ForcedSseCollectionError::Body(BoundedBodyError::Transport(
                            "upstream SSE stream stalled".to_string(),
                        ))
                    })?;
                let read_at = std::time::Instant::now();
                let Some(frame) = frame else {
                    break;
                };
                let frame = frame.map_err(|error| {
                    ForcedSseCollectionError::Body(BoundedBodyError::Transport(error.to_string()))
                })?;
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                feed_forced_sse_chunk(
                    &mut framer,
                    &mut accumulator,
                    &mut prefix_raw,
                    &data,
                    &mut wire_seen,
                    wire_limit,
                    codex_cache.as_ref(),
                    tps.as_ref().map(|tps| (tps, format, read_at)),
                )?;
                if accumulator.is_terminal() {
                    break;
                }
            }
        }
    }

    let eof_at = std::time::Instant::now();
    if !accumulator.is_terminal() {
        let mut finish_error = None;
        framer
            .finish(|event| {
                if let Some(tps) = &tps {
                    tps.observe_payload(format, event.event(), event.data(), tps.last_read_at());
                }
                if let Some(observation) = &codex_cache {
                    let value = event
                        .data()
                        .and_then(|data| serde_json::from_str::<Value>(data).ok());
                    observation.observe(event.event(), value.as_ref());
                }
                if finish_error.is_none() {
                    finish_error = accumulator.ingest(&event).err();
                }
            })
            .map_err(|error| ForcedSseCollectionError::Stream(frame_error_to_stream(error)))?;
        if let Some(error) = finish_error {
            return Err(ForcedSseCollectionError::Stream(error));
        }
    }

    if !accumulator.saw_any_event() {
        if let Some(tps) = &tps {
            if let Ok(value) = serde_json::from_slice::<Value>(&prefix_raw) {
                tps.observe_json(format, &value, eof_at);
            }
        }
        if let Some(observation) = &codex_cache {
            let value = serde_json::from_slice::<Value>(&prefix_raw).ok();
            observation.observe(None, value.as_ref());
        }
    }
    if let Some(tps) = &tps {
        tps.clean_eof(format, eof_at);
    }
    Ok(ForcedSseCollection {
        status,
        accumulator,
        prefix_raw,
    })
}

/// forceStream SSE→JSON: collect upstream SSE and collapse to chat.completion JSON.
async fn proxy_sse_to_json_response(
    response: UpstreamResponse,
    model: &str,
    plan: &RequestPlan,
    attempt_log: Option<AttemptLog>,
) -> Response {
    let codex_cache = attempt_log
        .as_ref()
        .map(AttemptLog::codex_cache)
        .filter(|observation| observation.snapshot().is_some());
    let tps = attempt_log.as_ref().map(AttemptLog::tps);
    let collected =
        match collect_forced_sse_observed(response, codex_cache, tps, plan.target_format).await {
            Ok(collected) => collected,
            Err(ForcedSseCollectionError::Body(error)) => {
                return collected_body_failure_response(error, attempt_log).await;
            }
            Err(ForcedSseCollectionError::Stream(error)) => {
                return stream_limit_failure_response(error, attempt_log).await;
            }
        };
    let status = collected.status;
    let saw_sse = collected.accumulator.saw_any_event();
    let json_body = match collected.accumulator.finish(Some(model)) {
        Ok(Some(value)) => value,
        Ok(None) => {
            if !saw_sse && !collected.prefix_raw.is_empty() {
                // Bare non-SSE JSON fallback (forced upstream ignored stream).
                // Single representation: prefix holds the complete JSON body.
                serde_json::from_slice(&collected.prefix_raw).unwrap_or_else(|_| {
                    json!({
                        "error": {
                            "message": "Failed to convert forced SSE stream to JSON",
                            "type": "server_error",
                            "code": "sse_to_json_failed"
                        }
                    })
                })
            } else {
                // SSE with no convertible content, or comment-only stream.
                json!({
                    "error": {
                        "message": "Failed to convert forced SSE stream to JSON",
                        "type": "server_error",
                        "code": "sse_to_json_failed"
                    }
                })
            }
        }
        Err(error) => {
            return stream_limit_failure_response(error, attempt_log).await;
        }
    };

    let out = Bytes::from(serde_json::to_vec(&json_body).unwrap_or_default());

    let usage = extract_token_usage_from_bytes(&out);
    if let Some(attempt_log) = attempt_log {
        attempt_log
            .finish("success", Some(status.as_u16()), usage.as_ref(), None)
            .await;
    }
    let resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(out))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to build response",
            )
                .into_response()
        });
    let _ = plan; // reserved for future format-specific collapse
    with_cors_response(resp)
}

async fn proxy_response(
    response: UpstreamResponse,
    provider: &str,
    plan: &RequestPlan,
    attempt_log: Option<AttemptLog>,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let body_bytes = match read_original_body(
        response,
        plan.target_format,
        provider,
        attempt_log.as_ref(),
    )
    .await
    {
        Ok(body) => body,
        Err(error) => return collected_body_failure_response(error, attempt_log).await,
    };

    // 9router parity (open-sse/handlers/chatCore/nonStreamingHandler.js +
    // open-sse/shared/clineEnvelope.js unwrapClineEnvelope): unwrap before any
    // consumer reads choices/usage so non-stream clients get a bare OpenAI
    // body. No-op unless the provider opts
    // in via transport.quirks.clineEnvelope (cline/clinepass).
    let unenveloped_body = unwrap_cline_envelope(&body_bytes, provider);

    let token_usage = extract_token_usage_from_bytes(unenveloped_body.as_ref());

    // 9router parity: translate non-streaming response body when source
    // and target formats differ (handleNonStreamingResponse).
    // For Responses API format (Codex), the raw body is a response.completed JSON,
    // not SSE chunks. We parse it directly instead of using the streaming SSE transform.
    let translated_body = if plan.needs_translation() {
        if plan.target_format == registry::Format::OpenAiResponses
            || plan.target_format == registry::Format::Codex
        {
            // The Codex/Responses API returns a response.completed JSON body for non-streaming.
            // Parse out the text content and build a proper chat.completion response.
            translate_codex_non_streaming(unenveloped_body.as_ref())
                .unwrap_or_else(|| unenveloped_body.clone())
        } else if plan.target_format == registry::Format::Claude
            && plan.source_format == registry::Format::OpenAi
            && serde_json::from_slice::<Value>(unenveloped_body.as_ref()).is_ok()
        {
            // GitHub Copilot Claude /v1/messages (and other Claude-upstream
            // non-stream paths): full Messages JSON → chat.completion.
            match serde_json::from_slice::<Value>(unenveloped_body.as_ref()) {
                Ok(mut val) => {
                    crate::core::translator::response::non_streaming::claude_to_openai_non_streaming(
                            &mut val,
                        );
                    Bytes::from(
                        serde_json::to_vec(&val).unwrap_or_else(|_| unenveloped_body.to_vec()),
                    )
                }
                Err(_) => unenveloped_body.clone(),
            }
        } else {
            use crate::core::translator::registry::ResponseTransformState;
            let mut state = ResponseTransformState::default();
            let mut chunks = match registry::global_registry().translate_response(
                plan.target_format,
                plan.source_format,
                unenveloped_body.as_ref(),
                &mut state,
            ) {
                Ok(chunks) => chunks,
                Err(error) => {
                    if let Some(attempt_log) = attempt_log {
                        attempt_log.tps().invalidate();
                        attempt_log
                            .finish(
                                "error",
                                Some(StatusCode::BAD_GATEWAY.as_u16()),
                                None,
                                Some(error_kind::LOCAL_FAILURE),
                            )
                            .await;
                    }
                    return with_cors_response(
                        (
                            StatusCode::BAD_GATEWAY,
                            [(header::CONTENT_TYPE, "application/json")],
                            json!({
                                "error": {
                                    "message": error.message,
                                    "type": "upstream_error",
                                    "code": error.code
                                }
                            })
                            .to_string(),
                        )
                            .into_response(),
                    );
                }
            };
            chunks.extend(registry::global_registry().finish_stream(
                plan.target_format,
                plan.source_format,
                &mut state,
            ));
            if let Some(error) = state.failure.clone() {
                if let Some(attempt_log) = attempt_log {
                    attempt_log.tps().invalidate();
                    attempt_log
                        .finish(
                            "error",
                            Some(StatusCode::BAD_GATEWAY.as_u16()),
                            None,
                            Some(error_kind::LOCAL_FAILURE),
                        )
                        .await;
                }
                return with_cors_response(
                    (
                        StatusCode::BAD_GATEWAY,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({
                            "error": {
                                "message": error.message,
                                "type": "upstream_error",
                                "code": error.code
                            }
                        })
                        .to_string(),
                    )
                        .into_response(),
                );
            }
            if !chunks.is_empty() {
                let mut result = String::new();
                for chunk in &chunks {
                    if let Some(data) = chunk.strip_prefix("data: ") {
                        result = data.to_string();
                        if result == "[DONE]" {
                            continue;
                        }
                    }
                }
                if result.is_empty() {
                    unenveloped_body.clone()
                } else {
                    Bytes::from(result)
                }
            } else {
                unenveloped_body.clone()
            }
        }
    } else {
        unenveloped_body.clone()
    };
    let final_body = Body::from(translated_body);

    if let Some(attempt_log) = attempt_log {
        attempt_log
            .finish("success", Some(status.as_u16()), token_usage.as_ref(), None)
            .await;
    }

    build_proxied_response(status, &headers, final_body)
}

/// Unwrap Cline's non-stream envelope: {"success":true,"data":{...choices...}}.
///
/// Port of 9router `open-sse/shared/clineEnvelope.js` `unwrapClineEnvelope`
/// (v0.5.75): scoped to providers opting in via `transport.quirks.clineEnvelope`
/// (cline/clinepass) so no other provider's body is ever rewritten. The error
/// envelope ({"success":false,...}) never matches and passes through untouched.
fn unwrap_cline_envelope(body: &[u8], provider: &str) -> Bytes {
    if !matches!(provider, "cline" | "clinepass") {
        return Bytes::copy_from_slice(body);
    }
    let Ok(val) = serde_json::from_slice::<Value>(body) else {
        return Bytes::copy_from_slice(body);
    };
    let Some(success) = val.get("success") else {
        return Bytes::copy_from_slice(body);
    };
    if success != &Value::Bool(true) {
        return Bytes::copy_from_slice(body);
    }
    let data = match val.get("data") {
        Some(Value::Object(_)) => val.get("data").unwrap().clone(),
        _ => return Bytes::copy_from_slice(body),
    };
    serde_json::to_vec(&data).map_or_else(|_| Bytes::copy_from_slice(body), Bytes::from)
}

/// Translate a non-streaming Codex/Responses API response into standard Chat Completions format.
///
/// The Codex backend returns response.completed JSON for non-streaming requests:
/// ```json
/// {"type":"response.completed","response":{"output":[{"type":"message","content":[{"type":"output_text","text":"Hello"}]}],"usage":{"input_tokens":10,"output_tokens":5}}}
/// ```
///
/// This extracts the text content and builds a proper chat.completion response.
fn translate_codex_non_streaming(body: &[u8]) -> Option<Bytes> {
    let val: serde_json::Value = serde_json::from_slice(body).ok()?;

    // Navigate: response > output > [0] > content > [{output_text}]
    let output = val.pointer("/response/output").and_then(|v| v.as_array())?;

    let mut text_parts: Vec<String> = Vec::new();
    for item in output {
        let content = item.get("content").and_then(|v| v.as_array())?;
        for part in content {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                text_parts.push(text.to_string());
            }
        }
    }

    let content_text = text_parts.join("");

    // Extract usage
    let usage = val.pointer("/response/usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let response = serde_json::json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000")),
        "object": "chat.completion",
        "created": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        "model": "codex",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content_text,
            },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    });

    serde_json::to_string(&response).ok().map(Bytes::from)
}

async fn proxy_response_with_pending_tracking(
    response: UpstreamResponse,
    provider: String,
    model: String,
    normalize_for_dashboard: bool,
    plan: &RequestPlan,
    custom_tool_names: Option<String>,
    tool_name_map: Option<serde_json::Map<String, serde_json::Value>>,
    mut attempt_log: Option<AttemptLog>,
) -> Response {
    // Extract formats before stream closure to avoid lifetime issues
    let needs_stream_translation = plan.needs_translation();
    let stream_source_format = plan.source_format;
    let stream_target_format = plan.target_format;
    let stop_on_response_completed = stream_target_format == Format::OpenAiResponses
        && (crate::core::model::models_dev::is_opencode_provider(&provider)
            || (provider == "codex"
                && stream_source_format == Format::Claude
                && !normalize_for_dashboard));
    let status = response.status();
    let headers = response.headers().clone();

    // 9router streamingHandler: reject non-SSE content-types when client expects stream
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if !ct.is_empty()
        && !ct.contains("text/event-stream")
        && !ct.contains("application/octet-stream")
        && !ct.contains("application/x-ndjson")
        && (ct.contains("text/html")
            || ct.contains("application/json")
            || ct.contains("text/plain"))
    {
        // Collect body and return structured error instead of piping garbage as SSE
        let diagnostic = read_upstream_diagnostic(response, diagnostic_body_limit()).await;
        let mut msg = String::from_utf8_lossy(&diagnostic.bytes)
            .chars()
            .take(500)
            .collect::<String>();
        if diagnostic.truncated {
            msg.push_str(" [openproxy: upstream diagnostic body truncated]");
        } else if diagnostic.transport_failed {
            msg.push_str(
                " [openproxy: upstream diagnostic body incomplete after transport failure]",
            );
        }
        tracing::warn!(
            target: "openproxy::chat",
            "STREAM_GUARD non-SSE content-type={} status={} body_snip={}",
            ct,
            status.as_u16(),
            msg.chars().take(120).collect::<String>()
        );
        if let Some(attempt_log) = attempt_log.take() {
            attempt_log
                .finish(
                    "error",
                    Some(StatusCode::BAD_GATEWAY.as_u16()),
                    None,
                    Some(error_kind::UPSTREAM_FAILURE),
                )
                .await;
        }
        let err = json!({
            "error": {
                "message": format!("Upstream returned non-SSE content-type '{ct}': {msg}"),
                "type": "server_error",
                "code": "upstream_non_sse"
            }
        });
        return with_cors_response(
            (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "application/json")],
                err.to_string(),
            )
                .into_response(),
        );
    }
    // Dashboard normalization follows the resolved upstream wire format, not
    // the configurable provider name. Custom Gemini-compatible routes, Vertex,
    // and Antigravity all carry Gemini events.
    let transformer = normalize_for_dashboard
        .then(|| dashboard_transformer_for_format(stream_target_format))
        .flatten();
    let passthrough = transformer.is_none() && !needs_stream_translation;
    let body = match response {
        UpstreamResponse::Reqwest(response) => {
            let provider = provider.clone();
            let model = model.clone();
            let custom_tool_names = custom_tool_names.clone();
            let tool_name_map = tool_name_map.clone();
            let mut attempt_log = attempt_log;
            let stream = async_stream::stream! {
                let mut upstream = response.bytes_stream();
                let mut dispatch = StreamDispatch::new(
                    stream_target_format,
                    stream_source_format,
                    &ct,
                    transformer,
                    custom_tool_names.as_deref(),
                    tool_name_map,
                    stop_on_response_completed,
                );
                dispatch.codex_cache = attempt_log.as_ref().map(AttemptLog::codex_cache)
                    .filter(|observation| observation.snapshot().is_some());
                dispatch.tps = attempt_log.as_ref().map(AttemptLog::tps);
                loop {
                    let next = tokio::time::timeout(SSE_STALL_TIMEOUT, upstream.try_next()).await;
                    match next {
                        Err(_elapsed) => {
                            // Upstream went silent for SSE_STALL_TIMEOUT; treat
                            // as an error so the client can retry.
                            tracing::warn!(
                                target: "openproxy::chat::stream",
                                provider = %provider,
                                model = %model,
                                "SSE stalled, closing stream"
                            );
                            let usage = dispatch.usage.clone();
                            if let Some(log) = attempt_log.take() {
                                log.tps().invalidate();
                                log.finish("error", Some(502), usage.as_ref(), Some(error_kind::UPSTREAM_FAILURE)).await;
                            }
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(dispatch.streaming_error(
                                "Upstream SSE stream stalled",
                                "server_error",
                                None,
                            )));
                            return;
                        }
                        Ok(Ok(Some(chunk))) => {
                            let read_at = std::time::Instant::now();
                            let batch = dispatch.feed_at(&chunk, read_at);
                            dispatch.sync_trace_into(&mut attempt_log);
                            // Passthrough bytes are committed before observer
                            // failures; framing is observational on native routes.
                            if passthrough {
                                yield Ok::<Bytes, std::io::Error>(chunk);
                            }
                            for output in batch.output {
                                yield Ok::<Bytes, std::io::Error>(output);
                            }
                            if let Some(error) = batch.error {
                                let usage = dispatch.usage.clone();
                                if let Some(mut log) = attempt_log.take() {
                                    log.record_error_details(Some(error.code), &error.message);
                                    log.tps().invalidate();
                                    log.finish("error", Some(502), usage.as_ref(), Some(stream_error_kind(error.code))).await;
                                }
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(dispatch.streaming_error(
                                    &error.message, "upstream_error", Some(error.code),
                                )));
                                return;
                            }
                            if batch.response_completed {
                                let usage = dispatch.usage.clone();
                                if let Some(log) = attempt_log.take() {
                                    log.finish("success", Some(status.as_u16()), usage.as_ref(), None).await;
                                }
                                return;
                            }
                        }
                        Ok(Ok(None)) => break,
                        Ok(Err(_)) => {
                            let usage = dispatch.usage.clone();
                            if let Some(log) = attempt_log.take() {
                                log.tps().invalidate();
                                log.finish("error", Some(502), usage.as_ref(), Some(error_kind::UPSTREAM_FAILURE)).await;
                            }
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(dispatch.streaming_error(
                                "Upstream stream error",
                                "server_error",
                                None,
                            )));
                            return;
                        }
                    }
                }
                let batch = dispatch.finish();
                dispatch.sync_trace_into(&mut attempt_log);
                for output in batch.output {
                    yield Ok::<Bytes, std::io::Error>(output);
                }
                if let Some(error) = batch.error {
                    let usage = dispatch.usage.clone();
                    if let Some(mut log) = attempt_log.take() {
                        log.record_error_details(Some(error.code), &error.message);
                        log.tps().invalidate();
                        log.finish("error", Some(502), usage.as_ref(), Some(stream_error_kind(error.code))).await;
                    }
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(dispatch.streaming_error(
                        &error.message, "upstream_error", Some(error.code),
                    )));
                    return;
                }
                let usage = dispatch.usage.clone();
                if let Some(log) = attempt_log.take() {
                    log.finish("success", Some(status.as_u16()), usage.as_ref(), None).await;
                }
            };
            Body::from_stream(stream)
        }
        UpstreamResponse::Hyper(response) => {
            let (_, mut body) = response.into_parts();
            let provider = provider.clone();
            let model = model.clone();
            let custom_tool_names2 = custom_tool_names.clone();
            let tool_name_map2 = tool_name_map.clone();
            let mut attempt_log = attempt_log;
            let stream = async_stream::stream! {
                let mut dispatch = StreamDispatch::new(
                    stream_target_format,
                    stream_source_format,
                    &ct,
                    transformer,
                    custom_tool_names2.as_deref(),
                    tool_name_map2,
                    stop_on_response_completed,
                );
                dispatch.codex_cache = attempt_log.as_ref().map(AttemptLog::codex_cache)
                    .filter(|observation| observation.snapshot().is_some());
                dispatch.tps = attempt_log.as_ref().map(AttemptLog::tps);
                loop {
                    let next = tokio::time::timeout(SSE_STALL_TIMEOUT, body.frame()).await;
                    let read_at = std::time::Instant::now();
                    let frame_result = match next {
                        Err(_elapsed) => {
                            tracing::warn!(
                                target: "openproxy::chat::stream",
                                provider = %provider,
                                model = %model,
                                "SSE stalled, closing stream"
                            );
                            let usage = dispatch.usage.clone();
                            if let Some(log) = attempt_log.take() {
                                log.tps().invalidate();
                                log.finish("error", Some(502), usage.as_ref(), Some(error_kind::UPSTREAM_FAILURE)).await;
                            }
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(dispatch.streaming_error(
                                "Upstream SSE stream stalled",
                                "server_error",
                                None,
                            )));
                            return;
                        }
                        Ok(Some(result)) => result,
                        Ok(None) => break,
                    };
                    match frame_result {
                        Ok(frame) => {
                            if let Ok(data) = frame.into_data() {
                                let batch = dispatch.feed_at(&data, read_at);
                                dispatch.sync_trace_into(&mut attempt_log);
                                if passthrough {
                                    yield Ok::<Bytes, std::io::Error>(data);
                                }
                                for output in batch.output {
                                    yield Ok::<Bytes, std::io::Error>(output);
                                }
                                if let Some(error) = batch.error {
                                    let usage = dispatch.usage.clone();
                                    if let Some(mut log) = attempt_log.take() {
                                        log.record_error_details(Some(error.code), &error.message);
                                        log.tps().invalidate();
                                        log.finish("error", Some(502), usage.as_ref(), Some(stream_error_kind(error.code))).await;
                                    }
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(dispatch.streaming_error(
                                        &error.message, "upstream_error", Some(error.code),
                                    )));
                                    return;
                                }
                                if batch.response_completed {
                                    let usage = dispatch.usage.clone();
                                    if let Some(log) = attempt_log.take() {
                                        log.finish("success", Some(status.as_u16()), usage.as_ref(), None).await;
                                    }
                                    return;
                                }
                            }
                        }
                        Err(_) => {
                            let usage = dispatch.usage.clone();
                            if let Some(log) = attempt_log.take() {
                                log.tps().invalidate();
                                log.finish("error", Some(502), usage.as_ref(), Some(error_kind::UPSTREAM_FAILURE)).await;
                            }
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(dispatch.streaming_error(
                                "Upstream stream error",
                                "server_error",
                                None,
                            )));
                            return;
                        }
                    }
                }
                let batch = dispatch.finish();
                dispatch.sync_trace_into(&mut attempt_log);
                for output in batch.output {
                    yield Ok::<Bytes, std::io::Error>(output);
                }
                if let Some(error) = batch.error {
                    let usage = dispatch.usage.clone();
                    if let Some(mut log) = attempt_log.take() {
                        log.record_error_details(Some(error.code), &error.message);
                        log.tps().invalidate();
                        log.finish("error", Some(502), usage.as_ref(), Some(stream_error_kind(error.code))).await;
                    }
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(dispatch.streaming_error(
                        &error.message, "upstream_error", Some(error.code),
                    )));
                    return;
                }
                let usage = dispatch.usage.clone();
                if let Some(log) = attempt_log.take() {
                    log.finish("success", Some(status.as_u16()), usage.as_ref(), None).await;
                }
            };
            Body::from_stream(stream)
        }
    };

    let mut response = build_proxied_response(status, &headers, body);
    // SSE-specific headers (9router parity): prevent nginx/proxy buffering
    // and keep the SSE connection alive through intermediary proxies.
    response
        .headers_mut()
        .insert("Connection", "keep-alive".parse().unwrap());
    response
        .headers_mut()
        .insert("X-Accel-Buffering", "no".parse().unwrap());
    response
        .headers_mut()
        .insert("Cache-Control", "no-cache".parse().unwrap());
    response
        .headers_mut()
        .insert("Content-Type", "text/event-stream".parse().unwrap());
    response
}

struct StreamDispatch {
    tps: Option<UpstreamTpsObservation>,
    frame_read_at: Option<std::time::Instant>,
    codex_cache: Option<crate::core::executor::codex_cache::CodexCacheObservation>,
    framer: Option<TextStreamFramer>,
    stop_on_response_completed: bool,
    usage: Option<TokenUsage>,
    dashboard_transformer:
        Option<Box<dyn crate::core::translator::response_transform::StreamingTransformer>>,
    translation_state: Option<crate::core::translator::registry::ResponseTransformState>,
    source: Format,
    target: Format,
    next_response_sequence_number: u64,
    /// Bounded metadata trace of upstream/emitted SSE events; active only
    /// for translated Claude streams (see `stream_trace::trace_applies`).
    trace: Option<crate::server::stream_trace::StreamEventTrace>,
}

#[derive(Default)]
struct DispatchBatch {
    output: Vec<Bytes>,
    response_completed: bool,
    error: Option<StreamLimitError>,
}

impl StreamDispatch {
    fn new(
        source: Format,
        target: Format,
        content_type: &str,
        dashboard_transformer: Option<
            Box<dyn crate::core::translator::response_transform::StreamingTransformer>,
        >,
        custom_tool_names: Option<&str>,
        tool_name_map: Option<serde_json::Map<String, serde_json::Value>>,
        stop_on_response_completed: bool,
    ) -> Self {
        let mut translation_state = (dashboard_transformer.is_none() && source != target)
            .then(crate::core::translator::registry::ResponseTransformState::default);
        if let (Some(state), Some(names)) = (translation_state.as_mut(), custom_tool_names) {
            if !names.is_empty() {
                state.responses.state.insert(
                    "customToolNames".to_string(),
                    Value::String(names.to_string()),
                );
            }
        }
        // C46 harness reverse tool-name map: Claude Code name → client
        // name. The Claude→OpenAI response transform reads this as
        // `toolNameMap` in the per-stream Claude state, restoring the
        // client's tool names on tool_calls.
        if let (Some(state), Some(map)) = (translation_state.as_mut(), tool_name_map) {
            if !map.is_empty() {
                state
                    .anthropic
                    .claude_state
                    .insert("toolNameMap".to_string(), Value::Object(map));
            }
        }
        let translation_active = translation_state.is_some();
        let trace = crate::server::stream_trace::trace_applies(source, target, translation_active)
            .then(crate::server::stream_trace::StreamEventTrace::default);
        Self {
            tps: None,
            frame_read_at: None,
            framer: source
                .text_stream_mode(Some(content_type))
                .map(TextStreamFramer::new),
            stop_on_response_completed,
            usage: None,
            codex_cache: None,
            dashboard_transformer,
            translation_state,
            source,
            target,
            next_response_sequence_number: 0,
            trace,
        }
    }

    #[cfg(test)]
    fn feed(&mut self, chunk: &[u8]) -> DispatchBatch {
        self.feed_at(chunk, std::time::Instant::now())
    }

    /// Sync the stream trace (when active) into the attempt log so it
    /// survives finish and interrupted/drop paths.
    fn sync_trace_into(&self, attempt_log: &mut Option<AttemptLog>) {
        if let (Some(trace), Some(log)) = (&self.trace, attempt_log.as_mut()) {
            log.observe_stream_trace(trace);
        }
    }

    fn feed_at(&mut self, chunk: &[u8], read_at: std::time::Instant) -> DispatchBatch {
        let mut batch = DispatchBatch::default();
        if let Some(tps) = self.tps.clone() {
            let result = tps.feed_segments(chunk, read_at, |segment, at| {
                if batch.response_completed {
                    return Ok(());
                }
                self.frame_read_at = at;
                let next = self.feed_segment(segment);
                batch.output.extend(next.output);
                batch.response_completed |= next.response_completed;
                if let Some(error) = next.error {
                    return Err(error);
                }
                Ok(())
            });
            if let Err(error) = result {
                tps.invalidate();
                batch.error = Some(error);
            }
        } else {
            self.frame_read_at = Some(read_at);
            batch = self.feed_segment(chunk);
        }
        batch
    }

    fn feed_segment(&mut self, chunk: &[u8]) -> DispatchBatch {
        let Some(mut framer) = self.framer.take() else {
            if let Some(usage) = extract_token_usage_from_bytes(chunk) {
                self.usage = Some(usage);
            }
            let mut batch = DispatchBatch::default();
            if let Some(transformer) = self.dashboard_transformer.as_mut() {
                for line in
                    transform_sse_stream(&Bytes::copy_from_slice(chunk), transformer.as_mut())
                {
                    if let Some(frame) = sse_frame_for_dashboard(&line) {
                        batch.output.push(frame);
                    }
                }
            } else if let Some(state) = self.translation_state.as_mut() {
                match registry::global_registry().translate_response_payload(
                    self.source,
                    self.target,
                    chunk,
                    state,
                ) {
                    Ok(chunks) => append_translated_chunks(&mut batch.output, chunks),
                    Err(error) => batch.error = Some(error),
                }
            }
            return batch;
        };

        let mut batch = DispatchBatch::default();
        let mut consumer_error = None;
        let frame_result = framer.feed(chunk, |frame| {
            if consumer_error.is_some() || batch.response_completed {
                return;
            }
            if let Err(error) = self.consume_frame(frame, &mut batch) {
                consumer_error = Some(error);
            }
        });
        self.framer = Some(framer);
        batch.error = consumer_error;
        if batch.error.is_none() && !batch.response_completed {
            batch.error = frame_result.err().map(frame_error_to_stream);
        }
        batch
    }

    fn finish(&mut self) -> DispatchBatch {
        let eof_at = std::time::Instant::now();
        let mut batch = DispatchBatch::default();
        if let Some(mut framer) = self.framer.take() {
            let mut consumer_error = None;
            let frame_result = framer.finish(|frame| {
                if consumer_error.is_some() {
                    return;
                }
                if let Err(error) = self.consume_frame(frame, &mut batch) {
                    consumer_error = Some(error);
                }
            });
            self.framer = Some(framer);
            batch.error = consumer_error.or_else(|| frame_result.err().map(frame_error_to_stream));
        }
        if batch.error.is_none() {
            if self.source == Format::OpenAiResponses
                && self.target == Format::Claude
                && self
                    .translation_state
                    .as_ref()
                    .is_some_and(|state| !state.responses.completed_sent)
            {
                batch.error = Some(StreamLimitError {
                    code: "upstream_stream_truncated",
                    message: "Responses stream ended without response.completed".to_string(),
                });
            } else if self.source == Format::Claude
                && self.target == Format::OpenAiResponses
                && self.translation_state.as_ref().is_some_and(|state| {
                    // Only flag streams whose Responses projection started but
                    // never reached response.completed (Claude died before
                    // message_stop; the second hop completes on that chunk).
                    // A pending `needCompleted` means message_stop already
                    // arrived and the terminal is only deferred until usage
                    // or EOF (C-a6api split-usage semantics) — such streams
                    // flush the terminal in finish_transforms below instead.
                    state
                        .responses
                        .state
                        .get("started")
                        .and_then(|v| v.as_bool())
                        == Some(true)
                        && state
                            .responses
                            .state
                            .get("completedSent")
                            .and_then(|v| v.as_bool())
                            != Some(true)
                        && state
                            .responses
                            .state
                            .get("needCompleted")
                            .and_then(|v| v.as_bool())
                            != Some(true)
                })
            {
                batch.error = Some(StreamLimitError {
                    code: "upstream_stream_truncated",
                    message: "Claude stream ended before completion".to_string(),
                });
            } else {
                batch.error = self.finish_transforms(&mut batch.output);
            }
        }
        if batch.error.is_none() {
            if let Some(tps) = &self.tps {
                tps.clean_eof(self.source, eof_at);
            }
        } else if let Some(tps) = &self.tps {
            tps.invalidate();
        }
        batch
    }

    /// `append_translated_chunks` + emitted-side trace observation. Each
    /// produced SSE frame is inspected for its event name (and item type /
    /// tool name for output_item.added); finish reasons inside chat chunks
    /// are recorded separately.
    fn trace_translated_chunks(&self, output: &mut Vec<Bytes>, chunks: Vec<String>) {
        for line in &chunks {
            if let Some(trace) = &self.trace {
                trace.observe_emitted(line);
                if let Some(finish) = extract_finish_reason(line) {
                    trace.observe_finish_reason(&finish);
                }
            }
        }
        append_translated_chunks(output, chunks);
    }

    fn finish_transforms(&mut self, output: &mut Vec<Bytes>) -> Option<StreamLimitError> {
        if let Some(state) = self.translation_state.as_mut() {
            let chunks = registry::global_registry().finish_stream(self.source, self.target, state);
            if let Some(error) = state.failure.clone() {
                return Some(error);
            }
            self.trace_translated_chunks(output, chunks);
        }
        None
    }

    fn consume_frame(
        &mut self,
        frame: TextStreamFrame<'_>,
        batch: &mut DispatchBatch,
    ) -> Result<(), StreamLimitError> {
        if let Some(tps) = &self.tps {
            tps.observe_payload(
                self.source,
                frame.event(),
                frame.payload(),
                self.frame_read_at,
            );
        }
        if let Some(trace) = &self.trace {
            let parsed_for_trace = frame
                .payload()
                .and_then(|payload| serde_json::from_str::<Value>(payload).ok());
            trace.observe_upstream(frame.event(), parsed_for_trace.as_ref());
        }
        if self.stop_on_response_completed
            && self.translation_state.is_none()
            && frame.event() == Some("response.completed")
        {
            batch.response_completed = true;
        }
        // Native passthrough has no downstream parser, so parse once here for
        // usage/completion observation. Translated/dashboard streams let their
        // transformer parse the source payload and observe usage on the emitted
        // OpenAI frames instead. A Responses stream without an `event:` field
        // still needs one source parse to detect response.completed.
        let dashboard_multiline_data = self.dashboard_transformer.is_some()
            && frame
                .payload()
                .is_some_and(|payload| payload.contains('\n'));
        let parse_source = dashboard_multiline_data
            || (self.dashboard_transformer.is_none() && self.translation_state.is_none())
            || (self.stop_on_response_completed && frame.event() != Some("response.completed"));
        let parsed = parse_source
            .then(|| frame.payload())
            .flatten()
            .and_then(|payload| serde_json::from_str::<Value>(payload).ok());
        if let Some(observation) = &self.codex_cache {
            let diagnostic_value = if parsed.is_none() {
                frame
                    .payload()
                    .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
            } else {
                None
            };
            observation.observe(frame.event(), parsed.as_ref().or(diagnostic_value.as_ref()));
        }
        if let Some(value) = parsed.as_ref() {
            self.observe_response_sequence(value);
            if let Some(usage) = extract_token_usage_from_value(value) {
                self.usage = Some(usage);
            }
            if self.stop_on_response_completed
                && self.translation_state.is_none()
                && value.get("type").and_then(Value::as_str) == Some("response.completed")
            {
                batch.response_completed = true;
            }
        }
        if !parse_source {
            if let Some(usage) = frame
                .payload()
                .and_then(|payload| extract_token_usage_from_bytes(payload.as_bytes()))
            {
                self.usage = Some(usage);
            }
        }

        let output_start = batch.output.len();
        if let Some(transformer) = self.dashboard_transformer.as_mut() {
            batch.output.extend(transform_dashboard_frame(
                &frame,
                self.source,
                parsed.as_ref(),
                transformer.as_mut(),
            ));
        } else if let (Some(state), Some(payload)) =
            (self.translation_state.as_mut(), frame.payload())
        {
            let translated = registry::global_registry().translate_response_payload(
                self.source,
                self.target,
                payload.as_bytes(),
                state,
            );
            let completed_sent = state.responses.completed_sent;
            if self.stop_on_response_completed && completed_sent {
                batch.response_completed = true;
            }
            match translated {
                Ok(chunks) => self.trace_translated_chunks(&mut batch.output, chunks),
                Err(error) => return Err(error),
            }
        }
        if !parse_source {
            // Only inspect output produced for this frame. Re-scanning the
            // whole transport-chunk batch here would make N coalesced events
            // perform 1+2+...+N JSON parses.
            for output in &batch.output[output_start..] {
                if let Some(usage) = extract_token_usage_from_bytes(output) {
                    self.usage = Some(usage);
                }
            }
        }
        Ok(())
    }

    fn observe_response_sequence(&mut self, value: &Value) {
        if self.target != Format::OpenAiResponses {
            return;
        }
        if let Some(sequence_number) = value.get("sequence_number").and_then(Value::as_u64) {
            self.next_response_sequence_number = self
                .next_response_sequence_number
                .max(sequence_number.saturating_add(1));
        }
    }

    fn next_responses_sequence_number(&self) -> u64 {
        let translated = self
            .translation_state
            .as_ref()
            .and_then(|state| state.responses.state.get("seq"))
            .and_then(Value::as_u64)
            .map_or(0, |sequence_number| sequence_number.saturating_add(1));
        self.next_response_sequence_number.max(translated)
    }

    fn streaming_error(&mut self, error_msg: &str, error_type: &str, code: Option<&str>) -> String {
        if let Some(trace) = &self.trace {
            trace.observe_stream_error(code.unwrap_or("upstream_stream_error"));
        }
        let friendly = crate::core::utils::error::friendly_error_message(502, error_msg);
        if self.target == Format::OpenAiResponses {
            return crate::core::translator::response::openai_responses::format_error_event(
                self.next_responses_sequence_number(),
                code.unwrap_or("upstream_stream_error"),
                &friendly,
            );
        }
        if self.target == Format::Claude {
            let msg = json!({
                "type": "error",
                "error": {"type": error_type, "message": friendly, "code": code}
            });
            return format!("event: error\ndata: {msg}\n\n");
        }
        let msg = serde_json::json!({
            "error": {
                "message": friendly,
                "type": error_type,
                "code": code
            }
        });
        format!(
            "data: {}\n\n",
            serde_json::to_string(&msg).unwrap_or_default()
        )
    }
}

fn dashboard_transformer_for_format(
    format: Format,
) -> Option<Box<dyn crate::core::translator::response_transform::StreamingTransformer>> {
    match format {
        Format::Claude => transformer_for_provider("claude"),
        Format::Gemini | Format::Vertex | Format::Antigravity => transformer_for_provider("gemini"),
        Format::Ollama => transformer_for_provider("ollama"),
        Format::OpenAi | Format::OpenAiResponses | Format::OpenAiResponse | Format::Codex => {
            transformer_for_provider("openai")
        }
    }
}

fn frame_error_to_stream(error: FrameError) -> StreamLimitError {
    StreamLimitError {
        code: error.code(),
        message: error.to_string(),
    }
}

/// Classify a terminal stream error: failures the upstream itself reported
/// are upstream failures; everything else is our translation/framing layer.
fn stream_error_kind(code: &str) -> &'static str {
    match code {
        "upstream_error_event" => error_kind::UPSTREAM_FAILURE,
        _ => error_kind::LOCAL_FAILURE,
    }
}

fn append_translated_chunks(output: &mut Vec<Bytes>, chunks: Vec<String>) {
    for line in chunks {
        if let Some(frame) = sse_frame_for_dashboard(&line) {
            output.push(frame);
        }
    }
}

/// Extract `choices[0].finish_reason` from a chat-completion style emitted
/// line (metadata only, used by the stream trace).
fn extract_finish_reason(line: &str) -> Option<String> {
    let data = line.trim().strip_prefix("data: ")?;
    let value: Value = serde_json::from_str(data.trim()).ok()?;
    value
        .pointer("/choices/0/finish_reason")?
        .as_str()
        .filter(|reason| !reason.is_empty())
        .map(str::to_string)
}

fn transform_dashboard_frame(
    frame: &TextStreamFrame<'_>,
    source: Format,
    parsed_payload: Option<&Value>,
    transformer: &mut dyn crate::core::translator::response_transform::StreamingTransformer,
) -> Vec<Bytes> {
    let metadata = if frame.is_sse() {
        frame
            .raw()
            .lines()
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter(|line| {
                line.starts_with(':')
                    || line.starts_with("event:")
                    || line.starts_with("id:")
                    || line.starts_with("retry:")
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::new()
    };
    let Some(payload) = frame.payload() else {
        return (!metadata.is_empty())
            .then(|| Bytes::from(format!("{metadata}\n\n")))
            .into_iter()
            .collect();
    };

    // Legacy dashboard transformers consume framed SSE. Do not parse+serialize
    // here: the selected transformer is the sole JSON parser for translated
    // dashboard traffic.
    let normalized_payload = parsed_payload.and_then(|value| serde_json::to_string(value).ok());
    let payload = normalized_payload.as_deref().unwrap_or(payload);
    let input = Bytes::from(format!("data: {payload}\n\n"));
    let mut output = transform_sse_stream(&input, transformer)
        .into_iter()
        .filter_map(|line| sse_frame_for_dashboard(&line))
        .collect::<Vec<_>>();
    if !metadata.is_empty() {
        if let Some(first) = output.first_mut() {
            let mut combined = Vec::with_capacity(metadata.len() + 1 + first.len());
            combined.extend_from_slice(metadata.as_bytes());
            combined.push(b'\n');
            combined.extend_from_slice(first);
            *first = Bytes::from(combined);
        } else {
            output.push(Bytes::from(format!("{metadata}\n\n")));
        }
    }
    output
}

fn sse_frame_for_dashboard(line: &str) -> Option<Bytes> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 9router parity: preserve all standard SSE line types without wrapping.
    // - data: {...}          → data frame
    // - event: name          → event type header
    // - id: ...              → event id
    // - retry: ...           → retry interval
    // - : comment            → comment (keep-alive)
    // Everything else gets data: prefix added.
    let framed = if trimmed.starts_with("data:")
        || trimmed.starts_with("event:")
        || trimmed.starts_with("id:")
        || trimmed.starts_with("retry:")
        || trimmed.starts_with(':')
    {
        format!("{trimmed}\n\n")
    } else {
        format!("data: {trimmed}\n\n")
    };

    Some(Bytes::from(framed))
}

fn build_dashboard_sse_body(text: Option<&str>, usage: Option<&TokenUsage>) -> Bytes {
    let mut frames = String::new();

    if let Some(text) = text.filter(|text| !text.is_empty()) {
        let escaped = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
        frames.push_str("data: {\"choices\":[{\"delta\":{\"content\":");
        frames.push_str(&escaped);
        frames.push_str("},\"finish_reason\":null}]}\n\n");
    }

    frames.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]");
    if let Some(usage) = usage {
        let usage_json = serde_json::to_string(usage).unwrap_or_else(|_| "{}".to_string());
        frames.push_str(",\"usage\":");
        frames.push_str(&usage_json);
    }
    frames.push_str("}\n\n");
    frames.push_str("data: [DONE]\n\n");

    Bytes::from(frames)
}

fn build_dashboard_sse_response(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: Bytes,
) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;

    for (name, value) in headers {
        if should_preserve_dashboard_sse_header(name.as_str()) {
            response.headers_mut().insert(name, value.clone());
        }
    }

    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    response
}

fn should_preserve_dashboard_sse_header(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    lowered == "trace-id"
        || lowered.starts_with("x-")
        || lowered.ends_with("-request-id")
        || lowered == "alb_receive_time"
        || lowered == "alb_request_id"
}

fn extract_dashboard_assistant_text_from_bytes(body: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(body).ok()?;

    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
    {
        return Some(text.to_string());
    }

    let content = value.get("content")?.as_array()?;
    let mut text_parts = Vec::new();
    let mut thinking_parts = Vec::new();
    for item in content {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                text_parts.push(text.to_string());
            }
            continue;
        }
        if let Some(thinking) = item.get("thinking").and_then(Value::as_str) {
            if !thinking.is_empty() {
                thinking_parts.push(thinking.to_string());
            }
        }
    }

    if !text_parts.is_empty() {
        return Some(text_parts.join(""));
    }

    if thinking_parts.is_empty() {
        None
    } else {
        Some(thinking_parts.join("\n"))
    }
}

fn build_proxied_response(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: Body,
) -> Response {
    let mut proxied = Response::new(body);
    *proxied.status_mut() = status;
    let connection_tokens = connection_header_tokens(headers);

    for (name, value) in headers {
        if is_hop_by_hop_header(name.as_str())
            || connection_tokens.contains(&name.as_str().to_ascii_lowercase())
        {
            continue;
        }
        proxied.headers_mut().insert(name, value.clone());
    }

    proxied
}

/// Strip the SSE `data:` prefix from a chunk, returning the JSON payload.
/// SSE data lines look like `data: {...}` or `data: {...}\n\nbuffer`.
/// If the body is valid JSON already (non-streaming path), return as-is.
fn strip_sse_data_prefix(body: &[u8]) -> &[u8] {
    let trimmed = body.split(|&b| b == b'\n').next().unwrap_or(body);
    if trimmed.starts_with(b"data:") {
        let after = &trimmed[b"data:".len()..];
        let after = after
            .strip_prefix(b" ")
            .or_else(|| after.strip_prefix(b"\t"))
            .unwrap_or(after);
        if serde_json::from_slice::<serde_json::Value>(after).is_ok() {
            return after;
        }
    }
    // Fall back: try parsing the whole body as JSON (non-streaming / already-stripped).
    if serde_json::from_slice::<serde_json::Value>(body).is_ok() {
        return body;
    }
    body
}

fn extract_token_usage_from_bytes(body: &[u8]) -> Option<TokenUsage> {
    let body = strip_sse_data_prefix(body);
    let value = serde_json::from_slice::<Value>(body).ok()?;

    extract_token_usage_from_value(&value)
}

fn extract_token_usage_from_value(value: &Value) -> Option<TokenUsage> {
    let usage_obj = value
        .get("usage")
        .and_then(Value::as_object)
        .or_else(|| {
            value
                .get("data")
                .and_then(|d| d.get("usage"))
                .and_then(Value::as_object)
        })
        .or_else(|| {
            value
                .get("result")
                .and_then(|d| d.get("usage"))
                .and_then(Value::as_object)
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|d| d.get("usage"))
                .and_then(Value::as_object)
        });

    let known_fields = [
        "prompt_tokens",
        "input_tokens",
        "completion_tokens",
        "output_tokens",
        "total_tokens",
        "reasoning_tokens",
        "cached_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ];

    if let Some(usage) = usage_obj {
        let nested_cached_tokens = usage
            .get("prompt_tokens_details")
            .or_else(|| usage.get("input_tokens_details"))
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64);
        // Anthropic names the field `thinking_tokens` inside
        // `output_tokens_details`; OpenAI-compatible upstreams use
        // `reasoning_tokens` inside `completion_tokens_details`.
        let nested_reasoning_tokens = usage
            .get("completion_tokens_details")
            .or_else(|| usage.get("output_tokens_details"))
            .and_then(|details| {
                details
                    .get("reasoning_tokens")
                    .or_else(|| details.get("thinking_tokens"))
            })
            .and_then(Value::as_u64);
        return Some(TokenUsage {
            prompt_tokens: extract_u64(usage, "prompt_tokens"),
            input_tokens: extract_u64(usage, "input_tokens"),
            completion_tokens: extract_u64(usage, "completion_tokens"),
            output_tokens: extract_u64(usage, "output_tokens"),
            total_tokens: extract_u64(usage, "total_tokens"),
            reasoning_tokens: extract_u64(usage, "reasoning_tokens").or(nested_reasoning_tokens),
            cached_tokens: extract_u64(usage, "cached_tokens").or(nested_cached_tokens),
            cache_read_input_tokens: extract_u64(usage, "cache_read_input_tokens")
                .or(nested_cached_tokens),
            cache_creation_input_tokens: extract_u64(usage, "cache_creation_input_tokens"),
            extra: usage
                .iter()
                .filter(|(key, _)| !known_fields.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        });
    }

    // Fallback: some providers put input_tokens/output_tokens directly at the
    // top level (e.g. Anthropic, some proxies). Only use this when at least
    // one token field is present to avoid creating a zero-filled entry for
    // responses that have no usage data at all.
    let input = extract_u64_from_value(value, "input_tokens");
    let prompt = extract_u64_from_value(value, "prompt_tokens");
    let output = extract_u64_from_value(value, "output_tokens");
    let completion = extract_u64_from_value(value, "completion_tokens");
    let total = extract_u64_from_value(value, "total_tokens");
    if input + prompt + output + completion + total > 0 {
        return Some(TokenUsage {
            prompt_tokens: opt(prompt).or(opt(input)),
            input_tokens: opt(input).filter(|_| prompt == 0),
            completion_tokens: opt(completion).or(opt(output)),
            output_tokens: opt(output).filter(|_| completion == 0),
            total_tokens: opt(total),
            reasoning_tokens: opt(extract_u64_from_value(value, "reasoning_tokens")),
            cached_tokens: opt(extract_u64_from_value(value, "cached_tokens")),
            cache_read_input_tokens: opt(extract_u64_from_value(value, "cache_read_input_tokens")),
            cache_creation_input_tokens: opt(extract_u64_from_value(
                value,
                "cache_creation_input_tokens",
            )),
            extra: BTreeMap::new(),
        });
    }

    None
}

fn extract_u64(obj: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    obj.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    })
}

fn extract_u64_from_value(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(|v| match v {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .unwrap_or(0)
}

fn opt(v: u64) -> Option<u64> {
    if v > 0 {
        Some(v)
    } else {
        None
    }
}

/// Extract the error message AND raw body bytes from an upstream error response.
/// This preserves the upstream body for verbatim passthrough (H23).
async fn extract_upstream_error_with_body(response: UpstreamResponse) -> (String, Option<Vec<u8>>) {
    let status = response.status();
    let body_bytes = read_upstream_diagnostic(response, diagnostic_body_limit())
        .await
        .bytes;
    let text = String::from_utf8_lossy(&body_bytes).to_string();
    let message = if let Ok(value) = serde_json::from_str::<Value>(&text) {
        if let Some(msg) = value
            .get("error")
            .and_then(|error| error.get("message").or(Some(error)))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            msg.to_string()
        } else if let Some(msg) = value
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            msg.to_string()
        } else {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                status
                    .canonical_reason()
                    .unwrap_or("Upstream request failed")
                    .to_string()
            } else {
                trimmed.to_string()
            }
        }
    } else {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            status
                .canonical_reason()
                .unwrap_or("Upstream request failed")
                .to_string()
        } else {
            trimmed.to_string()
        }
    };
    let raw_body = if body_bytes.is_empty() {
        None
    } else {
        Some(body_bytes.to_vec())
    };
    (message, raw_body)
}

fn upstream_error_diagnostic(status: u16, body: Option<&[u8]>) -> (Option<String>, String) {
    let Some(payload) = body.and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok()) else {
        return (None, format!("Upstream returned HTTP {status}"));
    };

    fn string(value: Option<&Value>) -> Option<&str> {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
    let error = payload.get("error");
    let code = [
        error.and_then(|value| value.pointer("/details/error_code")),
        error.and_then(|value| value.get("code")),
        error.and_then(|value| value.get("type")),
        payload.get("code"),
        payload.get("type"),
    ]
    .into_iter()
    .find_map(|value| string(value));
    let message = error
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| string(error))
        .or_else(|| string(payload.get("message")))
        .map(str::to_string)
        .unwrap_or_else(|| format!("Upstream returned HTTP {status}"));

    (code.map(str::to_string), message)
}

async fn collected_body_failure_response(
    error: BoundedBodyError,
    attempt_log: Option<AttemptLog>,
) -> Response {
    if let Some(attempt_log) = attempt_log {
        attempt_log.tps().invalidate();
        attempt_log
            .finish(
                "error",
                Some(StatusCode::BAD_GATEWAY.as_u16()),
                None,
                Some(error_kind::LOCAL_FAILURE),
            )
            .await;
    }
    json_error_response(
        StatusCode::BAD_GATEWAY,
        &format!("Failed to read complete upstream response: {error}"),
    )
}

async fn stream_limit_failure_response(
    error: StreamLimitError,
    attempt_log: Option<AttemptLog>,
) -> Response {
    if let Some(attempt_log) = attempt_log {
        attempt_log.tps().invalidate();
        attempt_log
            .finish(
                "error",
                Some(StatusCode::BAD_GATEWAY.as_u16()),
                None,
                Some(error_kind::LOCAL_FAILURE),
            )
            .await;
    }
    with_cors_response(
        (
            StatusCode::BAD_GATEWAY,
            [(header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": error.message,
                    "type": "upstream_error",
                    "code": error.code
                }
            })
            .to_string(),
        )
            .into_response(),
    )
}

fn is_identity_encoded(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.trim().is_empty() || value.eq_ignore_ascii_case("identity"))
}

fn declared_content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
}

fn retry_after_from_headers(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    // Standard retry-after header (HTTP/1.1)
    if let Some(value) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
        let trimmed = value.trim();
        if let Ok(seconds) = trimmed.parse::<i64>() {
            return Some(Utc::now() + ChronoDuration::seconds(seconds.max(0)));
        }
        if let Ok(timestamp) = DateTime::parse_from_rfc2822(trimmed) {
            return Some(timestamp.with_timezone(&Utc));
        }
    }

    // Google-specific rate limit headers (used by Antigravity / Cloud Code)
    // x-ratelimit-reset-after: seconds until rate limit resets (relative)
    if let Some(value) = headers
        .get("x-ratelimit-reset-after")
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(seconds) = value.trim().parse::<i64>() {
            if seconds > 0 {
                return Some(Utc::now() + ChronoDuration::seconds(seconds));
            }
        }
    }

    // x-ratelimit-reset: unix timestamp (seconds) when rate limit resets (absolute)
    if let Some(value) = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(ts) = value.trim().parse::<i64>() {
            let now = Utc::now().timestamp();
            if ts > now {
                return Some(Utc::now() + ChronoDuration::seconds(ts - now));
            }
        }
    }

    None
}

/// Anthropic unified rate-limit reset: derive the advisory retry-after
/// instant from `anthropic-ratelimit-unified-*` headers, but only for a
/// window that is actually rejected. Candidates (latest wins):
/// - `5h-Status: rejected` → `5h-Reset`
/// - `7d-Status: rejected` → `7d-Reset`
/// - `Unified-Status: rejected` → `Unified-Reset` (even when the 5h/7d
///   windows read allowed — live captures show a rejected unified status
///   with per-window statuses still allowed)
///
/// Reset values parse as a unix epoch (float tolerated) or RFC 3339;
/// past instants are ignored. Callers gate this on HTTP 429 — window
/// headers are present on successful responses too, where they describe
/// utilization, not a wait requirement.
fn unified_reset_retry_after(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    // The unified window has no window token in its header names:
    // `anthropic-ratelimit-unified-status` / `-reset`, while 5h/7d carry
    // `-5h-`/`-7d-` between `unified` and the field.
    let header_name = |window: &str, field: &str| -> String {
        if window == "unified" {
            format!("anthropic-ratelimit-unified-{field}")
        } else {
            format!("anthropic-ratelimit-unified-{window}-{field}")
        }
    };
    let status_is_rejected = |window: &str| -> bool {
        headers
            .get(header_name(window, "status"))
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().eq_ignore_ascii_case("rejected"))
            .unwrap_or(false)
    };

    let parse_reset = |window: &str| -> Option<DateTime<Utc>> {
        let raw = headers
            .get(header_name(window, "reset"))
            .and_then(|value| value.to_str().ok())?
            .trim()
            .to_string();
        let parsed = raw
            .parse::<f64>()
            .ok()
            .and_then(|epoch| DateTime::<Utc>::from_timestamp(epoch as i64, 0))
            .or_else(|| {
                DateTime::parse_from_rfc3339(&raw)
                    .ok()
                    .map(|t| t.with_timezone(&Utc))
            })?;
        let now = Utc::now();
        (parsed > now).then_some(parsed)
    };

    let mut candidates = Vec::with_capacity(3);
    for window in ["5h", "7d", "unified"] {
        if status_is_rejected(window) {
            candidates.extend(parse_reset(window));
        }
    }
    candidates.into_iter().max()
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "content-length"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn connection_header_tokens(headers: &reqwest::header::HeaderMap) -> HashSet<String> {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn attempt_error_response(error: ProviderAttemptError) -> Response {
    // H23: When upstream_body is available, return it verbatim instead
    // of constructing a new error body.
    if let Some(body_bytes) = error.upstream_body {
        let status_code = StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_GATEWAY);
        // Usage-limit reset timer (429-only carve-out, see C13): when the
        // upstream error declares a reset instant, normalize error.message
        // to a concise "(resets in …)" display so clients rendering it
        // verbatim do not misread Z.AI's China-local timestamp. Status, error
        // type/code and the Retry-After synthesis below are untouched;
        // Retry-After stays driven solely by retry_after, never by the display
        // instant.
        let body_bytes = if error.status == 429 {
            crate::core::utils::error::maybe_add_reset_suffix(&body_bytes, Utc::now())
                .unwrap_or(body_bytes)
        } else {
            body_bytes
        };
        let mut response = (status_code, Body::from(body_bytes)).into_response();
        if let Some(retry_after) = error.retry_after {
            let seconds = (retry_after - Utc::now()).num_seconds().max(1).to_string();
            if let Ok(value) = seconds.parse() {
                response.headers_mut().insert("retry-after", value);
            }
        }
        return response;
    }

    // Prefer a status that matches the error text when upstream lied about the code
    // (e.g. free-console proxies returning 401 for "model not supported").
    let status_code =
        crate::core::utils::error::infer_status_from_message(error.status, &error.message);
    let status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY);
    let friendly =
        crate::core::utils::error::friendly_error_message(status.as_u16(), &error.message);
    let body = crate::core::utils::error::build_error_body(status.as_u16(), Some(&friendly));
    let mut response = (status, Json(body)).into_response();

    if let Some(retry_after) = error.retry_after {
        let seconds = (retry_after - Utc::now()).num_seconds().max(1).to_string();
        if let Ok(value) = seconds.parse() {
            response.headers_mut().insert("retry-after", value);
        }
    }

    response
}

fn json_error_response(status: StatusCode, message: &str) -> Response {
    let status_code =
        crate::core::utils::error::infer_status_from_message(status.as_u16(), message);
    let status = StatusCode::from_u16(status_code).unwrap_or(status);
    let friendly = crate::core::utils::error::friendly_error_message(status.as_u16(), message);
    let body = crate::core::utils::error::build_error_body(status.as_u16(), Some(&friendly));
    with_cors_response((status, Json(body)).into_response())
}

/// Claude subscription traffic must present as Claude Code: either a
/// genuine `claude-cli/…` client or the OpenCode mask plugin's marker
/// header. Presence-based on purpose — the API key still authenticates.
pub(crate) fn claude_mask_client_allowed(headers: &HeaderMap) -> bool {
    if headers.contains_key("x-openproxy-claude-mask") {
        return true;
    }
    headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ua| ua.starts_with("claude-cli/"))
}

/// Presence (not value) of the mask-plugin marker in the already-collected
/// client headers. Case-insensitive: the normalized map lookup is
/// case-sensitive but keys are only lowercase by upstream convention
/// (`headers_map` lowercases at ingress), so scan explicitly. Pure
/// function — same input, same verdict. No UA logic here.
fn claude_mask_marker_present(headers: &BTreeMap<String, String>) -> bool {
    headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case("x-openproxy-claude-mask"))
}

fn with_cors_response(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("x-openproxy-request-id"),
    );
    response
}

fn cors_preflight_response(methods: &str) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_str(methods).unwrap_or(HeaderValue::from_static("GET, POST, OPTIONS")),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use axum::{
        body::Body,
        http::{HeaderMap, StatusCode},
        response::Response,
    };
    use bytes::Bytes;
    use chrono::{DateTime, Duration as ChronoDuration, Utc};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};

    use super::{
        apply_harness_prompt_id, apply_harness_request_id, apply_harness_session_header,
        attempt_error_response, build_dashboard_sse_response, build_proxied_response,
        claude_mask_marker_present, derive_harness_uuid, extract_token_usage_from_value,
        has_native_codex_web_search, is_refreshable_auth_failure, select_connection,
        select_connection_with_supporters, should_prefetch_message_images, tool_image_support,
        unified_reset_retry_after, upstream_error_diagnostic, StreamDispatch,
    };
    use crate::core::account_fallback::ProviderAttemptError;
    use crate::core::chat::RequestPlan;
    use crate::core::model::models_dev::ModelsDevCatalog;
    use crate::core::translator::registry::Format;
    use crate::core::translator::response_transform::OpenAiTransformer;
    use crate::types::{AppDb, CustomModel, ProviderConnection};

    #[test]
    fn translated_messages_dispatch_is_independent_of_transport_boundaries() {
        let fixture = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_chunk_fixture\",\"model\":\"gpt-6-luna\",\"created_at\":1791072000}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_a\",\"type\":\"function_call\",\"call_id\":\"call_real_a\",\"name\":\"echo_probe\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_b\",\"type\":\"function_call\",\"call_id\":\"call_real_b\",\"name\":\"echo_secondary\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_b\",\"delta\":\"{\\\"nonce\\\":\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_a\",\"delta\":\"{\\\"nonce\\\":17}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_b\",\"delta\":\"29}\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":23,\"output_tokens\":7}}}\n\n"
        );
        let translate = |chunk_size| {
            let mut dispatch = StreamDispatch::new(
                Format::OpenAiResponses,
                Format::Claude,
                "text/event-stream",
                None,
                None,
                None,
                true,
            );
            let mut output = Vec::new();
            let mut completed = false;
            for chunk in fixture.as_bytes().chunks(chunk_size) {
                let batch = dispatch.feed(chunk);
                assert!(batch.error.is_none(), "{:?}", batch.error);
                completed |= batch.response_completed;
                output.extend(batch.output);
            }
            let tail = dispatch.finish();
            assert!(tail.error.is_none(), "{:?}", tail.error);
            output.extend(tail.output);
            assert!(completed);
            output
        };
        let coalesced = translate(fixture.len());
        assert_eq!(translate(1), coalesced);
        assert_eq!(translate(17), coalesced);
    }

    #[test]
    fn upstream_error_diagnostic_prefers_specific_codes_and_structured_messages() {
        let (code, message) = upstream_error_diagnostic(
            400,
            Some(br#"{"error":{"type":"invalid_request_error","code":"bad_request","message":"Invalid tool","details":{"error_code":"tool_schema_invalid"}}}"#),
        );
        assert_eq!(code.as_deref(), Some("tool_schema_invalid"));
        assert_eq!(message, "Invalid tool");

        let (code, message) = upstream_error_diagnostic(
            403,
            Some(br#"{"error":{"type":"permission_error","message":"Denied"}}"#),
        );
        assert_eq!(code.as_deref(), Some("permission_error"));
        assert_eq!(message, "Denied");
    }

    #[test]
    fn upstream_error_diagnostic_does_not_fall_back_to_raw_body() {
        assert_eq!(
            upstream_error_diagnostic(502, Some(b"<html>proxy failure</html>")),
            (None, "Upstream returned HTTP 502".to_string())
        );
        assert_eq!(
            upstream_error_diagnostic(429, None),
            (None, "Upstream returned HTTP 429".to_string())
        );
        assert_eq!(
            upstream_error_diagnostic(400, Some(br#"{"error":{"message":"bad"}}"#)),
            (None, "bad".to_string())
        );
    }

    #[tokio::test]
    async fn tps_synthetic_executor_bodies_are_excluded_on_every_consumer_path() {
        use crate::core::executor::generation_timing::mark_generation_send;
        use crate::server::upstream_tps::UpstreamTpsObservation;
        let fixture = concat!(
            "data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\n",
            "data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n"
        );
        for provider in ["zed", "trae", "devin-cli", "dv"] {
            for mode in 0..5 {
                let observation = UpstreamTpsObservation::default();
                observation
                    .timing()
                    .scope(async {
                        mark_generation_send();
                    })
                    .await;
                // Production excludes the shared attempt observation before execute.
                super::exclude_synthetic_tps(provider, &observation);
                let consumer = observation.clone();
                let read_at =
                    observation.timing().started().unwrap() + std::time::Duration::from_secs(1);
                if mode == 3 {
                    let response = axum::http::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(reqwest::Body::from(fixture))
                        .unwrap();
                    assert!(super::collect_forced_sse_observed(
                        super::UpstreamResponse::Reqwest(reqwest::Response::from(response)),
                        None,
                        Some(consumer),
                        Format::OpenAi
                    )
                    .await
                    .is_ok());
                } else if mode == 4 {
                    consumer.observe_json(Format::OpenAi, &json!({"choices":[{"index":0,"finish_reason":"stop"}],"usage":{"completion_tokens":7}}), read_at);
                } else {
                    let mut dispatch = StreamDispatch::new(
                        Format::OpenAi,
                        if mode == 1 {
                            Format::OpenAiResponses
                        } else {
                            Format::OpenAi
                        },
                        "text/event-stream",
                        if mode == 2 {
                            super::dashboard_transformer_for_format(Format::OpenAi)
                        } else {
                            None
                        },
                        None,
                        None,
                        false,
                    );
                    dispatch.tps = Some(consumer);
                    let batch = dispatch.feed_at(fixture.as_bytes(), read_at);
                    assert!(batch.error.is_none());
                    let tail = dispatch.finish();
                    assert!(tail.error.is_none());
                    if mode != 0 {
                        assert!(
                            !batch.output.is_empty() || !tail.output.is_empty(),
                            "{provider} / mode {mode}"
                        );
                    }
                }
                assert!(observation.snapshot().is_none(), "{provider} / mode {mode}");
            }
        }
    }

    #[tokio::test]
    async fn tps_codex_real_preflight_replay_retains_actual_terminal_read_time() {
        use crate::core::executor::{ClientPool, CodexExecutionRequest, CodexExecutor};
        use crate::server::upstream_tps::UpstreamTpsObservation;
        use futures_util::TryStreamExt;
        use std::sync::Arc;

        let fixture = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[],\"usage\":{\"output_tokens\":7}}}\n\n";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = axum::Router::new().fallback(axum::routing::any(move || async move {
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from(fixture))
                .unwrap()
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let executor = CodexExecutor::new(
            Arc::new(ClientPool::new()),
            Some(crate::types::ProviderNode {
                id: "codex".into(),
                r#type: "codex".into(),
                name: "TPS fixture".into(),
                base_url: Some(format!("http://{address}/backend-api/codex/responses")),
                ..Default::default()
            }),
        )
        .unwrap();
        for mode in 0..3 {
            let observation = UpstreamTpsObservation::default();
            let result = observation
                .timing()
                .scope(executor.execute(CodexExecutionRequest {
                    model: "gpt-5".into(),
                    body: json!({"model":"gpt-5","input":"hello","stream":true}),
                    stream: true,
                    credentials: ProviderConnection {
                        id: "tps-codex-account".into(),
                        provider: "codex".into(),
                        auth_type: "oauth".into(),
                        access_token: Some("fixture-token".into()),
                        ..Default::default()
                    },
                    proxy: None,
                }))
                .await
                .unwrap();
            let timing = observation.timing();
            let prefix = timing
                .prefix()
                .expect("real Codex preflight recorded prefix timing");
            let expected_micros = prefix
                .last_read_at
                .duration_since(timing.started().unwrap())
                .as_micros() as u64;
            // The replay consumer arrives late; this wait must not inflate TPS time.
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            if mode == 2 {
                assert!(super::collect_forced_sse_observed(
                    result.response,
                    None,
                    Some(observation.clone()),
                    Format::Codex
                )
                .await
                .is_ok());
            } else {
                let mut dispatch = StreamDispatch::new(
                    Format::Codex,
                    if mode == 0 {
                        Format::Codex
                    } else {
                        Format::OpenAi
                    },
                    "text/event-stream",
                    None,
                    None,
                    None,
                    false,
                );
                dispatch.tps = Some(observation.clone());
                let super::UpstreamResponse::Reqwest(response) = result.response else {
                    panic!("Codex uses Reqwest")
                };
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.try_next().await.unwrap() {
                    dispatch.feed_at(&chunk, std::time::Instant::now());
                }
                dispatch.finish();
            }
            assert_eq!(
                observation.snapshot().unwrap()["elapsedMicros"],
                expected_micros
            );
            assert_eq!(observation.snapshot().unwrap()["generatedOutputTokens"], 7);
        }
        server.abort();
    }

    #[tokio::test]
    async fn tps_dispatch_exact_original_read_time_is_shared_by_all_live_paths() {
        use crate::core::executor::generation_timing::mark_generation_send;
        use crate::server::upstream_tps::UpstreamTpsObservation;
        for mode in 0..3 {
            let observation = UpstreamTpsObservation::default();
            observation
                .timing()
                .scope(async {
                    mark_generation_send();
                })
                .await;
            let source = Format::OpenAi;
            let target = if mode == 1 {
                Format::OpenAiResponses
            } else {
                source
            };
            let transformer = if mode == 2 {
                super::dashboard_transformer_for_format(source)
            } else {
                None
            };
            let mut dispatch = StreamDispatch::new(
                source,
                target,
                "text/event-stream",
                transformer,
                None,
                None,
                false,
            );
            dispatch.tps = Some(observation.clone());
            let started = observation.timing().started().unwrap();
            dispatch.feed_at(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}],\"usage\":{\"completion_tokens\":99}}\n\n", started + std::time::Duration::from_secs(1));
            dispatch.feed_at(b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}],\"usage\":{\"completion_tokens\":7}}\n\ndata: [DONE]\n\n", started + std::time::Duration::from_secs(2));
            // A downstream yield/late heartbeat cannot replace terminal timing.
            dispatch.feed_at(
                b": heartbeat\n\n",
                started + std::time::Duration::from_secs(9),
            );
            dispatch.finish();
            assert_eq!(
                observation.snapshot().unwrap(),
                json!({"version":1,"generatedOutputTokens":7,"elapsedMicros":2_000_000,"endKind":"protocol_terminal"})
            );
        }
    }

    #[test]
    fn claude_mask_marker_present_matches_gate_presence_semantics() {
        // Presence, not value: empty counts (parity with
        // `claude_mask_client_allowed` contains_key).
        assert!(claude_mask_marker_present(&BTreeMap::from([(
            "x-openproxy-claude-mask".to_string(),
            "1".to_string(),
        )])));
        assert!(claude_mask_marker_present(&BTreeMap::from([(
            "x-openproxy-claude-mask".to_string(),
            String::new(),
        )])));
        // Normalized-map lookup is case-sensitive by type: scan
        // case-insensitively so a mixed-case insert still matches.
        assert!(claude_mask_marker_present(&BTreeMap::from([(
            "X-OpenProxy-Claude-Mask".to_string(),
            "1".to_string(),
        )])));
        assert!(!claude_mask_marker_present(&BTreeMap::new()));
    }

    #[test]
    fn harness_session_header_injects_when_active_and_client_wins() {
        // Active harness + no client header → inject the request session.
        let mut headers = BTreeMap::new();
        apply_harness_session_header(&mut headers, true, "sess-1");
        assert_eq!(headers["x-claude-code-session-id"], "sess-1");
        // Client header wins — never overwritten.
        let mut headers = BTreeMap::from([(
            "x-claude-code-session-id".to_string(),
            "client-sess".to_string(),
        )]);
        apply_harness_session_header(&mut headers, true, "sess-2");
        assert_eq!(headers["x-claude-code-session-id"], "client-sess");
        // Inactive harness (apikey/third-party) → untouched.
        let mut headers = BTreeMap::new();
        apply_harness_session_header(&mut headers, false, "sess-3");
        assert!(!headers.contains_key("x-claude-code-session-id"));
    }

    #[test]
    fn harness_prompt_and_request_id_headers_inject_when_active() {
        // Same insert-if-missing contract as the session header: inject on
        // the harness path, client value wins, no-op otherwise.
        let mut headers = BTreeMap::new();
        apply_harness_prompt_id(&mut headers, true, "prompt-1");
        apply_harness_request_id(&mut headers, true, "request-1");
        assert_eq!(headers["x-claude-code-prompt-id"], "prompt-1");
        assert_eq!(headers["x-client-request-id"], "request-1");

        let mut headers = BTreeMap::from([
            (
                "x-claude-code-prompt-id".to_string(),
                "client-p".to_string(),
            ),
            ("x-client-request-id".to_string(), "client-r".to_string()),
        ]);
        apply_harness_prompt_id(&mut headers, true, "prompt-2");
        apply_harness_request_id(&mut headers, true, "request-2");
        assert_eq!(headers["x-claude-code-prompt-id"], "client-p");
        assert_eq!(headers["x-client-request-id"], "client-r");

        let mut headers = BTreeMap::new();
        apply_harness_prompt_id(&mut headers, false, "prompt-3");
        apply_harness_request_id(&mut headers, false, "request-3");
        assert!(!headers.contains_key("x-claude-code-prompt-id"));
        assert!(!headers.contains_key("x-client-request-id"));
    }

    #[test]
    fn harness_uuid_derivation_is_deterministic_and_v4_shaped() {
        // Same seed → same id; different seed → different id; the output is
        // a v4-shaped UUID (version nibble 4, RFC-4122 variant) like every
        // live CC session/prompt id, hiding the deterministic derivation.
        let a = derive_harness_uuid("openproxy:claude-harness:session:key-1:2026-10-06");
        let b = derive_harness_uuid("openproxy:claude-harness:session:key-1:2026-10-06");
        assert_eq!(a, b);
        assert_eq!(a.len(), 36);
        let other_key = derive_harness_uuid("openproxy:claude-harness:session:key-2:2026-10-06");
        let other_day = derive_harness_uuid("openproxy:claude-harness:session:key-1:2026-10-07");
        assert_ne!(a, other_key);
        assert_ne!(a, other_day);
        for id in [a, other_key, other_day] {
            let uuid = uuid::Uuid::parse_str(&id).expect("valid uuid");
            assert_eq!(uuid.get_version_num(), 4, "version nibble must be 4");
            assert_eq!(uuid.get_variant(), uuid::Variant::RFC4122);
        }
        // Prompt seeds derive independently from session seeds.
        let prompt = derive_harness_uuid("openproxy:claude-harness:prompt:key-1:2026-10-06T22");
        assert_ne!(
            prompt,
            derive_harness_uuid("openproxy:claude-harness:session:key-1:2026-10-06")
        );
    }

    #[test]
    fn usage_extraction_reads_anthropic_thinking_tokens() {
        // Live 2.1.289 usage carries the thinking breakdown as
        // output_tokens_details.thinking_tokens (MITM E16); the extractor
        // must surface it as reasoning_tokens alongside the OpenAI shape.
        let anthropic = json!({
            "usage": {
                "input_tokens": 2,
                "output_tokens": 126,
                "cache_read_input_tokens": 27149,
                "output_tokens_details": { "thinking_tokens": 28 }
            }
        });
        let parsed = extract_token_usage_from_value(&anthropic).expect("usage");
        assert_eq!(parsed.reasoning_tokens, Some(28));

        let openai = json!({
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "completion_tokens_details": { "reasoning_tokens": 5 }
            }
        });
        let parsed = extract_token_usage_from_value(&openai).expect("usage");
        assert_eq!(parsed.reasoning_tokens, Some(5));
    }

    #[test]
    fn refresh_guard_skips_claude_policy_rejections() {
        let policy = r#"{"error":{"message":"OAuth authentication is currently not allowed for this organization."}}"#;
        assert!(!is_refreshable_auth_failure(
            StatusCode::UNAUTHORIZED,
            Some(policy.as_bytes())
        ));
        // Structured 403 codes now include error.details.error_code; the
        // live scope fixture must not classify as refreshable (permission,
        // not expiry — a refresh cannot add scopes).
        let scope = br#"{"error":{"type":"permission_error","message":"OAuth token does not meet scope requirement any_of(user:ccr_inference, user:profile)","details":{"error_code":"oauth_scope_insufficient"}}}"#;
        assert!(!is_refreshable_auth_failure(
            StatusCode::FORBIDDEN,
            Some(scope)
        ));
        // Plain 401 without policy text still refreshes.
        let plain = r#"{"error":{"message":"invalid token"}}"#;
        assert!(is_refreshable_auth_failure(
            StatusCode::UNAUTHORIZED,
            Some(plain.as_bytes())
        ));
    }

    #[test]
    fn unified_reset_headers_yield_retry_after_only_when_rejected() {
        // 5h window rejected with a future epoch → advisory reset instant.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "anthropic-ratelimit-unified-5h-status",
            reqwest::header::HeaderValue::from_static("rejected"),
        );
        headers.insert(
            "anthropic-ratelimit-unified-5h-reset",
            reqwest::header::HeaderValue::from_str(
                &(Utc::now() + ChronoDuration::seconds(1800))
                    .timestamp()
                    .to_string(),
            )
            .unwrap(),
        );
        let reset = unified_reset_retry_after(&headers).expect("rejected 5h yields a reset");
        assert!(reset > Utc::now());

        // 7d window rejected → also yields, RFC 3339 form tolerated.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "anthropic-ratelimit-unified-7d-status",
            reqwest::header::HeaderValue::from_static("Rejected"),
        );
        headers.insert(
            "anthropic-ratelimit-unified-7d-reset",
            reqwest::header::HeaderValue::from_str(
                &(Utc::now() + ChronoDuration::seconds(3600))
                    .to_rfc3339()
                    .to_string(),
            )
            .unwrap(),
        );
        assert!(unified_reset_retry_after(&headers).is_some());

        // Allowed / absent-status / past-reset headers never synthesize.
        let allowed = reqwest::header::HeaderMap::from_iter([(
            reqwest::header::HeaderName::from_static("anthropic-ratelimit-unified-5h-status"),
            reqwest::header::HeaderValue::from_static("allowed"),
        )]);
        assert!(unified_reset_retry_after(&allowed).is_none());

        let mut past = reqwest::header::HeaderMap::new();
        past.insert(
            "anthropic-ratelimit-unified-5h-status",
            reqwest::header::HeaderValue::from_static("rejected"),
        );
        past.insert(
            "anthropic-ratelimit-unified-5h-reset",
            reqwest::header::HeaderValue::from_str(
                &(Utc::now() - ChronoDuration::seconds(60))
                    .timestamp()
                    .to_string(),
            )
            .unwrap(),
        );
        assert!(unified_reset_retry_after(&past).is_none());

        // Unified rejected while both windows read allowed (live capture
        // shape): the unified reset still yields.
        let mut unified_only = reqwest::header::HeaderMap::new();
        unified_only.insert(
            "anthropic-ratelimit-unified-status",
            reqwest::header::HeaderValue::from_static("rejected"),
        );
        unified_only.insert(
            "anthropic-ratelimit-unified-5h-status",
            reqwest::header::HeaderValue::from_static("allowed"),
        );
        unified_only.insert(
            "anthropic-ratelimit-unified-7d-status",
            reqwest::header::HeaderValue::from_static("allowed"),
        );
        unified_only.insert(
            "anthropic-ratelimit-unified-reset",
            reqwest::header::HeaderValue::from_str(
                &(Utc::now() + ChronoDuration::seconds(900))
                    .timestamp()
                    .to_string(),
            )
            .unwrap(),
        );
        assert!(unified_reset_retry_after(&unified_only).is_some());
    }

    #[test]
    fn tool_image_gate_uses_advertised_model_capability_and_overrides() {
        let mut snapshot = AppDb::default();
        assert_eq!(
            tool_image_support(&snapshot, "glm", "glm-5.3-flash", None),
            Some(true)
        );
        assert_eq!(
            tool_image_support(&snapshot, "glm", "glm-5.3", None),
            Some(false)
        );
        assert_eq!(tool_image_support(&snapshot, "glm", "glm-4.6v", None), None);

        let published = ModelsDevCatalog::default().load();
        assert_eq!(
            tool_image_support(
                &snapshot,
                "opencode-go",
                "glm-5.3-flash",
                published.find("opencode-go", "glm-5.3-flash"),
            ),
            Some(true)
        );

        snapshot.custom_models.push(CustomModel {
            provider_alias: "glm".into(),
            id: "glm-5.3-flash".into(),
            r#type: "llm".into(),
            name: None,
            extra: BTreeMap::from([(
                "opencode".into(),
                json!({
                    "modalities": {"input": ["text"], "output": ["text"]},
                    "attachment": false
                }),
            )]),
        });
        assert_eq!(
            tool_image_support(&snapshot, "glm", "glm-5.3-flash", None),
            Some(false)
        );

        snapshot.custom_models.push(CustomModel {
            provider_alias: "glm".into(),
            id: "glm-4.6v".into(),
            r#type: "llm".into(),
            name: None,
            extra: BTreeMap::from([(
                "opencode".into(),
                json!({
                    "modalities": {"input": ["text", "image"], "output": ["text"]}
                }),
            )]),
        });
        assert_eq!(
            tool_image_support(&snapshot, "glm", "glm-4.6v", None),
            Some(true)
        );
    }

    fn connection(id: &str, priority: u32) -> ProviderConnection {
        ProviderConnection {
            id: id.to_string(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            name: Some(id.into()),
            priority: Some(priority),
            is_active: Some(true),
            created_at: None,
            updated_at: None,
            display_name: None,
            email: None,
            global_priority: None,
            default_model: Some("gpt-4.1".into()),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: Some(format!("sk-{id}")),
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
        }
    }

    #[test]
    fn native_openai_and_claude_passthrough_never_enter_image_fetching() {
        let openai_body = json!({
            "model": "gpt-4.1",
            "messages": [{"role": "user", "content": [{
                "type": "image_url",
                "image_url": {"url": "https://example.test/image.png"}
            }]}]
        });
        let openai = RequestPlan::new(
            Some("/v1/chat/completions"),
            &openai_body,
            "openai",
            "gpt-4.1",
        );
        assert!(openai.passthrough);
        assert!(!should_prefetch_message_images(&openai));

        let claude_body = json!({
            "model": "claude-sonnet-4",
            "max_tokens": 128,
            "messages": [{"role": "user", "content": [{
                "type": "image",
                "source": {"type": "url", "url": "https://example.test/image.png"}
            }]}]
        });
        let claude = RequestPlan::new(
            Some("/v1/messages"),
            &claude_body,
            "claude",
            "claude-sonnet-4",
        );
        assert!(claude.passthrough);
        assert!(!should_prefetch_message_images(&claude));
    }

    #[test]
    fn image_prefetch_errors_map_to_terminal_413_or_502() {
        let size = super::image_prefetch_attempt_error(
            crate::core::translator::helpers::image_helper::ImagePrefetchError::ImageTooLarge {
                limit: 10,
            },
        );
        assert_eq!(size.status, 413);

        let validation = super::image_prefetch_attempt_error(
            crate::core::translator::helpers::image_helper::ImagePrefetchError::InvalidMagic,
        );
        assert_eq!(validation.status, 502);
    }

    #[test]
    fn codex_web_search_boundary_detects_native_tool_even_when_choice_is_none() {
        assert!(!has_native_codex_web_search(&json!({"messages": []})));
        assert!(has_native_codex_web_search(
            &json!({"tools": [{"type": "web_search"}]})
        ));
        assert!(has_native_codex_web_search(
            &json!({"tools": [{"type": "web_search"}], "tool_choice": "none"})
        ));
    }

    #[test]
    fn select_connection_ignores_persisted_routing_cooldowns() {
        let locked_until = (Utc::now() + ChronoDuration::seconds(90)).to_rfc3339();
        let mut preferred = connection("preferred", 1);
        preferred.rate_limited_until = Some(locked_until.clone());
        preferred.extra.insert(
            "modelLock_gpt-4.1".into(),
            Value::String(locked_until.clone()),
        );
        preferred
            .extra
            .insert("degradedUntil".into(), Value::String(locked_until));

        let fallback = connection("fallback", 2);

        let snapshot = AppDb {
            provider_connections: vec![preferred.clone(), fallback],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("legacy cooldown fields must not suppress routing");

        assert_eq!(selected.id, preferred.id);
    }

    #[test]
    fn codex_discovered_model_uses_only_supporting_accounts() {
        let mut first = connection("first", 1);
        first.provider = "codex".into();
        first.default_model = None;
        let mut supporting = connection("supporting", 2);
        supporting.provider = "codex".into();
        supporting.default_model = None;
        let snapshot = AppDb {
            provider_connections: vec![first, supporting.clone()],
            ..AppDb::default()
        };
        let supporters = HashSet::from([supporting.id.clone()]);

        let selected = select_connection_with_supporters(
            &snapshot,
            "codex",
            "gpt-discovered",
            &HashSet::new(),
            Some(&supporters),
        )
        .expect("supporting account should be selected");

        assert_eq!(selected.id, supporting.id);
    }

    #[test]
    fn select_connection_skips_inactive_connections() {
        let mut inactive = connection("inactive", 1);
        inactive.is_active = Some(false);

        let available = connection("active", 2);

        let snapshot = AppDb {
            provider_connections: vec![inactive, available.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should select an account");

        assert_eq!(selected.id, "active");
    }

    #[test]
    fn select_connection_skips_connections_without_credentials() {
        let mut no_creds = connection("no-creds", 1);
        no_creds.api_key = None;
        no_creds.access_token = None;

        let with_creds = connection("with-creds", 2);

        let snapshot = AppDb {
            provider_connections: vec![no_creds, with_creds.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should select an account");

        assert_eq!(selected.id, "with-creds");
    }

    #[test]
    fn select_connection_prioritizes_by_priority_field() {
        let low_priority = connection("low-priority", 2);
        let high_priority = connection("high-priority", 1);

        let snapshot = AppDb {
            provider_connections: vec![low_priority, high_priority.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should select an account");

        assert_eq!(selected.id, "high-priority");
    }

    #[test]
    fn select_connection_filters_by_model_support() {
        let mut conn_a = connection("conn-a", 1);
        conn_a.default_model = None;
        conn_a
            .provider_specific_data
            .insert("enabledModels".into(), json!(["gpt-4o"]));

        let mut conn_b = connection("conn-b", 2);
        conn_b.default_model = None;
        conn_b
            .provider_specific_data
            .insert("enabledModels".into(), json!(["gpt-4.1"]));

        let snapshot = AppDb {
            provider_connections: vec![conn_a, conn_b.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should select an account");

        assert_eq!(selected.id, "conn-b");
    }

    #[test]
    fn a6api_selects_only_the_key_with_the_exact_model_id() {
        let mut wildcard = connection("wildcard", 1);
        wildcard.provider = "a6api".into();
        wildcard.default_model = None;

        let mut wrong_suffix = connection("wrong-suffix", 2);
        wrong_suffix.provider = "a6api".into();
        wrong_suffix.default_model = None;
        wrong_suffix.provider_specific_data.insert(
            "enabledModels".into(),
            json!(["vendor/deepseek-v4.1-flash"]),
        );

        let mut supporting = connection("supporting", 3);
        supporting.provider = "a6api".into();
        supporting.default_model = None;
        supporting
            .provider_specific_data
            .insert("enabledModels".into(), json!(["deepseek-v4.1-flash"]));

        let snapshot = AppDb {
            provider_connections: vec![wildcard, wrong_suffix, supporting.clone()],
            ..AppDb::default()
        };
        let selected =
            select_connection(&snapshot, "a6api", "deepseek-v4.1-flash", &HashSet::new())
                .expect("exact supporting A6API key should be selected");
        assert_eq!(selected.id, supporting.id);
        assert!(select_connection(&snapshot, "a6api", "missing", &HashSet::new()).is_none());
    }

    #[test]
    fn select_connection_returns_none_when_all_excluded() {
        let conn_a = connection("conn-a", 1);
        let conn_b = connection("conn-b", 2);

        let snapshot = AppDb {
            provider_connections: vec![conn_a, conn_b],
            ..AppDb::default()
        };

        let excluded: HashSet<String> = ["conn-a".to_string(), "conn-b".to_string()]
            .into_iter()
            .collect();

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &excluded);
        assert!(
            selected.is_none(),
            "should return None when all accounts excluded"
        );
    }

    #[test]
    fn select_connection_returns_none_when_no_connections_match() {
        let snapshot = AppDb::default();

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new());
        assert!(
            selected.is_none(),
            "should return None when no connections exist"
        );
    }

    #[tokio::test]
    async fn attempt_error_response_appends_reset_timer_on_429() {
        async fn body_text(response: Response) -> (StatusCode, Value, bool) {
            let status = response.status();
            let has_retry_after = response.headers().contains_key("retry-after");
            let collected = response
                .into_body()
                .collect()
                .await
                .expect("error body should collect");
            let raw = collected.to_bytes();
            let parsed: Value = serde_json::from_slice(&raw).expect("suffixed body stays JSON");
            (status, parsed, has_retry_after)
        }

        fn attempt(status: u16, body: &[u8]) -> ProviderAttemptError {
            ProviderAttemptError {
                status,
                message: "upstream failure".to_string(),
                retry_after: None,
                upstream_body: Some(body.to_vec()),
            }
        }

        let now =
            DateTime::from_timestamp(Utc::now().timestamp(), 0).expect("valid test timestamp");
        // Mid-minute reset: renderer-side clock truncation must not flip minutes.
        let resets_at = (now + ChronoDuration::seconds(2 * 3600 + 14 * 60 + 30)).timestamp();
        let codex_body = format!(
            r#"{{"error":{{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_at":{resets_at}}}}}"#
        );

        // Codex JSON 429: message gains the timer, type/code/status kept, no Retry-After.
        let (status, parsed, has_retry_after) =
            body_text(attempt_error_response(attempt(429, codex_body.as_bytes()))).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            parsed["error"]["message"],
            "The usage limit has been reached (resets in 2h 14m)"
        );
        assert_eq!(parsed["error"]["type"], "usage_limit_reached");
        assert!(
            !has_retry_after,
            "display reset must not synthesize Retry-After"
        );

        // SSE-wrapped Codex 429 (synthetic preflight body): same suffix.
        let sse_body = format!("event: error\ndata: {codex_body}\n\n");
        let (_, parsed, _) =
            body_text(attempt_error_response(attempt(429, sse_body.as_bytes()))).await;
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("(resets in 2h 14m)"));

        // Non-429 with a reset signal: untouched.
        let (status, parsed, _) =
            body_text(attempt_error_response(attempt(500, codex_body.as_bytes()))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            parsed["error"]["message"],
            "The usage limit has been reached"
        );

        // 429 without a reset signal: byte-identical passthrough.
        let plain = br#"{"error":{"type":"rate_limit_exceeded","message":"Slow down"}}"#;
        let response = attempt_error_response(attempt(429, plain));
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let collected = response
            .into_body()
            .collect()
            .await
            .expect("error body should collect");
        assert_eq!(collected.to_bytes().as_ref(), plain);
    }

    #[tokio::test]
    async fn build_dashboard_sse_response_returns_collectable_sse_body() {
        let body = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        );
        let response = build_dashboard_sse_response(
            StatusCode::OK,
            &reqwest::header::HeaderMap::new(),
            body.clone(),
        );

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "text/event-stream; charset=utf-8"
        );
        assert_eq!(
            response.headers()[axum::http::header::CACHE_CONTROL],
            "no-cache"
        );

        let collected = response
            .into_body()
            .collect()
            .await
            .expect("dashboard SSE body should collect");

        assert_eq!(collected.to_bytes(), body);
    }

    #[tokio::test]
    async fn build_proxied_response_preserves_plain_body_roundtrip() {
        let body = Bytes::from_static(b"hello world");
        let response = build_proxied_response(
            StatusCode::OK,
            &reqwest::header::HeaderMap::new(),
            axum::body::Body::from(body.clone()),
        );

        let collected = response
            .into_body()
            .collect()
            .await
            .expect("plain proxied body should collect");

        assert_eq!(collected.to_bytes(), body);
    }

    /// 9router parity (open-sse/shared/clineEnvelope.js unwrapClineEnvelope +
    /// tests/unit/cline-free-models-envelope.test.js): non-stream Cline/ClinePass
    /// responses wrapped in {"success":true,"data":...} unwrap to data before
    /// usage extraction/translation; the error envelope passes through untouched.
    #[test]
    fn unwrap_cline_envelope_success_unwraps_to_data() {
        let body = br#"{"success":true,"data":{"choices":[{"message":{"content":"Hi"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}}}"#;
        for provider in ["cline", "clinepass"] {
            let out = super::unwrap_cline_envelope(body, provider);
            let val: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(val["choices"][0]["message"]["content"], "Hi");
            assert!(val.get("success").is_none());
        }
    }

    #[test]
    fn unwrap_cline_envelope_error_passes_through() {
        let body = br#"{"success":false,"error":"empty response content"}"#;
        for provider in ["cline", "clinepass"] {
            let out = super::unwrap_cline_envelope(body, provider);
            let val: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(val["success"], false);
            assert_eq!(val["error"], "empty response content");
        }
    }

    #[test]
    fn unwrap_cline_envelope_non_opt_in_provider_untouched() {
        // The unwrap is opt-in via transport.quirks.clineEnvelope so it can
        // never rewrite another provider's body — including one that happens
        // to return {"success":true,"data":...} for its own reasons.
        let body = br#"{"success":true,"data":{"choices":[{"message":{"content":"Hi"}}]}}"#;
        let out = super::unwrap_cline_envelope(body, "openai");
        let val: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(val["success"], true);
        assert_eq!(val["data"]["choices"][0]["message"]["content"], "Hi");
        assert!(val.get("choices").is_none());
    }

    #[test]
    fn unwrap_cline_envelope_bare_body_unchanged() {
        let body = br#"{"choices":[{"message":{"content":"Hi"}}]}"#;
        for provider in ["cline", "clinepass"] {
            let out = super::unwrap_cline_envelope(body, provider);
            let val: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(val["choices"][0]["message"]["content"], "Hi");
        }
    }

    #[test]
    fn detects_fragmented_responses_completion_frame() {
        let mut observer = StreamDispatch::new(
            Format::OpenAiResponses,
            Format::OpenAiResponses,
            "text/event-stream",
            None,
            None,
            None,
            true,
        );
        assert!(!observer.feed(b"event: response.compl").response_completed);
        assert!(
            observer
                .feed(b"eted\r\ndata: {\"type\":\"response.completed\"}\r\n\r\n: ping\r\n\r\n")
                .response_completed
        );
    }

    #[tokio::test]
    async fn codex_cache_observes_original_native_translated_and_forced_usage() {
        use crate::core::executor::codex_cache::CodexCacheObservation;
        let fixture = b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"output\":[],\"usage\":{\"input_tokens\":100,\"output_tokens\":1,\"input_tokens_details\":{\"cached_tokens\":80}}}}\n\n";
        for target in [Format::OpenAiResponses, Format::OpenAi] {
            let observation = CodexCacheObservation::default();
            observation.sent(
                &json!({"model":"test","input":[]}),
                &Default::default(),
                "https://example.test/responses",
            );
            let mut dispatch = StreamDispatch::new(
                Format::OpenAiResponses,
                target,
                "text/event-stream",
                None,
                None,
                None,
                false,
            );
            dispatch.codex_cache = Some(observation.clone());
            dispatch.feed(&fixture[..17]);
            assert!(observation.snapshot().unwrap()["completedAt"].is_null());
            let batch = dispatch.feed(&fixture[17..]);
            assert!(batch.error.is_none());
            assert!(!batch.response_completed); // Logging does not change stop policy.
            let data = observation.snapshot().unwrap();
            assert_eq!(data["cachedTokens"], 80);
            assert!(data["completedAt"].is_string());
        }
        let observation = CodexCacheObservation::default();
        observation.sent(
            &json!({"model":"test","input":[]}),
            &Default::default(),
            "https://example.test/responses",
        );
        let mut response = axum::http::Response::new(reqwest::Body::from(fixture.to_vec()));
        response
            .headers_mut()
            .insert("content-type", "text/event-stream".parse().unwrap());
        let response = super::UpstreamResponse::Reqwest(reqwest::Response::from(response));
        assert!(
            super::collect_forced_sse(response, Some(observation.clone()))
                .await
                .is_ok()
        );
        assert_eq!(observation.snapshot().unwrap()["cachedTokens"], 80);

        // EOF/framing failure after observed usage must not erase it.
        let observation = CodexCacheObservation::default();
        observation.sent(
            &json!({"model":"test","input":[]}),
            &Default::default(),
            "https://example.test/responses",
        );
        let body = b"data: {\"usage\":{\"input_tokens\":100,\"input_tokens_details\":{\"cached_tokens\":80}}}\n\n\xff";
        let response = reqwest::Response::from(axum::http::Response::new(reqwest::Body::from(
            body.to_vec(),
        )));
        assert!(super::collect_forced_sse(
            super::UpstreamResponse::Reqwest(response),
            Some(observation.clone())
        )
        .await
        .is_err());
        let data = observation.snapshot().unwrap();
        assert_eq!(data["cachedTokens"], 80);
        assert!(data["completedAt"].is_null());
    }

    #[test]
    fn usage_and_dashboard_framing_survive_every_split() {
        let fixture = b": ping\r\nevent: chunk\r\nid: 4\r\nretry: 250\r\ndata: {\"choices\":[],\r\ndata: \"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\r\n\r\n";
        for split in 0..=fixture.len() {
            let mut usage = StreamDispatch::new(
                Format::OpenAi,
                Format::OpenAi,
                "text/event-stream",
                None,
                None,
                None,
                false,
            );
            usage.feed(&fixture[..split]);
            usage.feed(&fixture[split..]);
            assert_eq!(
                usage.usage.as_ref().and_then(|value| value.total_tokens),
                Some(5)
            );

            let mut dashboard = StreamDispatch::new(
                Format::OpenAi,
                Format::OpenAi,
                "text/event-stream",
                Some(Box::new(OpenAiTransformer::new())),
                None,
                None,
                false,
            );
            let mut output = dashboard.feed(&fixture[..split]).output;
            output.extend(dashboard.feed(&fixture[split..]).output);
            let output = output
                .iter()
                .map(|bytes| String::from_utf8_lossy(bytes))
                .collect::<String>();
            assert!(output.contains("total_tokens"));
            assert!(output.contains(": ping"));
            assert!(output.contains("event: chunk"));
            assert!(output.contains("id: 4"));
            assert!(output.contains("retry: 250"));
        }
    }

    #[test]
    fn translated_stream_observes_usage_only_frame() {
        let mut dispatch = StreamDispatch::new(
            Format::OpenAi,
            Format::Gemini,
            "text/event-stream",
            None,
            None,
            None,
            false,
        );
        dispatch.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\n");
        dispatch.feed(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
        dispatch.feed(
            b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":16,\"completion_tokens\":25,\"total_tokens\":41}}\n\n",
        );
        assert_eq!(
            dispatch
                .usage
                .as_ref()
                .and_then(|value| value.prompt_tokens),
            Some(16)
        );
        assert_eq!(
            dispatch
                .usage
                .as_ref()
                .and_then(|value| value.completion_tokens),
            Some(25)
        );
    }

    #[test]
    fn dashboard_line_dispatch_handles_split_ollama_ndjson_records() {
        let fixture = concat!(
            "{\"model\":\"llama\",\"message\":{\"content\":\"line-one\"},\"done\":false}\n",
            "{\"model\":\"llama\",\"message\":{\"content\":\"line-two\"},\"done\":false}\n"
        );
        for split in 0..=fixture.len() {
            let mut dashboard = StreamDispatch::new(
                Format::Ollama,
                Format::OpenAi,
                "application/x-ndjson",
                super::transformer_for_provider("ollama"),
                None,
                None,
                false,
            );
            let mut output = dashboard.feed(&fixture.as_bytes()[..split]).output;
            output.extend(dashboard.feed(&fixture.as_bytes()[split..]).output);
            let output = output
                .iter()
                .map(|bytes| String::from_utf8_lossy(bytes))
                .collect::<String>();
            assert!(output.contains("line-one"), "split {split}: {output}");
            assert!(output.contains("line-two"), "split {split}: {output}");
        }
    }

    #[test]
    fn dashboard_dispatch_uses_resolved_wire_format() {
        let gemini = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"format-gemini\"}]}}]}\n\n";
        for source in [Format::Gemini, Format::Vertex, Format::Antigravity] {
            let mut dashboard = StreamDispatch::new(
                source,
                Format::OpenAi,
                "text/event-stream",
                super::dashboard_transformer_for_format(source),
                None,
                None,
                false,
            );
            let output = dashboard
                .feed(gemini)
                .output
                .iter()
                .map(|bytes| String::from_utf8_lossy(bytes))
                .collect::<String>();
            assert!(output.contains("format-gemini"), "{source:?}: {output}");
        }
    }
}
