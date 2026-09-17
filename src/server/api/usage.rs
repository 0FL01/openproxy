use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing, Json, Router};
use chrono::{Duration as ChronoDuration, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use crate::core::usage::quota_fetcher::{
    codex_account_id, consume_codex_rate_limit_reset_credit, fetch_antigravity_quota,
    fetch_claude_quota, fetch_codebuddy_quota, fetch_codex_quota, fetch_deepseek_usage,
    fetch_github_quota, fetch_glm_quota, fetch_grok_cli_quota, fetch_kimi_oauth_usage,
    fetch_kimi_usage, fetch_kiro_quota, fetch_minimax_quota, fetch_ollama_quota,
    fetch_opencode_go_quota, fetch_vercel_ai_gateway_quota, get_codex_rate_limit_reset_credits,
};
use crate::oauth::token_refresh::{dispatch_oauth_refresh, refresh_codex_token};
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
            | "kiro"
            | "ollama"
            | "vercel-ai-gateway"
            | "codebuddy-cn"
            | "codebuddy-intl"
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
        "kiro" => fetch_kiro_quota(token, provider, psd).await,
        "antigravity" => fetch_antigravity_quota(token, provider).await,
        "grok-cli" => fetch_grok_cli_quota(token).await,
        "ollama" => fetch_ollama_quota(token).await,
        // Kimi OAuth connections hit /v1/usages with Bearer + X-Msh-* headers.
        "kimi" | "kimi-coding" => fetch_kimi_oauth_usage(token, psd).await,
        _ => serde_json::json!({}),
    }
}

fn usage_message_for_provider(provider: &str) -> String {
    match provider {
        "qwen" => "Qwen connected. Usage tracked per request.".to_string(),
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
}

async fn get_connection_usage(
    State(state): State<AppState>,
    axum::extract::Path(connection_id): axum::extract::Path<String>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    // ?force=1 bypasses the in-memory quota cache (9router v0.5.55 parity).
    let force = params.get("force").map(|v| v.as_str()) == Some("1");

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
    // 9router route.js:135-136: Kiro's headless api-key flow persists
    // authType "api_key" (underscore) while generic apikey providers persist
    // "apikey" — accept both spellings.
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
    if is_apikey_eligible {
        if let Some(api_key) = connection
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let provider = connection.provider.clone();
            let psd = connection.provider_specific_data.clone();
            let result = match provider.as_str() {
                "glm" | "glm-cn" => fetch_glm_quota(api_key, &provider).await,
                "minimax" => fetch_minimax_quota(api_key, &provider).await,
                "kimi" => fetch_kimi_usage(api_key).await,
                "deepseek" => fetch_deepseek_usage(api_key).await,
                "opencode-go" => fetch_opencode_go_quota(api_key).await,
                "kiro" => fetch_kiro_quota(api_key, &provider, &psd).await,
                "vercel-ai-gateway" => fetch_vercel_ai_gateway_quota(api_key).await,
                "codebuddy-cn" | "codebuddy-intl" => {
                    fetch_codebuddy_quota(api_key, &provider).await
                }
                // Ollama has no live API-key quota fetcher yet.
                _ => serde_json::json!({}),
            };
            if let Some(quotas) = result.get("quotas") {
                live_quotas = quotas.clone();
            }
            if let Some(msg) = result.get("message").and_then(|v| v.as_str()) {
                live_message = Some(msg.to_string());
            }
        }
    }

    let mut live_plan: Option<Value> = None;
    let mut live_reset_credits: Option<Value> = None;
    if is_oauth {
        // 9router route.js:158-183 — refresh credentials before the quota
        // call and force-retry once on an auth-expired message.
        let result = fetch_oauth_quota_with_refresh(connection).await;
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

/// Refresh an OAuth connection's tokens via the provider's refresh flow and
/// return a cloned connection with the refreshed credentials. 9router
/// `refreshAndUpdateCredentials` parity (route.js:23-117). Returns the
/// original connection untouched on refresh failure (JS keeps the stale
/// accessToken when one exists).
async fn refresh_oauth_connection(
    connection: &ProviderConnection,
    force: bool,
) -> Result<ProviderConnection, String> {
    let Some(refresh_token) = connection
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(connection.clone());
    };

    // JS executor.needsRefresh(credentials): refresh when expired or missing.
    let needs_refresh = force
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
        };
    if !needs_refresh {
        return Ok(connection.clone());
    }

    let provider = connection.provider.clone();
    let psd = connection.provider_specific_data.clone();
    let result = dispatch_oauth_refresh(&provider, refresh_token, &psd).await?;

    let mut updated = connection.clone();
    updated.access_token = Some(result.access_token);
    if let Some(new_refresh) = result.refresh_token {
        updated.refresh_token = Some(new_refresh);
    }
    if let Some(expires_in) = result.expires_in {
        let expiry = Utc::now() + ChronoDuration::seconds(expires_in);
        updated.expires_at = Some(expiry.to_rfc3339());
    }
    Ok(updated)
}

/// Fetch the OAuth quota, refreshing credentials first if stale/expired and
/// force-retrying once when the quota response reports an auth-expired
/// message. 9router route.js:158-183 parity.
async fn fetch_oauth_quota_with_refresh(connection: &ProviderConnection) -> Value {
    // 1. Refresh before the fetch when needed.
    let connection = match refresh_oauth_connection(connection, false).await {
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
        if let Ok(retried_conn) = refresh_oauth_connection(&connection, true).await {
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
    let mut values = Vec::new();
    if let Some(m) = &result.message {
        values.push(m.clone());
    }
    if let Some(c) = &result.code {
        values.push(c.clone());
    }
    if let Some(d) = result.raw.get("detail").and_then(|v| v.as_str()) {
        values.push(d.to_string());
    }
    if let Some(e) = result.raw.get("error").and_then(|v| v.as_str()) {
        values.push(e.to_string());
    }
    if result.status == 401 {
        values.push("401".to_string());
    }
    values.iter().any(|v| is_auth_expired_message(v))
}

async fn persist_codex_tokens(
    state: &AppState,
    connection_id: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_in: Option<i64>,
) -> Result<(), String> {
    state
        .db
        .update(|db| {
            if let Some(conn) = db
                .provider_connections
                .iter_mut()
                .find(|entry| entry.id == connection_id)
            {
                conn.access_token = Some(access_token.to_string());
                if let Some(rt) = refresh_token.map(str::trim).filter(|s| !s.is_empty()) {
                    conn.refresh_token = Some(rt.to_string());
                }
                if let Some(secs) = expires_in {
                    conn.expires_in = Some(secs);
                    conn.expires_at = Some(
                        (chrono::Utc::now() + ChronoDuration::seconds(secs.max(0))).to_rfc3339(),
                    );
                }
                conn.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
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
    let Some(mut connection) = snapshot
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
    if is_oauth {
        if let Some(refresh_token) = connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match refresh_codex_token(refresh_token).await {
                Ok(refreshed) => {
                    if let Err(e) = persist_codex_tokens(
                        &state,
                        &connection_id,
                        &refreshed.access_token,
                        refreshed.refresh_token.as_deref(),
                        refreshed.expires_in,
                    )
                    .await
                    {
                        return (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": e })),
                        )
                            .into_response();
                    }
                    connection.access_token = Some(refreshed.access_token);
                    if let Some(rt) = refreshed.refresh_token {
                        connection.refresh_token = Some(rt);
                    }
                }
                Err(e) => {
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(json!({ "error": format!("Credential refresh failed: {e}") })),
                    )
                        .into_response();
                }
            }
        }
    }

    let account_id = codex_account_id(&connection.provider_specific_data);
    let access_token = connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(token) = access_token else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "No Codex access token available. Please re-authorize the connection."
            })),
        )
            .into_response();
    };

    let mut result = get_codex_rate_limit_reset_credits(token, account_id.as_deref()).await;
    if let Err(err) = &result {
        if is_oauth
            && is_auth_expired_message(err)
            && connection
                .refresh_token
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
        {
            if let Some(refresh_token) = connection.refresh_token.clone() {
                if let Ok(refreshed) = refresh_codex_token(&refresh_token).await {
                    let _ = persist_codex_tokens(
                        &state,
                        &connection_id,
                        &refreshed.access_token,
                        refreshed.refresh_token.as_deref(),
                        refreshed.expires_in,
                    )
                    .await;
                    result = get_codex_rate_limit_reset_credits(
                        &refreshed.access_token,
                        account_id.as_deref(),
                    )
                    .await;
                }
            }
        }
    }

    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => {
            tracing::warn!(provider = "codex", error = %e, "Codex reset credits GET failed");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            )
                .into_response()
        }
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
    let Some(mut connection) = snapshot
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
    if is_oauth {
        if let Some(refresh_token) = connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match refresh_codex_token(refresh_token).await {
                Ok(refreshed) => {
                    if let Err(e) = persist_codex_tokens(
                        &state,
                        &connection_id,
                        &refreshed.access_token,
                        refreshed.refresh_token.as_deref(),
                        refreshed.expires_in,
                    )
                    .await
                    {
                        return (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": e })),
                        )
                            .into_response();
                    }
                    connection.access_token = Some(refreshed.access_token);
                    if let Some(rt) = refreshed.refresh_token {
                        connection.refresh_token = Some(rt);
                    }
                }
                Err(e) => {
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(json!({ "error": format!("Credential refresh failed: {e}") })),
                    )
                        .into_response();
                }
            }
        }
    }

    let access_token = connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Prefer OpenAI consume when we have a token; fall back to local clear only
    // when no token is present (legacy local-only semantics).
    let Some(token) = access_token else {
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
    };

    let redeem_request_id = uuid::Uuid::new_v4().to_string();
    let mut consume_result =
        match consume_codex_rate_limit_reset_credit(&token, &redeem_request_id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(provider = "codex", error = %e, "Codex reset credits POST failed");
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e })),
                )
                    .into_response();
            }
        };

    if is_oauth
        && is_auth_expired_consume_result(&consume_result)
        && connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty())
    {
        if let Some(refresh_token) = connection.refresh_token.clone() {
            match refresh_codex_token(&refresh_token).await {
                Ok(refreshed) => {
                    let _ = persist_codex_tokens(
                        &state,
                        &connection_id,
                        &refreshed.access_token,
                        refreshed.refresh_token.as_deref(),
                        refreshed.expires_in,
                    )
                    .await;
                    if let Ok(retry) = consume_codex_rate_limit_reset_credit(
                        &refreshed.access_token,
                        &redeem_request_id,
                    )
                    .await
                    {
                        consume_result = retry;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        provider = "codex",
                        error = %e,
                        "Codex reset credits force refresh failed"
                    );
                }
            }
        }
    }

    if consume_result.ok {
        // Secondary: clear local rate-limit / backoff so routing can reuse the account.
        let _ = clear_local_codex_rate_limit(&state, &connection_id).await;
    }

    consume_result_response(&consume_result, &redeem_request_id)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "kiro",
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
}
