use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration as ChronoDuration, Utc};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::server::api::models_metadata::{ModelMetadataFacts, OpenCodeModelConfig};
use crate::server::state::AppState;
use crate::types::{CustomModel, ProviderConnection};

/// GLM Coding Plan (`api.z.ai`) models endpoints.
///
/// Live-verified: both accept the same API key via `Bearer` *or* `x-api-key`
/// (chat itself uses `x-api-key`; the models surfaces accept either).
/// - `.../coding/paas/v4/models` → OpenAI list shape, full routable id set,
///   no metadata. Source of truth for ids.
/// - `.../api/v1/models` → rich `{"models":[{slug,display_name,
///   context_window/max_context_window,input_modalities,
///   supported_reasoning_levels:[{effort}],visibility,supported_in_api}]}`.
///   Fail-open enrichment for the intersection only.
const GLM_PAAS_MODELS_URL: &str = "https://api.z.ai/api/coding/paas/v4/models";
const GLM_RICH_MODELS_URL: &str = "https://api.z.ai/api/v1/models";

const OPENAI_COMPATIBLE_PREFIX: &str = "openai-compatible-";
const ANTHROPIC_COMPATIBLE_PREFIX: &str = "anthropic-compatible-";
const OLLAMA_LOCAL_DEFAULT_HOST: &str = "http://localhost:11434";
/// Live-verified: `/v1/models` → 200 OpenAI shape (`{"object":"list","data":[…]}`),
/// with or without a Bearer key. `/api/v1/models` → 404.
const OLLAMA_CLOUD_OPENAI_MODELS_URL: &str = "https://ollama.com/v1/models";
/// Live-verified: → 200 Ollama-native shape (`{"models":[{"name":…,"model":…}]}`),
/// no `data` envelope, hence `parse_ollama_native_models`.
const OLLAMA_CLOUD_NATIVE_TAGS_URL: &str = "https://ollama.com/api/tags";

/// Live-verified: no key → 403 PERMISSION_DENIED; `x-goog-api-key: <bogus>` →
/// 400 API_KEY_INVALID, i.e. the header is honoured. Never use `?key=`, which
/// would leak the key into logged URLs.
const GEMINI_API_MODELS_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const GEMINI_API_MODELS_PAGE_SIZE: &str = "1000";
const GEMINI_API_MODELS_MAX_PAGES: usize = 10;

const GEMINI_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GEMINI_CLI_MODELS_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

const KIRO_AUTH_SERVICE: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";
const KIRO_MODELS_URL: &str = "https://codewhisperer.us-east-1.amazonaws.com";
const KIRO_MODELS_TARGET: &str = "AmazonCodeWhispererService.ListAvailableModels";

const OPENROUTER_REFERER: &str = "https://endpoint-proxy.local";
const OPENROUTER_TITLE: &str = "Endpoint Proxy";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsResponse {
    pub provider: String,
    pub connection_id: String,
    pub models: Vec<ProviderModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

pub(super) async fn list_provider_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(connection) = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
    else {
        return json_error(StatusCode::NOT_FOUND, "Connection not found");
    };

    match fetch_provider_models_response(&state, &connection).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => json_error(error.status, &error.message),
    }
}

pub(super) async fn import_provider_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(connection) = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
    else {
        return json_error(StatusCode::NOT_FOUND, "Connection not found");
    };

    let provider = connection.provider.clone();
    let provider_alias = storage_alias_for_provider(&provider);
    let legacy_alias = connection.name.clone().unwrap_or_else(|| provider.clone());
    if legacy_alias != provider_alias {
        let has_legacy = state
            .db
            .snapshot()
            .custom_models
            .iter()
            .any(|m| m.provider_alias == legacy_alias);
        if has_legacy {
            let legacy = legacy_alias.clone();
            let correct = provider_alias.clone();
            let _ = state
                .db
                .update(move |db| {
                    for m in &mut db.custom_models {
                        if m.provider_alias == legacy {
                            m.provider_alias = correct.clone();
                        }
                    }
                })
                .await;
        }
    }

    let Ok(payload) = fetch_provider_models_response(&state, &connection).await else {
        let snapshot = state.db.snapshot();
        let existing = snapshot
            .custom_models
            .iter()
            .filter(|m| m.provider_alias == provider_alias)
            .count();
        return Json(json!({
            "provider": provider,
            "connectionId": connection.id,
            "imported": 0,
            "skipped": existing,
            "total": existing,
        }))
        .into_response();
    };

    let now = chrono::Utc::now().to_rfc3339();
    let mut imported = 0usize;
    let mut skipped = 0usize;
    let total = payload.models.len();

    for model in payload.models {
        let model_id = model.id.trim().to_string();
        if model_id.is_empty() {
            continue;
        }

        // Snapshot read to decide import vs skip (mirrors create_custom_model).
        let exists_before =
            state.db.snapshot().custom_models.iter().any(|m| {
                m.provider_alias == provider_alias && m.id == model_id && m.r#type == "llm"
            });
        if exists_before {
            skipped += 1;
            continue;
        }

        let mut extra = model.extra.clone();
        extra
            .entry("source".to_string())
            .or_insert_with(|| serde_json::Value::String("imported".to_string()));
        extra
            .entry("importedAt".to_string())
            .or_insert_with(|| serde_json::Value::String(now.clone()));

        let name = if model.name.is_empty() {
            None
        } else {
            Some(model.name)
        };

        let model_id_owned = model_id.clone();
        let alias = provider_alias.clone();
        let result = state
            .db
            .update(move |db| {
                // Re-check inside the write lock to guard a concurrent insert.
                let exists = db.custom_models.iter().any(|m| {
                    m.provider_alias == alias && m.id == model_id_owned && m.r#type == "llm"
                });
                if exists {
                    return;
                }

                db.custom_models.push(CustomModel {
                    provider_alias: alias,
                    id: model_id_owned,
                    r#type: "llm".to_string(),
                    name,
                    extra,
                });
            })
            .await;

        match result {
            Ok(_) => imported += 1,
            Err(_) => skipped += 1,
        }
    }

    Json(json!({
        "provider": provider,
        "connectionId": connection.id,
        "imported": imported,
        "skipped": skipped,
        "total": total,
    }))
    .into_response()
}

pub(super) async fn fetch_compatible_model_ids(connection: &ProviderConnection) -> Vec<String> {
    let models = if is_openai_compatible_provider(&connection.provider) {
        fetch_openai_compatible_models(connection)
            .await
            .ok()
            .map(|payload| payload.models)
            .unwrap_or_default()
    } else if is_anthropic_compatible_provider(&connection.provider) {
        fetch_anthropic_compatible_models(connection)
            .await
            .ok()
            .map(|payload| payload.models)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    dedupe_model_ids(
        models
            .into_iter()
            .map(|model| model.id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect(),
    )
}

/// Generic dynamic-discovery fallback used by `/v1/models`.
///
/// For compatible providers this keeps the exact `fetch_compatible_model_ids`
/// behavior. For built-in providers it runs the same discovery the dashboard
/// uses (`fetch_provider_models_response`), so a provider whose static catalog
/// entry is missing/empty is still exposed through `/v1/models` instead of
/// silently disappearing. Providers without a discovery fetcher return
/// immediately with no network traffic.
pub(super) async fn fetch_discovered_model_ids(
    state: &AppState,
    connection: &ProviderConnection,
) -> Vec<String> {
    if is_openai_compatible_provider(&connection.provider)
        || is_anthropic_compatible_provider(&connection.provider)
    {
        return fetch_compatible_model_ids(connection).await;
    }

    let models = fetch_provider_models_response(state, connection)
        .await
        .ok()
        .map(|payload| payload.models)
        .unwrap_or_default();

    dedupe_model_ids(
        models
            .into_iter()
            .map(|model| model.id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect(),
    )
}

/// Whether the server has a models-listing fetcher for this provider.
///
/// Mirrors the match arms in `fetch_provider_models_response`. Used by
/// `/v1/models` to decide whether a catalog-less connection can fall back to
/// live discovery without issuing doomed network requests.
pub(super) fn supports_models_discovery(provider: &str) -> bool {
    is_openai_compatible_provider(provider)
        || is_anthropic_compatible_provider(provider)
        || matches!(
            provider,
            "kiro"
                | "gemini-cli"
                | "ollama-local"
                | "claude"
                | "anthropic"
                | "gemini"
                | "qwen"
                | "codex"
                | "glm"
                | "antigravity"
                | "github"
                | "qoder"
                | "openai"
                | "openrouter"
                | "opencode-zen"
                | "opencode-go"
                | "alicode"
                | "alicode-intl"
                | "volcengine-ark"
                | "byteplus"
                | "deepseek"
                | "groq"
                | "xai"
                | "mistral"
                | "perplexity"
                | "together"
                | "fireworks"
                | "cerebras"
                | "cohere"
                | "nebius"
                | "siliconflow"
                | "hyperbolic"
                | "ollama"
                | "chutes"
                | "nvidia"
                | "xiaomi-mimo"
                | "xiaomi-tokenplan"
                | "aimlapi"
                | "modal"
                | "reka"
                | "kluster"
                | "morph"
                | "longcat"
                | "scaleway"
                | "sambanova"
                | "nscale"
                | "baseten"
                | "nous-research"
                | "glhf"
                | "kilocode"
        )
}

pub(super) async fn fetch_models_for_connection(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<Vec<ProviderModel>, (StatusCode, String)> {
    fetch_provider_models_response(state, connection)
        .await
        .map(|payload| payload.models)
        .map_err(|error| (error.status, error.message))
}

#[derive(Debug)]
struct RouteError {
    status: StatusCode,
    message: String,
}

impl RouteError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

#[derive(Debug)]
enum FetchJsonError {
    Http(StatusCode, String),
    Network(String),
    Decode(String),
}

#[derive(Debug)]
struct RefreshResult {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

async fn fetch_provider_models_response(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    if is_openai_compatible_provider(&connection.provider) {
        return fetch_openai_compatible_models(connection).await;
    }

    if is_anthropic_compatible_provider(&connection.provider) {
        return fetch_anthropic_compatible_models(connection).await;
    }

    match connection.provider.as_str() {
        "kiro" => fetch_kiro_models_with_fallback(state, connection).await,
        "gemini-cli" => fetch_gemini_cli_models_with_fallback(state, connection).await,
        "ollama-local" => fetch_ollama_local_models(connection).await,
        "claude" | "anthropic" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_anthropic_models(
                connection,
                "https://api.anthropic.com/v1/models",
                &token,
                Some("2023-06-01"),
            )
            .await
        }
        "gemini" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_gemini_api_models(connection, &token).await
        }
        "qwen" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_openai_style_models_with_bearer(
                connection,
                &resolve_qwen_models_url(connection),
                &token,
            )
            .await
        }
        "glm" => fetch_glm_models(connection).await,
        "codex" => {
            let inventory = state
                .codex_models
                .models_for_connection(state, connection)
                .await
                .map_err(|error| RouteError::new(error.status, error.message))?;
            Ok(response_with_models(
                connection,
                inventory.models.iter().map(codex_provider_model).collect(),
                inventory.warning,
            ))
        }
        "antigravity" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_antigravity_models(connection, &token).await
        }
        "github" => {
            let token = primary_token(connection)
                .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
            fetch_github_models(connection, &token).await
        }
        // Qoder exposes an OpenAI-compatible models listing behind its API host.
        // Full COSY-signed catalog is a deeper port; this is enough for the
        // dashboard "Fetch Qoder Models" import button.
        "qoder" => {
            fetch_first_party_openai_style_models(connection, "https://api.qoder.com/v1/models")
                .await
        }
        "openai" => {
            fetch_first_party_openai_style_models(connection, "https://api.openai.com/v1/models")
                .await
        }
        "openrouter" => {
            fetch_openrouter_models(connection, "https://openrouter.ai/api/v1/models").await
        }
        "opencode-zen" | "opencode-go" => fetch_opencode_models(state, connection).await,
        "alicode" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://coding.dashscope.aliyuncs.com/v1/models",
            )
            .await
        }
        "alicode-intl" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://coding-intl.dashscope.aliyuncs.com/v1/models",
            )
            .await
        }
        "volcengine-ark" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://ark.cn-beijing.volces.com/api/coding/v3/models",
            )
            .await
        }
        "byteplus" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://ark.ap-southeast.bytepluses.com/api/coding/v3/models",
            )
            .await
        }
        "deepseek" => {
            fetch_first_party_openai_style_models(connection, "https://api.deepseek.com/models")
                .await
        }
        "groq" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://api.groq.com/openai/v1/models",
            )
            .await
        }
        "xai" => {
            fetch_first_party_openai_style_models(connection, "https://api.x.ai/v1/models").await
        }
        "mistral" => {
            fetch_first_party_openai_style_models(connection, "https://api.mistral.ai/v1/models")
                .await
        }
        "perplexity" => {
            fetch_first_party_openai_style_models(connection, "https://api.perplexity.ai/models")
                .await
        }
        "together" => {
            fetch_first_party_openai_style_models(connection, "https://api.together.xyz/v1/models")
                .await
        }
        "fireworks" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://api.fireworks.ai/inference/v1/models",
            )
            .await
        }
        "cerebras" => {
            fetch_first_party_openai_style_models(connection, "https://api.cerebras.ai/v1/models")
                .await
        }
        "cohere" => {
            fetch_first_party_openai_style_models(connection, "https://api.cohere.ai/v1/models")
                .await
        }
        "nebius" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://api.studio.nebius.ai/v1/models",
            )
            .await
        }
        "siliconflow" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://api.siliconflow.com/v1/models",
            )
            .await
        }
        "hyperbolic" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://api.hyperbolic.xyz/v1/models",
            )
            .await
        }
        "ollama" => fetch_ollama_cloud_models(connection).await,
        "chutes" => {
            fetch_first_party_openai_style_models(connection, "https://llm.chutes.ai/v1/models")
                .await
        }
        "nvidia" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://integrate.api.nvidia.com/v1/models",
            )
            .await
        }
        "xiaomi-mimo" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://api.xiaomimimo.com/v1/models",
            )
            .await
        }
        "xiaomi-tokenplan" => {
            let region = connection
                .provider_specific_data
                .get("region")
                .and_then(|v| v.as_str())
                .unwrap_or("sgp");
            let base = match region {
                "cn" => "https://token-plan-cn.xiaomimimo.com/v1",
                "ams" => "https://token-plan-ams.xiaomimimo.com/v1",
                _ => "https://token-plan-sgp.xiaomimimo.com/v1",
            };
            fetch_first_party_openai_style_models(connection, &format!("{base}/models")).await
        }
        "aimlapi" => {
            fetch_first_party_openai_style_models(connection, "https://api.aimlapi.com/v1/models")
                .await
        }
        "modal" => {
            fetch_first_party_openai_style_models(connection, "https://api.modal.com/v1/models")
                .await
        }
        "reka" => {
            fetch_first_party_openai_style_models(connection, "https://api.reka.ai/v1/models").await
        }
        "kluster" => {
            fetch_first_party_openai_style_models(connection, "https://api.kluster.ai/v1/models")
                .await
        }
        "morph" => {
            fetch_first_party_openai_style_models(connection, "https://api.morphllm.com/v1/models")
                .await
        }
        "longcat" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://api.longcat.chat/openai/v1/models",
            )
            .await
        }
        "scaleway" => {
            fetch_first_party_openai_style_models(connection, "https://api.scaleway.ai/v1/models")
                .await
        }
        "sambanova" => {
            fetch_first_party_openai_style_models(connection, "https://api.sambanova.ai/v1/models")
                .await
        }
        "nscale" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://inference.api.nscale.com/v1/models",
            )
            .await
        }
        "baseten" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://inference.baseten.co/v1/models",
            )
            .await
        }
        "nous-research" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://inference-api.nousresearch.com/v1/models",
            )
            .await
        }
        "glhf" => {
            fetch_first_party_openai_style_models(
                connection,
                "https://glhf.chat/api/openai/v1/models",
            )
            .await
        }
        "kilocode" => {
            // Kilo Code exposes an OpenAI-compatible models listing at
            // https://api.kilo.ai/api/openrouter/models (without /v1 — the
            // /v1/models path returns 405). Returns 368 models including
            // :free variants like stepfun/step-3.7-flash:free.
            fetch_first_party_openai_style_models(
                connection,
                "https://api.kilo.ai/api/openrouter/models",
            )
            .await
        }
        other => Err(RouteError::bad_request(format!(
            "Provider {other} does not support models listing"
        ))),
    }
}

async fn fetch_first_party_openai_style_models(
    connection: &ProviderConnection,
    url: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let token = primary_token(connection)
        .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
    fetch_openai_style_models_with_bearer(connection, url, &token).await
}

async fn fetch_opencode_models(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let snapshot = state
        .models_dev
        .snapshot()
        .await
        .map_err(|error| RouteError::new(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let models = snapshot
        .models(&connection.provider)
        .ok_or_else(|| RouteError::bad_request("Unsupported OpenCode provider"))?
        .iter()
        .map(|metadata| {
            let mut value = metadata.catalog_json();
            let object = value
                .as_object_mut()
                .expect("catalog model must be an object");
            object.remove("id");
            object.remove("name");
            object.remove("kind");
            ProviderModel {
                id: metadata.id.clone(),
                name: metadata.name.clone(),
                extra: object
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            }
        })
        .collect();
    Ok(response_with_models(connection, models, None))
}

/// GLM Coding Plan models: paas id list + fail-open rich enrichment.
///
/// The paas endpoint is the same family as the OpenAI chat transport, so its
/// ids are routable by construction. The rich endpoint only covers a subset
/// (live: 3 of 10) but carries `context_window`, `input_modalities` and
/// `supported_reasoning_levels` needed for correct modalities/effort/context
/// display. A rich failure therefore never fails the whole fetch.
async fn fetch_glm_models(
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let token = primary_token(connection)
        .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
    let client = http_client()?;
    let paas_payload = fetch_json(
        client
            .get(GLM_PAAS_MODELS_URL)
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {token}")),
    )
    .await
    .map_err(map_upstream_route_error)?;
    let base = parse_openai_style_models(&paas_payload);

    let enriched = fetch_json(
        client
            .get(GLM_RICH_MODELS_URL)
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {token}")),
    )
    .await
    .ok()
    .map(|payload| parse_glm_rich_models(&payload))
    .unwrap_or_default();

    Ok(response_with_models(
        connection,
        merge_glm_models(base, enriched),
        None,
    ))
}

/// Merge paas ids (source of truth) with rich metadata by id.
/// Rich-only ids are appended; paas-only ids keep their flat shape.
fn merge_glm_models(base: Vec<ProviderModel>, enriched: Vec<ProviderModel>) -> Vec<ProviderModel> {
    let mut by_id: BTreeMap<String, ProviderModel> = BTreeMap::new();
    for model in base {
        by_id.insert(model.id.clone(), model);
    }
    for model in enriched {
        by_id.insert(model.id.clone(), model);
    }
    by_id.into_values().collect()
}

/// Parse the z.ai rich `{"models":[{slug,...}]}` shape into enriched models.
/// Skips hidden/unsupported entries; `max_context_window` wins over
/// `context_window`; empty `supported_reasoning_levels` means no reasoning
/// (emits empty `reasoningEfforts`, never omits the key).
fn parse_glm_rich_models(payload: &Value) -> Vec<ProviderModel> {
    payload
        .get("models")
        .or_else(|| payload.get("data"))
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(glm_enriched_model)
        .collect()
}

fn glm_enriched_model(item: &Value) -> Option<ProviderModel> {
    let object = item.as_object()?;
    if object.get("visibility").and_then(Value::as_str) == Some("hide")
        || object.get("supported_in_api").and_then(Value::as_bool) == Some(false)
    {
        return None;
    }
    let id = object
        .get("slug")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    let name = object
        .get("display_name")
        .or_else(|| object.get("displayName"))
        .or_else(|| object.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(id);

    let context = object
        .get("max_context_window")
        .or_else(|| object.get("context_window"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0);
    let input_modalities: Vec<String> = object
        .get("input_modalities")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let efforts: Vec<String> = object
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|level| level.get("effort").and_then(Value::as_str))
                .map(str::trim)
                .filter(|effort| !effort.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let has_vision = input_modalities.iter().any(|value| value == "image");
    let mut capabilities = vec!["tools".to_string()];
    if !efforts.is_empty() {
        capabilities.push("reasoning".to_string());
    }
    if has_vision {
        capabilities.push("vision".to_string());
    }

    let mut extra = BTreeMap::new();
    extra.insert("kind".to_string(), Value::String("llm".to_string()));
    // Coding Plan uses its OpenAI-compatible endpoint by default.
    extra.insert(
        "targetFormat".to_string(),
        Value::String("openai".to_string()),
    );
    if let Some(context) = context {
        extra.insert("contextWindow".to_string(), Value::from(context));
    }
    extra.insert("capabilities".to_string(), json!(capabilities));
    extra.insert("reasoningEfforts".to_string(), json!(efforts));

    let output_modalities = vec!["text".to_string()];
    let config = OpenCodeModelConfig::from_facts(ModelMetadataFacts {
        name: Some(name.to_string()),
        context,
        input: None,
        output: None,
        capabilities: &capabilities,
        modalities: Some((input_modalities.as_slice(), output_modalities.as_slice())),
        attachment: Some(has_vision),
        reasoning: if efforts.is_empty() { None } else { Some(true) },
        tool_call: Some(true),
        efforts: Some(efforts.as_slice()),
    });
    if let Ok(value) = serde_json::to_value(config) {
        extra.insert("opencode".to_string(), value);
    }

    Some(ProviderModel {
        id: id.to_string(),
        name: name.to_string(),
        extra,
    })
}

/// OpenRouter listing carries the same `HTTP-Referer` + `X-Title` attribution
/// as the chat executor. OpenRouter-fronted gateways (kilocode, nvidia, llm7)
/// deliberately omit them.
async fn fetch_openrouter_models(
    connection: &ProviderConnection,
    url: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let token = primary_token(connection)
        .ok_or_else(|| RouteError::unauthorized("No valid token found"))?;
    let client = http_client()?;
    let request = client
        .get(url)
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header("HTTP-Referer", OPENROUTER_REFERER)
        .header("X-Title", OPENROUTER_TITLE);
    let payload = fetch_json(request)
        .await
        .map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_openai_style_models(&payload),
        None,
    ))
}

async fn fetch_openai_compatible_models(
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let base_url = provider_specific_string(connection, "baseUrl").ok_or_else(|| {
        RouteError::bad_request("No base URL configured for OpenAI compatible provider")
    })?;
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let token = connection.api_key.clone().unwrap_or_default();
    fetch_openai_style_models_with_bearer(connection, &url, &token).await
}

async fn fetch_anthropic_compatible_models(
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let base_url = provider_specific_string(connection, "baseUrl").ok_or_else(|| {
        RouteError::bad_request("No base URL configured for Anthropic compatible provider")
    })?;
    let normalized = normalize_anthropic_models_base_url(&base_url);
    let token = connection.api_key.clone().unwrap_or_default();
    fetch_anthropic_models(connection, &normalized, &token, Some("2023-06-01")).await
}

async fn fetch_openai_style_models_with_bearer(
    connection: &ProviderConnection,
    url: &str,
    token: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let request = client
        .get(url)
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {token}"));
    let payload = fetch_json(request)
        .await
        .map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_openai_style_models(&payload),
        None,
    ))
}

async fn fetch_anthropic_models(
    connection: &ProviderConnection,
    url: &str,
    token: &str,
    version: Option<&str>,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let mut request = client
        .get(url)
        .header(CONTENT_TYPE, "application/json")
        .header("x-api-key", token)
        .header(AUTHORIZATION, format!("Bearer {token}"));
    if let Some(version) = version {
        request = request.header("anthropic-version", version);
    }
    let payload = fetch_json(request)
        .await
        .map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_array_models(payload.get("data").or_else(|| payload.get("models"))),
        None,
    ))
}

async fn fetch_gemini_api_models(
    connection: &ProviderConnection,
    token: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let mut models: Vec<ProviderModel> = Vec::new();
    let mut page_token: Option<String> = None;

    for _ in 0..GEMINI_API_MODELS_MAX_PAGES {
        let mut query: Vec<(&str, String)> =
            vec![("pageSize", GEMINI_API_MODELS_PAGE_SIZE.to_string())];
        if let Some(next) = page_token.as_deref() {
            query.push(("pageToken", next.to_string()));
        }

        let request = client
            .get(GEMINI_API_MODELS_URL)
            .query(&query)
            .header(CONTENT_TYPE, "application/json")
            .header("x-goog-api-key", token);
        let payload = fetch_json(request).await.map_err(map_gemini_fetch_error)?;

        models.extend(parse_gemini_api_models(&payload));

        page_token = payload
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|next| !next.is_empty())
            .map(str::to_string);
        if page_token.is_none() {
            break;
        }
    }

    Ok(response_with_models(connection, models, None))
}

/// `models.list` returns `{"models":[{"name":"models/gemini-2.5-flash",
/// "displayName":…,"supportedGenerationMethods":["generateContent",…]}]}`.
/// The `models/` prefix must be stripped (the chat path re-adds it when
/// building `…/v1beta/models/<id>:generateContent`), and entries that cannot
/// serve `generateContent` — embedders, `aqa`, prediction-only models — must
/// not enter the chat catalog.
fn parse_gemini_api_models(payload: &Value) -> Vec<ProviderModel> {
    payload
        .get("models")
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let id = object
                .get("name")
                .or_else(|| object.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .map(|name| name.strip_prefix("models/").unwrap_or(name))
                .filter(|id| !id.is_empty())?;

            if let Some(methods) = object
                .get("supportedGenerationMethods")
                .and_then(Value::as_array)
            {
                let supports_chat = methods.iter().any(|method| {
                    matches!(
                        method.as_str(),
                        Some("generateContent") | Some("streamGenerateContent")
                    )
                });
                if !supports_chat {
                    return None;
                }
            }

            let extra = object
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "id" | "name" | "displayName"))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();

            Some(ProviderModel {
                id: id.to_string(),
                name: object
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_string(),
                extra,
            })
        })
        .collect()
}

/// Keeps Google's own status (401 invalid credential, 403 PERMISSION_DENIED for
/// a missing key, 400 API_KEY_INVALID) instead of collapsing to a bare 500, and
/// points the operator at the fix.
fn map_gemini_fetch_error(error: FetchJsonError) -> RouteError {
    match error {
        FetchJsonError::Http(status, body) => {
            let detail = gemini_error_detail(&body);
            let hint = if matches!(
                status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::BAD_REQUEST
            ) {
                " — check the Gemini API key (https://aistudio.google.com/app/apikey)"
            } else {
                ""
            };
            RouteError::new(
                status,
                format!(
                    "Failed to fetch Gemini models: {} {detail}{hint}",
                    status.as_u16()
                ),
            )
        }
        FetchJsonError::Network(message) | FetchJsonError::Decode(message) => {
            RouteError::internal(format!("Failed to fetch Gemini models: {message}"))
        }
    }
}

fn gemini_error_detail(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|payload| {
            payload
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

async fn fetch_antigravity_models(
    connection: &ProviderConnection,
    token: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let request = client
        .post("https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:models")
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .json(&json!({}));
    let payload = fetch_json(request)
        .await
        .map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_array_models(payload.get("models")),
        None,
    ))
}

async fn fetch_github_models(
    connection: &ProviderConnection,
    token: &str,
) -> Result<ProviderModelsResponse, RouteError> {
    let client = http_client()?;
    let request = client
        .get("https://api.githubcopilot.com/models")
        .header(CONTENT_TYPE, "application/json")
        .header("Copilot-Integration-Id", "vscode-chat")
        .header("editor-version", "vscode/1.107.1")
        .header("editor-plugin-version", "copilot-chat/0.26.7")
        .header("user-agent", "GitHubCopilotChat/0.26.7")
        .header(AUTHORIZATION, format!("Bearer {token}"));
    let payload = fetch_json(request)
        .await
        .map_err(map_upstream_route_error)?;
    let models = payload
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let capabilities = item.get("capabilities")?.as_object()?;
            if capabilities.get("type").and_then(Value::as_str) != Some("chat") {
                return None;
            }
            if item
                .get("policy")
                .and_then(Value::as_object)
                .and_then(|policy| policy.get("state"))
                .and_then(Value::as_str)
                == Some("disabled")
            {
                return None;
            }

            let id = item.get("id").and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }

            let mut extra = BTreeMap::new();
            if let Some(version) = item.get("version") {
                extra.insert("version".to_string(), version.clone());
            }
            if let Some(capabilities) = item.get("capabilities") {
                extra.insert("capabilities".to_string(), capabilities.clone());
            }
            if let Some(is_default) = item.get("model_picker_enabled").and_then(Value::as_bool) {
                extra.insert("isDefault".to_string(), Value::Bool(is_default));
            }

            Some(ProviderModel {
                id: id.to_string(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_string(),
                extra,
            })
        })
        .collect();

    Ok(response_with_models(connection, models, None))
}

async fn fetch_kiro_models_with_fallback(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let profile_arn = provider_specific_string(connection, "profileArn");
    let access_token = connection.access_token.clone();
    let refresh_token = connection.refresh_token.clone();

    let mut warning = None;

    if let (Some(access_token), Some(profile_arn)) = (access_token, profile_arn) {
        match fetch_kiro_models(&access_token, &profile_arn).await {
            Ok(models) => return Ok(response_with_models(connection, models, None)),
            Err(error) if error.contains("AccessDeniedException") && refresh_token.is_some() => {
                if let Some(refresh_token) = refresh_token.as_deref() {
                    if let Ok(refreshed) =
                        refresh_kiro_token(refresh_token, &connection.provider_specific_data).await
                    {
                        persist_refreshed_credentials(state, connection, &refreshed).await;
                        if let Ok(models) =
                            fetch_kiro_models(&refreshed.access_token, &profile_arn).await
                        {
                            return Ok(response_with_models(connection, models, None));
                        }
                    }
                }
                warning = Some(format!("Failed to fetch Kiro models: {error}"));
            }
            Err(error) => {
                warning = Some(format!("Failed to fetch Kiro models: {error}"));
            }
        }
    }

    Ok(response_with_models(connection, Vec::new(), warning))
}

async fn fetch_gemini_cli_models_with_fallback(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let Some(access_token) = connection.access_token.clone() else {
        return Err(RouteError::unauthorized("No valid token found"));
    };

    let project_id = connection
        .project_id
        .clone()
        .or_else(|| provider_specific_string(connection, "projectId"));

    let mut response = send_gemini_cli_models_request(&access_token, project_id.as_deref()).await;

    if matches!(response, Err(FetchJsonError::Http(status, _)) if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN)
    {
        if let Some(refresh_token) = connection.refresh_token.as_deref() {
            if let Ok(refreshed) = refresh_google_token(
                refresh_token,
                GEMINI_CLIENT_ID,
                crate::oauth::secret::gemini_cli_client_secret(),
            )
            .await
            {
                persist_refreshed_credentials(state, connection, &refreshed).await;
                response =
                    send_gemini_cli_models_request(&refreshed.access_token, project_id.as_deref())
                        .await;
            }
        }
    }

    match response {
        Ok(payload) => {
            let models = parse_gemini_cli_models(&payload);
            Ok(response_with_models(connection, models, None))
        }
        Err(FetchJsonError::Http(status, body)) => Ok(response_with_models(
            connection,
            Vec::new(),
            Some(format!(
                "Failed to fetch Gemini CLI models: {} {}",
                status.as_u16(),
                body
            )),
        )),
        Err(FetchJsonError::Network(message)) | Err(FetchJsonError::Decode(message)) => {
            Ok(response_with_models(
                connection,
                Vec::new(),
                Some(format!("Failed to fetch Gemini CLI models: {message}")),
            ))
        }
    }
}

async fn fetch_ollama_local_models(
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let url = format!("{}/api/tags", resolve_ollama_local_host(connection));
    let client = http_client()?;
    let request = client.get(url).header(CONTENT_TYPE, "application/json");
    let payload = fetch_json(request)
        .await
        .map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_ollama_native_models(&payload),
        None,
    ))
}

/// Ollama Cloud catalog. Prefers the OpenAI-shaped `/v1/models` listing (same
/// host and version prefix the chat endpoint uses) and falls back to the native
/// `/api/tags` shape, which needs `parse_ollama_native_models`.
///
/// The Bearer key is optional here: both endpoints answer 200 unauthenticated,
/// so a fresh connection can import its catalog before the key is pasted.
async fn fetch_ollama_cloud_models(
    connection: &ProviderConnection,
) -> Result<ProviderModelsResponse, RouteError> {
    let token = primary_token(connection);
    let client = http_client()?;

    let mut request = client
        .get(OLLAMA_CLOUD_OPENAI_MODELS_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json");
    if let Some(token) = token.as_deref() {
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));
    }

    match fetch_json(request).await {
        Ok(payload) => {
            let models = parse_openai_style_models(&payload);
            if !models.is_empty() {
                return Ok(response_with_models(connection, models, None));
            }
        }
        Err(error) => {
            tracing::debug!(
                "Ollama Cloud {} failed ({}), falling back to {}",
                OLLAMA_CLOUD_OPENAI_MODELS_URL,
                fetch_json_error_message(error),
                OLLAMA_CLOUD_NATIVE_TAGS_URL
            );
        }
    }

    let mut fallback = client
        .get(OLLAMA_CLOUD_NATIVE_TAGS_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json");
    if let Some(token) = token.as_deref() {
        fallback = fallback.header(AUTHORIZATION, format!("Bearer {token}"));
    }

    let payload = fetch_json(fallback)
        .await
        .map_err(map_upstream_route_error)?;
    Ok(response_with_models(
        connection,
        parse_ollama_native_models(&payload),
        None,
    ))
}

async fn fetch_kiro_models(
    access_token: &str,
    profile_arn: &str,
) -> Result<Vec<ProviderModel>, String> {
    let client = http_client().map_err(|error| error.message)?;
    let request = client
        .post(KIRO_MODELS_URL)
        .header(CONTENT_TYPE, "application/x-amz-json-1.0")
        .header("x-amz-target", KIRO_MODELS_TARGET)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .header(ACCEPT, "application/json")
        .json(&json!({
            "origin": "AI_EDITOR",
            "profileArn": profile_arn,
        }));

    let payload = fetch_json(request)
        .await
        .map_err(fetch_json_error_message)?;
    let models: Vec<ProviderModel> = payload
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("modelId").and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }

            let mut extra = BTreeMap::new();
            if let Some(description) = item.get("description") {
                extra.insert("description".to_string(), description.clone());
            }
            if let Some(rate_multiplier) = item.get("rateMultiplier") {
                extra.insert("rateMultiplier".to_string(), rate_multiplier.clone());
            }
            if let Some(rate_unit) = item.get("rateUnit") {
                extra.insert("rateUnit".to_string(), rate_unit.clone());
            }
            if let Some(max_input_tokens) = item
                .get("tokenLimits")
                .and_then(Value::as_object)
                .and_then(|limits| limits.get("maxInputTokens"))
            {
                extra.insert("maxInputTokens".to_string(), max_input_tokens.clone());
            }

            Some(ProviderModel {
                id: id.to_string(),
                name: item
                    .get("modelName")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_string(),
                extra,
            })
        })
        .collect();
    Ok(expand_kiro_model_variants(models))
}

async fn send_gemini_cli_models_request(
    access_token: &str,
    project_id: Option<&str>,
) -> Result<Value, FetchJsonError> {
    let client = http_client().map_err(|error| FetchJsonError::Network(error.message))?;
    let body = project_id
        .map(|project| json!({ "project": project }))
        .unwrap_or_else(|| json!({}));
    let request = client
        .post(GEMINI_CLI_MODELS_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .header("User-Agent", "google-api-nodejs-client/9.15.1")
        .header(
            "X-Goog-Api-Client",
            "google-cloud-sdk vscode_cloudshelleditor/0.1",
        )
        .json(&body);
    fetch_json(request).await
}

async fn refresh_google_token(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<RefreshResult, String> {
    let client = http_client().map_err(|error| error.message)?;
    let request = client
        .post(GOOGLE_TOKEN_URL)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ]);

    let payload = fetch_json(request)
        .await
        .map_err(fetch_json_error_message)?;
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Google refresh response did not include access_token".to_string())?;

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: payload
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in: payload.get("expires_in").and_then(Value::as_i64),
    })
}

async fn refresh_kiro_token(
    refresh_token: &str,
    provider_specific_data: &BTreeMap<String, Value>,
) -> Result<RefreshResult, String> {
    // Delegate to the shared OAuth refresh path so external_idp / OIDC /
    // Cognito branches stay in lockstep with chat + credential manager.
    let result =
        crate::oauth::token_refresh::refresh_kiro_token(refresh_token, provider_specific_data)
            .await?;
    Ok(RefreshResult {
        access_token: result.access_token,
        refresh_token: result.refresh_token,
        expires_in: result.expires_in,
    })
}

async fn persist_refreshed_credentials(
    state: &AppState,
    connection: &ProviderConnection,
    refresh: &RefreshResult,
) {
    let connection_id = connection.id.clone();
    let access_token = refresh.access_token.clone();
    let refresh_token = refresh.refresh_token.clone();
    let expires_in = refresh.expires_in;

    let _ = state
        .db
        .update(|db| {
            let Some(target) = db
                .provider_connections
                .iter_mut()
                .find(|candidate| candidate.id == connection_id)
            else {
                return;
            };

            target.access_token = Some(access_token.clone());
            if let Some(refresh_token) = &refresh_token {
                target.refresh_token = Some(refresh_token.clone());
            }
            if let Some(expires_in) = expires_in {
                target.expires_in = Some(expires_in);
                target.expires_at =
                    Some((Utc::now() + ChronoDuration::seconds(expires_in)).to_rfc3339());
            }
            target.updated_at = Some(Utc::now().to_rfc3339());
        })
        .await;
}

fn parse_openai_style_models(payload: &Value) -> Vec<ProviderModel> {
    if let Some(array) = payload.as_array() {
        return parse_array_models(Some(&Value::Array(array.clone())));
    }

    parse_array_models(
        payload
            .get("data")
            .or_else(|| payload.get("models"))
            .or_else(|| payload.get("results")),
    )
}

/// Ollama-native `/api/tags` shape: `{"models":[{"name":"gpt-oss:120b",
/// "model":"gpt-oss:120b","modified_at":…,"size":…,"digest":…,"details":{…}}]}`.
/// Both `name` and `model` carry the tag, and there is no `data` envelope or
/// `id` field, so the OpenAI parser's `id` lookup never applies.
fn parse_ollama_native_models(payload: &Value) -> Vec<ProviderModel> {
    let items = payload
        .get("models")
        .or_else(|| payload.get("data"))
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .or_else(|| payload.as_array().map(|items| items.as_slice()))
        .unwrap_or_default();

    items
        .iter()
        .filter_map(|item| {
            if item.is_string() {
                return provider_model_from_value(item);
            }

            let object = item.as_object()?;
            let id = object
                .get("model")
                .or_else(|| object.get("name"))
                .or_else(|| object.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())?;

            let extra = object
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "id" | "model" | "name"))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();

            Some(ProviderModel {
                id: id.to_string(),
                name: id.to_string(),
                extra,
            })
        })
        .collect()
}

fn parse_array_models(value: Option<&Value>) -> Vec<ProviderModel> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(provider_model_from_value).collect())
        .unwrap_or_default()
}

fn parse_gemini_cli_models(payload: &Value) -> Vec<ProviderModel> {
    if let Some(items) = payload.get("models").and_then(Value::as_array) {
        return items
            .iter()
            .filter_map(|item| {
                let id = item
                    .get("id")
                    .or_else(|| item.get("model"))
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())?;

                let mut extra = BTreeMap::new();
                if let Some(display_name) = item.get("displayName") {
                    extra.insert("displayName".to_string(), display_name.clone());
                }

                Some(ProviderModel {
                    id: id.to_string(),
                    name: item
                        .get("displayName")
                        .or_else(|| item.get("name"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(id)
                        .to_string(),
                    extra,
                })
            })
            .collect();
    }

    payload
        .get("models")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .filter(|(_, info)| {
                    !info
                        .get("isInternal")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .map(|(id, info)| ProviderModel {
                    id: id.to_string(),
                    name: info
                        .get("displayName")
                        .or_else(|| info.get("name"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(id)
                        .to_string(),
                    extra: BTreeMap::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn codex_provider_model(model: &crate::server::codex_catalog::CodexModelMetadata) -> ProviderModel {
    let mut extra = BTreeMap::new();
    extra.insert("kind".to_string(), Value::String("llm".to_string()));
    extra.insert(
        "targetFormat".to_string(),
        Value::String("openai-responses".to_string()),
    );
    if let Some(context) = model.context_window {
        extra.insert("contextWindow".to_string(), Value::from(context));
    }
    extra.insert("capabilities".to_string(), json!(model.capabilities));
    extra.insert(
        "reasoningEfforts".to_string(),
        json!(model.reasoning_efforts),
    );
    ProviderModel {
        id: model.id.clone(),
        name: model.name.clone(),
        extra,
    }
}

fn expand_kiro_model_variants(models: Vec<ProviderModel>) -> Vec<ProviderModel> {
    let mut expanded = Vec::with_capacity(models.len() * 4);
    for model in models {
        let base_id = model.id.clone();
        let base_name = model.name.clone();
        let base_extra = model.extra.clone();
        let is_auto = base_id == "auto" || base_id.contains("auto");

        expanded.push(model);

        let make_variant = |suffix: &str, variant: &str| -> ProviderModel {
            let mut extra = base_extra.clone();
            extra.insert(
                "originalModelId".to_string(),
                Value::String(base_id.clone()),
            );
            extra.insert("variant".to_string(), Value::String(variant.to_string()));
            ProviderModel {
                id: format!("{base_id}{suffix}"),
                name: base_name.clone(),
                extra,
            }
        };

        expanded.push(make_variant("-thinking", "thinking"));
        if !is_auto {
            expanded.push(make_variant("-agentic", "agentic"));
            expanded.push(make_variant("-thinking-agentic", "thinking-agentic"));
        }
    }
    expanded
}

fn provider_model_from_value(value: &Value) -> Option<ProviderModel> {
    match value {
        Value::String(text) => {
            let id = text.trim();
            if id.is_empty() {
                return None;
            }
            Some(ProviderModel {
                id: id.to_string(),
                name: id.to_string(),
                extra: BTreeMap::new(),
            })
        }
        Value::Object(object) => {
            let id = object
                .get("id")
                .or_else(|| object.get("slug"))
                .or_else(|| object.get("model"))
                .or_else(|| object.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())?;

            let name = object
                .get("display_name")
                .or_else(|| object.get("displayName"))
                .or_else(|| object.get("name"))
                .or_else(|| object.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(id);

            let extra = object
                .iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "id" | "slug" | "model" | "name" | "display_name" | "displayName"
                    )
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();

            Some(ProviderModel {
                id: id.to_string(),
                name: name.to_string(),
                extra,
            })
        }
        _ => None,
    }
}

fn normalize_anthropic_models_base_url(base_url: &str) -> String {
    let mut normalized = base_url.trim().trim_end_matches('/').to_string();
    if normalized.ends_with("/messages") {
        normalized.truncate(normalized.len() - "/messages".len());
    }
    format!("{normalized}/models")
}

fn resolve_qwen_models_url(connection: &ProviderConnection) -> String {
    let fallback = "https://portal.qwen.ai/v1/models";
    let Some(raw) = provider_specific_string(connection, "resourceUrl") else {
        return fallback.to_string();
    };

    if raw.starts_with("http://") || raw.starts_with("https://") {
        return format!("{}/models", raw.trim_end_matches('/'));
    }

    format!("https://{}/v1/models", raw.trim_end_matches('/'))
}

fn resolve_ollama_local_host(connection: &ProviderConnection) -> String {
    provider_specific_string(connection, "baseUrl")
        .unwrap_or_else(|| OLLAMA_LOCAL_DEFAULT_HOST.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn response_with_models(
    connection: &ProviderConnection,
    models: Vec<ProviderModel>,
    warning: Option<String>,
) -> ProviderModelsResponse {
    ProviderModelsResponse {
        provider: connection.provider.clone(),
        connection_id: connection.id.clone(),
        models,
        warning,
    }
}

fn primary_token(connection: &ProviderConnection) -> Option<String> {
    provider_specific_string(connection, "copilotToken")
        .or_else(|| connection.access_token.clone())
        .or_else(|| connection.api_key.clone())
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn provider_specific_string(connection: &ProviderConnection, key: &str) -> Option<String> {
    connection
        .provider_specific_data
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn is_openai_compatible_provider(provider: &str) -> bool {
    provider.starts_with(OPENAI_COMPATIBLE_PREFIX)
}

fn is_anthropic_compatible_provider(provider: &str) -> bool {
    provider.starts_with(ANTHROPIC_COMPATIBLE_PREFIX)
}

fn storage_alias_for_provider(provider: &str) -> String {
    if is_openai_compatible_provider(provider) || is_anthropic_compatible_provider(provider) {
        return provider.to_string();
    }
    let catalog = crate::core::model::catalog::provider_catalog();
    if let Some(alias) = catalog.static_alias_for_provider(provider) {
        return alias.to_string();
    }
    provider.to_string()
}

fn dedupe_model_ids(mut ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    ids
}

fn http_client() -> Result<reqwest::Client, RouteError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| RouteError::internal("Failed to fetch models"))
}

async fn fetch_json(request: reqwest::RequestBuilder) -> Result<Value, FetchJsonError> {
    let response = request
        .send()
        .await
        .map_err(|error| FetchJsonError::Network(error.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(FetchJsonError::Http(status, body));
    }

    response
        .json::<Value>()
        .await
        .map_err(|error| FetchJsonError::Decode(error.to_string()))
}

fn map_upstream_route_error(error: FetchJsonError) -> RouteError {
    match error {
        FetchJsonError::Http(status, _) => RouteError::new(
            status,
            format!("Failed to fetch models: {}", status.as_u16()),
        ),
        FetchJsonError::Network(_) | FetchJsonError::Decode(_) => {
            RouteError::internal("Failed to fetch models")
        }
    }
}

fn fetch_json_error_message(error: FetchJsonError) -> String {
    match error {
        FetchJsonError::Http(status, body) => {
            if body.is_empty() {
                status.as_u16().to_string()
            } else {
                body
            }
        }
        FetchJsonError::Network(message) | FetchJsonError::Decode(message) => message,
    }
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(provider: &str) -> ProviderConnection {
        ProviderConnection {
            id: "conn-1".to_string(),
            provider: provider.to_string(),
            auth_type: "oauth".to_string(),
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
            api_key: None,
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
    fn normalize_anthropic_base_url_strips_messages_suffix() {
        assert_eq!(
            normalize_anthropic_models_base_url("https://example.com/v1/messages"),
            "https://example.com/v1/models"
        );
        assert_eq!(
            normalize_anthropic_models_base_url("https://example.com/v1/messages/"),
            "https://example.com/v1/models"
        );
    }

    #[test]
    fn supports_models_discovery_covers_builtin_and_compatible_providers() {
        assert!(supports_models_discovery("opencode-zen"));
        assert!(supports_models_discovery("nvidia"));
        assert!(supports_models_discovery("openrouter"));
        assert!(supports_models_discovery("kilocode"));
        assert!(supports_models_discovery("ollama-local"));
        assert!(supports_models_discovery("openai-compatible-chat"));
        assert!(supports_models_discovery("anthropic-compatible-chat"));
        assert!(supports_models_discovery("glm"));
        assert!(!supports_models_discovery("totally-unknown-provider"));
    }

    #[test]
    fn glm_rich_and_paas_shapes_merge_without_losing_ids() {
        // Live shapes: paas/v4/models is the full id set without metadata,
        // api/v1/models is rich metadata for a subset.
        let paas = json!({
            "object": "list",
            "data": [
                { "id": "glm-5.3", "object": "model" },
                { "id": "glm-5-turbo", "object": "model" },
                { "id": "glm-4.7", "object": "model" }
            ]
        });
        let rich = json!({ "models": [
            {"slug": "glm-5.3", "display_name": "glm-5.3", "visibility": "list",
             "supported_in_api": true, "context_window": 204800,
             "max_context_window": 1048576,
             "input_modalities": ["text"],
             "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}, {"effort": "max"}]},
            {"slug": "glm-5.3-flash", "display_name": "glm-5.3-flash",
             "visibility": "list", "supported_in_api": true,
             "context_window": 1048576,
             "input_modalities": ["text", "image"],
             "supported_reasoning_levels": [{"effort": "low"}, {"effort": "max"}]},
            {"slug": "glm-5-turbo", "display_name": "glm-5-turbo",
             "visibility": "list", "supported_in_api": true,
             "context_window": 204800, "max_context_window": 204800,
             "input_modalities": ["text"], "supported_reasoning_levels": []},
            {"slug": "glm-hidden", "visibility": "hide"},
            {"slug": "glm-disabled", "supported_in_api": false}
        ]});

        let merged = merge_glm_models(
            parse_openai_style_models(&paas),
            parse_glm_rich_models(&rich),
        );
        let ids: Vec<&str> = merged.iter().map(|model| model.id.as_str()).collect();
        // Union: paas ids kept, rich-only flash appended, hidden/disabled dropped.
        assert_eq!(
            ids,
            vec!["glm-4.7", "glm-5-turbo", "glm-5.3", "glm-5.3-flash"]
        );

        let by_id = |id: &str| merged.iter().find(|model| model.id == id).expect(id);
        // max_context_window wins over context_window.
        assert_eq!(
            by_id("glm-5.3").extra.get("contextWindow"),
            Some(&json!(1048576))
        );
        // Vision only where input_modalities contains image.
        assert!(by_id("glm-5.3-flash").extra["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.contains(&json!("vision"))));
        assert!(by_id("glm-5.3").extra["capabilities"]
            .as_array()
            .is_some_and(|caps| !caps.contains(&json!("vision"))));
        // Empty levels: no reasoning cap, but the key is emitted (not omitted).
        assert!(by_id("glm-5-turbo").extra["capabilities"]
            .as_array()
            .is_some_and(|caps| !caps.contains(&json!("reasoning"))));
        assert_eq!(
            by_id("glm-5-turbo").extra.get("reasoningEfforts"),
            Some(&json!([]))
        );
        // Paas-only id keeps the flat shape.
        assert_eq!(by_id("glm-4.7").name, "glm-4.7");
        // Transport marker follows the Coding Plan OpenAI endpoint.
        assert_eq!(
            by_id("glm-5.3").extra.get("targetFormat"),
            Some(&json!("openai"))
        );
    }

    #[test]
    fn ollama_cloud_openai_shape_parses_tag_ids() {
        // Live shape of https://ollama.com/v1/models.
        let payload = json!({
            "object": "list",
            "data": [
                { "id": "gpt-oss:120b", "object": "model", "created": 1754352000, "owned_by": "ollama" },
                { "id": "glm-5.2", "object": "model", "created": 1781622000, "owned_by": "ollama" }
            ]
        });

        let ids: Vec<String> = parse_openai_style_models(&payload)
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, vec!["gpt-oss:120b", "glm-5.2"]);
    }

    #[test]
    fn ollama_native_tags_shape_parses_without_data_envelope() {
        // Live shape of https://ollama.com/api/tags (and the local daemon's).
        let payload = json!({
            "models": [
                { "name": "gpt-oss:120b", "model": "gpt-oss:120b", "size": 65290180781u64,
                  "digest": "d98fe6ba01e6", "details": { "family": "" } },
                { "name": "kimi-k2.7-code", "model": "kimi-k2.7-code", "size": 0 }
            ]
        });

        let models = parse_ollama_native_models(&payload);
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-oss:120b", "kimi-k2.7-code"]);
        assert_eq!(models[0].name, "gpt-oss:120b");
        assert!(models[0].extra.contains_key("digest"));
        assert!(!models[0].extra.contains_key("model"));
    }

    #[test]
    fn ollama_native_parser_still_reads_openai_shape() {
        let payload = json!({ "data": [{ "id": "llama3.2:3b" }] });
        let ids: Vec<String> = parse_ollama_native_models(&payload)
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, vec!["llama3.2:3b"]);
    }

    #[test]
    fn gemini_models_list_strips_prefix_and_drops_non_chat_models() {
        // Live shape of GET /v1beta/models (x-goog-api-key auth).
        let payload = json!({
            "models": [
                {
                    "name": "models/gemini-2.5-flash",
                    "displayName": "Gemini 2.5 Flash",
                    "inputTokenLimit": 1048576,
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/text-embedding-004",
                    "displayName": "Text Embedding 004",
                    "supportedGenerationMethods": ["embedContent"]
                },
                {
                    "name": "models/aqa",
                    "displayName": "Model that performs Attributed Question Answering",
                    "supportedGenerationMethods": ["generateAnswer"]
                },
                {
                    "name": "models/gemini-3-pro-preview",
                    "displayName": "Gemini 3 Pro Preview"
                }
            ]
        });

        let models = parse_gemini_api_models(&payload);
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["gemini-2.5-flash", "gemini-3-pro-preview"]);
        assert_eq!(models[0].name, "Gemini 2.5 Flash");
        assert!(models[0].extra.contains_key("inputTokenLimit"));
    }

    #[test]
    fn gemini_fetch_error_preserves_status_and_upstream_message() {
        let error = FetchJsonError::Http(
            StatusCode::FORBIDDEN,
            json!({
                "error": {
                    "code": 403,
                    "message": "Method doesn't allow unregistered callers",
                    "status": "PERMISSION_DENIED"
                }
            })
            .to_string(),
        );

        let mapped = map_gemini_fetch_error(error);
        assert_eq!(mapped.status, StatusCode::FORBIDDEN);
        assert!(mapped.message.contains("unregistered callers"));
        assert!(mapped.message.contains("aistudio.google.com"));
    }

    #[test]
    fn openrouter_and_kilocode_free_variants_survive_parsing() {
        // Live shapes: openrouter.ai/api/v1/models and
        // api.kilo.ai/api/openrouter/models both wrap entries in `data` and
        // expose `:free` ids that discovery must keep.
        let payload = json!({
            "data": [
                { "id": "tencent/hy3:free", "name": "HY3 (free)", "context_length": 262144,
                  "pricing": { "prompt": "0", "completion": "0" } },
                { "id": "stepfun/step-3.7-flash:free", "name": "Step 3.7 Flash (free)" },
                { "id": "anthropic/claude-opus-4.5", "name": "Claude Opus 4.5" }
            ]
        });

        let ids: Vec<String> = parse_openai_style_models(&payload)
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "tencent/hy3:free",
                "stepfun/step-3.7-flash:free",
                "anthropic/claude-opus-4.5"
            ]
        );
    }

    #[test]
    fn opencode_zen_list_shape_parses_without_prefix_munging() {
        // Live shape: {"object":"list","data":[{"id":"gpt-5.6-sol","object":"model",…}]}
        let payload = json!({
            "object": "list",
            "data": [
                { "id": "gpt-5.6-sol", "object": "model", "owned_by": "opencode" },
                { "id": "claude-opus-5", "object": "model", "owned_by": "opencode" },
                { "id": "hy3-free", "object": "model", "owned_by": "opencode" }
            ]
        });

        let models = parse_openai_style_models(&payload);
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-5.6-sol", "claude-opus-5", "hy3-free"]);
        assert_eq!(models[0].name, "gpt-5.6-sol");
    }

    #[test]
    fn opencode_zen_discovery_does_not_require_a_token() {
        // noAuth provider: primary_token() is empty yet discovery must proceed.
        let connection = connection("opencode-zen");
        assert!(primary_token(&connection).is_none());
        assert!(supports_models_discovery("opencode-zen"));
    }

    #[test]
    fn openrouter_attribution_matches_chat_executor_values() {
        assert_eq!(OPENROUTER_REFERER, "https://endpoint-proxy.local");
        assert_eq!(OPENROUTER_TITLE, "Endpoint Proxy");
        assert_eq!(
            crate::core::executor::provider_config_base_url("openrouter").as_deref(),
            Some("https://openrouter.ai/api/v1/chat/completions")
        );
    }

    #[test]
    fn resolve_qwen_models_url_uses_resource_url_variants() {
        let mut connection = connection("qwen");
        connection.provider_specific_data.insert(
            "resourceUrl".to_string(),
            Value::String("tenant.qwen.ai".to_string()),
        );
        assert_eq!(
            resolve_qwen_models_url(&connection),
            "https://tenant.qwen.ai/v1/models"
        );

        connection.provider_specific_data.insert(
            "resourceUrl".to_string(),
            Value::String("https://tenant.qwen.ai/base".to_string()),
        );
        assert_eq!(
            resolve_qwen_models_url(&connection),
            "https://tenant.qwen.ai/base/models"
        );
    }

    #[test]
    fn parse_gemini_cli_models_filters_internal_entries() {
        let payload = json!({
            "models": {
                "gemini-2.5-pro": { "displayName": "Gemini 2.5 Pro", "isInternal": false },
                "internal-model": { "displayName": "Internal", "isInternal": true }
            }
        });

        let models = parse_gemini_cli_models(&payload);
        assert_eq!(
            models,
            vec![ProviderModel {
                id: "gemini-2.5-pro".to_string(),
                name: "Gemini 2.5 Pro".to_string(),
                extra: BTreeMap::new(),
            }]
        );
    }

    #[test]
    fn test_expand_kiro_model_variants() {
        let original = ProviderModel {
            id: "amazon-nova-pro-v1.0".to_string(),
            name: "Amazon Nova Pro v1.0".to_string(),
            extra: BTreeMap::from([(
                "rateMultiplier".to_string(),
                Value::String("1.0".to_string()),
            )]),
        };

        let expanded = expand_kiro_model_variants(vec![original]);
        assert_eq!(expanded.len(), 4);

        let ids: Vec<&str> = expanded.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "amazon-nova-pro-v1.0",
                "amazon-nova-pro-v1.0-thinking",
                "amazon-nova-pro-v1.0-agentic",
                "amazon-nova-pro-v1.0-thinking-agentic",
            ]
        );

        assert_eq!(expanded[0].name, "Amazon Nova Pro v1.0");
        assert_eq!(expanded[0].extra.get("rateMultiplier").unwrap(), "1.0");
        assert!(expanded[0].extra.get("originalModelId").is_none());

        for (idx, variant) in ["thinking", "agentic", "thinking-agentic"]
            .iter()
            .enumerate()
        {
            let model = &expanded[idx + 1];
            assert_eq!(model.name, "Amazon Nova Pro v1.0");
            assert_eq!(
                model.extra.get("originalModelId"),
                Some(&Value::String("amazon-nova-pro-v1.0".to_string()))
            );
            assert_eq!(
                model.extra.get("variant"),
                Some(&Value::String((*variant).to_string()))
            );
            assert_eq!(
                model.extra.get("rateMultiplier"),
                Some(&Value::String("1.0".to_string()))
            );
        }
    }

    #[test]
    fn test_expand_kiro_model_variants_skips_agentic_for_auto() {
        // Bare "auto" id: only base + -thinking (no -agentic or -thinking-agentic).
        let auto_model = ProviderModel {
            id: "auto".to_string(),
            name: "Auto".to_string(),
            extra: BTreeMap::new(),
        };
        let expanded = expand_kiro_model_variants(vec![auto_model]);
        let ids: Vec<&str> = expanded.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["auto", "auto-thinking"]);

        // "default-auto" (id containing "auto") gets the same treatment.
        let default_auto = ProviderModel {
            id: "default-auto".to_string(),
            name: "Default Auto".to_string(),
            extra: BTreeMap::new(),
        };
        let expanded = expand_kiro_model_variants(vec![default_auto]);
        let ids: Vec<&str> = expanded.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["default-auto", "default-auto-thinking"]);
    }

    #[test]
    fn import_tags_models_with_source_imported() {
        let model = ProviderModel {
            id: "meta/llama-3.1-8b".to_string(),
            name: "Llama 3.1 8B".to_string(),
            extra: BTreeMap::from([("context_length".to_string(), Value::from(131_072u64))]),
        };
        let now = "2026-08-22T00:00:00Z".to_string();

        let mut extra = model.extra.clone();
        extra
            .entry("source".to_string())
            .or_insert_with(|| Value::String("imported".to_string()));
        extra
            .entry("importedAt".to_string())
            .or_insert_with(|| Value::String(now.clone()));

        let name = if model.name.is_empty() {
            None
        } else {
            Some(model.name)
        };
        let custom = CustomModel {
            provider_alias: "nvidia".to_string(),
            id: model.id.trim().to_string(),
            r#type: "llm".to_string(),
            name,
            extra,
        };

        assert_eq!(custom.provider_alias, "nvidia");
        assert_eq!(custom.id, "meta/llama-3.1-8b");
        assert_eq!(custom.r#type, "llm");
        assert_eq!(custom.name.as_deref(), Some("Llama 3.1 8B"));
        assert_eq!(
            custom.extra.get("source").unwrap().as_str(),
            Some("imported")
        );
        assert_eq!(
            custom.extra.get("importedAt").unwrap().as_str(),
            Some("2026-08-22T00:00:00Z")
        );
        assert_eq!(
            custom.extra.get("context_length").unwrap().as_u64(),
            Some(131_072)
        );
    }

    #[test]
    fn import_skip_check_matches_existing_custom_model() {
        let existing = CustomModel {
            provider_alias: "nvidia".to_string(),
            id: "meta/llama-3.1-8b".to_string(),
            r#type: "llm".to_string(),
            name: None,
            extra: BTreeMap::new(),
        };
        let provider_alias = "nvidia".to_string();
        let model_id = "meta/llama-3.1-8b".to_string();

        let exists = [existing.clone()]
            .iter()
            .any(|m| m.provider_alias == provider_alias && m.id == model_id && m.r#type == "llm");
        assert!(exists, "existing (provider_alias, id, type) should match");

        let missing = [existing.clone()].iter().any(|m| {
            m.provider_alias == provider_alias && m.id == "different/model" && m.r#type == "llm"
        });
        assert!(!missing, "different id should not match");
    }
}
