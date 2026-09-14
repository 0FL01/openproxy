use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use bcrypt::{hash, verify, DEFAULT_COST};
use jsonwebtoken::{encode, EncodingKey, Header as JwtHeader};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::server::auth::login_limiter::LockoutError;
use crate::server::auth::{
    increment_token_epoch, jwt_secret, require_api_key, require_api_key_with_reload, revoke_jti,
};

use crate::server::state::AppState;
use crate::types::Settings;

#[derive(Debug, Deserialize)]
pub struct PasswordLoginRequest {
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AuthTokenClaims {
    authenticated: bool,
    exp: usize,
    /// JWT ID — unique per-token identifier for revocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jti: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub api_key_id: String,
    pub created_at: i64,
    pub last_active: i64,
    pub is_valid: bool,
}

#[derive(Debug, Deserialize)]
pub struct LogoutRequest {
    pub session_id: Option<String>,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// POST /api/auth/login
/// Creates a JWT cookie for browser dashboard auth.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PasswordLoginRequest>,
) -> Response {
    let snapshot = state.db.snapshot();
    if is_tunnel_request(&headers, &snapshot.settings) && !snapshot.settings.tunnel_dashboard_access
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Dashboard access via tunnel is disabled" })),
        )
            .into_response();
    }

    let client_ip = client_ip_from_headers(&headers);

    // Reserve the attempt slot before checking the password so an attacker
    // cannot bypass the limit by timing requests around bcrypt. A successful
    // password check immediately resets the counter via the second call below.
    if let Err(LockoutError::Locked { retry_after_secs }) =
        state.login_limiter.check_and_record(client_ip, false).await
    {
        return lockout_response(retry_after_secs);
    }

    let provided_password = req.password;
    let valid = match settings_password_hash(&snapshot.settings) {
        Some(hash) => verify(&provided_password, hash).unwrap_or(false),
        None => crate::core::auth::timing_safe_eq(
            &provided_password,
            &crate::core::auth::dashboard_initial_password(),
        ),
    };

    if !valid {
        if let Err(LockoutError::Locked { retry_after_secs }) =
            state.login_limiter.check_and_record(client_ip, false).await
        {
            return lockout_response(retry_after_secs);
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "Invalid password",
                "resetHint": "Forgot password? Run `openproxy auth reset-password` on the host to restore the generated initial password.",
            })),
        )
            .into_response();
    }

    let _ = state.login_limiter.check_and_record(client_ip, true).await;

    let expires_at = now_secs() + 86400;
    let jti = crate::server::auth::generate_jti();
    let token = match encode(
        &JwtHeader::default(),
        &AuthTokenClaims {
            authenticated: true,
            exp: expires_at as usize,
            jti: Some(jti),
        },
        &EncodingKey::from_secret(jwt_secret().as_bytes()),
    ) {
        Ok(token) => token,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to issue auth token: {error}") })),
            )
                .into_response();
        }
    };

    let secure_cookie = std::env::var("AUTH_COOKIE_SECURE").ok().as_deref() == Some("true")
        || headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.eq_ignore_ascii_case("https"))
            .unwrap_or(false);

    // Force a password change when the default password is still in use and the
    // client is remote (keeps local UX intact; mirrors 9router).
    let has_stored_hash = settings_password_hash(&snapshot.settings).is_some();
    let must_change_password =
        !has_stored_hash && std::env::var("INITIAL_PASSWORD").is_err() && !client_ip.is_loopback();

    let mut response = Json(json!({
        "success": true,
        "mustChangePassword": must_change_password,
    }))
    .into_response();
    let cookie = build_auth_cookie(&token, 86400, secure_cookie);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

/// GET /api/auth/status — Check if the browser has a dashboard session and
/// return the password-login state.
pub async fn auth_status(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let logged_in = crate::server::auth::require_dashboard_session(&headers, &state.db).is_ok();
    let snapshot = state.db.snapshot();
    let settings = &snapshot.settings;
    let has_password = settings_password_hash(settings).is_some();

    Json(json!({
        "authenticated": logged_in,
        "requireLogin": settings.require_login,
        "hasPassword": has_password,
    }))
    .into_response()
}

/// POST /api/auth/logout
/// Invalidates the current session
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LogoutRequest>,
) -> Response {
    if let Some(token) = crate::server::auth::extract_auth_token(&headers) {
        // Revoke by jti if we can decode it.
        if let Ok(decoded) = jsonwebtoken::decode::<AuthTokenClaims>(
            &token,
            &jsonwebtoken::DecodingKey::from_secret(jwt_secret().as_bytes()),
            &jsonwebtoken::Validation::default(),
        ) {
            if let Some(ref jti) = decoded.claims.jti {
                revoke_jti(jti);
            }
        }

        let mut response = Json(json!({
            "success": true,
            "message": "Logged out"
        }))
        .into_response();
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_static("auth_token=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
        );
        return response;
    }

    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let mut sessions = state.sessions.write().await;

    // If session_id provided, remove that specific session
    if let Some(session_id) = req.session_id {
        if let Some(session) = sessions.get(&session_id) {
            if session.api_key_id == api_key.id {
                sessions.remove(&session_id);
                return Json(json!({
                    "success": true,
                    "message": "Session logged out"
                }))
                .into_response();
            } else {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "success": false,
                        "error": "Session belongs to different user"
                    })),
                )
                    .into_response();
            }
        }
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "success": false,
                "error": "Session not found"
            })),
        )
            .into_response();
    }

    // Otherwise, remove all sessions for this API key
    sessions.retain(|_, session| session.api_key_id != api_key.id);

    Json(json!({
        "success": true,
        "message": "All sessions logged out"
    }))
    .into_response()
}

/// GET /api/auth/session/:session_id
/// Get session info
pub async fn get_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let _api_key = match require_api_key_with_reload(&headers, &state.db).await {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let sessions = state.sessions.read().await;

    match sessions.get(&session_id) {
        Some(session) => {
            let now = now_secs();
            let is_valid = now < (session.created_at + 86400);
            Json(SessionResponse {
                session_id: session.session_id.clone(),
                api_key_id: session.api_key_id.clone(),
                created_at: session.created_at,
                last_active: session.last_active,
                is_valid,
            })
            .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "Session not found"
            })),
        )
            .into_response(),
    }
}

/// GET /api/auth/sessions
/// List all sessions for the current API key
pub async fn list_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let sessions = state.sessions.read().await;
    let now = now_secs();

    let session_list: Vec<SessionResponse> = sessions
        .values()
        .filter(|s| s.api_key_id == api_key.id)
        .map(|session| {
            let is_valid = now < (session.created_at + 86400);
            SessionResponse {
                session_id: session.session_id.clone(),
                api_key_id: session.api_key_id.clone(),
                created_at: session.created_at,
                last_active: session.last_active,
                is_valid,
            }
        })
        .collect();

    Json(json!({
        "sessions": session_list,
        "count": session_list.len()
    }))
    .into_response()
}

/// DELETE /api/auth/sessions
/// Invalidate all sessions for the current API key
pub async fn delete_all_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let mut sessions = state.sessions.write().await;
    let before = sessions.len();
    sessions.retain(|_, session| session.api_key_id != api_key.id);
    let after = sessions.len();

    Json(json!({
        "success": true,
        "message": format!("Invalidated {} sessions", before - after)
    }))
    .into_response()
}

/// GET /api/user
/// Returns the current dashboard user's profile info.
///
/// OpenProxy is a single-user dashboard guarded by either a JWT cookie
/// (set by `POST /api/auth/login`) or a management API key. Since the
/// dashboard does not model multiple users, this endpoint synthesizes a
/// stable identity from the live auth/settings state so the Profile page
/// can render meaningful data.
pub async fn get_user(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) =
        crate::server::api::require_dashboard_or_management_api_key(&headers, &state)
    {
        return response;
    }

    let snapshot = state.db.snapshot();
    let has_password = settings_password_hash(&snapshot.settings).is_some();
    let auth_method = if crate::server::auth::extract_auth_token(&headers).is_some() {
        "dashboard_session"
    } else {
        "management_api_key"
    };

    Json(json!({
        "username": "admin",
        "email": null,
        "role": "owner",
        "authMethod": auth_method,
        "hasPassword": has_password,
        "requireLogin": snapshot.settings.require_login,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordChangeRequest {
    /// The existing dashboard password (plaintext). Required when a password
    /// hash is already stored in settings.
    current_password: Option<String>,
    /// The new dashboard password (plaintext). Will be bcrypt-hashed before
    /// storage. Must be at least 8 characters.
    new_password: String,
}

/// POST /api/auth/password
///
/// Change the dashboard password. The caller must present a valid dashboard
/// session (JWT cookie) or management API key.
///
/// - Verifies `current_password` against the stored bcrypt hash (if any).
/// - Bcrypt-hashes `new_password` and persists it in `settings.password`.
/// - Revokes all existing JWT sessions so the user must log in again.
pub async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PasswordChangeRequest>,
) -> Response {
    // Require either a dashboard session or management API key.
    if let Err(response) =
        crate::server::api::require_dashboard_or_management_api_key(&headers, &state)
    {
        return response;
    }

    let new_password = req.new_password.trim();
    if new_password.len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "New password must be at least 8 characters long" })),
        )
            .into_response();
    }

    let snapshot = state.db.snapshot();
    let current_hash = settings_password_hash(&snapshot.settings);

    // If a password hash already exists, require the current password for
    // verification.
    if let Some(hash) = current_hash {
        match req.current_password {
            Some(ref current) if !current.is_empty() => {
                if !verify(current, hash).unwrap_or(false) {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({ "error": "Current password is incorrect" })),
                    )
                        .into_response();
                }
            }
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Current password is required to set a new password" })),
                )
                    .into_response();
            }
        }
    }

    // Bcrypt-hash the new password.
    let hashed = match hash(new_password, DEFAULT_COST) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "failed to bcrypt-hash new password");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to hash password" })),
            )
                .into_response();
        }
    };

    // Persist the new password hash in settings.
    if let Err(e) = state
        .db
        .update(|db| {
            db.settings.password = Some(hashed);
            // Also clear the legacy `extra["password"]` field if present.
            db.settings.extra.remove("password");
        })
        .await
    {
        tracing::error!(error = %e, "failed to persist new password");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to save password" })),
        )
            .into_response();
    }

    // Only revoke existing sessions when rotating an already-set password.
    // First-time password set (force-change after default login) keeps the
    // freshly issued session cookie valid so the user can enter the dashboard.
    if current_hash.is_some() {
        crate::server::auth::increment_token_epoch();
        return Json(json!({
            "success": true,
            "message": "Password changed. All sessions have been invalidated. Please log in again.",
            "sessionsInvalidated": true,
        }))
        .into_response();
    }

    Json(json!({
        "success": true,
        "message": "Password set successfully.",
        "sessionsInvalidated": false,
    }))
    .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/password", post(change_password))
        .route("/api/auth/sessions", get(list_sessions))
        .route("/api/auth/sessions", delete(delete_all_sessions))
        .route("/api/auth/session/{session_id}", get(get_session))
        .route("/api/auth/status", get(auth_status))
        .route("/api/user", get(get_user))
}

pub(crate) fn settings_password_hash(settings: &Settings) -> Option<&str> {
    if let Some(hash) = settings.password.as_deref() {
        return Some(hash);
    }
    settings
        .extra
        .get("password")
        .and_then(|value| value.as_str())
}

/// Verify a plaintext dashboard password for sensitive re-auth actions
/// (database export/import). Mirrors 9router `verifyDashboardPassword`:
/// bcrypt against the stored hash when present, otherwise the persisted
/// initial password (see [`crate::core::auth::dashboard_initial_password`]).
pub(crate) fn verify_dashboard_password(password: Option<&str>, settings: &Settings) -> bool {
    let Some(password) = password.map(str::trim).filter(|p| !p.is_empty()) else {
        return false;
    };
    if let Some(hash) = settings_password_hash(settings) {
        return verify(password, hash).unwrap_or(false);
    }
    crate::core::auth::timing_safe_eq(password, &crate::core::auth::dashboard_initial_password())
}

fn is_tunnel_request(headers: &HeaderMap, settings: &Settings) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(':')
                .next()
                .unwrap_or(value)
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    if host.is_empty() {
        return false;
    }

    tunnel_host(&settings.tunnel_url).is_some_and(|tunnel_host| tunnel_host == host)
        || tunnel_host(&settings.tailscale_url).is_some_and(|tailscale_host| tailscale_host == host)
}

fn tunnel_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    url::Url::parse(trimmed)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host.to_ascii_lowercase()))
}

fn build_auth_cookie(token: &str, max_age_seconds: i64, secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!(
        "auth_token={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_seconds}{secure_flag}"
    )
}

/// Best-effort client IP extraction. The dashboard binds `127.0.0.1`, so most
/// real callers are either loopback or coming through a reverse proxy.
///
/// Order:
///   1. `x-9r-real-ip` — unspoofable TCP peer IP stamped by
///      [`crate::server::api::guard::real_ip_middleware`] (Fix 1). This is the
///      only trusted source; the headers below are only checked when this is
///      absent (e.g. in test environments that bypass the middleware).
///   2. `X-Forwarded-For` (first hop) — only when TRUST_PROXY=true.
///   3. `X-Real-IP` — only when TRUST_PROXY=true.
///   4. Loopback (`127.0.0.1`) — safe fallback when nothing else matches.
fn client_ip_from_headers(headers: &HeaderMap) -> std::net::IpAddr {
    // Priority 1: unspoofable TCP peer IP. The guard middleware strips
    // all client-supplied forwarding headers and stamps this one from
    // the verified connection socket.
    if let Some(value) = headers
        .get(super::guard::REAL_IP_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        if let Ok(ip) = value.trim().parse::<std::net::IpAddr>() {
            return ip;
        }
    }

    // Priority 2-3: reverse-proxy headers (only trusted when explicitly
    // enabled via TRUST_PROXY=true).
    if let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    {
        if let Some(first) = value.split(',').next() {
            if let Ok(ip) = first.trim().parse::<std::net::IpAddr>() {
                return ip;
            }
        }
    }
    if let Some(value) = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
    {
        if let Ok(ip) = value.trim().parse::<std::net::IpAddr>() {
            return ip;
        }
    }
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
}

/// HTTP 429 response for a rate-limited login attempt. Includes a
/// `Retry-After` header (seconds) and a JSON body the dashboard can render.
fn lockout_response(retry_after_secs: u64) -> Response {
    let reset_hint = "Forgot password? Run `openproxy auth reset-password` on the host to restore the generated initial password.";
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": format!(
                "Too many failed attempts. Try again in {retry_after_secs}s."
            ),
            "retry_after_secs": retry_after_secs,
            // camelCase alias for the login UI countdown
            "retryAfter": retry_after_secs,
            "resetHint": reset_hint,
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().append(header::RETRY_AFTER, value);
    }
    response
}
