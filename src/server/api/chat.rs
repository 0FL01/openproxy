use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

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

use crate::core::account_fallback::{
    build_model_lock_update, check_fallback_error, filter_available_accounts, ProviderAttemptError,
};
use crate::core::chat::RequestPlan;
use crate::core::executor::UpstreamResponse;
use crate::core::model::get_model_info;
use crate::core::proxy::resolve_proxy_target;
use crate::core::translator::helpers::image_helper::fetch_image_as_base64;
use crate::core::translator::helpers::modality_helper::{
    capabilities_for_format, strip_unsupported_modalities, ModalityCapabilities,
};
use crate::core::translator::registry::{self, Format};
use crate::core::translator::response_transform::{transform_sse_stream, transformer_for_provider};
use crate::core::utils::client_detector::{detect_client_tool, is_native_passthrough, ClientTool};
use crate::core::utils::stream_flags::resolve_stream_flags;
use crate::server::application_logs::{AttemptLog, RequestLogContext};
use crate::server::auth::{extract_api_key, require_api_key, require_api_key_with_reload};
use crate::server::state::AppState;
use crate::types::{AppDb, ProviderConnection, TokenUsage};

use super::auth_error_response;

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

pub(super) const CODEX_WEB_SEARCH_HEADER: &str = "x-openproxy-codex-web-search";
const CODEX_WEB_SEARCH_CONTEXT_SIZE_KEY: &str = "codexWebSearchContextSize";

#[derive(Clone, Debug)]
pub(super) struct CodexWebSearchInjected;

fn has_native_codex_web_search(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("type").and_then(Value::as_str) == Some("web_search"))
        })
}

fn codex_web_search_context_size(settings: &crate::types::Settings) -> Option<&'static str> {
    match settings
        .extra
        .get(CODEX_WEB_SEARCH_CONTEXT_SIZE_KEY)
        .and_then(Value::as_str)
    {
        Some("off") => None,
        Some("low") => Some("low"),
        Some("high") => Some("high"),
        Some("medium") | None => Some("medium"),
        Some(_) => Some("medium"),
    }
}

fn requests_codex_web_search(headers: &HeaderMap, body: &Value) -> bool {
    if body.get("tool_choice").and_then(Value::as_str) == Some("none") {
        return false;
    }

    let header_enabled = headers
        .get(CODEX_WEB_SEARCH_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
    header_enabled || has_native_codex_web_search(body)
}

fn mark_codex_web_search_injected(mut response: Response, injected: bool) -> Response {
    if injected {
        response.extensions_mut().insert(CodexWebSearchInjected);
    }
    response
}

fn codex_web_search_is_injected(context_size: Option<&str>, body: &Value) -> bool {
    context_size.is_some() && !has_native_codex_web_search(body)
}

fn codex_models_support_search(
    models: &[crate::server::codex_catalog::CodexModelMetadata],
    model: &str,
) -> bool {
    let model = model.strip_prefix("codex/").unwrap_or(model);
    models.iter().any(|candidate| {
        candidate.capabilities.iter().any(|value| value == "search")
            && (candidate.id == model
                || candidate.reasoning_efforts.iter().any(|effort| {
                    model == format!("{}-{effort}", candidate.id)
                        || model == format!("{}({effort})", candidate.id)
                }))
    })
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
    let _log =
        crate::server::request_logger::RequestLog::start("POST", "/v1/chat/completions", model);
    let response = with_cors_response(
        chat_completions_for_endpoint(state, headers, body, Some("/v1/chat/completions")).await,
    );
    _log.finish(response.status().as_u16());
    response
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

    chat_completions_impl(
        state,
        headers,
        body,
        Some("/api/dashboard/chat/completions"),
        false,
    )
    .await
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
    let request_log_context = authenticated_api_key
        .as_ref()
        .map(|api_key| RequestLogContext::new(state.db.clone(), api_key, model_str));

    let snapshot = state.db.snapshot();
    let resolved = get_model_info(model_str, &snapshot);

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

    // 9router parity: cache Claude-specific headers from incoming request
    // for replay on subsequent requests (claudeHeaderCache).
    crate::core::utils::claude_header_cache::cache_claude_headers(&headers_map);

    let client_tool = detect_client_tool(&headers_map, &body);
    let codex_web_search_requested = requests_codex_web_search(&headers, &body);

    // Accept/stream preference is applied via resolve_stream_flags on the plan
    // (does NOT mutate body.stream when client set stream:true — 9router parity).
    let accept_header = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let estimated_tokens = crate::core::context_limit::estimate_input_tokens(&body);
    if let Some(error) = context_limit_error(
        &state,
        &snapshot.settings,
        resolved.provider.as_deref().unwrap_or(model_str),
        &resolved.model,
        estimated_tokens,
    )
    .await
    {
        return attempt_error_response(error);
    }
    let mut plan = RequestPlan::new(
        endpoint,
        &body,
        resolved.provider.as_deref().unwrap_or(model_str),
        &resolved.model,
    );
    plan.passthrough = is_native_passthrough(client_tool, &plan.provider);
    apply_stream_plan(&mut plan, &body, accept_header.as_deref(), client_tool);
    let response = match execute_single_model(
        &state,
        &body,
        model_str,
        presented_api_key.as_deref(),
        request_log_context.as_ref(),
        endpoint,
        &plan,
        client_tool,
        Some(&headers_map),
        codex_web_search_requested,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => attempt_error_response(error),
    };

    response
}

/// Prefetch remote images in OpenAI/Claude message content arrays.
async fn prefetch_images_in_messages(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    let client = reqwest::Client::new();
    for msg in messages.iter_mut() {
        let content_array = match msg.get_mut("content") {
            Some(Value::Array(arr)) => arr,
            _ => continue,
        };
        for part in content_array.iter_mut() {
            if let Some(url) = part
                .get("image_url")
                .and_then(|iu| iu.get("url"))
                .and_then(|u| u.as_str())
            {
                if url.starts_with("http://") || url.starts_with("https://") {
                    if let Some(fetched) = fetch_image_as_base64(&client, url).await {
                        if let Some(img) =
                            part.get_mut("image_url").and_then(|iu| iu.as_object_mut())
                        {
                            img.insert("url".into(), Value::String(fetched.data_url));
                        }
                    }
                }
            }
            if let Some(source) = part.get("image").and_then(|im| im.get("source")) {
                if source.get("type").and_then(|t| t.as_str()) == Some("url") {
                    if let Some(url) = source.get("url").and_then(|u| u.as_str()) {
                        if url.starts_with("http://") || url.starts_with("https://") {
                            if let Some(fetched) = fetch_image_as_base64(&client, url).await {
                                if let Some(src) = part
                                    .get_mut("image")
                                    .and_then(|im| im.get_mut("source"))
                                    .and_then(|s| s.as_object_mut())
                                {
                                    src.insert("data".into(), Value::String(fetched.data_url));
                                    src.insert("type".into(), Value::String("base64".into()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
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

async fn native_context_window(state: &AppState, provider: &str, model: &str) -> Option<u32> {
    let provider = crate::core::context_limit::canonical_provider(provider)?;
    if crate::core::model::models_dev::is_opencode_provider(provider) {
        return state
            .models_dev
            .snapshot()
            .await
            .ok()?
            .find(provider, model)
            .and_then(|metadata| metadata.context_window);
    }
    if provider == "codex" {
        let snapshot = state.db.snapshot();
        return state
            .codex_models
            .union_active(state, &snapshot.provider_connections)
            .await
            .models
            .iter()
            .find(|metadata| metadata.id == model)
            .and_then(|metadata| metadata.context_window)
            .and_then(|value| u32::try_from(value).ok());
    }
    crate::core::model::catalog::provider_catalog()
        .find_model(provider, model)
        .and_then(|metadata| metadata.context_window)
}

fn context_limit_attempt_error(
    provider: &str,
    estimated_tokens: u64,
    effective_limit: u32,
    configured_limit: u32,
) -> ProviderAttemptError {
    ProviderAttemptError {
        status: 413,
        message: format!(
            "Context limit exceeded for provider {provider}: estimated {estimated_tokens} input tokens, effective limit {effective_limit}, configured limit {configured_limit}"
        ),
        retry_after: None,
        upstream_body: None,
    }
}

async fn context_limit_error(
    state: &AppState,
    settings: &crate::types::Settings,
    provider: &str,
    model: &str,
    estimated_tokens: u64,
) -> Option<ProviderAttemptError> {
    let configured =
        crate::core::context_limit::configured_limit(&settings.provider_context_limits, provider)?;
    let native = native_context_window(state, provider, model).await;
    let effective = crate::core::context_limit::effective_limit(configured, native);
    (estimated_tokens > u64::from(effective))
        .then(|| context_limit_attempt_error(provider, estimated_tokens, effective, configured))
}

async fn execute_single_model(
    state: &AppState,
    request_body: &Value,
    model_str: &str,
    api_key: Option<&str>,
    log_context: Option<&RequestLogContext>,
    endpoint: Option<&'static str>,
    base_plan: &RequestPlan,
    client_tool: Option<ClientTool>,
    client_headers: Option<&std::collections::HashMap<String, String>>,
    codex_web_search_requested: bool,
) -> Result<Response, ProviderAttemptError> {
    let snapshot = state.db.snapshot();
    let mut plan = base_plan.clone();
    if crate::core::model::models_dev::is_opencode_provider(&plan.provider) {
        let models = state
            .models_dev
            .snapshot()
            .await
            .map_err(|message| ProviderAttemptError {
                status: 503,
                message,
                retry_after: None,
                upstream_body: None,
            })?;
        let metadata = models
            .find(&plan.provider, plan.dispatch_model())
            .ok_or_else(|| ProviderAttemptError {
                status: 400,
                message: format!(
                    "Model {} is not published for {} by models.dev",
                    plan.dispatch_model(),
                    plan.provider
                ),
                retry_after: None,
                upstream_body: None,
            })?;
        plan.apply_opencode_metadata(metadata);
    }

    let estimated_tokens = crate::core::context_limit::estimate_input_tokens(request_body);
    if let Some(error) = context_limit_error(
        state,
        &snapshot.settings,
        &plan.provider,
        plan.dispatch_model(),
        estimated_tokens,
    )
    .await
    {
        return Err(error);
    }

    let mut body = request_body.clone();
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

    // Catalog stripList (image/audio) before modality strip — 9router translateRequest stripList
    if !plan.strip_list.is_empty() {
        let refs: Vec<&str> = plan.strip_list.iter().map(String::as_str).collect();
        registry::strip_content_types(&mut body, &refs);
    }

    // 1–2. Modality strip + image prefetch only when NOT passthrough (9router)
    if !plan.passthrough {
        let caps = capabilities_for_format(plan.source_format);
        strip_unsupported_modalities(&mut body, plan.source_format, &caps);

        if plan.target_format.needs_image_prefetch() {
            prefetch_images_in_messages(&mut body).await;
        }
    }

    // Dispatch uses catalog upstreamModelId when set
    let dispatch_model = plan.dispatch_model().to_string();
    if let Some(fields) = body.as_object_mut() {
        fields.insert("model".into(), Value::String(dispatch_model.clone()));
    }

    // 3. Translate or native passthrough normalize
    if plan.passthrough {
        tracing::debug!(
            target: "openproxy::chat",
            "PASSTHROUGH client={:?} provider={}",
            client_tool,
            plan.provider
        );
        if client_tool == Some(ClientTool::Claude) {
            crate::core::translator::request::claude_format::normalize_claude_passthrough(
                &mut body,
                &dispatch_model,
            );
        }
    } else if plan.needs_translation() {
        // Include rawHeaders so Kiro session-replay can resolve a stable
        // conversationId from client session headers (x-session-id, etc.).
        let mut creds = json!({
            "provider": plan.provider,
        });
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

    // Pin cache breakpoints LAST on Claude passthrough (9router
    // chatCore.js:306): every saver above can reshape system/tools/messages,
    // and a stale anchor costs a full prefix rewrite.
    if plan.passthrough && client_tool == Some(ClientTool::Claude) {
        crate::core::translator::request::claude_format::anchor_claude_cache(&mut body);
    }

    // Sync stream flag onto body for executors that read body.stream
    if let Some(obj) = body.as_object_mut() {
        obj.insert("stream".into(), Value::Bool(plan.stream));
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
        codex_web_search_requested,
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
    codex_web_search_requested: bool,
) -> Result<Response, ProviderAttemptError> {
    let mut excluded = HashSet::new();
    let mut last_error: Option<ProviderAttemptError> = None;
    let mut reloaded = false;
    let codex_supporters = if provider == "codex" {
        let snapshot = state.db.snapshot();
        state
            .codex_models
            .cached_supporters(model, &snapshot.provider_connections)
            .await
    } else {
        None
    };

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
            let retry_after = earliest_retry_after(&snapshot, provider, model, &excluded);
            if let Some(mut error) = last_error {
                if retry_after.is_some() {
                    error.retry_after = retry_after;
                }
                return Err(error);
            }

            // Stale-snapshot recovery: if the CLI added a provider
            // connection while the server was running, the in-memory
            // snapshot won't have it. Reload from SQLite once and retry.
            if !reloaded && retry_after.is_none() {
                reloaded = true;
                if state.db.reload_snapshot().await.is_ok() {
                    continue;
                }
            }

            return Err(ProviderAttemptError {
                status: if retry_after.is_some() { 503 } else { 400 },
                message: if retry_after.is_some() {
                    format!("All accounts for {provider}/{model} are cooling down")
                } else {
                    format!("No credentials for provider: {provider}")
                },
                retry_after,
                upstream_body: None,
            });
        };

        let native_codex_web_search_requested =
            codex_web_search_requested && has_native_codex_web_search(&request_body);
        let injected_codex_web_search_context =
            if codex_web_search_requested && !native_codex_web_search_requested {
                codex_web_search_context_size(&snapshot.settings)
            } else {
                None
            };
        let effective_codex_web_search_requested =
            native_codex_web_search_requested || injected_codex_web_search_context.is_some();

        let enable_codex_web_search = if effective_codex_web_search_requested && provider == "codex"
        {
            match state
                .codex_models
                .models_for_connection(state, &connection)
                .await
            {
                Ok(inventory) if codex_models_support_search(&inventory.models, model) => {
                    // Model discovery may refresh an expiring OAuth token. Use
                    // the persisted replacement for the actual request.
                    if let Some(refreshed) = state
                        .db
                        .snapshot()
                        .provider_connections
                        .iter()
                        .find(|candidate| candidate.id == connection.id)
                        .cloned()
                    {
                        connection = refreshed;
                    }
                    true
                }
                Ok(_) => {
                    last_error = Some(ProviderAttemptError::new(
                        400,
                        format!(
                            "Codex web search is not supported for model {model} on this account"
                        ),
                    ));
                    excluded.insert(connection.id.clone());
                    continue;
                }
                Err(error) => {
                    last_error = Some(ProviderAttemptError::new(
                        error.status.as_u16(),
                        format!(
                            "Unable to verify Codex web search support: {}",
                            error.message
                        ),
                    ));
                    excluded.insert(connection.id.clone());
                    continue;
                }
            }
        } else {
            false
        };
        let web_search_context_size = if enable_codex_web_search {
            injected_codex_web_search_context.map(str::to_string)
        } else {
            None
        };
        let codex_web_search_injected =
            codex_web_search_is_injected(web_search_context_size.as_deref(), &request_body);

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

        let attempt_log = match log_context {
            Some(context) => context.start_attempt(provider, model).await,
            None => None,
        };

        use crate::core::executor::{
            AntigravityExecutionRequest, AntigravityExecutor, AzureExecutionRequest, AzureExecutor,
            CodexExecutionRequest, CodexExecutor, CommandCodeExecutionRequest, CommandCodeExecutor,
            CursorExecutionRequest, CursorExecutor, DefaultExecutor, DevinCliExecutor,
            DevinExecutionRequest, ExecutionRequest, GeminiCliExecutionRequest, GeminiCliExecutor,
            GithubExecutionRequest, GithubExecutor, GrokWebExecutionRequest, GrokWebExecutor,
            IFlowExecutionRequest, IFlowExecutor, KimchiExecutor, KiroExecutionRequest,
            KiroExecutor, KiroExecutorResponse, OpenCodeExecutionRequest, OpenCodeExecutor,
            OpenCodeTier, ProviderExecutionRequest, ProviderExecutor, QoderExecutionRequest,
            QoderExecutor, QwenExecutionRequest, QwenExecutor, TraeExecutionRequest, TraeExecutor,
            VertexExecutionRequest, VertexExecutor, WindsurfExecutionRequest, WindsurfExecutor,
        };

        let is_codex_model = provider == "codex";
        let is_cursor_model =
            model.starts_with("cursor/") || provider == "cu" || provider == "cursor";
        let executor_result: Result<KiroExecutorResponse, ProviderAttemptError> = async {
            if provider == "kiro" {
                let executor = KiroExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Kiro executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                executor
                    .execute_request(KiroExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Kiro execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })
            } else if provider == "vertex" || provider == "vertex-partner" || provider == "vxp" {
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                    .execute(CodexExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        web_search_context_size: web_search_context_size.clone(),
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Codex execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if is_cursor_model {
                let executor = CursorExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Cursor executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute(CursorExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Cursor execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "qwen" {
                let executor = QwenExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Qwen executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(QwenExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Qwen execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "iflow" {
                let executor = IFlowExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("IFlow executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(IFlowExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("IFlow execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "gemini-cli" {
                let executor = GeminiCliExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("GeminiCli executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(GeminiCliExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("GeminiCli execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "qoder" {
                let executor = QoderExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Qoder executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(QoderExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Qoder execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "commandcode" {
                let executor = CommandCodeExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                    status: 500,
                    message: format!("CommandCode executor creation failed: {:?}", e),
                    retry_after: None,
                    upstream_body: None,
                })?;
                let result = executor
                    .execute_request(CommandCodeExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("CommandCode execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "grok-web" {
                let executor = GrokWebExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(GrokWebExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("GrokWeb execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "windsurf" || provider == "ws" {
                let executor = WindsurfExecutor::new(state.client_pool.clone());
                let result = executor
                    .execute_request(WindsurfExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("Windsurf execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url.clone(),
                    headers: HeaderMap::new(),
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "ollama-local" || provider == "ollama" {
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
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
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else if provider == "grok-cli"
                || provider == "gcli"
                || provider == "gb"
                || provider == "grok-build"
            {
                use crate::core::executor::{GrokCliExecutionRequest, GrokCliExecutor};
                let executor = GrokCliExecutor::new(state.client_pool.clone(), provider_node)
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("GrokCli executor creation failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                let result = executor
                    .execute_request(GrokCliExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream: true, // forceStream (9router)
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|e| ProviderAttemptError {
                        status: 500,
                        message: format!("GrokCli execution failed: {:?}", e),
                        retry_after: None,
                        upstream_body: None,
                    })?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            } else {
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
                let result = executor
                    .execute(ExecutionRequest {
                        model: model.to_string(),
                        body: request_body.clone(),
                        stream,
                        credentials: connection.clone(),
                        proxy,
                    })
                    .await
                    .map_err(|err| err.into_provider_attempt_error())?;
                Ok(KiroExecutorResponse {
                    response: result.response,
                    url: result.url,
                    headers: result.headers,
                    transformed_body: result.transformed_body,
                    transport: result.transport,
                })
            }
        }
        .await;

        let execution = executor_result;

        match execution {
            Ok(result) => {
                let status = result.response.status();
                if status.is_success() {
                    clear_connection_error_for_model(state, &connection.id, Some(model)).await;
                    if dashboard_stream {
                        let response = proxy_dashboard_sse(result.response, attempt_log).await;
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
                        let response =
                            mark_codex_web_search_injected(response, codex_web_search_injected);
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
                        attempt_log,
                    )
                    .await;
                    let response =
                        mark_codex_web_search_injected(response, codex_web_search_injected);
                    return Ok(response);
                }

                // 9router parity: retryAfter may come from the Retry-After header
                // OR the error JSON body (errorBody.retryAfter). Header wins; the
                // body is the fallback when a provider returns it only in JSON.
                let header_retry_after = retry_after_from_headers(result.response.headers());
                let (message, body_retry_after) =
                    extract_error_message_and_retry_after(result.response).await;
                if let Some(attempt_log) = attempt_log {
                    attempt_log
                        .finish("error", Some(status.as_u16()), None)
                        .await;
                }
                let retry_after = header_retry_after.or(body_retry_after);
                let current_backoff = connection.backoff_level.unwrap_or(0);
                let decision = check_fallback_error(status.as_u16(), &message, current_backoff);
                let cooldown = retry_after
                    .map(|timestamp| (timestamp - Utc::now()).to_std().unwrap_or_default())
                    .unwrap_or(decision.cooldown);
                last_error = Some(ProviderAttemptError {
                    status: status.as_u16(),
                    message: message.clone(),
                    retry_after,
                    upstream_body: None,
                });

                // 404 (model not found) should set a model-specific lock without
                // excluding the connection — other models on the same connection
                // should still be routable.
                if status.as_u16() == 404 {
                    let model_cooldown = std::time::Duration::from_secs(300);
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        model,
                        status.as_u16(),
                        &message,
                        model_cooldown,
                        current_backoff,
                    )
                    .await;
                }

                // Token refresh: on 401/403, try to refresh the access token
                // before giving up on this connection (9router parity).
                // On success, merge credentials (expires_at, refresh, PSD) and
                // continue the loop so the fresh snapshot picks up the token.
                if (status.as_u16() == 401 || status.as_u16() == 403)
                    && connection.refresh_token.is_some()
                {
                    if let Some(ref rt) = connection.refresh_token.clone() {
                        let refresh_provider = plan.provider.as_str();
                        if let Ok(result) = crate::oauth::token_refresh::dispatch_oauth_refresh(
                            refresh_provider,
                            rt,
                            &connection.provider_specific_data,
                        )
                        .await
                        {
                            let conn_id = connection.id.clone();
                            let new_access = result.access_token.clone();
                            let new_refresh = result.refresh_token.clone();
                            let expires_at = result.expires_in.map(|secs| {
                                (Utc::now() + ChronoDuration::seconds(secs)).to_rfc3339()
                            });
                            let last_refresh_at = Utc::now().to_rfc3339();
                            let _ = state
                                .db
                                .update(move |db| {
                                    if let Some(conn) =
                                        db.provider_connections.iter_mut().find(|c| c.id == conn_id)
                                    {
                                        conn.access_token = Some(new_access);
                                        // Preserve old refresh_token when response omits it
                                        if let Some(rt) = new_refresh {
                                            conn.refresh_token = Some(rt);
                                        }
                                        if let Some(exp) = expires_at {
                                            conn.expires_at = Some(exp);
                                        }
                                        conn.provider_specific_data.insert(
                                            "lastRefreshAt".into(),
                                            Value::String(last_refresh_at),
                                        );
                                        conn.last_error = None;
                                        conn.last_error_at = None;
                                        conn.error_code = None;
                                        conn.backoff_level = Some(0);
                                    }
                                })
                                .await;
                            continue;
                        }
                    }
                }

                if decision.should_fallback {
                    // 9router githubMonthlyResetMs: a GitHub 402 with the
                    // monthly-usage-limit message locks the ACCOUNT (model="")
                    // until the first of next month, and resets backoff to 0.
                    let github_reset = crate::core::account_fallback::github_monthly_reset_ms(
                        status.as_u16(),
                        &message,
                        &plan.provider,
                    );
                    if let Some(reset_at) = github_reset {
                        let cooldown_ms = (reset_at - Utc::now()).to_std().unwrap_or_default();
                        mark_connection_unavailable(
                            state,
                            &connection.id,
                            "",
                            status.as_u16(),
                            &message,
                            cooldown_ms,
                            0,
                        )
                        .await;
                        excluded.insert(connection.id.clone());
                        continue;
                    }
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        model,
                        status.as_u16(),
                        &message,
                        cooldown,
                        decision.new_backoff_level.unwrap_or(current_backoff + 1),
                    )
                    .await;
                    excluded.insert(connection.id.clone());
                    continue;
                }

                return Err(last_error.unwrap_or_else(|| {
                    ProviderAttemptError::new(
                        502,
                        "provider error after exhausting all connections",
                    )
                }));
            }
            Err(error) => {
                let message = format!("{:?}", error);
                if let Some(attempt_log) = attempt_log {
                    attempt_log.finish("error", Some(error.status), None).await;
                }
                let current_backoff = connection.backoff_level.unwrap_or(0);
                let decision = check_fallback_error(502, &message, current_backoff);
                let error_for_return = ProviderAttemptError::new(502, message.clone());
                last_error = Some(error);

                if decision.should_fallback {
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        model,
                        502,
                        &message,
                        decision.cooldown,
                        decision.new_backoff_level.unwrap_or(current_backoff + 1),
                    )
                    .await;
                    excluded.insert(connection.id.clone());
                    continue;
                }

                return Err(last_error.unwrap_or(error_for_return));
            }
        }
    }
}

async fn proxy_dashboard_sse(
    response: UpstreamResponse,
    attempt_log: Option<AttemptLog>,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let (body_bytes, body_complete) = collect_upstream_response_bytes(response).await;

    let token_usage = body_complete
        .then(|| extract_token_usage_from_bytes(&body_bytes))
        .flatten();
    if let Some(attempt_log) = attempt_log {
        if body_complete {
            attempt_log
                .finish("success", Some(status.as_u16()), token_usage.as_ref())
                .await;
        } else {
            attempt_log
                .finish("error", Some(StatusCode::BAD_GATEWAY.as_u16()), None)
                .await;
        }
    }

    let text = extract_dashboard_assistant_text_from_bytes(&body_bytes);
    let sse_body = build_dashboard_sse_body(text.as_deref(), token_usage.as_ref());
    build_dashboard_sse_response(status, &headers, sse_body)
}

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
    let now = Utc::now();

    // First: use filter_available_accounts to get accounts not in cooldown / not locked.
    let available =
        filter_available_accounts(&snapshot.provider_connections, provider, model, None, now);

    // Then: apply remaining filters that filter_available_accounts does not cover:
    //   - credentials presence
    //   - model support
    //   - excluded set (the call above passes None for exclude_id since we need
    //     to apply it separately alongside the other per-request filters)
    let mut candidates: Vec<_> = available
        .into_iter()
        .filter(|connection| {
            connection_has_credentials(connection)
                && !excluded.contains(&connection.id)
                && connection_supports_model(connection, model)
                && discovered_supporters
                    .is_none_or(|supporters| supporters.contains(&connection.id))
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

fn is_no_auth_provider(provider: &str) -> bool {
    matches!(
        provider,
        "opencode" | "opencode-zen" | "ollama-local" | "grok-web"
    )
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

fn is_connection_rate_limited(connection: &ProviderConnection, now: DateTime<Utc>) -> bool {
    connection
        .rate_limited_until
        .as_deref()
        .and_then(parse_timestamp)
        .is_some_and(|until| until > now)
}

fn is_model_locked(connection: &ProviderConnection, model: &str, now: DateTime<Utc>) -> bool {
    [format!("modelLock_{model}"), "modelLock___all".to_string()]
        .into_iter()
        .filter_map(|key| connection.extra.get(&key))
        .filter_map(Value::as_str)
        .filter_map(parse_timestamp)
        .any(|until| until > now)
}

fn connection_supports_model(connection: &ProviderConnection, model: &str) -> bool {
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

fn earliest_retry_after(
    snapshot: &AppDb,
    provider: &str,
    model: &str,
    _excluded: &HashSet<String>,
) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection.provider == provider
                && connection.is_active()
                && connection_has_credentials(connection)
                && connection_supports_model(connection, model)
        })
        .flat_map(|connection| {
            let mut retry_after = Vec::new();
            if let Some(until) = connection
                .rate_limited_until
                .as_deref()
                .and_then(parse_timestamp)
            {
                retry_after.push(until);
            }
            for key in [format!("modelLock_{model}"), "modelLock___all".to_string()] {
                if let Some(until) = connection
                    .extra
                    .get(&key)
                    .and_then(Value::as_str)
                    .and_then(parse_timestamp)
                {
                    retry_after.push(until);
                }
            }
            retry_after
        })
        .filter(|until| *until > now)
        .min()
}

async fn mark_connection_unavailable(
    state: &AppState,
    connection_id: &str,
    model: &str,
    status: u16,
    message: &str,
    cooldown: std::time::Duration,
    backoff_level: u32,
) {
    let connection_id = connection_id.to_string();
    let (model_lock_key, until_str) = build_model_lock_update(model, cooldown.as_secs() as i64);
    let message = message.to_string();
    let _ = state
        .db
        .update(move |db| {
            if let Some(connection) = db
                .provider_connections
                .iter_mut()
                .find(|connection| connection.id == connection_id)
            {
                connection
                    .extra
                    .insert(model_lock_key, Value::String(until_str));
                connection.last_error = Some(message.clone());
                connection.last_error_at = Some(Utc::now().to_rfc3339());
                connection.error_code = Some(status.to_string());
                connection.backoff_level = Some(backoff_level);
                connection.consecutive_errors = connection
                    .consecutive_errors
                    .map(|e| e.saturating_add(1))
                    .or(Some(1));
                connection.test_status = Some("unavailable".into());
            }
        })
        .await;
}

async fn clear_connection_error(state: &AppState, connection_id: &str) {
    clear_connection_error_for_model(state, connection_id, None).await;
}

/// Clear error state; only remove expired model locks and optionally the
/// succeeded model lock (9router clearAccountError selective clear).
async fn clear_connection_error_for_model(
    state: &AppState,
    connection_id: &str,
    succeeded_model: Option<&str>,
) {
    let connection_id = connection_id.to_string();
    let succeeded_model = succeeded_model.map(|s| s.to_string());
    let now = Utc::now();
    let _ = state
        .db
        .update(move |db| {
            if let Some(connection) = db
                .provider_connections
                .iter_mut()
                .find(|connection| connection.id == connection_id)
            {
                connection.last_error = None;
                connection.last_error_at = None;
                connection.error_code = None;
                connection.backoff_level = Some(0);
                connection.consecutive_errors = Some(0);
                connection.test_status = None;
                // Selective clear: remove expired locks + lock for succeeded model only
                let model_key = succeeded_model.as_ref().map(|m| format!("modelLock_{m}"));
                connection.extra.retain(|k, v| {
                    if !k.starts_with("modelLock_") {
                        return true;
                    }
                    // Drop expired
                    if let Some(exp) = v.as_str() {
                        if let Ok(t) = DateTime::parse_from_rfc3339(exp) {
                            if t.with_timezone(&Utc) <= now {
                                return false;
                            }
                        }
                    }
                    // Drop succeeded model lock
                    if let Some(ref mk) = model_key {
                        if k == mk {
                            return false;
                        }
                    }
                    true
                });
            }
        })
        .await;
}

/// forceStream SSE→JSON: collect upstream SSE and collapse to chat.completion JSON.
async fn proxy_sse_to_json_response(
    response: UpstreamResponse,
    model: &str,
    plan: &RequestPlan,
    attempt_log: Option<AttemptLog>,
) -> Response {
    let status = response.status();
    let (body_bytes, body_complete) = collect_upstream_response_bytes(response).await;

    let json_body = crate::core::chat::stream_to_json::sse_stream_to_json(&body_bytes, Some(model))
        .unwrap_or_else(|| {
            // Fallback: try parse as JSON already, else wrap error
            serde_json::from_slice(&body_bytes).unwrap_or_else(|_| {
                json!({
                    "error": {
                        "message": "Failed to convert forced SSE stream to JSON",
                        "type": "server_error",
                        "code": "sse_to_json_failed"
                    }
                })
            })
        });

    let out = Bytes::from(serde_json::to_vec(&json_body).unwrap_or_default());

    let usage = body_complete
        .then(|| extract_token_usage_from_bytes(&out))
        .flatten();
    if let Some(attempt_log) = attempt_log {
        if body_complete {
            attempt_log
                .finish("success", Some(status.as_u16()), usage.as_ref())
                .await;
        } else {
            attempt_log
                .finish("error", Some(StatusCode::BAD_GATEWAY.as_u16()), None)
                .await;
        }
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
    let (body_bytes, body_complete) = collect_upstream_response_bytes(response).await;

    // 9router parity (open-sse/handlers/chatCore/nonStreamingHandler.js +
    // open-sse/shared/clineEnvelope.js unwrapClineEnvelope): unwrap before any
    // consumer reads choices/usage so non-stream clients get a bare OpenAI
    // body. No-op unless the provider opts
    // in via transport.quirks.clineEnvelope (cline/clinepass).
    let unenveloped_body = unwrap_cline_envelope(&body_bytes, provider);

    let token_usage = body_complete
        .then(|| extract_token_usage_from_bytes(unenveloped_body.as_ref()))
        .flatten();

    let final_body = if body_complete {
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
                let chunks = registry::global_registry().translate_response(
                    plan.target_format,
                    plan.source_format,
                    unenveloped_body.as_ref(),
                    &mut state,
                );
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

        Body::from(translated_body)
    } else {
        Body::from(unenveloped_body)
    };

    if let Some(attempt_log) = attempt_log {
        if body_complete {
            attempt_log
                .finish("success", Some(status.as_u16()), token_usage.as_ref())
                .await;
        } else {
            attempt_log
                .finish("error", Some(StatusCode::BAD_GATEWAY.as_u16()), None)
                .await;
        }
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
    mut attempt_log: Option<AttemptLog>,
) -> Response {
    // Extract formats before stream closure to avoid lifetime issues
    let needs_stream_translation = plan.needs_translation();
    let stream_source_format = plan.source_format;
    let stream_target_format = plan.target_format;
    let stop_on_response_completed =
        crate::core::model::models_dev::is_opencode_provider(&provider)
            && stream_target_format == Format::OpenAiResponses;
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
        let (body_bytes, _) = collect_upstream_response_bytes(response).await;
        let msg = String::from_utf8_lossy(&body_bytes);
        let msg = if msg.len() > 500 {
            format!("{}…", &msg[..500])
        } else {
            msg.to_string()
        };
        tracing::warn!(
            target: "openproxy::chat",
            "STREAM_GUARD non-SSE content-type={} status={} body_snip={}",
            ct,
            status.as_u16(),
            msg.chars().take(120).collect::<String>()
        );
        if let Some(attempt_log) = attempt_log.take() {
            attempt_log
                .finish("error", Some(StatusCode::BAD_GATEWAY.as_u16()), None)
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
    let capture_sse_frames = ct.is_empty() || ct.contains("text/event-stream");

    let transformer = normalize_for_dashboard
        .then(|| transformer_for_provider(&provider))
        .flatten();
    // Qoder wraps every SSE chunk in a {statusCodeValue, body} envelope that
    // must be unwrapped before downstream consumers see it (9router wrapQoderSSE).
    // Usage arrives on a later `choices: []` frame, so a coalescer merges the
    // held finish + usage frames into one terminal chunk (9router sse.js).
    // Billing blocks arrive pre-detected as a real 403 by the executor's
    // first-frame peek — but the flag is still re-checked here so the
    // non-peeked dashboard path also short-circuits.
    let qoder_sse_unwrap = provider == "qoder";
    // Billing block detection state (9router v0.5.55 peekFirstQoderFrame).
    let mut qoder_seen_first_frame = false;
    let mut qoder_billing_block = false;
    let mut qoder_coalescer: Option<crate::core::executor::qoder::QoderSseCoalescer> =
        if qoder_sse_unwrap {
            Some(crate::core::executor::qoder::QoderSseCoalescer::new(&model))
        } else {
            None
        };
    let body = match response {
        UpstreamResponse::Reqwest(response) => {
            let provider = provider.clone();
            let model = model.clone();
            let mut transformer = transformer;
            let mut pending_text = String::new();
            let custom_tool_names = custom_tool_names.clone();
            let mut attempt_log = attempt_log;
            let stream = async_stream::stream! {
                let mut upstream = response.bytes_stream();
                // Persistent state for streaming format translation (e.g. Responses API -> Chat Completions).
                let mut t_state = if needs_stream_translation {
                    let mut s = crate::core::translator::registry::ResponseTransformState::default();
                    // Thread custom-tool names into streaming state so
                    // function_call vs custom_tool_call branching survives
                    // (9router chatCore customToolNames → stream handler).
                    if let Some(ref names) = custom_tool_names {
                        if !names.is_empty() {
                            s.responses.state.insert(
                                "customToolNames".to_string(),
                                Value::String(names.clone()),
                            );
                        }
                    }
                    Some(s)
                } else {
                    None
                };
                let mut usage_capture = StreamingUsageCapture::new(capture_sse_frames);
                let mut completion_frames = String::new();
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
                            let usage = usage_capture.usage.clone();
                            if let Some(log) = attempt_log.take() {
                                log.finish("error", Some(502), usage.as_ref()).await;
                            }
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(write_streaming_error(
                                "Upstream SSE stream stalled",
                                "server_error",
                            )));
                            return;
                        }
                        Ok(Ok(Some(chunk))) => {
                            usage_capture.observe(&chunk);
                            let response_completed = stop_on_response_completed
                                && responses_stream_completed(&mut completion_frames, &chunk);
                            if qoder_sse_unwrap {
                                for line in qoder_unwrap_sse_chunk(
                                    &chunk,
                                    &mut pending_text,
                                    &mut qoder_seen_first_frame,
                                    &mut qoder_billing_block,
                                    qoder_coalescer.as_mut(),
                                ) {
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(line));
                                }
                                if qoder_billing_block {
                                    // Billing block: close the stream now so the
                                    // client sees the 403-shaped error frame.
                                    // (Account fallback itself happens in the
                                    // executor's pre-stream peek; this flag is
                                    // the backstop for already-open streams.)
                                    let usage = usage_capture.usage.clone();
                                    if let Some(log) = attempt_log.take() {
                                        log.finish("error", Some(403), usage.as_ref()).await;
                                    }
                                    return;
                                }
                            } else if let Some(transformer) = transformer.as_mut() {
                                for line in transform_dashboard_sse_chunk(&chunk, transformer.as_mut(), &mut pending_text) {
                                    if let Some(frame) = sse_frame_for_dashboard(&line) {
                                        yield Ok::<Bytes, std::io::Error>(frame);
                                    }
                                }
                            } else if needs_stream_translation {
                                if let Some(ref mut t_state) = t_state {
                                    let chunks = registry::global_registry()
                                        .translate_response(
                                            stream_target_format,
                                            stream_source_format,
                                            &chunk,
                                            t_state,
                                        );
                                    for line in chunks {
                                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                                            yield Ok::<Bytes, std::io::Error>(frame);
                                        }
                                    }
                                } else {
                                    yield Ok::<Bytes, std::io::Error>(chunk);
                                }
                            } else {
                                yield Ok::<Bytes, std::io::Error>(chunk);
                            }
                            if response_completed {
                                if let Some(ref mut t_state) = t_state {
                                    for line in registry::global_registry().finish_stream(
                                        stream_source_format,
                                        stream_target_format,
                                        t_state,
                                    ) {
                                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                                            yield Ok::<Bytes, std::io::Error>(frame);
                                        }
                                    }
                                }
                                let usage = usage_capture.usage.clone();
                                if let Some(log) = attempt_log.take() {
                                    log.finish("success", Some(status.as_u16()), usage.as_ref()).await;
                                }
                                return;
                            }
                        }
                        Ok(Ok(None)) => break,
                        Ok(Err(_)) => {
                            let usage = usage_capture.usage.clone();
                            if let Some(log) = attempt_log.take() {
                                log.finish("error", Some(502), usage.as_ref()).await;
                            }
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(write_streaming_error(
                                "Upstream stream error",
                                "server_error",
                            )));
                            return;
                        }
                    }
                }
                if let Some(transformer) = transformer.as_mut() {
                    for line in flush_dashboard_sse_chunk(transformer.as_mut(), &mut pending_text) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                // Qoder end-of-stream: flush the usage coalescer (held
                // finish+usage → terminal chunk). Qoder only uses Reqwest
                // transport so the Hyper branch needs no equivalent.
                if qoder_sse_unwrap {
                    for line in qoder_coalescer_flush(qoder_coalescer.as_mut()) {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(line));
                    }
                }
                // End-of-stream flush: emit the terminal chunk + [DONE] for
                // buffered binary transforms (kiro EventStream → SSE).
                if let Some(ref mut t_state) = t_state {
                    for line in registry::global_registry().finish_stream(
                        stream_source_format,
                        stream_target_format,
                        t_state,
                    ) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                let usage = usage_capture.usage.clone();
                if let Some(log) = attempt_log.take() {
                    log.finish("success", Some(status.as_u16()), usage.as_ref()).await;
                }
            };
            Body::from_stream(stream)
        }
        UpstreamResponse::Hyper(response) => {
            let (_, mut body) = response.into_parts();
            let provider = provider.clone();
            let model = model.clone();
            let mut transformer = transformer;
            let mut pending_text = String::new();
            let custom_tool_names2 = custom_tool_names.clone();
            let mut attempt_log = attempt_log;
            let stream = async_stream::stream! {
                // Persistent state for streaming format translation (e.g. Responses API -> Chat Completions).
                let mut t_state = if needs_stream_translation {
                    let mut s = crate::core::translator::registry::ResponseTransformState::default();
                    if let Some(ref names) = custom_tool_names2 {
                        if !names.is_empty() {
                            s.responses.state.insert(
                                "customToolNames".to_string(),
                                Value::String(names.clone()),
                            );
                        }
                    }
                    Some(s)
                } else {
                    None
                };
                let mut usage_capture = StreamingUsageCapture::new(capture_sse_frames);
                loop {
                    let next = tokio::time::timeout(SSE_STALL_TIMEOUT, body.frame()).await;
                    let frame_result = match next {
                        Err(_elapsed) => {
                            tracing::warn!(
                                target: "openproxy::chat::stream",
                                provider = %provider,
                                model = %model,
                                "SSE stalled, closing stream"
                            );
                            let usage = usage_capture.usage.clone();
                            if let Some(log) = attempt_log.take() {
                                log.finish("error", Some(502), usage.as_ref()).await;
                            }
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(write_streaming_error(
                                "Upstream SSE stream stalled",
                                "server_error",
                            )));
                            return;
                        }
                        Ok(Some(result)) => result,
                        Ok(None) => break,
                    };
                    match frame_result {
                        Ok(frame) => {
                            if let Ok(data) = frame.into_data() {
                                usage_capture.observe(&data);
                                if let Some(transformer) = transformer.as_mut() {
                                    for line in transform_dashboard_sse_chunk(&data, transformer.as_mut(), &mut pending_text) {
                                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                                            yield Ok::<Bytes, std::io::Error>(frame);
                                        }
                                    }
                                } else if needs_stream_translation {
                                    if let Some(ref mut t_state) = t_state {
                                        let chunks = registry::global_registry()
                                            .translate_response(
                                                stream_target_format,
                                                stream_source_format,
                                                &data,
                                                t_state,
                                            );
                                        for line in chunks {
                                            if let Some(frame) = sse_frame_for_dashboard(&line) {
                                                yield Ok::<Bytes, std::io::Error>(frame);
                                            }
                                        }
                                    } else {
                                        yield Ok::<Bytes, std::io::Error>(data);
                                    }
                                } else {
                                    yield Ok::<Bytes, std::io::Error>(data);
                                }
                            }
                        }
                        Err(_) => {
                            let usage = usage_capture.usage.clone();
                            if let Some(log) = attempt_log.take() {
                                log.finish("error", Some(502), usage.as_ref()).await;
                            }
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(write_streaming_error(
                                "Upstream stream error",
                                "server_error",
                            )));
                            return;
                        }
                    }
                }
                if let Some(transformer) = transformer.as_mut() {
                    for line in flush_dashboard_sse_chunk(transformer.as_mut(), &mut pending_text) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                // End-of-stream flush: emit the terminal chunk + [DONE] for
                // buffered binary transforms (kiro EventStream → SSE).
                if let Some(ref mut t_state) = t_state {
                    for line in registry::global_registry().finish_stream(
                        stream_source_format,
                        stream_target_format,
                        t_state,
                    ) {
                        if let Some(frame) = sse_frame_for_dashboard(&line) {
                            yield Ok::<Bytes, std::io::Error>(frame);
                        }
                    }
                }
                let usage = usage_capture.usage.clone();
                if let Some(log) = attempt_log.take() {
                    log.finish("success", Some(status.as_u16()), usage.as_ref()).await;
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

struct StreamingUsageCapture {
    capture_sse_frames: bool,
    pending: Vec<u8>,
    usage: Option<TokenUsage>,
}

impl StreamingUsageCapture {
    fn new(capture_sse_frames: bool) -> Self {
        Self {
            capture_sse_frames,
            pending: Vec::new(),
            usage: None,
        }
    }

    fn observe(&mut self, chunk: &[u8]) {
        if let Some(usage) = extract_token_usage_from_bytes(chunk) {
            self.usage = Some(usage);
        }
        if !self.capture_sse_frames {
            return;
        }

        self.pending.extend_from_slice(chunk);
        while let Some((frame_end, separator_len)) = next_sse_frame(&self.pending) {
            let frame = self.pending[..frame_end].to_vec();
            self.pending.drain(..frame_end + separator_len);
            for line in String::from_utf8_lossy(&frame).lines() {
                let Some(payload) = line.trim().strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                if let Some(usage) = extract_token_usage_from_bytes(payload.as_bytes()) {
                    self.usage = Some(usage);
                }
            }
        }
    }
}

fn next_sse_frame(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, Some(right)) => Some((right, 4)),
        (None, None) => None,
    }
}

fn responses_stream_completed(buffer: &mut String, chunk: &[u8]) -> bool {
    buffer.push_str(&String::from_utf8_lossy(chunk));
    if buffer.contains("\r\n") {
        *buffer = buffer.replace("\r\n", "\n");
    }

    while let Some(end) = buffer.find("\n\n") {
        let frame: String = buffer.drain(..end + 2).collect();
        let mut completed = false;
        for line in frame.lines() {
            if line
                .strip_prefix("event:")
                .is_some_and(|event| event.trim() == "response.completed")
            {
                completed = true;
                break;
            }
            if let Some(data) = line.strip_prefix("data:") {
                completed = serde_json::from_str::<Value>(data.trim())
                    .ok()
                    .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
                    .as_deref()
                    == Some("response.completed");
                if completed {
                    break;
                }
            }
        }
        if completed {
            return true;
        }
    }
    false
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

/// Split raw upstream bytes into complete SSE lines and unwrap Qoder's
/// `{statusCodeValue, body}` envelope on each `data:` line (9router
/// wrapQoderSSE). Non-`data:` lines (keepalives) are dropped; the terminal
/// `[DONE]` frame passes through.
///
/// On the first `data:` line, checks for billing/quota blocks (9router v0.5.55
/// peekFirstQoderFrame). If detected, emits a synthetic 403 error frame and
/// sets `billing_block` to `true` so the caller can trigger account fallback.
fn qoder_unwrap_sse_chunk(
    chunk: &Bytes,
    pending_text: &mut String,
    seen_first_frame: &mut bool,
    billing_block: &mut bool,
    mut coalescer: Option<&mut crate::core::executor::qoder::QoderSseCoalescer>,
) -> Vec<String> {
    pending_text.push_str(&String::from_utf8_lossy(chunk));
    let mut out = Vec::new();
    while let Some(newline_index) = pending_text.find('\n') {
        let mut line = pending_text[..newline_index].to_string();
        if line.ends_with('\r') {
            line.pop();
        }
        pending_text.drain(..=newline_index);
        if line.is_empty() {
            continue;
        }
        // First-frame billing block detection (9router peekFirstQoderFrame).
        if !*seen_first_frame && line.starts_with("data:") {
            *seen_first_frame = true;
            if let Some(billing_err) =
                crate::core::executor::qoder::check_billing_in_sse_line(&line)
            {
                *billing_block = true;
                // Emit the billing error as a JSON error frame so the chat
                // handler sees status 403 and triggers account fallback.
                out.push(format!("data: {billing_err}\n\n"));
                out.push("data: [DONE]\n\n".to_string());
                return out;
            }
        }
        // Unwrap the {statusCodeValue, body} envelope, then run the inner
        // body through the usage coalescer (9router sse.js).
        let Some(unwrapped) =
            crate::core::executor::qoder::QoderExecutor::unwrap_qoder_envelope(&line)
        else {
            continue;
        };
        if let Some(coal) = coalescer.as_deref_mut() {
            let (frames, _terminal) = coal.handle_inner(&unwrapped);
            out.extend(frames);
            if coal.done_emitted() {
                out.push("data: [DONE]\n\n".to_string());
                return out;
            }
        } else if let Some(frame) =
            crate::core::executor::qoder::QoderExecutor::wrap_qoder_sse_line(&line)
        {
            out.push(frame);
        }
    }
    out
}

/// Flush a Qoder coalescer at end-of-stream: emit any held terminal
/// finish+usage chunk, then `[DONE]` (9router `coalescer.flush`).
fn qoder_coalescer_flush(
    coalescer: Option<&mut crate::core::executor::qoder::QoderSseCoalescer>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(coal) = coalescer {
        if let Some(t) = coal.flush() {
            out.push(t);
        }
    }
    out
}

fn transform_dashboard_sse_chunk(
    chunk: &Bytes,
    transformer: &mut dyn crate::core::translator::response_transform::StreamingTransformer,
    pending_text: &mut String,
) -> Vec<String> {
    pending_text.push_str(&String::from_utf8_lossy(chunk));
    let mut ready_lines = Vec::new();

    while let Some(newline_index) = pending_text.find('\n') {
        let mut line = pending_text[..newline_index].to_string();
        if line.ends_with('\r') {
            line.pop();
        }
        pending_text.drain(..=newline_index);
        if line.is_empty() {
            continue;
        }
        ready_lines.extend(transform_sse_stream(&Bytes::from(line), transformer));
    }

    ready_lines
}

fn flush_dashboard_sse_chunk(
    transformer: &mut dyn crate::core::translator::response_transform::StreamingTransformer,
    pending_text: &mut String,
) -> Vec<String> {
    if pending_text.trim().is_empty() {
        pending_text.clear();
        return Vec::new();
    }
    let mut line = std::mem::take(pending_text);
    if line.ends_with('\r') {
        line.pop();
    }
    let pending_len = line.len();
    let output = transform_sse_stream(&Bytes::from(line), transformer);
    if output.is_empty() {
        tracing::trace!(
            target: "openproxy::chat::stream",
            "flush_dashboard_sse_chunk: {} bytes of partial/invalid buffer content yielded no output lines",
            pending_len,
        );
    }
    output
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

async fn collect_upstream_response_bytes(response: UpstreamResponse) -> (Bytes, bool) {
    match response {
        UpstreamResponse::Reqwest(response) => {
            let mut stream = response.bytes_stream();
            let mut collected = Vec::new();
            let mut complete = true;

            loop {
                match stream.try_next().await {
                    Ok(Some(chunk)) => collected.extend_from_slice(&chunk),
                    Ok(None) => break,
                    Err(_) => {
                        complete = false;
                        break;
                    }
                }
            }

            (Bytes::from(collected), complete)
        }
        UpstreamResponse::Hyper(response) => {
            let (_, mut body) = response.into_parts();
            let mut collected = Vec::new();
            let mut complete = true;

            while let Some(frame_result) = body.frame().await {
                match frame_result {
                    Ok(frame) => {
                        if let Ok(data) = frame.into_data() {
                            collected.extend_from_slice(&data);
                        }
                    }
                    Err(_) => {
                        complete = false;
                        break;
                    }
                }
            }

            (Bytes::from(collected), complete)
        }
    }
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
        let nested_reasoning_tokens = usage
            .get("completion_tokens_details")
            .or_else(|| usage.get("output_tokens_details"))
            .and_then(|details| details.get("reasoning_tokens"))
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
    let input = extract_u64_from_value(&value, "input_tokens");
    let prompt = extract_u64_from_value(&value, "prompt_tokens");
    let output = extract_u64_from_value(&value, "output_tokens");
    let completion = extract_u64_from_value(&value, "completion_tokens");
    let total = extract_u64_from_value(&value, "total_tokens");
    if input + prompt + output + completion + total > 0 {
        return Some(TokenUsage {
            prompt_tokens: opt(prompt).or(opt(input)),
            input_tokens: opt(input).filter(|_| prompt == 0),
            completion_tokens: opt(completion).or(opt(output)),
            output_tokens: opt(output).filter(|_| completion == 0),
            total_tokens: opt(total),
            reasoning_tokens: opt(extract_u64_from_value(&value, "reasoning_tokens")),
            cached_tokens: opt(extract_u64_from_value(&value, "cached_tokens")),
            cache_read_input_tokens: opt(extract_u64_from_value(&value, "cache_read_input_tokens")),
            cache_creation_input_tokens: opt(extract_u64_from_value(
                &value,
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
    let (body_bytes, _) = collect_upstream_response_bytes(response).await;
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

/// Read the error response body once and return both the extracted message and
/// a body-based `retryAfter` (accepted as an ISO date or seconds).
async fn extract_error_message_and_retry_after(
    response: UpstreamResponse,
) -> (String, Option<DateTime<Utc>>) {
    let status = response.status();
    let text = match response {
        UpstreamResponse::Reqwest(response) => response.text().await.unwrap_or_default(),
        UpstreamResponse::Hyper(response) => {
            let (_, body) = response.into_parts();
            body.collect()
                .await
                .map(|collected| String::from_utf8_lossy(&collected.to_bytes()).into_owned())
                .unwrap_or_default()
        }
    };
    let retry_after = crate::core::account_fallback::parse_retry_after_from_body(text.as_bytes());
    let message = {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(message) = value
                .get("error")
                .and_then(|error| error.get("message").or(Some(error)))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                message.to_string()
            } else if let Some(message) = value
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                message.to_string()
            } else {
                fallback_error_text(status, &text)
            }
        } else {
            fallback_error_text(status, &text)
        }
    };
    (message, retry_after)
}

fn fallback_error_text(status: StatusCode, text: &str) -> String {
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

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .ok()
}

fn attempt_error_response(error: ProviderAttemptError) -> Response {
    // H23: When upstream_body is available, return it verbatim instead
    // of constructing a new error body.
    if let Some(body_bytes) = error.upstream_body {
        let status_code = StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_GATEWAY);
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

fn json_success_response(status: StatusCode, data: Value) -> Response {
    with_cors_response((status, Json(data)).into_response())
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

/// Produce an OpenAI-compatible SSE error chunk for mid-stream errors.
/// Clients (Claude Code, Gemini CLI, etc.) parse error chunks and surface
/// the message, so writing one before closing the stream lets them show
/// a useful error instead of a generic "connection closed" message.
fn write_streaming_error(error_msg: &str, error_type: &str) -> String {
    let friendly = crate::core::utils::error::friendly_error_message(502, error_msg);
    let msg = serde_json::json!({
        "error": {
            "message": friendly,
            "type": error_type,
            "code": null
        }
    });
    format!(
        "data: {}\n\n",
        serde_json::to_string(&msg).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use axum::{
        body::Body,
        http::{HeaderMap, HeaderValue, StatusCode},
        response::Response,
    };
    use bytes::Bytes;
    use chrono::{Duration as ChronoDuration, Utc};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};

    use super::{
        build_dashboard_sse_response, build_proxied_response, codex_models_support_search,
        codex_web_search_context_size, codex_web_search_is_injected, earliest_retry_after,
        mark_codex_web_search_injected, requests_codex_web_search, responses_stream_completed,
        select_connection, select_connection_with_supporters, CodexWebSearchInjected,
    };
    use crate::server::codex_catalog::CodexModelMetadata;
    use crate::types::{AppDb, ProviderConnection, Settings};

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
    fn codex_web_search_intent_is_client_independent_and_respects_none() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-openproxy-codex-web-search",
            HeaderValue::from_static("true"),
        );
        assert!(requests_codex_web_search(
            &headers,
            &json!({"messages": []})
        ));
        assert!(requests_codex_web_search(
            &HeaderMap::new(),
            &json!({"tools": [{"type": "web_search"}]})
        ));
        assert!(!requests_codex_web_search(
            &headers,
            &json!({"tools": [{"type": "web_search"}], "tool_choice": "none"})
        ));
        assert!(codex_web_search_is_injected(Some("low"), &json!({})));
        assert!(!codex_web_search_is_injected(
            Some("high"),
            &json!({"tools": [{"type": "web_search"}]})
        ));
        assert!(!codex_web_search_is_injected(None, &json!({})));

        let injected = mark_codex_web_search_injected(Response::new(Body::empty()), true);
        assert!(injected
            .extensions()
            .get::<CodexWebSearchInjected>()
            .is_some());
        let native = mark_codex_web_search_injected(Response::new(Body::empty()), false);
        assert!(native
            .extensions()
            .get::<CodexWebSearchInjected>()
            .is_none());
    }

    #[test]
    fn codex_web_search_context_defaults_to_medium_and_supports_off() {
        let mut settings = Settings::default();
        assert_eq!(codex_web_search_context_size(&settings), Some("medium"));

        for value in ["low", "medium", "high"] {
            settings.extra.insert(
                "codexWebSearchContextSize".into(),
                Value::String(value.into()),
            );
            let context_size = codex_web_search_context_size(&settings);
            assert_eq!(context_size, Some(value));
            assert!(codex_web_search_is_injected(context_size, &json!({})));
        }

        settings.extra.insert(
            "codexWebSearchContextSize".into(),
            Value::String("off".into()),
        );
        let context_size = codex_web_search_context_size(&settings);
        assert_eq!(context_size, None);
        assert!(!codex_web_search_is_injected(context_size, &json!({})));
    }

    #[test]
    fn codex_web_search_capability_matches_exact_model_and_reasoning_variant() {
        let models = vec![CodexModelMetadata {
            id: "gpt-5.6-luna".into(),
            name: "Luna".into(),
            context_window: None,
            capabilities: vec!["tools".into(), "search".into()],
            reasoning_efforts: vec!["low".into(), "high".into()],
        }];

        assert!(codex_models_support_search(&models, "gpt-5.6-luna"));
        assert!(codex_models_support_search(&models, "gpt-5.6-luna-high"));
        assert!(!codex_models_support_search(&models, "gpt-other"));

        let mut unsupported = models;
        unsupported[0]
            .capabilities
            .retain(|value| value != "search");
        assert!(!codex_models_support_search(&unsupported, "gpt-5.6-luna"));
    }

    #[test]
    fn select_connection_skips_excluded_and_locked_accounts() {
        let locked_until = (Utc::now() + ChronoDuration::seconds(90)).to_rfc3339();
        let mut excluded_connection = connection("excluded", 1);
        excluded_connection.default_model = Some("gpt-4.1".into());

        let mut locked_connection = connection("locked", 2);
        locked_connection
            .extra
            .insert("modelLock_gpt-4.1".into(), Value::String(locked_until));

        let chosen_connection = connection("chosen", 3);

        let snapshot = AppDb {
            provider_connections: vec![
                excluded_connection.clone(),
                locked_connection,
                chosen_connection.clone(),
            ],
            ..AppDb::default()
        };

        let excluded = HashSet::from([excluded_connection.id]);
        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &excluded)
            .expect("third account should remain selectable");

        assert_eq!(selected.id, chosen_connection.id);
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
    fn earliest_retry_after_reports_locked_model_deadline() {
        let early = Utc::now() + ChronoDuration::seconds(30);
        let late = Utc::now() + ChronoDuration::seconds(90);
        let mut early_locked = connection("early", 1);
        early_locked.extra.insert(
            "modelLock_gpt-4.1".into(),
            Value::String(early.to_rfc3339()),
        );

        let mut late_rate_limited = connection("late", 2);
        late_rate_limited.rate_limited_until = Some(late.to_rfc3339());

        let snapshot = AppDb {
            provider_connections: vec![late_rate_limited, early_locked],
            ..AppDb::default()
        };

        let retry_after = earliest_retry_after(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("retry-after should be derived from the earliest blocked account");

        assert!(retry_after <= early + ChronoDuration::seconds(1));
    }

    #[test]
    fn select_connection_skips_rate_limited_accounts() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut rate_limited = connection("rate-limited", 1);
        rate_limited.rate_limited_until = Some(future);

        let available = connection("available", 2);

        let snapshot = AppDb {
            provider_connections: vec![rate_limited, available.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should select an account");

        assert_eq!(selected.id, "available");
    }

    #[test]
    fn select_connection_respects_model_locks_for_specific_model() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut locked = connection("locked-model", 1);
        locked
            .extra
            .insert("modelLock_gpt-4.1".into(), Value::String(future));

        let available = connection("available", 2);

        let snapshot = AppDb {
            provider_connections: vec![locked, available.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should select an account");

        assert_eq!(selected.id, "available");
    }

    #[test]
    fn select_connection_skips_account_level_lock() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut all_locked = connection("all-locked", 1);
        all_locked
            .extra
            .insert("modelLock___all".into(), Value::String(future));

        let available = connection("available", 2);

        let snapshot = AppDb {
            provider_connections: vec![all_locked, available.clone()],
            ..AppDb::default()
        };

        let selected = select_connection(&snapshot, "openai", "gpt-4.1", &HashSet::new())
            .expect("should select an account");

        assert_eq!(selected.id, "available");
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

    #[test]
    fn is_connection_rate_limited_detects_expired_timestamp() {
        let past = (Utc::now() - ChronoDuration::seconds(10)).to_rfc3339();
        let mut conn = connection("conn", 1);
        conn.rate_limited_until = Some(past);

        assert!(
            !super::is_connection_rate_limited(&conn, Utc::now()),
            "expired rate_limited_until should not block connection"
        );
    }

    #[test]
    fn is_connection_rate_limited_allows_null_timestamp() {
        let conn = connection("conn", 1);
        assert!(
            !super::is_connection_rate_limited(&conn, Utc::now()),
            "null rate_limited_until should not block connection"
        );
    }

    #[test]
    fn is_model_locked_returns_false_when_no_lock() {
        let conn = connection("conn", 1);
        assert!(
            !super::is_model_locked(&conn, "gpt-4.1", Utc::now()),
            "connection without lock should not be locked"
        );
    }

    #[test]
    fn is_model_locked_checks_specific_model_key() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut conn = connection("conn", 1);
        conn.extra
            .insert("modelLock_gpt-4.1".into(), Value::String(future));

        assert!(
            super::is_model_locked(&conn, "gpt-4.1", Utc::now()),
            "specific model lock should block that model"
        );
        assert!(
            !super::is_model_locked(&conn, "gpt-4o", Utc::now()),
            "specific model lock should not block different model"
        );
    }

    #[test]
    fn is_model_locked_checks_account_level_all_key() {
        let future = (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339();
        let mut conn = connection("conn", 1);
        conn.extra
            .insert("modelLock___all".into(), Value::String(future));

        assert!(
            super::is_model_locked(&conn, "any-model", Utc::now()),
            "account-level lock should block any model"
        );
    }

    #[test]
    fn is_model_locked_expired_lock_allows_connection() {
        let past = (Utc::now() - ChronoDuration::seconds(10)).to_rfc3339();
        let mut conn = connection("conn", 1);
        conn.extra
            .insert("modelLock_gpt-4.1".into(), Value::String(past));

        assert!(
            !super::is_model_locked(&conn, "gpt-4.1", Utc::now()),
            "expired model lock should not block"
        );
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
        let mut buffer = String::new();
        assert!(!responses_stream_completed(
            &mut buffer,
            b"event: response.compl"
        ));
        assert!(responses_stream_completed(
            &mut buffer,
            b"eted\r\ndata: {\"type\":\"response.completed\"}\r\n\r\n: ping\r\n\r\n"
        ));
    }
}
