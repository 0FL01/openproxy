use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing, Json, Router};
use serde::Serialize;
use serde_json::{json, Value};

use crate::core::usage::quota_fetcher::{
    codex_account_id, consume_codex_rate_limit_reset_credit, fetch_a6api_quota,
    fetch_antigravity_quota, fetch_claude_quota, fetch_codebuddy_quota, fetch_codex_quota,
    fetch_commandcode_quota, fetch_deepseek_usage, fetch_github_quota, fetch_glm_quota,
    fetch_kimi_oauth_usage, fetch_kimi_usage, fetch_minimax_quota, fetch_ollama_quota,
    fetch_opencode_go_quota, fetch_vercel_ai_gateway_quota, get_codex_rate_limit_reset_credits,
};
use crate::oauth::token_refresh::{
    connection_credential_generation, CONNECTION_REFRESH_COORDINATOR,
};
use crate::server::state::AppState;
use crate::types::ProviderConnection;

fn require_usage_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

/// 9router `USAGE_APIKEY_PROVIDERS` parity (providers.js:163-165 — 12 registry
/// entries with `features.usageApikey`). Providers without a live-quota fetcher
/// fall back to a static message / per-request history (never 500).
fn is_usage_apikey_provider(provider: &str) -> bool {
    matches!(
        provider,
        "glm"
            | "glm-cn"
            | "minimax"
            | "kimi"
            | "deepseek"
            | "opencode-go"
            | "ollama"
            | "vercel-ai-gateway"
            | "codebuddy-cn"
            | "codebuddy-intl"
            | "commandcode"
            | "a6api"
    )
}

/// Dispatch to the correct OAuth quota fetcher for `connection`. Returns
/// `{}` for providers that don't expose a live quota endpoint.
pub async fn fetch_oauth_quota(connection: &ProviderConnection) -> Value {
    let token = match connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t,
        None => return serde_json::json!({}),
    };
    let provider = connection.provider.as_str();
    let psd = &connection.provider_specific_data;
    match provider {
        "github" | "github-copilot" => fetch_github_quota(token, provider).await,
        "claude" => fetch_claude_quota(token, provider).await,
        "codex" => {
            let account_id = codex_account_id(psd);
            fetch_codex_quota(token, account_id.as_deref()).await
        }
        "antigravity" => fetch_antigravity_quota(token, provider).await,
        "ollama" => fetch_ollama_quota(token).await,
        // Kimi OAuth connections hit /v1/usages with Bearer + X-Msh-* headers.
        "kimi" | "kimi-coding" => fetch_kimi_oauth_usage(token, psd).await,
        _ => serde_json::json!({}),
    }
}

fn usage_message_for_provider(provider: &str) -> String {
    match provider {
        "ollama" => "Ollama Cloud uses a free tier with light usage limits (resets every 5h & 7d). For detailed usage tracking, visit ollama.com/settings/keys.".to_string(),
        other => format!("Usage API not implemented for {other}"),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/usage/{connection_id}",
            routing::get(get_connection_usage),
        )
        .route(
            "/api/usage/{connection_id}/codex-reset-credits",
            routing::get(get_connection_codex_reset_credits).post(reset_connection_credits),
        )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionUsageResponse {
    connection_id: String,
    message: String,
    quotas: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
}

async fn get_connection_usage(
    State(state): State<AppState>,
    axum::extract::Path(connection_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(connection) = snapshot
        .provider_connections
        .iter()
        .find(|entry| entry.id == connection_id)
    else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Connection not found" })),
        )
            .into_response();
    };

    let is_oauth = connection.auth_type == "oauth";
    // Generic apikey providers persist "apikey" — accept both spellings.
    let is_apikey_eligible = (connection.auth_type == "apikey"
        || connection.auth_type == "api_key")
        && is_usage_apikey_provider(&connection.provider);
    if !is_oauth && !is_apikey_eligible {
        return Json(serde_json::json!({
            "message": "Usage not available for this connection"
        }))
        .into_response();
    }

    // Live quota fetch for whitelisted apikey providers (GLM, MiniMax). Falls
    // back to a static info message when the fetcher returns one. We never
    // surface upstream errors as HTTP failures — the dashboard treats
    // `quotas: {}` + `message` as "connected, but quota unavailable".
    let mut live_quotas = serde_json::json!({});
    let mut live_message: Option<String> = None;
    let mut live_status: Option<String> = None;
    if is_apikey_eligible {
        if let Some(api_key) = connection
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let provider = connection.provider.clone();
            let result = match provider.as_str() {
                "glm" | "glm-cn" => fetch_glm_quota(api_key, &provider).await,
                "minimax" => fetch_minimax_quota(api_key, &provider).await,
                "kimi" => fetch_kimi_usage(api_key).await,
                "deepseek" => fetch_deepseek_usage(api_key).await,
                "opencode-go" => fetch_opencode_go_quota(api_key).await,
                "vercel-ai-gateway" => fetch_vercel_ai_gateway_quota(api_key).await,
                "codebuddy-cn" | "codebuddy-intl" => {
                    fetch_codebuddy_quota(api_key, &provider).await
                }
                "commandcode" => fetch_commandcode_quota(api_key).await,
                "a6api" => fetch_a6api_quota(api_key).await,
                // Ollama has no live API-key quota fetcher yet.
                _ => serde_json::json!({}),
            };
            if let Some(quotas) = result.get("quotas") {
                live_quotas = quotas.clone();
            }
            if let Some(msg) = result.get("message").and_then(|v| v.as_str()) {
                live_message = Some(msg.to_string());
            }
            if let Some(status) = result.get("status").and_then(Value::as_str) {
                live_status = Some(status.to_string());
            }
        }
    }

    let mut live_plan: Option<Value> = None;
    let mut live_reset_credits: Option<Value> = None;
    if is_oauth {
        // 9router route.js:158-183 — refresh credentials before the quota
        // call and force-retry once on an auth-expired message.
        let result = fetch_oauth_quota_with_refresh(&state, connection).await;
        if let Some(quotas) = result.get("quotas") {
            live_quotas = quotas.clone();
        }
        if let Some(msg) = result.get("message").and_then(|v| v.as_str()) {
            live_message = Some(msg.to_string());
        }
        if let Some(plan) = result.get("plan") {
            live_plan = Some(plan.clone());
        }
        if let Some(reset_credits) = result.get("resetCredits") {
            live_reset_credits = Some(reset_credits.clone());
        }
    }

    // When quotas are populated, skip the generic fallback message so the
    // frontend renders the QuotaTable instead of a text-only message.
    let message = if live_quotas.as_object().is_some_and(|o| !o.is_empty()) {
        live_message.unwrap_or_default()
    } else {
        live_message.unwrap_or_else(|| usage_message_for_provider(&connection.provider))
    };

    let mut body = serde_json::to_value(ConnectionUsageResponse {
        connection_id,
        message,
        quotas: live_quotas,
        status: live_status,
    })
    .unwrap_or_else(|_| json!({}));
    if let Some(obj) = body.as_object_mut() {
        if let Some(plan) = live_plan {
            obj.insert("plan".to_string(), plan);
        }
        if let Some(reset_credits) = live_reset_credits {
            obj.insert("resetCredits".to_string(), reset_credits);
        }
    }
    Json(body).into_response()
}

fn is_codex_reset_auth_type(auth_type: &str) -> bool {
    matches!(
        auth_type.trim().to_ascii_lowercase().as_str(),
        "oauth" | "access_token" | "accesstoken"
    )
}

fn is_auth_expired_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "expired",
        "authentication",
        "unauthorized",
        "401",
        "re-authorize",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

fn refresh_error_status(error: &str) -> Option<u16> {
    error
        .split_once("HTTP ")
        .and_then(|(_, suffix)| suffix.get(..3))
        .and_then(|value| value.parse::<u16>().ok())
}

fn refresh_error_code(error: &str) -> Option<&str> {
    let (_, code) = error.rsplit_once(": ")?;
    (!code.is_empty()
        && code.len() <= 80
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
    .then_some(code)
}

fn is_permanent_codex_refresh_rejection(error: &str) -> bool {
    matches!(
        refresh_error_code(error),
        Some(
            "invalid_grant"
                | "refresh_token_reused"
                | "refresh_token_expired"
                | "refresh_token_invalidated"
        )
    )
}

#[derive(Debug)]
enum CodexResetCreditsRequestError {
    MissingAccessToken,
    Refresh(String),
    CreditsApi(crate::core::usage::quota_fetcher::CodexResetCreditsFetchError),
    ConsumeTransport,
}

struct PreparedCodexConnection {
    connection: ProviderConnection,
    refresh_error: Option<String>,
}

/// Refresh an OAuth connection's tokens via the provider's refresh flow and
/// return a cloned connection with the refreshed credentials. 9router
/// `refreshAndUpdateCredentials` parity (route.js:23-117). Returns the
/// original connection untouched on refresh failure (JS keeps the stale
/// accessToken when one exists).
async fn refresh_oauth_connection(
    state: &AppState,
    connection: &ProviderConnection,
    force: bool,
) -> Result<ProviderConnection, String> {
    let Some(_refresh_token) = connection
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(connection.clone());
    };

    // JS executor.needsRefresh(credentials): refresh when expired or missing.
    let has_access_token = connection
        .access_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty());
    let needs_refresh = if connection.provider == "codex" {
        force || !has_access_token || crate::oauth::token_refresh::codex_refresh_due(connection)
    } else {
        force
            || !has_access_token
            || match connection.expires_at.as_deref() {
                Some(expires_at) => crate::oauth::token_refresh::needs_refresh_with_lead(
                    &Some(expires_at.to_string()),
                    // Refresh a bit early (2 min) to avoid a doomed fetch.
                    120_000,
                ),
                None => connection
                    .access_token
                    .as_deref()
                    .is_none_or(|t| t.trim().is_empty()),
            }
    };
    if !needs_refresh {
        return Ok(connection.clone());
    }

    let observed_generation = connection_credential_generation(connection);
    CONNECTION_REFRESH_COORDINATOR
        .refresh_connection(
            state.db.clone(),
            &connection.provider,
            &connection.id,
            observed_generation,
        )
        .await
        .map(|result| result.connection)
}

async fn prepare_codex_connection(
    state: &AppState,
    connection: &ProviderConnection,
    is_oauth: bool,
) -> PreparedCodexConnection {
    if !is_oauth
        || connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        return PreparedCodexConnection {
            connection: connection.clone(),
            refresh_error: None,
        };
    }

    match refresh_oauth_connection(state, connection, false).await {
        Ok(connection) => PreparedCodexConnection {
            connection,
            refresh_error: None,
        },
        Err(error) => {
            tracing::warn!(
                provider = "codex",
                refresh_status = refresh_error_status(&error),
                refresh_error_code = refresh_error_code(&error).unwrap_or("unknown"),
                "proactive Codex credential refresh failed; trying the stored access token"
            );
            PreparedCodexConnection {
                connection: connection.clone(),
                refresh_error: Some(error),
            }
        }
    }
}

async fn fetch_codex_reset_credits_with_auth<F, Fut>(
    state: &AppState,
    connection: &ProviderConnection,
    is_oauth: bool,
    fetch: F,
) -> Result<Value, CodexResetCreditsRequestError>
where
    F: Fn(String, Option<String>) -> Fut,
    Fut: std::future::Future<
        Output = Result<Value, crate::core::usage::quota_fetcher::CodexResetCreditsFetchError>,
    >,
{
    let prepared = prepare_codex_connection(state, connection, is_oauth).await;
    let account_id = codex_account_id(&prepared.connection.provider_specific_data);
    let Some(access_token) = prepared
        .connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
    else {
        return Err(prepared
            .refresh_error
            .map(CodexResetCreditsRequestError::Refresh)
            .unwrap_or(CodexResetCreditsRequestError::MissingAccessToken));
    };

    match fetch(access_token, account_id.clone()).await {
        Ok(value) => Ok(value),
        Err(error)
            if error.status == Some(401)
                && is_oauth
                && prepared
                    .connection
                    .refresh_token
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|token| !token.is_empty()) =>
        {
            let refreshed = refresh_oauth_connection(state, &prepared.connection, true)
                .await
                .map_err(CodexResetCreditsRequestError::Refresh)?;
            let access_token = refreshed
                .access_token
                .as_deref()
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(str::to_string)
                .ok_or(CodexResetCreditsRequestError::MissingAccessToken)?;
            fetch(access_token, account_id)
                .await
                .map_err(CodexResetCreditsRequestError::CreditsApi)
        }
        Err(error) => Err(CodexResetCreditsRequestError::CreditsApi(error)),
    }
}

async fn consume_codex_reset_credit_with_auth<F, Fut>(
    state: &AppState,
    prepared: &PreparedCodexConnection,
    is_oauth: bool,
    redeem_request_id: &str,
    consume: F,
) -> Result<
    crate::core::usage::quota_fetcher::CodexResetCreditConsumeResult,
    CodexResetCreditsRequestError,
>
where
    F: Fn(String, String) -> Fut,
    Fut: std::future::Future<
        Output = Result<crate::core::usage::quota_fetcher::CodexResetCreditConsumeResult, String>,
    >,
{
    let Some(access_token) = prepared
        .connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
    else {
        return Err(prepared
            .refresh_error
            .clone()
            .map(CodexResetCreditsRequestError::Refresh)
            .unwrap_or(CodexResetCreditsRequestError::MissingAccessToken));
    };

    let result = consume(access_token, redeem_request_id.to_string())
        .await
        .map_err(|_| CodexResetCreditsRequestError::ConsumeTransport)?;
    if !is_auth_expired_consume_result(&result)
        || !is_oauth
        || prepared
            .connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        return Ok(result);
    }

    let refreshed = refresh_oauth_connection(state, &prepared.connection, true)
        .await
        .map_err(CodexResetCreditsRequestError::Refresh)?;
    let access_token = refreshed
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or(CodexResetCreditsRequestError::MissingAccessToken)?;
    consume(access_token, redeem_request_id.to_string())
        .await
        .map_err(|_| CodexResetCreditsRequestError::ConsumeTransport)
}

fn codex_reset_credits_error_response(error: CodexResetCreditsRequestError) -> Response {
    let (status, message, failure_kind) = match error {
        CodexResetCreditsRequestError::MissingAccessToken => (
            axum::http::StatusCode::UNAUTHORIZED,
            "No Codex access token is available. Please re-authorize the connection.",
            "missing_access_token",
        ),
        CodexResetCreditsRequestError::Refresh(error) => {
            let permanent = is_permanent_codex_refresh_rejection(&error);
            let status = if permanent {
                axum::http::StatusCode::UNAUTHORIZED
            } else {
                axum::http::StatusCode::BAD_GATEWAY
            };
            let message = if permanent {
                "The Codex refresh credential was rejected. Please re-authorize the connection."
            } else {
                "Codex OAuth refresh failed. Check the provider OAuth configuration and try again."
            };
            tracing::warn!(
                provider = "codex",
                refresh_status = refresh_error_status(&error),
                refresh_error_code = refresh_error_code(&error).unwrap_or("unknown"),
                permanent,
                "Codex credential refresh failed"
            );
            (status, message, "credential_refresh_failed")
        }
        CodexResetCreditsRequestError::CreditsApi(error) => {
            let unauthorized = error.status == Some(401);
            tracing::warn!(
                provider = "codex",
                upstream_status = error.status,
                upstream_error_code = error.code.as_deref().unwrap_or("unknown"),
                "Codex reset credits GET failed"
            );
            if unauthorized {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    "Codex rejected the access token. Please re-authorize the connection.",
                    "access_token_rejected",
                )
            } else {
                (
                    axum::http::StatusCode::BAD_GATEWAY,
                    "Codex reset credits are temporarily unavailable.",
                    "credits_api_unavailable",
                )
            }
        }
        CodexResetCreditsRequestError::ConsumeTransport => (
            axum::http::StatusCode::BAD_GATEWAY,
            "Codex reset credit result could not be confirmed. Check the credit status before retrying.",
            "consume_request_failed",
        ),
    };
    (
        status,
        Json(json!({ "error": message, "code": failure_kind })),
    )
        .into_response()
}

/// Fetch the OAuth quota, refreshing credentials first if stale/expired and
/// force-retrying once when the quota response reports an auth-expired
/// message. 9router route.js:158-183 parity.
async fn fetch_oauth_quota_with_refresh(
    state: &AppState,
    connection: &ProviderConnection,
) -> Value {
    // 1. Refresh before the fetch when needed.
    let connection = match refresh_oauth_connection(state, connection, false).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "usage oauth refresh failed for {}: {}",
                connection.provider,
                e
            );
            // Keep the stored token (JS returns stale accessToken on failure).
            connection.clone()
        }
    };

    // 2. First fetch.
    let result = fetch_oauth_quota(&connection).await;

    // 3. Force-retry once if the quota response signals auth-expired.
    let msg = result.get("message").and_then(|v| v.as_str()).unwrap_or("");
    if is_auth_expired_message(msg) && connection.refresh_token.is_some() {
        if let Ok(retried_conn) = refresh_oauth_connection(state, &connection, true).await {
            let retry = fetch_oauth_quota(&retried_conn).await;
            if retry
                .get("message")
                .and_then(|v| v.as_str())
                .is_none_or(|m| !is_auth_expired_message(m))
            {
                return retry;
            }
        }
    }

    result
}

fn is_auth_expired_consume_result(
    result: &crate::core::usage::quota_fetcher::CodexResetCreditConsumeResult,
) -> bool {
    result.status == 401
}

async fn clear_local_codex_rate_limit(state: &AppState, connection_id: &str) -> Result<(), String> {
    state
        .db
        .update(|db| {
            if let Some(conn) = db
                .provider_connections
                .iter_mut()
                .find(|entry| entry.id == connection_id)
            {
                conn.rate_limited_until = None;
                conn.consecutive_errors = Some(0);
                conn.backoff_level = Some(0);
                conn.last_error = None;
                conn.last_error_at = None;
                conn.error_code = None;
                conn.extra.insert(
                    "credits_reset_at".to_string(),
                    json!(chrono::Utc::now().to_rfc3339()),
                );
            }
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn consume_result_response(
    result: &crate::core::usage::quota_fetcher::CodexResetCreditConsumeResult,
    redeem_request_id: &str,
) -> Response {
    if result.ok {
        return Json(json!({
            "code": result.code,
            "reset": true,
            "windows_reset": result.windows_reset,
            "redeemRequestId": redeem_request_id,
            "credit": result.raw.get("credit").cloned().unwrap_or(Value::Null),
        }))
        .into_response();
    }

    if result.no_credit {
        return (
            axum::http::StatusCode::CONFLICT,
            Json(json!({
                "code": "no_credit",
                "reset": false,
                "windows_reset": result.windows_reset,
                "message": "No Codex reset credits available.",
            })),
        )
            .into_response();
    }

    if result.status == 401 {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({
                "code": "access_token_rejected",
                "reset": false,
                "windows_reset": result.windows_reset,
                "message": "Codex rejected the access token. Please re-authorize the connection.",
            })),
        )
            .into_response();
    }

    let status = if (400..500).contains(&result.status) {
        axum::http::StatusCode::from_u16(result.status)
            .unwrap_or(axum::http::StatusCode::BAD_GATEWAY)
    } else {
        axum::http::StatusCode::BAD_GATEWAY
    };
    (
        status,
        Json(json!({
            "code": result.code.clone().unwrap_or_else(|| "unknown_response".to_string()),
            "reset": false,
            "windows_reset": result.windows_reset,
            "message": result
                .message
                .clone()
                .unwrap_or_else(|| "Codex reset credit consume returned an unexpected response.".to_string()),
        })),
    )
        .into_response()
}

// Handler for GET /api/usage/:connection_id/codex-reset-credits
async fn get_connection_codex_reset_credits(
    State(state): State<AppState>,
    axum::extract::Path(connection_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(connection) = snapshot
        .provider_connections
        .iter()
        .find(|entry| entry.id == connection_id)
        .cloned()
    else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({ "error": "Connection not found" })),
        )
            .into_response();
    };

    if connection.provider != "codex" {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Codex reset credits are only available for Codex connections."
            })),
        )
            .into_response();
    }

    if !is_codex_reset_auth_type(&connection.auth_type) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Codex reset credits require an OAuth or access-token connection."
            })),
        )
            .into_response();
    }

    let is_oauth = connection.auth_type.eq_ignore_ascii_case("oauth");
    match fetch_codex_reset_credits_with_auth(
        &state,
        &connection,
        is_oauth,
        |token, account_id| async move {
            get_codex_rate_limit_reset_credits(&token, account_id.as_deref()).await
        },
    )
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => codex_reset_credits_error_response(error),
    }
}

// Handler for POST /api/usage/:connection_id/codex-reset-credits
async fn reset_connection_credits(
    State(state): State<AppState>,
    axum::extract::Path(connection_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(connection) = snapshot
        .provider_connections
        .iter()
        .find(|entry| entry.id == connection_id)
        .cloned()
    else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({ "error": "Connection not found" })),
        )
            .into_response();
    };

    if connection.provider != "codex" {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Codex reset credits are only available for Codex connections."
            })),
        )
            .into_response();
    }

    if !is_codex_reset_auth_type(&connection.auth_type) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Codex reset credits require an OAuth or access-token connection."
            })),
        )
            .into_response();
    }

    let is_oauth = connection.auth_type.eq_ignore_ascii_case("oauth");
    let prepared = prepare_codex_connection(&state, &connection, is_oauth).await;
    let access_token = prepared
        .connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Prefer OpenAI consume when we have a token; fall back to local clear only
    // when no token is present (legacy local-only semantics).
    if access_token.is_none() {
        if let Some(error) = prepared.refresh_error.clone() {
            return codex_reset_credits_error_response(CodexResetCreditsRequestError::Refresh(
                error,
            ));
        }
        if let Err(e) = clear_local_codex_rate_limit(&state, &connection_id).await {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            )
                .into_response();
        }
        return Json(json!({
            "code": "local_clear",
            "reset": true,
            "windows_reset": 0,
            "message": "No Codex access token available; cleared local rate-limit/backoff state only.",
            "localOnly": true,
        }))
        .into_response();
    }

    let redeem_request_id = uuid::Uuid::new_v4().to_string();
    let consume_result = match consume_codex_reset_credit_with_auth(
        &state,
        &prepared,
        is_oauth,
        &redeem_request_id,
        |token, request_id| async move {
            consume_codex_rate_limit_reset_credit(&token, &request_id).await
        },
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return codex_reset_credits_error_response(error),
    };

    if consume_result.ok {
        // Secondary: clear local rate-limit / backoff so routing can reuse the account.
        let _ = clear_local_codex_rate_limit(&state, &connection_id).await;
    }

    consume_result_response(&consume_result, &redeem_request_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    static CODEX_REFRESH_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        old_value: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old_value = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, old_value }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.old_value.take() {
                unsafe { std::env::set_var(self.key, value) };
            } else {
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    fn codex_oauth_connection(id: &str, expires_at: &str) -> ProviderConnection {
        ProviderConnection {
            id: id.to_string(),
            provider: "codex".to_string(),
            auth_type: "oauth".to_string(),
            access_token: Some("stored-access-token".to_string()),
            refresh_token: Some("stored-refresh-token".to_string()),
            expires_at: Some(expires_at.to_string()),
            ..Default::default()
        }
    }

    async fn app_state_with_codex_connection(
        connection: ProviderConnection,
    ) -> (AppState, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(
            crate::db::Db::load_from(directory.path())
                .await
                .expect("db"),
        );
        db.update(|state| state.provider_connections.push(connection))
            .await
            .expect("seed codex connection");
        (AppState::new(db), directory)
    }

    fn expired_credits_error() -> crate::core::usage::quota_fetcher::CodexResetCreditsFetchError {
        crate::core::usage::quota_fetcher::CodexResetCreditsFetchError {
            status: Some(401),
            code: Some("unauthorized".to_string()),
        }
    }

    #[test]
    fn usage_routes_are_defined() {
        let _app = routes();
    }

    #[test]
    fn usage_apikey_providers_are_explicit() {
        for provider in [
            "glm",
            "glm-cn",
            "minimax",
            "kimi",
            "deepseek",
            "opencode-go",
            "ollama",
            "vercel-ai-gateway",
            "codebuddy-cn",
            "codebuddy-intl",
        ] {
            assert!(is_usage_apikey_provider(provider));
        }
        assert!(!is_usage_apikey_provider("openai"));
    }

    #[test]
    fn auth_expiry_patterns_match_provider_errors() {
        assert!(is_auth_expired_message("401 Unauthorized"));
        assert!(is_auth_expired_message("Token expired"));
        assert!(!is_auth_expired_message("ok"));
    }

    #[tokio::test]
    async fn reset_credit_get_uses_stored_access_token_after_proactive_refresh_fails() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
        let refresh_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": { "code": "refresh_token_reused" }
            })))
            .expect(1)
            .mount(&refresh_server)
            .await;
        let _token_url = EnvVarGuard::set(
            "OPENPROXY_CODEX_TOKEN_URL",
            &format!("{}/oauth/token", refresh_server.uri()),
        );

        let mut connection = codex_oauth_connection("read-fallback", "2020-01-01T00:00:00Z");
        connection
            .provider_specific_data
            .insert("lastRefreshAt".into(), json!("2020-01-01T00:00:00Z"));
        let (state, _directory) = app_state_with_codex_connection(connection.clone()).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_tokens = Arc::new(Mutex::new(Vec::new()));
        let fetch = {
            let calls = Arc::clone(&calls);
            let observed_tokens = Arc::clone(&observed_tokens);
            move |token: String, _account_id: Option<String>| {
                let calls = Arc::clone(&calls);
                let observed_tokens = Arc::clone(&observed_tokens);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    observed_tokens.lock().unwrap().push(token);
                    Ok(json!({ "availableCount": 2, "credits": [] }))
                }
            }
        };

        let result = fetch_codex_reset_credits_with_auth(&state, &connection, true, fetch)
            .await
            .expect("a valid stored access token should still be tried");

        assert_eq!(result["availableCount"], 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            observed_tokens.lock().unwrap().as_slice(),
            ["stored-access-token"]
        );
        refresh_server.verify().await;
    }

    #[tokio::test]
    async fn reset_credit_get_refreshes_once_only_after_upstream_401() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
        let refresh_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_json(json!({
                "grant_type": "refresh_token",
                "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",
                "refresh_token": "stored-refresh-token"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "refreshed-access-token",
                "refresh_token": "rotated-refresh-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&refresh_server)
            .await;
        let _token_url = EnvVarGuard::set(
            "OPENPROXY_CODEX_TOKEN_URL",
            &format!("{}/oauth/token", refresh_server.uri()),
        );

        let connection = codex_oauth_connection("read-retry", "2099-01-01T00:00:00Z");
        let (state, _directory) = app_state_with_codex_connection(connection.clone()).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_tokens = Arc::new(Mutex::new(Vec::new()));
        let fetch = {
            let calls = Arc::clone(&calls);
            let observed_tokens = Arc::clone(&observed_tokens);
            move |token: String, _account_id: Option<String>| {
                let calls = Arc::clone(&calls);
                let observed_tokens = Arc::clone(&observed_tokens);
                async move {
                    observed_tokens.lock().unwrap().push(token);
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(expired_credits_error())
                    } else {
                        Ok(json!({ "availableCount": 1, "credits": [] }))
                    }
                }
            }
        };

        let result = fetch_codex_reset_credits_with_auth(&state, &connection, true, fetch)
            .await
            .expect("request should succeed after one auth refresh");

        assert_eq!(result["availableCount"], 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            observed_tokens.lock().unwrap().as_slice(),
            ["stored-access-token", "refreshed-access-token"]
        );
        let snapshot = state.db.snapshot();
        let saved = snapshot
            .provider_connections
            .iter()
            .find(|candidate| candidate.id == "read-retry")
            .expect("connection remains configured");
        assert_eq!(
            saved.refresh_token.as_deref(),
            Some("rotated-refresh-token")
        );
        refresh_server.verify().await;
    }

    #[tokio::test]
    async fn reset_credit_get_does_not_refresh_for_non_401_upstream_errors() {
        let connection = codex_oauth_connection("read-no-retry", "2099-01-01T00:00:00Z");
        let (state, _directory) = app_state_with_codex_connection(connection.clone()).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let fetch = {
            let calls = Arc::clone(&calls);
            move |_token: String, _account_id: Option<String>| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(
                        crate::core::usage::quota_fetcher::CodexResetCreditsFetchError {
                            status: Some(503),
                            code: Some("upstream_unavailable".to_string()),
                        },
                    )
                }
            }
        };

        let error = fetch_codex_reset_credits_with_auth(&state, &connection, true, fetch)
            .await
            .expect_err("non-auth upstream response remains a failure");

        assert!(matches!(
            error,
            CodexResetCreditsRequestError::CreditsApi(
                crate::core::usage::quota_fetcher::CodexResetCreditsFetchError {
                    status: Some(503),
                    ..
                }
            )
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reset_credit_post_reuses_redemption_id_for_the_single_401_retry() {
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
        let refresh_server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "refreshed-access-token",
                "refresh_token": "rotated-refresh-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&refresh_server)
            .await;
        let _token_url = EnvVarGuard::set(
            "OPENPROXY_CODEX_TOKEN_URL",
            &format!("{}/oauth/token", refresh_server.uri()),
        );

        let connection = codex_oauth_connection("post-retry", "2099-01-01T00:00:00Z");
        let (state, _directory) = app_state_with_codex_connection(connection.clone()).await;
        let prepared = prepare_codex_connection(&state, &connection, true).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let redeem_request_id = "same-logical-redemption";
        let consume = {
            let calls = Arc::clone(&calls);
            let observed = Arc::clone(&observed);
            move |token: String, request_id: String| {
                let calls = Arc::clone(&calls);
                let observed = Arc::clone(&observed);
                async move {
                    observed.lock().unwrap().push((token, request_id));
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(
                            crate::core::usage::quota_fetcher::CodexResetCreditConsumeResult {
                                ok: false,
                                no_credit: false,
                                status: 401,
                                code: Some("unauthorized".to_string()),
                                windows_reset: 0.0,
                                message: None,
                                raw: Value::Null,
                            },
                        )
                    } else {
                        Ok(
                            crate::core::usage::quota_fetcher::CodexResetCreditConsumeResult {
                                ok: true,
                                no_credit: false,
                                status: 200,
                                code: Some("reset".to_string()),
                                windows_reset: 1.0,
                                message: None,
                                raw: Value::Null,
                            },
                        )
                    }
                }
            }
        };

        let result = consume_codex_reset_credit_with_auth(
            &state,
            &prepared,
            true,
            redeem_request_id,
            consume,
        )
        .await
        .expect("credit consume should succeed after one auth retry");

        assert!(result.ok);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            observed.lock().unwrap().as_slice(),
            [
                (
                    "stored-access-token".to_string(),
                    redeem_request_id.to_string()
                ),
                (
                    "refreshed-access-token".to_string(),
                    redeem_request_id.to_string()
                )
            ]
        );
        refresh_server.verify().await;
    }

    #[tokio::test]
    async fn reset_credit_post_does_not_retry_ambiguous_consume_failure() {
        let connection = codex_oauth_connection("post-no-retry", "2099-01-01T00:00:00Z");
        let (state, _directory) = app_state_with_codex_connection(connection.clone()).await;
        let prepared = prepare_codex_connection(&state, &connection, true).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let consume = {
            let calls = Arc::clone(&calls);
            move |_token: String, _request_id: String| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err("network outcome is ambiguous".to_string())
                }
            }
        };

        let error = consume_codex_reset_credit_with_auth(
            &state,
            &prepared,
            true,
            "one-attempt-only",
            consume,
        )
        .await
        .expect_err("network error must not trigger an automatic retry");

        assert!(matches!(
            error,
            CodexResetCreditsRequestError::ConsumeTransport
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn refresh_failure_response_does_not_echo_upstream_details() {
        let response = codex_reset_credits_error_response(CodexResetCreditsRequestError::Refresh(
            "Refresh request returned HTTP 401: refresh_token_reused".to_string(),
        ));
        let (status, body) = response_json(response).await;

        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "credential_refresh_failed");
        assert!(body["error"].as_str().unwrap().contains("re-authorize"));
        assert!(!body.to_string().contains("refresh_token_reused"));
    }

    #[tokio::test]
    async fn oauth_client_rejection_is_not_misreported_as_user_reauthorization() {
        let response = codex_reset_credits_error_response(CodexResetCreditsRequestError::Refresh(
            "Refresh request returned HTTP 401: invalid_client".to_string(),
        ));
        let (status, body) = response_json(response).await;

        assert_eq!(status, axum::http::StatusCode::BAD_GATEWAY);
        assert_eq!(body["code"], "credential_refresh_failed");
        assert!(body["error"].as_str().unwrap().contains("configuration"));
    }

    async fn response_json(response: Response) -> (axum::http::StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        (
            status,
            serde_json::from_slice(&bytes).expect("JSON response"),
        )
    }
}
