//! Authenticated, provider-specific transport for the native Claude runtime.
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::core::model::get_model_info;
use crate::server::auth::{self, require_api_key_with_reload};
use crate::server::claude_agent::{BridgeEvent, NativeError, PROVIDER};
use crate::server::state::AppState;
use crate::types::ApiKey;

pub(super) fn generation_routes(state: AppState) -> Router<AppState> {
    let generation = Router::new()
        .route("/v2/claude-agent/stream", post(stream))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            super::admission::admit_llm_request,
        ))
        .layer(DefaultBodyLimit::max(super::LLM_BODY_LIMIT_BYTES));
    Router::new()
        .merge(generation)
        .merge(
            Router::new()
                .route("/v2/claude-agent/idle", post(idle))
                .layer(DefaultBodyLimit::max(4096)),
        )
        .route_layer(middleware::from_fn_with_state(state, authenticate))
}

pub(super) fn login_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/claude-agent/login", post(start_login))
        .route(
            "/api/claude-agent/login/{id}",
            get(login_status).delete(cancel_login),
        )
        .route("/api/claude-agent/login/{id}/input", post(login_input))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .route_layer(middleware::from_fn_with_state(state, authenticate_login))
}

async fn authenticate(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    if request.uri().query().is_some() {
        return error_response(NativeError::new(
            "invalid_request",
            "Native transport does not accept query credentials or parameters",
        ));
    }
    match require_api_key_with_reload(request.headers(), &state.db).await {
        Ok(key) => {
            request.extensions_mut().insert(key);
            next.run(request).await
        }
        Err(error) => super::auth_error_response(error),
    }
}

#[derive(Clone)]
struct LoginOwner(String);

async fn authenticate_login(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.uri().query().is_some() || !same_origin(request.headers(), request.uri()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Native login requires a same-origin dashboard request"})),
        )
            .into_response();
    }
    let owner = if auth::extract_api_key(request.headers()).is_some() {
        match require_api_key_with_reload(request.headers(), &state.db).await {
            Ok(key) => format!("key:{}", key.id),
            Err(error) => return super::auth_error_response(error),
        }
    } else {
        // Anonymous dashboard mode is not authority to remotely drive a native
        // credential process. An actual dashboard session or key is required.
        if !state.db.snapshot().settings.require_login {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Enable dashboard login before signing in to native Claude Code"}))).into_response();
        }
        if let Err(error) = auth::require_dashboard_session(request.headers(), &state.db) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error":error.message()})),
            )
                .into_response();
        }
        let Some(token) = auth::extract_auth_token(request.headers()) else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        format!(
            "dashboard:{}",
            digest(auth::jwt_secret().as_bytes(), &[&token])
        )
    };
    request.extensions_mut().insert(LoginOwner(owner));
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

fn same_origin(headers: &HeaderMap, uri: &axum::http::Uri) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value != "same-origin" && value != "none")
    {
        return false;
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(origin) = origin
        .to_str()
        .ok()
        .and_then(|value| url::Url::parse(value).ok())
    else {
        return false;
    };
    if !matches!(origin.scheme(), "http" | "https")
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return false;
    }
    let trusted = matches!(
        std::env::var("TRUST_PROXY").as_deref(),
        Ok("true" | "1" | "yes")
    );
    let host = trusted
        .then(|| headers.get("x-forwarded-host"))
        .flatten()
        .or_else(|| headers.get(header::HOST))
        .and_then(|value| value.to_str().ok())
        .or_else(|| uri.authority().map(|authority| authority.as_str()));
    let scheme = trusted
        .then(|| headers.get("x-forwarded-proto"))
        .flatten()
        .and_then(|value| value.to_str().ok())
        .or_else(|| uri.scheme_str())
        .unwrap_or("http");
    if !matches!(scheme, "http" | "https") {
        return false;
    }
    let Some(expected) = host.and_then(|host| url::Url::parse(&format!("{scheme}://{host}")).ok())
    else {
        return false;
    };
    origin.origin() == expected.origin()
}

fn digest(secret: &[u8], fields: &[&str]) -> String {
    let mut hmac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts arbitrary keys");
    hmac.update(b"openproxy/claude-agent/v2\0");
    for field in fields {
        hmac.update(&(field.len() as u64).to_be_bytes());
        hmac.update(field.as_bytes());
    }
    hex::encode(hmac.finalize().into_bytes())
}

fn scoped_id(key: &ApiKey, id: &str) -> Result<String, NativeError> {
    if id.is_empty()
        || id.len() > 256
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b',')
    {
        return Err(NativeError::new(
            "invalid_request",
            "Invalid native client session ID",
        ));
    }
    Ok(digest(key.key.as_bytes(), &[&key.id, id]))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NativeRequest {
    model: String,
    kind: Kind,
    #[serde(rename = "sessionID", skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(rename = "sourceID", skip_serializing_if = "Option::is_none")]
    source_id: Option<String>,
    system: NativeSystem,
    prompt: Vec<Value>,
    #[serde(default)]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Primary,
    Title,
    Generate,
    Compaction,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NativeSystem {
    #[serde(default)]
    agent: String,
    #[serde(default)]
    instructions: String,
}

async fn stream(
    State(state): State<AppState>,
    Extension(key): Extension<ApiKey>,
    Json(mut request): Json<NativeRequest>,
) -> Response {
    let snapshot = state.db.snapshot();
    let mut model = get_model_info(&request.model, &snapshot);
    let Some(connection) = snapshot.provider_connections.iter().find(|connection| {
        connection.provider == PROVIDER
            && connection.auth_type == "external"
            && connection.is_active()
    }) else {
        return error_response(NativeError::new(
            "claude_agent_needs_auth",
            "Connect your Claude subscription through native dashboard login first",
        ));
    };
    let output_alias = super::v1_models::output_alias(
        crate::core::model::catalog::provider_catalog(),
        connection,
        PROVIDER,
        PROVIDER,
    );
    if model.provider.as_deref() != Some(PROVIDER) {
        if let Some((prefix, native_model)) = request
            .model
            .split_once('/')
            .filter(|(prefix, _)| *prefix == output_alias)
        {
            model.provider = Some(PROVIDER.into());
            model.model = native_model.into();
        } else {
            return error_response(NativeError::new(
                "invalid_request",
                "This endpoint only accepts configured Claude Agent models",
            ));
        }
    }
    if model.model.trim().is_empty() {
        return error_response(NativeError::new(
            "invalid_request",
            "A native Claude model ID is required",
        ));
    }
    let disabled = super::models_disabled::disabled_models_from_db(&snapshot);
    let alias = request
        .model
        .split_once('/')
        .map(|(alias, _)| alias)
        .unwrap_or(PROVIDER);
    if [PROVIDER, alias].iter().any(|alias| {
        disabled
            .get(*alias)
            .is_some_and(|ids| ids.contains(&model.model))
    }) {
        return error_response(NativeError::new(
            "invalid_request",
            "This Claude Agent model is disabled",
        ));
    }
    let custom = snapshot.custom_models.iter().any(|custom| {
        custom.id == model.model
            && [PROVIDER, output_alias.as_str()].contains(&custom.provider_alias.as_str())
    });
    if !custom
        && connection
            .provider_specific_data
            .get("enabledModels")
            .and_then(Value::as_array)
            .filter(|models| !models.is_empty())
            .is_some_and(|models| {
                !models.iter().any(|entry| {
                    entry.as_str().is_some_and(|id| {
                        id == model.model
                            || id == format!("{output_alias}/{}", model.model)
                            || id == format!("{PROVIDER}/{}", model.model)
                    })
                })
            })
    {
        return error_response(NativeError::new(
            "invalid_request",
            "This Claude Agent model is not enabled",
        ));
    }
    if state.claude_agent.login_active().await {
        return error_response(NativeError::new(
            "claude_agent_login_active",
            "Wait for native Claude login to finish",
        ));
    }
    if request.kind == Kind::Primary && request.session_id.is_none() {
        return error_response(NativeError::new(
            "invalid_request",
            "Primary native requests require a sessionID",
        ));
    }
    for id in [&mut request.session_id, &mut request.source_id]
        .into_iter()
        .flatten()
    {
        match scoped_id(&key, id) {
            Ok(scoped) => *id = scoped,
            Err(error) => return error_response(error),
        }
    }
    request.model = model.model;
    let mut call = match state
        .claude_agent
        .call(json!({"op":"run","request":request}))
        .await
    {
        Ok(call) => call,
        Err(error) => return error_response(error),
    };
    let first = match tokio::time::timeout(Duration::from_secs(30), call.recv()).await {
        Ok(Some(BridgeEvent::Part(part))) => part,
        Ok(Some(BridgeEvent::Error(error))) => return error_response(error),
        _ => {
            return error_response(NativeError::new(
                "claude_agent_start_timeout",
                "Claude Agent did not start its response",
            ))
        }
    };
    let body = async_stream::stream! {
        yield Ok::<_, std::io::Error>(ndjson(&first));
        while let Some(event) = call.recv().await {
            match event {
                BridgeEvent::Part(part) => {
                    state.claude_agent.observe_inventory(&part).await;
                    yield Ok(ndjson(&part));
                },
                BridgeEvent::Error(error) => {
                    yield Ok(ndjson(&json!({"type":"error","error":{"message":error.message,"code":error.code}})));
                    break;
                }
                BridgeEvent::Done => break,
                BridgeEvent::Result(_) => break,
            }
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(body))
        .unwrap()
}

fn ndjson(value: &Value) -> bytes::Bytes {
    let mut bytes = serde_json::to_vec(value).expect("JSON Value serializes");
    bytes.push(b'\n');
    bytes.into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdleRequest {
    #[serde(rename = "sessionID")]
    session_id: String,
}

async fn idle(
    State(state): State<AppState>,
    Extension(key): Extension<ApiKey>,
    Json(request): Json<IdleRequest>,
) -> Response {
    let id = match scoped_id(&key, &request.session_id) {
        Ok(id) => id,
        Err(error) => return error_response(error),
    };
    match state
        .claude_agent
        .call(json!({"op":"idle","sessionID":id}))
        .await
    {
        Ok(mut call) => {
            while let Some(event) = call.recv().await {
                match event {
                    BridgeEvent::Done => return StatusCode::NO_CONTENT.into_response(),
                    BridgeEvent::Error(error) => return error_response(error),
                    _ => {}
                }
            }
            error_response(NativeError::new(
                "claude_agent_unavailable",
                "Native runtime disconnected",
            ))
        }
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartLogin {
    name: Option<String>,
}

async fn start_login(
    State(state): State<AppState>,
    Extension(owner): Extension<LoginOwner>,
    Json(request): Json<StartLogin>,
) -> Response {
    let name = request.name.as_deref().unwrap_or("Claude Agent").trim();
    if name.is_empty() || name.len() > 256 {
        return error_response(NativeError::new(
            "invalid_request",
            "Connection name must contain 1–256 bytes",
        ));
    }
    match state
        .claude_agent
        .start_login(owner.0, name.to_string())
        .await
    {
        Ok(id) => Json(json!({"attemptId":id})).into_response(),
        Err(error) => error_response(error),
    }
}

async fn login_status(
    State(state): State<AppState>,
    Extension(owner): Extension<LoginOwner>,
    Path(id): Path<String>,
) -> Response {
    match state.claude_agent.login_status(&owner.0, &id).await {
        Ok(status) => Json(status).into_response(),
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginInput {
    input: String,
}

async fn login_input(
    State(state): State<AppState>,
    Extension(owner): Extension<LoginOwner>,
    Path(id): Path<String>,
    Json(request): Json<LoginInput>,
) -> Response {
    match state
        .claude_agent
        .login_input(&owner.0, &id, &request.input)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => error_response(error),
    }
}

async fn cancel_login(
    State(state): State<AppState>,
    Extension(owner): Extension<LoginOwner>,
    Path(id): Path<String>,
) -> Response {
    match state.claude_agent.cancel_login(&owner.0, &id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => error_response(error),
    }
}

fn error_response(error: NativeError) -> Response {
    let status = match error.code.as_str() {
        "invalid_request" | "INVALID_REQUEST" | "INVALID_COMMAND" => StatusCode::BAD_REQUEST,
        "claude_agent_needs_auth" => StatusCode::UNAUTHORIZED,
        "claude_agent_login_not_found" => StatusCode::NOT_FOUND,
        "claude_agent_login_active" | "claude_agent_login_finished" => StatusCode::CONFLICT,
        "claude_agent_capacity" | "BUSY" => StatusCode::TOO_MANY_REQUESTS,
        "claude_agent_body_too_large" | "RESOURCE_LIMIT" => StatusCode::PAYLOAD_TOO_LARGE,
        "SHUTDOWN" | "CONFIGURATION" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    };
    (status, Json(json!({"error":{"message":error.message,"code":error.code,"type":"claude_agent_error"}}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_login_origin_uses_request_scheme_and_http2_authority() {
        let uri: axum::http::Uri = "http://proxy.invalid:4625/api/claude-agent/login"
            .parse()
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://proxy.invalid:4625".parse().unwrap());
        assert!(same_origin(&headers, &uri));
        headers.insert(
            header::ORIGIN,
            "https://proxy.invalid:4625".parse().unwrap(),
        );
        assert!(!same_origin(&headers, &uri));
    }
}
