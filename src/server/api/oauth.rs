use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use rand::RngCore;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::str;
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

use crate::oauth::device_code;
use crate::oauth::pending::PendingOAuthFlow;
use crate::oauth::providers;
use crate::oauth::{OAuthProviderConfig, TokenResponse};
use crate::server::auth::{extract_api_key, require_api_key_with_reload};
use crate::server::state::AppState;
use crate::types::ProviderConnection;

use crate::core::utils::antigravity_project::extract_google_project_id;

const PKCE_FLOW_TTL_SECS: i64 = 600;
const DEVICE_FLOW_TTL_SECS: i64 = 900;
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const CLAUDE_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
const CLAUDE_SCOPE: &str = "org:create_api_key user:profile user:inference";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_SCOPE: &str = "openid profile email offline_access";
const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const CODEX_FIXED_PORT: u64 = 1455;
const CODEX_CALLBACK_PATH: &str = "/auth/callback";
const XAI_FIXED_PORT: u16 = 56121;
const XAI_CALLBACK_PATH: &str = "/callback";
const XAI_PROXY_TIMEOUT_MS: u64 = 300_000;
const XAI_TOKEN_URL_DEFAULT: &str = "https://auth.x.ai/oauth2/token";
const XAI_AUTHORIZE_URL_DEFAULT: &str = "https://auth.x.ai/oauth2/authorize";
const GEMINI_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GEMINI_USER_INFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const ANTIGRAVITY_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const ANTIGRAVITY_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ANTIGRAVITY_USER_INFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
const ANTIGRAVITY_LOAD_CODE_ASSIST_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const ANTIGRAVITY_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
const CLINE_AUTHORIZE_URL: &str = "https://api.cline.bot/api/v1/auth/authorize";
const CLINE_TOKEN_URL: &str = "https://api.cline.bot/api/v1/auth/token";
const CODEX_PROXY_TIMEOUT_MS: u64 = 300_000;

#[derive(Clone, Default)]
pub struct CodexProxyState {
    inner: Arc<Mutex<CodexProxyInner>>,
}

#[derive(Default)]
struct CodexProxyInner {
    server: Option<CodexProxyServer>,
    sessions: HashMap<String, CodexPendingExchange>,
}

struct CodexProxyServer {
    shutdown_tx: Option<oneshot::Sender<()>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexPendingExchange {
    code_verifier: String,
    redirect_uri: String,
    status: String,
    created_at: i64,
    connection_id: Option<String>,
    email: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexProxyStartResult {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl CodexProxyState {
    pub fn new() -> Self {
        Self::default()
    }

    async fn start(&self, state: AppState, app_port: u16) -> CodexProxyStartResult {
        let mut inner = self.inner.lock().await;
        if inner.server.is_some() {
            return CodexProxyStartResult {
                success: true,
                reason: None,
            };
        }

        let listener = match TcpListener::bind(("127.0.0.1", CODEX_FIXED_PORT as u16)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                return CodexProxyStartResult {
                    success: false,
                    reason: Some("port_busy".to_string()),
                };
            }
            Err(error) => {
                return CodexProxyStartResult {
                    success: false,
                    reason: Some(error.to_string()),
                };
            }
        };

        let proxy_state = self.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept_result = listener.accept() => {
                        let (mut stream, _) = match accept_result {
                            Ok(pair) => pair,
                            Err(_) => break,
                        };
                        let proxy_state = proxy_state.clone();
                        let app_state = state.clone();
                        tokio::spawn(async move {
                            handle_codex_proxy_connection(proxy_state, app_state, app_port, &mut stream).await;
                        });
                    }
                }
            }
        });

        let proxy_state = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(CODEX_PROXY_TIMEOUT_MS)).await;
            proxy_state.stop().await;
        });

        inner.server = Some(CodexProxyServer {
            shutdown_tx: Some(shutdown_tx),
        });
        CodexProxyStartResult {
            success: true,
            reason: None,
        }
    }

    async fn stop(&self) {
        let server = {
            let mut inner = self.inner.lock().await;
            inner.server.take()
        };
        if let Some(mut server) = server {
            if let Some(shutdown_tx) = server.shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }
        }
    }

    async fn register_session(&self, state: &str, code_verifier: &str, redirect_uri: &str) -> bool {
        if state.trim().is_empty()
            || code_verifier.trim().is_empty()
            || redirect_uri.trim().is_empty()
        {
            return false;
        }

        let mut inner = self.inner.lock().await;
        inner.sessions.insert(
            state.to_string(),
            CodexPendingExchange {
                code_verifier: code_verifier.to_string(),
                redirect_uri: redirect_uri.to_string(),
                status: "pending".to_string(),
                created_at: chrono::Utc::now().timestamp_millis(),
                connection_id: None,
                email: None,
                error: None,
            },
        );
        true
    }

    async fn get_session(&self, state: &str) -> Option<CodexPendingExchange> {
        let inner = self.inner.lock().await;
        inner.sessions.get(state).cloned()
    }

    async fn clear_session(&self, state: &str) {
        let mut inner = self.inner.lock().await;
        inner.sessions.remove(state);
    }

    async fn set_session_done(&self, state: &str, connection_id: String, email: Option<String>) {
        let mut inner = self.inner.lock().await;
        if let Some(session) = inner.sessions.get_mut(state) {
            session.status = "done".to_string();
            session.connection_id = Some(connection_id);
            session.email = email;
            session.error = None;
        }
    }

    async fn set_session_error(&self, state: &str, error: String) {
        let mut inner = self.inner.lock().await;
        if let Some(session) = inner.sessions.get_mut(state) {
            session.status = "error".to_string();
            session.error = Some(error);
        }
    }
}

/// xAI fixed-port OAuth callback proxy on 127.0.0.1:56121.
/// Parallel to CodexProxyState (port 1455) — kept separate so the codex hot-path
/// stays byte-equivalent.
#[derive(Clone, Default)]
pub struct XaiProxyState {
    inner: Arc<Mutex<XaiProxyInner>>,
}

#[derive(Default)]
struct XaiProxyInner {
    server: Option<XaiProxyServer>,
    sessions: HashMap<String, CodexPendingExchange>,
}

struct XaiProxyServer {
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl XaiProxyState {
    pub fn new() -> Self {
        Self::default()
    }

    async fn start(&self, state: AppState, app_port: u16) -> CodexProxyStartResult {
        let mut inner = self.inner.lock().await;
        if inner.server.is_some() {
            return CodexProxyStartResult {
                success: true,
                reason: None,
            };
        }

        let listener = match TcpListener::bind(("127.0.0.1", XAI_FIXED_PORT)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                return CodexProxyStartResult {
                    success: false,
                    reason: Some("port_busy".to_string()),
                };
            }
            Err(error) => {
                return CodexProxyStartResult {
                    success: false,
                    reason: Some(error.to_string()),
                };
            }
        };

        let proxy_state = self.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept_result = listener.accept() => {
                        let (mut stream, _) = match accept_result {
                            Ok(pair) => pair,
                            Err(_) => break,
                        };
                        let proxy_state = proxy_state.clone();
                        let app_state = state.clone();
                        tokio::spawn(async move {
                            handle_xai_proxy_connection(proxy_state, app_state, app_port, &mut stream).await;
                        });
                    }
                }
            }
        });

        let proxy_state = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(XAI_PROXY_TIMEOUT_MS)).await;
            proxy_state.stop().await;
        });

        inner.server = Some(XaiProxyServer {
            shutdown_tx: Some(shutdown_tx),
        });
        CodexProxyStartResult {
            success: true,
            reason: None,
        }
    }

    async fn stop(&self) {
        let server = {
            let mut inner = self.inner.lock().await;
            inner.server.take()
        };
        if let Some(mut server) = server {
            if let Some(shutdown_tx) = server.shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }
        }
    }

    async fn register_session(&self, state: &str, code_verifier: &str, redirect_uri: &str) -> bool {
        if state.trim().is_empty()
            || code_verifier.trim().is_empty()
            || redirect_uri.trim().is_empty()
        {
            return false;
        }

        let mut inner = self.inner.lock().await;
        inner.sessions.insert(
            state.to_string(),
            CodexPendingExchange {
                code_verifier: code_verifier.to_string(),
                redirect_uri: redirect_uri.to_string(),
                status: "pending".to_string(),
                created_at: chrono::Utc::now().timestamp_millis(),
                connection_id: None,
                email: None,
                error: None,
            },
        );
        true
    }

    async fn get_session(&self, state: &str) -> Option<CodexPendingExchange> {
        let inner = self.inner.lock().await;
        inner.sessions.get(state).cloned()
    }

    async fn clear_session(&self, state: &str) {
        let mut inner = self.inner.lock().await;
        inner.sessions.remove(state);
    }

    async fn set_session_done(&self, state: &str, connection_id: String, email: Option<String>) {
        let mut inner = self.inner.lock().await;
        if let Some(session) = inner.sessions.get_mut(state) {
            session.status = "done".to_string();
            session.connection_id = Some(connection_id);
            session.email = email;
            session.error = None;
        }
    }

    async fn set_session_error(&self, state: &str, error: String) {
        let mut inner = self.inner.lock().await;
        if let Some(session) = inner.sessions.get_mut(state) {
            session.status = "error".to_string();
            session.error = Some(error);
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceCodeBody {
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefreshBody {
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceCodeCompatQuery {
    pub start_url: Option<String>,
    pub region: Option<String>,
    pub auth_method: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthExchangeCompatBody {
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    state: Option<String>,
    meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CodexStartProxyQuery {
    app_port: Option<u16>,
    state: Option<String>,
    code_verifier: Option<String>,
    redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CodexPollStatusQuery {
    state: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StartResponse {
    pub auth_url: String,
    pub state: String,
    pub provider: String,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct CallbackResponse {
    pub success: bool,
    pub provider: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct PollResponse {
    pub success: bool,
    pub provider: String,
    pub expires_in: Option<u64>,
    pub pending: Option<bool>,
    pub retry_after: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RefreshResponse {
    pub success: bool,
    pub access_token: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub provider: String,
    pub connected: bool,
    pub auth_type: String,
    pub expires_at: Option<String>,
    pub needs_refresh: Option<bool>,
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OAuthError {
    pub error: OAuthErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct OAuthErrorDetail {
    pub message: String,
    pub code: String,
    pub provider: String,
}

fn make_error(message: &str, code: &str, provider: &str) -> Json<OAuthError> {
    Json(OAuthError {
        error: OAuthErrorDetail {
            message: message.to_string(),
            code: code.to_string(),
            provider: provider.to_string(),
        },
    })
}

fn make_error_response(status: StatusCode, message: &str, code: &str, provider: &str) -> Response {
    (status, make_error(message, code, provider)).into_response()
}

fn generate_code_verifier() -> String {
    let mut random_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut random_bytes);
    URL_SAFE_NO_PAD.encode(random_bytes)
}

fn generate_code_verifier_with_len(bytes: usize) -> String {
    let mut random_bytes = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut random_bytes);
    URL_SAFE_NO_PAD.encode(random_bytes)
}

fn generate_code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    URL_SAFE_NO_PAD.encode(hash)
}

fn generate_state() -> String {
    let mut random_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut random_bytes);
    URL_SAFE_NO_PAD.encode(random_bytes)
}

fn get_provider_config(provider: &str) -> Option<OAuthProviderConfig> {
    providers::get_config(provider)
}

fn is_pkce_provider(provider: &str) -> bool {
    matches!(provider, "claude" | "codex" | "gitlab" | "xai")
}

fn is_device_code_provider(provider: &str) -> bool {
    matches!(
        provider,
        "github"
            | "kimi-coding"
            | "kilocode"
            | "codebuddy"
            | "codebuddy-cn"
            | "codebuddy-intl"
            | "grok-cli"
            | "qwen"
    )
}

fn claude_authorize_url() -> String {
    std::env::var("OPENPROXY_CLAUDE_AUTHORIZE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CLAUDE_AUTHORIZE_URL.to_string())
}

fn claude_token_url() -> String {
    std::env::var("OPENPROXY_CLAUDE_TOKEN_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CLAUDE_TOKEN_URL.to_string())
}

fn codex_authorize_url() -> String {
    std::env::var("OPENPROXY_CODEX_AUTHORIZE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CODEX_AUTHORIZE_URL.to_string())
}

fn codex_token_url() -> String {
    std::env::var("OPENPROXY_CODEX_TOKEN_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CODEX_TOKEN_URL.to_string())
}

fn gemini_token_url() -> String {
    std::env::var("OPENPROXY_GEMINI_TOKEN_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| GEMINI_TOKEN_URL.to_string())
}

fn gemini_user_info_url() -> String {
    std::env::var("OPENPROXY_GEMINI_USER_INFO_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| GEMINI_USER_INFO_URL.to_string())
}

fn antigravity_token_url() -> String {
    std::env::var("OPENPROXY_ANTIGRAVITY_TOKEN_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ANTIGRAVITY_TOKEN_URL.to_string())
}

fn antigravity_user_info_url() -> String {
    std::env::var("OPENPROXY_ANTIGRAVITY_USER_INFO_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ANTIGRAVITY_USER_INFO_URL.to_string())
}

fn antigravity_load_code_assist_endpoint() -> String {
    std::env::var("OPENPROXY_ANTIGRAVITY_LOAD_CODE_ASSIST_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ANTIGRAVITY_LOAD_CODE_ASSIST_ENDPOINT.to_string())
}

fn encode_query_value(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn encode_component_value(value: &str) -> String {
    encode_query_value(value).replace('+', "%20")
}

fn cline_token_url() -> String {
    std::env::var("OPENPROXY_CLINE_TOKEN_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CLINE_TOKEN_URL.to_string())
}

fn build_query_url(base: &str, params: &[(&str, String)]) -> String {
    let query_string = params
        .iter()
        .map(|(key, value)| format!("{key}={}", encode_query_value(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}?{query_string}")
}

fn antigravity_load_metadata() -> Value {
    crate::core::config::app_constants::agy_load_metadata()
}

fn first_nonempty_str<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn cline_expires_in(expires_at: Option<&str>) -> Option<i64> {
    let expires_at = expires_at.map(str::trim).filter(|value| !value.is_empty());
    match expires_at {
        Some(value) => chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|parsed| (parsed.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds()),
        None => Some(3600),
    }
}

fn decode_cline_exchange_code(code: &str) -> Option<Value> {
    let mut padded = code.to_string();
    let padding = 4 - (padded.len() % 4);
    if padding != 4 {
        padded.push_str(&"=".repeat(padding));
    }

    let decoded = STANDARD.decode(padded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let last_brace = decoded.rfind('}')?;
    let parsed: Value = serde_json::from_str(decoded[..=last_brace].trim()).ok()?;
    Some(json!({
        "access_token": parsed
            .get("accessToken")
            .or_else(|| parsed.get("access_token"))
            .cloned()
            .unwrap_or(Value::String(String::new())),
        "refresh_token": parsed
            .get("refreshToken")
            .or_else(|| parsed.get("refresh_token"))
            .cloned()
            .unwrap_or(Value::Null),
        "email": parsed
            .get("email")
            .cloned()
            .unwrap_or(Value::String(String::new())),
        "firstName": parsed
            .get("firstName")
            .cloned()
            .unwrap_or(Value::Null),
        "lastName": parsed
            .get("lastName")
            .cloned()
            .unwrap_or(Value::Null),
        "expires_at": parsed
            .get("expiresAt")
            .or_else(|| parsed.get("expires_at"))
            .cloned()
            .unwrap_or(Value::Null),
    }))
}

const GITLAB_DEFAULT_BASE: &str = "https://gitlab.com";
const CURSOR_ACCESS_TOKEN_KEYS: &[&str] = &["cursorAuth/accessToken", "cursorAuth/token"];
const CURSOR_MACHINE_ID_KEYS: &[&str] = &[
    "storage.serviceMachineId",
    "storage.machineId",
    "telemetry.machineId",
];

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn render_codex_result_page(success: bool, message: &str) -> String {
    let color = if success { "#22c55e" } else { "#ef4444" };
    let icon = if success { "&#10003;" } else { "&#10007;" };
    let title = if success {
        "Authentication Successful"
    } else {
        "Authentication Failed"
    };

    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{title}</title><style>body{{font-family:system-ui;display:flex;justify-content:center;align-items:center;height:100vh;margin:0;background:#f5f5f5}}.c{{text-align:center;padding:2rem;background:#fff;border-radius:8px;box-shadow:0 2px 10px rgba(0,0,0,.1)}}.i{{color:{color};font-size:3rem}}h1{{margin:1rem 0}}p{{color:#666}}</style></head><body><div class=\"c\"><div class=\"i\">{icon}</div><h1>{title}</h1><p>{message}</p><p>Closing in <span id=\"cd\">3</span>s...</p><script>let n=3;const c=document.getElementById(\"cd\");const t=setInterval(()=>{{n--;c.textContent=n;if(n<=0){{clearInterval(t);window.close();}}}},1000);</script></div></body></html>"
    )
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status_line: &str,
    headers: &[(&str, String)],
    body: &str,
) {
    let mut response = format!("HTTP/1.1 {status_line}\r\n");
    for (key, value) in headers {
        response.push_str(key);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str(&format!("Content-Length: {}\r\n", body.len()));
    response.push_str("Connection: close\r\n\r\n");
    response.push_str(body);
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn handle_codex_proxy_connection(
    proxy_state: CodexProxyState,
    state: AppState,
    app_port: u16,
    stream: &mut tokio::net::TcpStream,
) {
    let mut buffer = vec![0u8; 16 * 1024];
    let bytes_read = match stream.read(&mut buffer).await {
        Ok(bytes_read) if bytes_read > 0 => bytes_read,
        _ => return,
    };
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let parsed = match url::Url::parse(&format!("http://localhost{target}")) {
        Ok(url) => url,
        Err(_) => {
            write_http_response(
                stream,
                "400 Bad Request",
                &[("Content-Type", "text/plain; charset=utf-8".to_string())],
                "Invalid callback URL",
            )
            .await;
            return;
        }
    };

    if parsed.path() != "/callback" && parsed.path() != "/auth/callback" {
        write_http_response(
            stream,
            "404 Not Found",
            &[("Content-Type", "text/plain; charset=utf-8".to_string())],
            "Not found",
        )
        .await;
        return;
    }

    let code = parsed.query_pairs().find_map(|(key, value)| {
        if key == "code" {
            Some(value.into_owned())
        } else {
            None
        }
    });
    let state_param = parsed.query_pairs().find_map(|(key, value)| {
        if key == "state" {
            Some(value.into_owned())
        } else {
            None
        }
    });
    let error_param = parsed.query_pairs().find_map(|(key, value)| {
        if key == "error" {
            Some(value.into_owned())
        } else {
            None
        }
    });
    let error_description = parsed.query_pairs().find_map(|(key, value)| {
        if key == "error_description" {
            Some(value.into_owned())
        } else {
            None
        }
    });

    if let Some(state_value) = state_param.as_deref() {
        if let Some(session) = proxy_state.get_session(state_value).await {
            let response_page = if let Some(error) = error_param {
                let message = error_description.unwrap_or(error);
                proxy_state
                    .set_session_error(state_value, message.clone())
                    .await;
                render_codex_result_page(false, &message)
            } else if let Some(code) = code {
                match exchange_codex_compat(&code, &session.redirect_uri, &session.code_verifier)
                    .await
                {
                    Ok(connection) => {
                        match create_imported_oauth_connection(&state, connection).await {
                            Ok(saved) => {
                                proxy_state
                                    .set_session_done(
                                        state_value,
                                        saved.id.clone(),
                                        saved.email.clone(),
                                    )
                                    .await;
                                render_codex_result_page(true, "You can close this window.")
                            }
                            Err(error) => {
                                let message = error.to_string();
                                proxy_state
                                    .set_session_error(state_value, message.clone())
                                    .await;
                                render_codex_result_page(false, &message)
                            }
                        }
                    }
                    Err(error) => {
                        proxy_state
                            .set_session_error(state_value, error.clone())
                            .await;
                        render_codex_result_page(false, &error)
                    }
                }
            } else {
                let message = "No authorization code received".to_string();
                proxy_state
                    .set_session_error(state_value, message.clone())
                    .await;
                render_codex_result_page(false, &message)
            };

            write_http_response(
                stream,
                "200 OK",
                &[("Content-Type", "text/html; charset=utf-8".to_string())],
                &response_page,
            )
            .await;
            proxy_state.stop().await;
            return;
        }
    }

    let redirect_suffix = parsed
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let redirect_url = format!("http://localhost:{app_port}/callback{redirect_suffix}");
    write_http_response(stream, "302 Found", &[("Location", redirect_url)], "").await;
    proxy_state.stop().await;
}

async fn handle_xai_proxy_connection(
    proxy_state: XaiProxyState,
    state: AppState,
    app_port: u16,
    stream: &mut tokio::net::TcpStream,
) {
    let mut buffer = vec![0u8; 16 * 1024];
    let bytes_read = match stream.read(&mut buffer).await {
        Ok(bytes_read) if bytes_read > 0 => bytes_read,
        _ => return,
    };
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let parsed = match url::Url::parse(&format!("http://localhost{target}")) {
        Ok(url) => url,
        Err(_) => {
            write_http_response(
                stream,
                "400 Bad Request",
                &[("Content-Type", "text/plain; charset=utf-8".to_string())],
                "Invalid callback URL",
            )
            .await;
            return;
        }
    };

    if parsed.path() != XAI_CALLBACK_PATH && parsed.path() != "/auth/callback" {
        write_http_response(
            stream,
            "404 Not Found",
            &[("Content-Type", "text/plain; charset=utf-8".to_string())],
            "Not found",
        )
        .await;
        return;
    }

    let code = parsed.query_pairs().find_map(|(key, value)| {
        if key == "code" {
            Some(value.into_owned())
        } else {
            None
        }
    });
    let state_param = parsed.query_pairs().find_map(|(key, value)| {
        if key == "state" {
            Some(value.into_owned())
        } else {
            None
        }
    });
    let error_param = parsed.query_pairs().find_map(|(key, value)| {
        if key == "error" {
            Some(value.into_owned())
        } else {
            None
        }
    });
    let error_description = parsed.query_pairs().find_map(|(key, value)| {
        if key == "error_description" {
            Some(value.into_owned())
        } else {
            None
        }
    });

    if let Some(state_value) = state_param.as_deref() {
        if let Some(session) = proxy_state.get_session(state_value).await {
            let response_page = if let Some(error) = error_param {
                let message = error_description.unwrap_or(error);
                proxy_state
                    .set_session_error(state_value, message.clone())
                    .await;
                render_codex_result_page(false, &message)
            } else if let Some(code) = code {
                match exchange_xai_compat(&code, &session.redirect_uri, &session.code_verifier)
                    .await
                {
                    Ok(connection) => {
                        match create_imported_oauth_connection(&state, connection).await {
                            Ok(saved) => {
                                proxy_state
                                    .set_session_done(
                                        state_value,
                                        saved.id.clone(),
                                        saved.email.clone(),
                                    )
                                    .await;
                                render_codex_result_page(true, "You can close this window.")
                            }
                            Err(error) => {
                                let message = error.to_string();
                                proxy_state
                                    .set_session_error(state_value, message.clone())
                                    .await;
                                render_codex_result_page(false, &message)
                            }
                        }
                    }
                    Err(error) => {
                        proxy_state
                            .set_session_error(state_value, error.clone())
                            .await;
                        render_codex_result_page(false, &error)
                    }
                }
            } else {
                let message = "No authorization code received".to_string();
                proxy_state
                    .set_session_error(state_value, message.clone())
                    .await;
                render_codex_result_page(false, &message)
            };

            write_http_response(
                stream,
                "200 OK",
                &[("Content-Type", "text/html; charset=utf-8".to_string())],
                &response_page,
            )
            .await;
            proxy_state.stop().await;
            return;
        }
    }

    let redirect_suffix = parsed
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let redirect_url = format!("http://localhost:{app_port}/callback{redirect_suffix}");
    write_http_response(stream, "302 Found", &[("Location", redirect_url)], "").await;
    proxy_state.stop().await;
}

async fn store_connection(
    db: &crate::db::Db,
    account_id: &str,
    provider: &str,
    token_response: &TokenResponse,
    redirect_uri: Option<&str>,
) -> anyhow::Result<()> {
    let provider_config = get_provider_config(provider);
    let _client_id = provider_config
        .as_ref()
        .and_then(|c| c.get_param("client_id"))
        .unwrap_or("openproxy")
        .to_string();

    let _now = now_secs();
    let expires_at = token_response.expires_in.map(|secs| {
        let expires = chrono::Utc::now() + chrono::Duration::seconds(secs);
        expires.to_rfc3339()
    });

    let _redirect_uri = redirect_uri
        .map(|s| s.to_string())
        .or_else(|| {
            provider_config
                .as_ref()
                .and_then(|c| c.get_param("redirect_uri"))
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "http://localhost:4623/oauth/callback".to_string());

    db.update(|db| {
        let snapshot = db;
        if let Some(conn_idx) = snapshot
            .provider_connections
            .iter()
            .position(|conn| conn.provider == provider && conn.id.contains(account_id))
        {
            snapshot.provider_connections[conn_idx].access_token =
                Some(token_response.access_token.clone());
            snapshot.provider_connections[conn_idx].refresh_token =
                token_response.refresh_token.clone();
            snapshot.provider_connections[conn_idx].expires_at = expires_at;
            snapshot.provider_connections[conn_idx].scope = token_response.scope.clone();
            snapshot.provider_connections[conn_idx].updated_at =
                Some(chrono::Utc::now().to_rfc3339());
        } else {
            let connection_id = format!("{}-{}", account_id, Uuid::new_v4());
            let connection = ProviderConnection {
                id: connection_id,
                provider: provider.to_string(),
                auth_type: "oauth".to_string(),
                name: None,
                priority: Some(100),
                is_active: Some(true),
                created_at: Some(chrono::Utc::now().to_rfc3339()),
                updated_at: Some(chrono::Utc::now().to_rfc3339()),
                display_name: None,
                email: None,
                global_priority: None,
                default_model: None,
                access_token: Some(token_response.access_token.clone()),
                refresh_token: token_response.refresh_token.clone(),
                expires_at,
                token_type: token_response.token_type.clone(),
                scope: token_response.scope.clone(),
                id_token: token_response.id_token.clone(),
                project_id: None,
                api_key: None,
                test_status: None,
                last_tested: None,
                last_error: None,
                last_error_at: None,
                rate_limited_until: None,
                expires_in: token_response.expires_in,
                error_code: None,
                consecutive_use_count: None,
                backoff_level: None,
                consecutive_errors: None,
                proxy_url: None,
                proxy_label: None,
                use_connection_proxy: None,
                runtime_transport: None,
                provider_specific_data: std::collections::BTreeMap::new(),
                extra: std::collections::BTreeMap::new(),
            };
            snapshot.provider_connections.push(connection);
        }
    })
    .await?;
    Ok(())
}

fn internal_error_response(message: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message })),
    )
        .into_response()
}

fn next_provider_priority(connections: &[ProviderConnection], provider: &str) -> u32 {
    connections
        .iter()
        .filter(|connection| connection.provider == provider)
        .map(|connection| connection.priority.unwrap_or(0))
        .max()
        .unwrap_or(0)
        + 1
}

async fn create_imported_oauth_connection(
    state: &AppState,
    mut connection: ProviderConnection,
) -> anyhow::Result<ProviderConnection> {
    let now = chrono::Utc::now().to_rfc3339();
    let provider = connection.provider.clone();
    let email_for_upsert = connection
        .email
        .as_deref()
        .filter(|email| !email.is_empty())
        .map(str::to_string);
    let mut saved = None;

    state
        .db
        .update(|db| {
            if let Some(email) = email_for_upsert.as_deref() {
                if let Some(existing) = db.provider_connections.iter_mut().find(|candidate| {
                    candidate.provider == provider
                        && candidate.auth_type == "oauth"
                        && candidate.email.as_deref() == Some(email)
                }) {
                    existing.display_name = connection.display_name.clone();
                    existing.email = connection.email.clone();
                    existing.access_token = connection.access_token.clone();
                    existing.refresh_token = connection.refresh_token.clone();
                    existing.expires_at = connection.expires_at.clone();
                    existing.expires_in = connection.expires_in;
                    existing.test_status = connection.test_status.clone();
                    existing.last_error = connection.last_error.clone();
                    existing.last_error_at = connection.last_error_at.clone();
                    existing.token_type = connection.token_type.clone();
                    existing.scope = connection.scope.clone();
                    existing.id_token = connection.id_token.clone();
                    existing.project_id = connection.project_id.clone();
                    existing.provider_specific_data = connection.provider_specific_data.clone();
                    existing.updated_at = Some(now.clone());
                    saved = Some(existing.clone());
                    return;
                }
            }

            if connection.name.is_none() {
                connection.name = Some(
                    connection
                        .email
                        .as_deref()
                        .filter(|email| !email.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            format!(
                                "Account {}",
                                db.provider_connections
                                    .iter()
                                    .filter(|candidate| candidate.provider == provider)
                                    .count()
                                    + 1
                            )
                        }),
                );
            }

            if connection.priority.is_none() {
                connection.priority =
                    Some(next_provider_priority(&db.provider_connections, &provider));
            }
            if connection.id.is_empty() {
                connection.id = Uuid::new_v4().to_string();
            }
            if connection.is_active.is_none() {
                connection.is_active = Some(true);
            }
            if connection.created_at.is_none() {
                connection.created_at = Some(now.clone());
            }
            connection.updated_at = Some(now.clone());

            db.provider_connections.push(connection.clone());
            saved = Some(connection.clone());
        })
        .await?;

    let saved = saved.ok_or_else(|| anyhow::anyhow!("Failed to save provider connection"))?;
    super::quota_auto_ping::reconcile_quota_auto_ping(state);
    Ok(saved)
}

fn decode_jwt_claims(access_token: &str) -> Option<Value> {
    let mut parts = access_token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let mut padded = payload.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }

    let decoded = URL_SAFE.decode(padded).ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn cursor_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn cursor_candidate_paths() -> Vec<PathBuf> {
    let home = cursor_home_dir();
    match std::env::consts::OS {
        "macos" => vec![
            home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
            home.join(
                "Library/Application Support/Cursor - Insiders/User/globalStorage/state.vscdb",
            ),
        ],
        "windows" => {
            let app_data = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Roaming"));
            let local_app_data = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Local"));
            vec![
                app_data
                    .join("Cursor")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
                app_data
                    .join("Cursor - Insiders")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
                local_app_data
                    .join("Cursor")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
                local_app_data
                    .join("Programs")
                    .join("Cursor")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
            ]
        }
        _ => vec![
            home.join(".config")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
            home.join(".config")
                .join("cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
        ],
    }
}

fn normalize_cursor_db_value(value: &str) -> String {
    match serde_json::from_str::<Value>(value) {
        Ok(Value::String(parsed)) => parsed,
        _ => value.to_string(),
    }
}

fn extract_cursor_tokens_from_db(
    db_path: &std::path::Path,
) -> Result<(Option<String>, Option<String>), rusqlite::Error> {
    let connection =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    let query = |keys: &[&str]| -> Result<Option<String>, rusqlite::Error> {
        for key in keys {
            let value: Option<String> = connection
                .query_row(
                    "SELECT value FROM itemTable WHERE key=? LIMIT 1",
                    [key],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(value) = value {
                return Ok(Some(normalize_cursor_db_value(&value)));
            }
        }
        Ok(None)
    };

    Ok((
        query(CURSOR_ACCESS_TOKEN_KEYS)?,
        query(CURSOR_MACHINE_ID_KEYS)?,
    ))
}

fn cursor_is_installed() -> bool {
    if std::env::consts::OS != "linux" {
        return true;
    }

    if Command::new("which")
        .arg("cursor")
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        return true;
    }

    cursor_home_dir()
        .join(".local")
        .join("share")
        .join("applications")
        .join("cursor.desktop")
        .is_file()
}

fn cursor_import_instructions() -> Value {
    json!({
        "provider": "cursor",
        "method": "import_token",
        "instructions": {
            "title": "How to get your Cursor token",
            "steps": [
                "1. Open Cursor IDE and make sure you're logged in",
                "2. Find the state.vscdb file:",
                "   - Linux: ~/.config/Cursor/User/globalStorage/state.vscdb",
                "   - macOS: /Users/<user>/Library/Application Support/Cursor/User/globalStorage/state.vscdb",
                "   - Windows: %APPDATA%\\Cursor\\User\\globalStorage\\state.vscdb",
                "3. Open the database with SQLite browser or CLI:",
                "   sqlite3 state.vscdb \"SELECT value FROM itemTable WHERE key='cursorAuth/accessToken'\"",
                "4. Also get the machine ID:",
                "   sqlite3 state.vscdb \"SELECT value FROM itemTable WHERE key='storage.serviceMachineId'\"",
                "5. Paste both values in the form below"
            ],
            "alternativeMethod": [
                "Or use this one-liner to get both values:",
                "sqlite3 state.vscdb \"SELECT key, value FROM itemTable WHERE key IN ('cursorAuth/accessToken', 'storage.serviceMachineId')\""
            ]
        },
        "requiredFields": [
            {
                "name": "accessToken",
                "label": "Access Token",
                "description": "From cursorAuth/accessToken in state.vscdb",
                "type": "textarea"
            },
            {
                "name": "machineId",
                "label": "Machine ID",
                "description": "From storage.serviceMachineId in state.vscdb",
                "type": "text"
            }
        ]
    })
}

fn validate_cursor_import_token(
    access_token: &str,
    machine_id: &str,
) -> Result<(String, String), String> {
    if access_token.is_empty() {
        return Err("Access token is required".to_string());
    }
    if machine_id.is_empty() {
        return Err("Machine ID is required".to_string());
    }
    if access_token.len() < 50 {
        return Err("Invalid token format. Token appears too short.".to_string());
    }

    let normalized_machine_id = machine_id.replace('-', "");
    if normalized_machine_id.len() < 32
        || !normalized_machine_id
            .chars()
            .all(|ch| ch.is_ascii_hexdigit())
    {
        return Err("Invalid machine ID format. Expected UUID format.".to_string());
    }

    Ok((access_token.to_string(), machine_id.to_string()))
}

async fn gitlab_pat_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid request body" })),
            )
                .into_response()
        }
    };

    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid request body" })),
            )
                .into_response()
        }
    };

    let token = body
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if token.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Personal Access Token is required" })),
        )
            .into_response();
    }

    let base = body
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(GITLAB_DEFAULT_BASE)
        .trim_end_matches('/')
        .to_string();

    let user_response = match reqwest::Client::new()
        .get(format!("{base}/api/v4/user"))
        .header("Private-Token", token.clone())
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return internal_error_response(error.to_string()),
    };

    if !user_response.status().is_success() {
        let err = user_response.text().await.unwrap_or_default();
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": format!("GitLab token verification failed: {err}")
            })),
        )
            .into_response();
    }

    let user: Value = match user_response.json().await {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };
    let email = user
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| user.get("public_email").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let display_name = user
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| user.get("username").and_then(Value::as_str))
        .unwrap_or(email.as_str())
        .to_string();

    let connection = ProviderConnection {
        provider: "gitlab".to_string(),
        auth_type: "oauth".to_string(),
        display_name: Some(display_name),
        email: Some(email.clone()),
        access_token: Some(token),
        test_status: Some("active".to_string()),
        provider_specific_data: std::collections::BTreeMap::from([
            (
                "username".to_string(),
                Value::String(
                    user.get("username")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
            ),
            ("email".to_string(), Value::String(email)),
            (
                "name".to_string(),
                Value::String(
                    user.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
            ),
            ("baseUrl".to_string(), Value::String(base.clone())),
            (
                "authKind".to_string(),
                Value::String("personal_access_token".to_string()),
            ),
        ]),
        ..Default::default()
    };

    match create_imported_oauth_connection(&state, connection).await {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(error) => internal_error_response(error.to_string()),
    }
}

async fn cursor_import_instructions_route() -> Response {
    Json(cursor_import_instructions()).into_response()
}

async fn cursor_import_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let Some(access_token_raw) = body.get("accessToken").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Access token is required" })),
        )
            .into_response();
    };
    if access_token_raw.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Access token is required" })),
        )
            .into_response();
    }

    let Some(machine_id_raw) = body.get("machineId").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Machine ID is required" })),
        )
            .into_response();
    };
    if machine_id_raw.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Machine ID is required" })),
        )
            .into_response();
    }

    let access_token = access_token_raw.trim();
    let machine_id = machine_id_raw.trim();
    let (validated_access_token, validated_machine_id) =
        match validate_cursor_import_token(access_token, machine_id) {
            Ok(value) => value,
            Err(error) => return internal_error_response(error),
        };

    let claims = decode_jwt_claims(&validated_access_token);
    let email = claims
        .as_ref()
        .and_then(|value| value.get("email"))
        .or_else(|| claims.as_ref().and_then(|value| value.get("sub")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let user_id = claims
        .as_ref()
        .and_then(|value| value.get("sub"))
        .or_else(|| claims.as_ref().and_then(|value| value.get("user_id")))
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut provider_specific_data = std::collections::BTreeMap::from([
        (
            "machineId".to_string(),
            Value::String(validated_machine_id.clone()),
        ),
        (
            "authMethod".to_string(),
            Value::String("imported".to_string()),
        ),
        (
            "provider".to_string(),
            Value::String("Imported".to_string()),
        ),
    ]);
    if let Some(user_id) = user_id {
        provider_specific_data.insert("userId".to_string(), Value::String(user_id));
    }

    let connection = ProviderConnection {
        provider: "cursor".to_string(),
        auth_type: "oauth".to_string(),
        email: email.clone(),
        access_token: Some(validated_access_token),
        refresh_token: None,
        expires_at: Some((chrono::Utc::now() + chrono::Duration::seconds(86_400)).to_rfc3339()),
        test_status: Some("active".to_string()),
        provider_specific_data,
        ..Default::default()
    };

    match create_imported_oauth_connection(&state, connection).await {
        Ok(connection) => Json(json!({
            "success": true,
            "connection": {
                "id": connection.id,
                "provider": connection.provider,
                "email": connection.email
            }
        }))
        .into_response(),
        Err(error) => internal_error_response(error.to_string()),
    }
}

async fn cursor_auto_import_route() -> Response {
    let candidates = cursor_candidate_paths();
    let db_path = candidates
        .iter()
        .find(|candidate| std::fs::File::open(candidate).is_ok())
        .cloned();

    let Some(db_path) = db_path else {
        let checked_locations = candidates
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Json(json!({
            "found": false,
            "error": format!(
                "Cursor database not found. Checked locations:\n{}\n\nMake sure Cursor IDE is installed and opened at least once.",
                checked_locations
            )
        }))
        .into_response();
    };

    if std::env::consts::OS == "linux" && !cursor_is_installed() {
        return Json(json!({
            "found": false,
            "error": "Cursor config files found but Cursor IDE does not appear to be installed. Skipping auto-import."
        }))
        .into_response();
    }

    if let Ok((Some(access_token), Some(machine_id))) = extract_cursor_tokens_from_db(&db_path) {
        return Json(json!({
            "found": true,
            "accessToken": access_token,
            "machineId": machine_id
        }))
        .into_response();
    }

    Json(json!({
        "found": false,
        "windowsManual": true,
        "dbPath": db_path.to_string_lossy().to_string()
    }))
    .into_response()
}

fn build_claude_auth_url(redirect_uri: &str, state: &str, code_challenge: &str) -> String {
    build_query_url(
        &claude_authorize_url(),
        &[
            ("code", "true".to_string()),
            ("client_id", CLAUDE_CLIENT_ID.to_string()),
            ("response_type", "code".to_string()),
            ("redirect_uri", redirect_uri.to_string()),
            ("scope", CLAUDE_SCOPE.to_string()),
            ("code_challenge", code_challenge.to_string()),
            ("code_challenge_method", "S256".to_string()),
            ("state", state.to_string()),
        ],
    )
}

fn build_codex_auth_url(redirect_uri: &str, state: &str, code_challenge: &str) -> String {
    let params = [
        ("response_type", "code".to_string()),
        ("client_id", CODEX_CLIENT_ID.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("scope", CODEX_SCOPE.to_string()),
        ("code_challenge", code_challenge.to_string()),
        ("code_challenge_method", "S256".to_string()),
        ("id_token_add_organizations", "true".to_string()),
        ("codex_cli_simplified_flow", "true".to_string()),
        ("originator", "codex_cli_rs".to_string()),
        ("state", state.to_string()),
    ];
    let query_string = params
        .iter()
        .map(|(key, value)| format!("{key}={}", encode_component_value(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{query_string}", codex_authorize_url())
}

fn build_gitlab_auth_url(
    base_url: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    build_query_url(
        &format!("{}/oauth/authorize", base_url.trim_end_matches('/')),
        &[
            ("client_id", client_id.to_string()),
            ("redirect_uri", redirect_uri.to_string()),
            ("response_type", "code".to_string()),
            ("state", state.to_string()),
            ("scope", "api read_user".to_string()),
            ("code_challenge", code_challenge.to_string()),
            ("code_challenge_method", "S256".to_string()),
        ],
    )
}

fn build_google_auth_url(
    authorize_url: &str,
    client_id: &str,
    scope: &str,
    redirect_uri: &str,
    state: &str,
) -> String {
    build_query_url(
        authorize_url,
        &[
            ("client_id", client_id.to_string()),
            ("response_type", "code".to_string()),
            ("redirect_uri", redirect_uri.to_string()),
            ("scope", scope.to_string()),
            ("state", state.to_string()),
            ("access_type", "offline".to_string()),
            ("prompt", "consent".to_string()),
        ],
    )
}

fn build_cline_auth_url(redirect_uri: &str) -> String {
    build_query_url(
        CLINE_AUTHORIZE_URL,
        &[
            ("client_type", "extension".to_string()),
            ("callback_url", redirect_uri.to_string()),
            ("redirect_uri", redirect_uri.to_string()),
        ],
    )
}

fn build_auth_compat_response(
    provider: &str,
    flow_type: &str,
    auth_url: String,
    state: String,
    code_verifier: String,
    code_challenge: String,
    redirect_uri: String,
) -> Response {
    let mut payload = serde_json::Map::from_iter([
        ("authUrl".to_string(), Value::String(auth_url)),
        ("state".to_string(), Value::String(state)),
        ("codeVerifier".to_string(), Value::String(code_verifier)),
        ("codeChallenge".to_string(), Value::String(code_challenge)),
        ("redirectUri".to_string(), Value::String(redirect_uri)),
        ("flowType".to_string(), Value::String(flow_type.to_string())),
        (
            "callbackPath".to_string(),
            Value::String(if provider == "codex" {
                CODEX_CALLBACK_PATH.to_string()
            } else {
                "/callback".to_string()
            }),
        ),
    ]);
    if provider == "codex" {
        payload.insert("fixedPort".to_string(), Value::from(CODEX_FIXED_PORT));
    }
    Json(Value::Object(payload)).into_response()
}

async fn codex_start_proxy_compat(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<CodexStartProxyQuery>,
) -> Response {
    if provider != "codex" && provider != "xai" && provider != "zed" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Proxy only supported for codex/xai/trae/windsurf/zed" })),
        )
            .into_response();
    }

    // Zed: RSA native-app flow — start the callback listener on the
    // preferred port (58443, random fallback) and register the session
    // carrying the encoded private-key verifier.
    if provider == "zed" {
        let preferred = query
            .app_port
            .unwrap_or(crate::oauth::zed_auth::ZED_DEFAULT_NATIVE_APP_PORT);
        let state_value = query.state.clone().unwrap_or_default();
        let verifier = query.code_verifier.clone().unwrap_or_default();
        match state.zed_proxy.start(state.clone(), preferred).await {
            Ok(port) => {
                let registered = {
                    let mut inner = state.zed_proxy.inner.lock().await;
                    ZedProxyState::register_session_locked(&mut inner, &state_value, &verifier)
                };
                return Json(json!({
                    "success": true,
                    "port": port,
                    "callbackUrl": format!("http://127.0.0.1:{port}/"),
                    "serverSide": registered,
                }))
                .into_response();
            }
            Err(e) => {
                return Json(json!({ "success": false, "reason": e })).into_response();
            }
        }
    }

    let Some(app_port) = query.app_port else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing app_port" })),
        )
            .into_response();
    };

    let result = if provider == "xai" {
        state.xai_proxy.start(state.clone(), app_port).await
    } else {
        state.codex_proxy.start(state.clone(), app_port).await
    };
    let mut response =
        serde_json::Map::from_iter([("success".to_string(), Value::Bool(result.success))]);
    if let Some(reason) = result.reason {
        response.insert("reason".to_string(), Value::String(reason));
    }

    let server_side = if result.success {
        match (
            query.state.as_deref(),
            query.code_verifier.as_deref(),
            query.redirect_uri.as_deref(),
        ) {
            (Some(state_value), Some(code_verifier), Some(redirect_uri)) => {
                if provider == "xai" {
                    state
                        .xai_proxy
                        .register_session(state_value, code_verifier, redirect_uri)
                        .await
                } else {
                    state
                        .codex_proxy
                        .register_session(state_value, code_verifier, redirect_uri)
                        .await
                }
            }
            _ => false,
        }
    } else {
        false
    };
    response.insert("serverSide".to_string(), Value::Bool(server_side));
    Json(Value::Object(response)).into_response()
}

async fn codex_poll_status_compat(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<CodexPollStatusQuery>,
) -> Response {
    if provider != "codex" && provider != "xai" && provider != "zed" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Poll only supported for codex/xai/trae/windsurf/zed" })),
        )
            .into_response();
    }

    if provider == "zed" {
        let Some(state_param) = query
            .state
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing state" })),
            )
                .into_response();
        };
        let Some((status, error, connection_id, email)) =
            state.zed_proxy.get_session(state_param).await
        else {
            return Json(json!({ "status": "unknown" })).into_response();
        };
        let mut payload = json!({ "status": status });
        if !error.is_empty() {
            payload["error"] = json!(error);
        }
        if let Some(cid) = connection_id {
            payload["connectionId"] = json!(cid);
        }
        if let Some(mail) = email {
            payload["email"] = json!(mail);
        }
        return Json(payload).into_response();
    }

    let Some(state_param) = query
        .state
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing state" })),
        )
            .into_response();
    };

    let session = if provider == "xai" {
        state.xai_proxy.get_session(state_param).await
    } else {
        state.codex_proxy.get_session(state_param).await
    };

    let Some(session) = session else {
        return Json(json!({ "status": "unknown" })).into_response();
    };

    if session.status == "done" || session.status == "error" {
        let payload =
            serde_json::to_value(&session).unwrap_or_else(|_| json!({ "status": session.status }));
        if provider == "xai" {
            state.xai_proxy.clear_session(state_param).await;
        } else {
            state.codex_proxy.clear_session(state_param).await;
        }
        Json(payload).into_response()
    } else {
        Json(json!({ "status": session.status })).into_response()
    }
}

async fn codex_stop_proxy_compat(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Response {
    if provider != "codex" && provider != "xai" && provider != "zed" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Proxy only supported for codex/xai/trae/windsurf/zed" })),
        )
            .into_response();
    }

    match provider.as_str() {
        "xai" => state.xai_proxy.stop().await,
        "zed" => state.zed_proxy.stop().await,
        _ => state.codex_proxy.stop().await,
    }
    Json(json!({ "success": true })).into_response()
}

async fn authorize_oauth_compat(
    Path(provider): Path<String>,
    Query(params): Query<std::collections::BTreeMap<String, String>>,
) -> Response {
    let redirect_uri = params
        .get("redirect_uri")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("http://localhost:8080/callback")
        .to_string();
    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);
    let state = generate_state();

    match provider.as_str() {
        "claude" => build_auth_compat_response(
            &provider,
            "authorization_code_pkce",
            build_claude_auth_url(&redirect_uri, &state, &code_challenge),
            state,
            code_verifier,
            code_challenge,
            redirect_uri,
        ),
        "codex" => build_auth_compat_response(
            &provider,
            "authorization_code_pkce",
            build_codex_auth_url(&redirect_uri, &state, &code_challenge),
            state,
            code_verifier,
            code_challenge,
            redirect_uri,
        ),
        "gitlab" => build_auth_compat_response(
            &provider,
            "authorization_code_pkce",
            build_gitlab_auth_url(
                params
                    .get("baseUrl")
                    .map(String::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(GITLAB_DEFAULT_BASE),
                params
                    .get("clientId")
                    .map(String::as_str)
                    .unwrap_or_default(),
                &redirect_uri,
                &state,
                &code_challenge,
            ),
            state,
            code_verifier,
            code_challenge,
            redirect_uri,
        ),
        "antigravity" => build_auth_compat_response(
            &provider,
            "authorization_code",
            build_google_auth_url(
                ANTIGRAVITY_AUTHORIZE_URL,
                ANTIGRAVITY_CLIENT_ID,
                ANTIGRAVITY_SCOPE,
                &redirect_uri,
                &state,
            ),
            state,
            code_verifier,
            code_challenge,
            redirect_uri,
        ),
        "cline" => build_auth_compat_response(
            &provider,
            "authorization_code",
            build_cline_auth_url(&redirect_uri),
            state,
            code_verifier,
            code_challenge,
            redirect_uri,
        ),
        "xai" => {
            // xAI requires a 96-byte PKCE verifier (not the default 32-byte one).
            let code_verifier = generate_code_verifier_with_len(96);
            let code_challenge = generate_code_challenge(&code_verifier);
            build_auth_compat_response(
                &provider,
                "authorization_code_pkce",
                build_xai_auth_url(&redirect_uri, &state, &code_challenge),
                state,
                code_verifier,
                code_challenge,
                redirect_uri,
            )
        }
        _ => internal_error_response(format!("Unknown provider: {provider}")),
    }
}

async fn exchange_claude_compat(
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    state: Option<&str>,
) -> Result<ProviderConnection, String> {
    let (auth_code, code_state) = if let Some((before, after)) = code.split_once('#') {
        (before, after)
    } else {
        (code, "")
    };

    let response = reqwest::Client::new()
        .post(claude_token_url())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&json!({
            "code": auth_code,
            "state": if code_state.is_empty() { state.unwrap_or_default() } else { code_state },
            "grant_type": "authorization_code",
            "client_id": CLAUDE_CLIENT_ID,
            "redirect_uri": redirect_uri,
            "code_verifier": code_verifier,
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        let error = response.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed: {error}"));
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .map_err(|error| format!("Token exchange failed: {error}"))?;

    Ok(ProviderConnection {
        provider: "claude".to_string(),
        auth_type: "oauth".to_string(),
        access_token: Some(token_response.access_token),
        refresh_token: token_response.refresh_token,
        expires_at: token_response
            .expires_in
            .map(crate::oauth::expires_at_from_seconds),
        scope: token_response.scope,
        test_status: Some("active".to_string()),
        ..Default::default()
    })
}

fn extract_codex_account_info(
    id_token: Option<&str>,
) -> (Option<String>, serde_json::Map<String, Value>) {
    let mut provider_specific_data = serde_json::Map::new();
    let Some(id_token) = id_token else {
        return (None, provider_specific_data);
    };

    let claims = decode_jwt_claims(id_token);
    let email = claims
        .as_ref()
        .and_then(|value| value.get("email"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let openai_auth = claims
        .as_ref()
        .and_then(|value| value.get("https://api.openai.com/auth"))
        .and_then(Value::as_object);

    if let Some(account_id) = openai_auth
        .and_then(|value| value.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            claims
                .as_ref()
                .and_then(|value| value.get("account_id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
    {
        provider_specific_data.insert(
            "chatgptAccountId".to_string(),
            Value::String(account_id.to_string()),
        );
    }
    if let Some(plan_type) = openai_auth
        .and_then(|value| value.get("chatgpt_plan_type"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            claims
                .as_ref()
                .and_then(|value| value.get("plan_type"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
    {
        provider_specific_data.insert(
            "chatgptPlanType".to_string(),
            Value::String(plan_type.to_string()),
        );
    }

    (email, provider_specific_data)
}

async fn exchange_codex_compat(
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<ProviderConnection, String> {
    let response = reqwest::Client::new()
        .post(codex_token_url())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CODEX_CLIENT_ID),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        let error = response.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed: {error}"));
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .map_err(|error| format!("Token exchange failed: {error}"))?;
    let (email, provider_specific_data) =
        extract_codex_account_info(token_response.id_token.as_deref());

    Ok(ProviderConnection {
        provider: "codex".to_string(),
        auth_type: "oauth".to_string(),
        email,
        access_token: Some(token_response.access_token),
        refresh_token: token_response.refresh_token,
        expires_at: token_response
            .expires_in
            .map(crate::oauth::expires_at_from_seconds),
        test_status: Some("active".to_string()),
        provider_specific_data: provider_specific_data.into_iter().collect(),
        ..Default::default()
    })
}

fn xai_token_url() -> String {
    std::env::var("OPENPROXY_XAI_TOKEN_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| XAI_TOKEN_URL_DEFAULT.to_string())
}

fn xai_authorize_url() -> String {
    std::env::var("OPENPROXY_XAI_AUTHORIZE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| XAI_AUTHORIZE_URL_DEFAULT.to_string())
}

fn build_xai_auth_url(redirect_uri: &str, state: &str, code_challenge: &str) -> String {
    // nonce is required by xAI OIDC
    let mut random_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut random_bytes);
    let nonce = URL_SAFE_NO_PAD.encode(random_bytes);

    build_query_url(
        &xai_authorize_url(),
        &[
            ("response_type", "code".to_string()),
            ("client_id", XAI_CLIENT_ID.to_string()),
            ("redirect_uri", redirect_uri.to_string()),
            (
                "scope",
                "openid profile email offline_access grok-cli:access api:access".to_string(),
            ),
            ("state", state.to_string()),
            ("code_challenge", code_challenge.to_string()),
            ("code_challenge_method", "S256".to_string()),
            ("nonce", nonce),
            ("plan", "generic".to_string()),
            ("referrer", "cli-proxy-api".to_string()),
            ("prompt", "login".to_string()),
        ],
    )
}

async fn exchange_xai_compat(
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<ProviderConnection, String> {
    let response = reqwest::Client::new()
        .post(xai_token_url())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", XAI_CLIENT_ID),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        let error = response.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed: {error}"));
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .map_err(|error| format!("Token exchange failed: {error}"))?;

    let email = token_response
        .id_token
        .as_deref()
        .and_then(decode_jwt_claims)
        .and_then(|claims| {
            claims
                .get("email")
                .or_else(|| claims.get("preferred_username"))
                .or_else(|| claims.get("sub"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });

    Ok(ProviderConnection {
        provider: "xai".to_string(),
        auth_type: "oauth".to_string(),
        email,
        access_token: Some(token_response.access_token),
        refresh_token: token_response.refresh_token,
        expires_at: token_response
            .expires_in
            .map(crate::oauth::expires_at_from_seconds),
        scope: token_response.scope,
        id_token: token_response.id_token,
        test_status: Some("active".to_string()),
        ..Default::default()
    })
}

fn exchange_meta_string(meta: Option<&Value>, key: &str) -> Option<String> {
    meta.and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

async fn exchange_google_token(
    client_id: &str,
    client_secret: &str,
    token_url: String,
    code: &str,
    redirect_uri: &str,
) -> Result<Value, String> {
    let response = reqwest::Client::new()
        .post(token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            crate::core::config::app_constants::agy_cli_user_agent(),
        )
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        let error = response.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed: {error}"));
    }

    response
        .json()
        .await
        .map_err(|error| format!("Token exchange failed: {error}"))
}
async fn exchange_antigravity_compat(
    code: &str,
    redirect_uri: &str,
) -> Result<ProviderConnection, String> {
    let tokens = exchange_google_token(
        ANTIGRAVITY_CLIENT_ID,
        crate::oauth::secret::antigravity_client_secret(),
        antigravity_token_url(),
        code,
        redirect_uri,
    )
    .await?;
    let access_token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_string);
    let expires_in = tokens.get("expires_in").and_then(Value::as_i64);
    let scope = tokens
        .get("scope")
        .and_then(Value::as_str)
        .map(str::to_string);

    let user_info = match reqwest::Client::new()
        .get(format!("{}?alt=json", antigravity_user_info_url()))
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            response.json().await.unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };

    let mut project_id = None;
    let mut project_discovery_error = None;
    let mut tier_id: Option<String> = None;
    match reqwest::Client::new()
        .post(antigravity_load_code_assist_endpoint())
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .header(
            "User-Agent",
            crate::core::config::app_constants::agy_cli_user_agent(),
        )
        .json(&json!({ "metadata": antigravity_load_metadata() }))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(payload) => {
                project_id = extract_google_project_id(&payload);
                if project_id.is_none() {
                    project_discovery_error =
                        Some("Antigravity project discovery returned no project id".to_string());
                }
                if let Some(default_tier) = payload
                    .get("allowedTiers")
                    .and_then(Value::as_array)
                    .and_then(|tiers| {
                        tiers.iter().find_map(|tier| {
                            if tier.get("isDefault").and_then(Value::as_bool) == Some(true) {
                                tier.get("id")
                                    .and_then(Value::as_str)
                                    .map(str::trim)
                                    .filter(|value| !value.is_empty())
                                    .map(str::to_string)
                            } else {
                                None
                            }
                        })
                    })
                {
                    tier_id = Some(default_tier);
                }
            }
            Err(error) => {
                project_discovery_error = Some(format!(
                    "Antigravity project discovery response was invalid: {error}"
                ));
            }
        },
        Ok(response) => {
            project_discovery_error = Some(format!(
                "Antigravity project discovery failed with HTTP {}",
                response.status()
            ));
        }
        Err(error) => {
            project_discovery_error = Some(format!(
                "Antigravity project discovery request failed: {error}"
            ));
        }
    }

    if project_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(project_discovery_error.unwrap_or_else(|| {
            "requires_manual_project: Antigravity CLI found no Cloud Code project. Create a free GCP project and reconnect.".to_string()
        }));
    }
    let mut provider_specific_data = std::collections::BTreeMap::new();
    if let Some(tier) = tier_id {
        provider_specific_data.insert("tierId".to_string(), Value::String(tier));
    }
    provider_specific_data.insert(
        "clientProfile".to_string(),
        Value::String("cli".to_string()),
    );
    Ok(ProviderConnection {
        provider: "antigravity".to_string(),
        auth_type: "oauth".to_string(),
        email: user_info
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string),
        access_token: Some(access_token),
        refresh_token,
        expires_at: expires_in.map(crate::oauth::expires_at_from_seconds),
        scope,
        project_id,
        test_status: Some("active".to_string()),
        last_error: None,
        last_error_at: None,
        provider_specific_data,
        ..Default::default()
    })
}

async fn exchange_gitlab_compat(
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    meta: Option<&Value>,
) -> Result<ProviderConnection, String> {
    let base_url = exchange_meta_string(meta, "baseUrl")
        .unwrap_or_else(|| GITLAB_DEFAULT_BASE.to_string())
        .trim_end_matches('/')
        .to_string();
    let client_id = exchange_meta_string(meta, "clientId").unwrap_or_default();
    let client_secret = exchange_meta_string(meta, "clientSecret").unwrap_or_default();

    let mut body = vec![
        ("client_id", client_id.clone()),
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("code_verifier", code_verifier.to_string()),
    ];
    if !client_secret.is_empty() {
        body.push(("client_secret", client_secret));
    }

    let response = reqwest::Client::new()
        .post(format!("{base_url}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&body)
        .send()
        .await
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        let error = response.text().await.unwrap_or_default();
        return Err(format!("GitLab token exchange failed: {error}"));
    }

    let tokens: Value = response
        .json()
        .await
        .map_err(|error| format!("GitLab token exchange failed: {error}"))?;
    let access_token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_string);
    let expires_in = tokens.get("expires_in").and_then(Value::as_i64);
    let scope = tokens
        .get("scope")
        .and_then(Value::as_str)
        .map(str::to_string);

    let user = match reqwest::Client::new()
        .get(format!("{base_url}/api/v4/user"))
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            response.json().await.unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };

    Ok(ProviderConnection {
        provider: "gitlab".to_string(),
        auth_type: "oauth".to_string(),
        access_token: Some(access_token),
        refresh_token,
        expires_at: expires_in.map(crate::oauth::expires_at_from_seconds),
        scope,
        test_status: Some("active".to_string()),
        provider_specific_data: std::collections::BTreeMap::from([
            (
                "username".to_string(),
                Value::String(
                    user.get("username")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ),
            ),
            (
                "email".to_string(),
                Value::String(
                    user.get("email")
                        .or_else(|| user.get("public_email"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ),
            ),
            (
                "name".to_string(),
                Value::String(
                    user.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ),
            ),
            ("baseUrl".to_string(), Value::String(base_url)),
            ("clientId".to_string(), Value::String(client_id)),
            ("authKind".to_string(), Value::String("oauth".to_string())),
        ]),
        ..Default::default()
    })
}

async fn exchange_cline_compat(
    code: &str,
    redirect_uri: &str,
    provider_id: &str,
) -> Result<ProviderConnection, String> {
    let tokens = if let Some(decoded) = decode_cline_exchange_code(code) {
        decoded
    } else {
        let response = reqwest::Client::new()
            .post(cline_token_url())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&json!({
                "grant_type": "authorization_code",
                "code": code,
                "client_type": "extension",
                "redirect_uri": redirect_uri,
            }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            let error = response.text().await.unwrap_or_default();
            return Err(format!("Cline token exchange failed: {error}"));
        }
        let data: Value = response
            .json()
            .await
            .map_err(|error| format!("Cline token exchange failed: {error}"))?;
        json!({
            "access_token": data
                .get("data")
                .and_then(|value| value.get("accessToken"))
                .or_else(|| data.get("accessToken"))
                .cloned()
                .unwrap_or(Value::String(String::new())),
            "refresh_token": data
                .get("data")
                .and_then(|value| value.get("refreshToken"))
                .or_else(|| data.get("refreshToken"))
                .cloned()
                .unwrap_or(Value::Null),
            "email": data
                .get("data")
                .and_then(|value| value.get("userInfo"))
                .and_then(|value| value.get("email"))
                .cloned()
                .unwrap_or(Value::String(String::new())),
            "expires_at": data
                .get("data")
                .and_then(|value| value.get("expiresAt"))
                .or_else(|| data.get("expiresAt"))
                .cloned()
                .unwrap_or(Value::Null),
        })
    };

    let access_token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_string);
    let email = tokens
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut provider_specific_data = std::collections::BTreeMap::new();
    if let Some(first_name) = tokens
        .get("firstName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        provider_specific_data.insert(
            "firstName".to_string(),
            Value::String(first_name.to_string()),
        );
    }
    if let Some(last_name) = tokens
        .get("lastName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        provider_specific_data.insert("lastName".to_string(), Value::String(last_name.to_string()));
    }

    Ok(ProviderConnection {
        provider: provider_id.to_string(),
        auth_type: "oauth".to_string(),
        email,
        access_token: Some(access_token),
        refresh_token,
        expires_at: cline_expires_in(tokens.get("expires_at").and_then(Value::as_str))
            .map(crate::oauth::expires_at_from_seconds),
        test_status: Some("active".to_string()),
        provider_specific_data,
        ..Default::default()
    })
}

async fn exchange_oauth_compat(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid or empty request body" })),
            )
                .into_response()
        }
    };

    let body: OAuthExchangeCompatBody = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid or empty request body" })),
            )
                .into_response()
        }
    };

    let code = body.code.as_deref().map(str::trim).unwrap_or_default();
    let redirect_uri = body
        .redirect_uri
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let code_verifier = body
        .code_verifier
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let meta = body.meta.as_ref();

    if code.is_empty()
        || redirect_uri.is_empty()
        || (code_verifier.is_empty() && provider != "cline" && provider != "clinepass")
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing required fields" })),
        )
            .into_response();
    }

    let connection = match provider.as_str() {
        "claude" => {
            match exchange_claude_compat(code, redirect_uri, code_verifier, body.state.as_deref())
                .await
            {
                Ok(value) => value,
                Err(error) => return internal_error_response(error),
            }
        }
        "codex" => match exchange_codex_compat(code, redirect_uri, code_verifier).await {
            Ok(value) => value,
            Err(error) => return internal_error_response(error),
        },
        "gitlab" => match exchange_gitlab_compat(code, redirect_uri, code_verifier, meta).await {
            Ok(value) => value,
            Err(error) => return internal_error_response(error),
        },
        "antigravity" => match exchange_antigravity_compat(code, redirect_uri).await {
            Ok(value) => value,
            Err(error) => return internal_error_response(error),
        },
        "cline" | "clinepass" => {
            // 9router noPkceExchangeProviders: ["cline","clinepass","kimchi"]
            // — clinepass shares Cline's endpoints (registry clinepass.js:49-50)
            // but must store the connection under its own provider id.
            match exchange_cline_compat(code, redirect_uri, &provider).await {
                Ok(value) => value,
                Err(error) => return internal_error_response(error),
            }
        }
        "xai" => match exchange_xai_compat(code, redirect_uri, code_verifier).await {
            Ok(value) => value,
            Err(error) => return internal_error_response(error),
        },
        _ => return internal_error_response(format!("Unknown provider: {provider}")),
    };

    let saved = match create_imported_oauth_connection(&state, connection).await {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    // C22: onboarding is admitted once from the configured-connection
    // lifecycle. Generation never starts or waits for this polling work.
    if saved.provider == "antigravity"
        && crate::core::utils::antigravity_project::antigravity_project_id(&saved).is_some()
    {
        let onboarding = state.antigravity_onboarding.clone();
        let db = state.db.clone();
        let pool = state.client_pool.clone();
        let connection_id = saved.id.clone();
        let generation = crate::oauth::token_refresh::connection_credential_generation(&saved);
        tokio::spawn(async move {
            if let Err(error) = onboarding
                .ensure(db, pool, &connection_id, generation)
                .await
            {
                tracing::warn!(%connection_id, %error, "Antigravity onboarding failed");
            }
        });
    }

    let mut response_connection = serde_json::Map::from_iter([
        ("id".to_string(), Value::String(saved.id)),
        ("provider".to_string(), Value::String(saved.provider)),
    ]);
    if let Some(email) = saved.email {
        response_connection.insert("email".to_string(), Value::String(email));
    }
    if let Some(display_name) = saved.display_name {
        response_connection.insert("displayName".to_string(), Value::String(display_name));
    }

    Json(json!({
        "success": true,
        "connection": Value::Object(response_connection),
    }))
    .into_response()
}

// GET /api/oauth/:provider/start
pub async fn start_oauth_flow(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<StartQuery>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = &api_key.id;

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Unknown provider",
                "unknown_provider",
                &provider,
            )
        }
    };

    let code_verifier = if provider == "xai" {
        generate_code_verifier_with_len(96)
    } else {
        generate_code_verifier()
    };
    let code_challenge = generate_code_challenge(&code_verifier);
    let state_value = generate_state();

    let redirect_uri = query
        .redirect_uri
        .as_deref()
        .unwrap_or("http://localhost:4623/oauth/callback");

    let client_id = if provider == "xai" {
        XAI_CLIENT_ID
    } else {
        "openproxy"
    };
    let auth_url =
        provider_config.build_auth_url(client_id, redirect_uri, &state_value, &code_challenge);

    let now = now_secs();
    let flow = PendingOAuthFlow {
        state: state_value.clone(),
        code_verifier: code_verifier.clone(),
        provider: provider.clone(),
        account_id: account_id.clone(),
        redirect_uri: Some(redirect_uri.to_string()),
        device_code: None,
        user_code: None,
        created_at: now,
        expires_at: now + PKCE_FLOW_TTL_SECS,
    };

    if state.pending_flows.insert(flow).is_err() {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to store flow",
            "internal_error",
            &provider,
        );
    }

    Json(StartResponse {
        auth_url,
        state: state_value,
        provider: provider.clone(),
        expires_in: PKCE_FLOW_TTL_SECS as u64,
    })
    .into_response()
}

// GET /api/oauth/:provider/callback
pub async fn oauth_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if let Some(error) = &query.error {
        let desc = query.error_description.as_deref().unwrap_or(error);
        return make_error_response(StatusCode::BAD_REQUEST, desc, error, &provider);
    }

    let state_param = match &query.state {
        Some(s) => s,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Missing state parameter",
                "missing_state",
                &provider,
            )
        }
    };

    let code = match &query.code {
        Some(c) => c,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Missing code parameter",
                "missing_code",
                &provider,
            )
        }
    };

    let flow = match state.pending_flows.remove(state_param) {
        Some(f) => f,
        None => {
            return make_error_response(
                StatusCode::NOT_FOUND,
                "Flow not found or expired",
                "flow_not_found",
                &provider,
            )
        }
    };

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Unknown provider",
                "unknown_provider",
                &provider,
            )
        }
    };

    let redirect_uri = flow
        .redirect_uri
        .as_deref()
        .unwrap_or("http://localhost:4623/oauth/callback");

    let token_response = match device_code::exchange_code_for_token(
        &provider_config,
        code,
        &flow.code_verifier,
        redirect_uri,
        "openproxy",
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                &e.error_description.unwrap_or_else(|| e.error.clone()),
                &e.error,
                &provider,
            );
        }
    };

    if let Err(e) = store_connection(
        &state.db,
        &flow.account_id,
        &provider,
        &token_response,
        Some(redirect_uri),
    )
    .await
    {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to store connection: {}", e),
            "storage_error",
            &provider,
        );
    }

    Json(CallbackResponse {
        success: true,
        provider: provider.clone(),
        message: "OAuth flow completed successfully".to_string(),
    })
    .into_response()
}

// POST /api/oauth/:provider/device_code
pub async fn start_device_code(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(_query): Query<DeviceCodeBody>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    if !is_device_code_provider(&provider) {
        return make_error_response(
            StatusCode::BAD_REQUEST,
            "Provider does not support device code flow",
            "unsupported_flow",
            &provider,
        );
    }

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Unknown provider",
                "unknown_provider",
                &provider,
            )
        }
    };

    // KiloCode uses a custom device auth flow (initiateUrl + pollUrlBase)
    let device_resp = if provider == "kilocode" {
        match device_code::kilocode_start_device_flow(&provider_config).await {
            Ok(resp) => resp,
            Err(e) => {
                return make_error_response(
                    StatusCode::BAD_REQUEST,
                    &e.error_description.unwrap_or_else(|| e.error.clone()),
                    &e.error,
                    &provider,
                );
            }
        }
    } else {
        let client_id = provider_config
            .get_param("client_id")
            .unwrap_or("openproxy")
            .to_string();

        match device_code::start_device_flow(&provider_config, &client_id).await {
            Ok(resp) => resp,
            Err(e) => {
                return make_error_response(
                    StatusCode::BAD_REQUEST,
                    &e.error_description.unwrap_or_else(|| e.error.clone()),
                    &e.error,
                    &provider,
                );
            }
        }
    };

    let now = now_secs();
    let flow = PendingOAuthFlow {
        state: device_resp.device_code.clone(),
        code_verifier: String::new(),
        provider: provider.clone(),
        account_id: account_id.clone(),
        redirect_uri: None,
        device_code: Some(device_resp.device_code.clone()),
        user_code: Some(device_resp.user_code.clone()),
        created_at: now,
        expires_at: now + DEVICE_FLOW_TTL_SECS,
    };

    if state.pending_flows.insert(flow).is_err() {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to store flow",
            "internal_error",
            &provider,
        );
    }

    Json(DeviceCodeResponse {
        device_code: device_resp.device_code,
        user_code: device_resp.user_code,
        verification_uri: device_resp.verification_uri,
        interval: device_resp.interval,
        expires_in: device_resp.expires_in.unwrap_or(DEVICE_FLOW_TTL_SECS) as u64,
    })
    .into_response()
}

// POST /api/oauth/:provider/poll
pub async fn poll_device_code(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let (parts, body_stream) = request.into_parts();
    let headers = parts.headers;

    let body = match axum::body::to_bytes(body_stream, 8 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid request body",
                "invalid_body",
                &provider,
            );
        }
    };
    let body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid JSON body",
                "invalid_body",
                &provider,
            );
        }
    };

    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    let device_code = match body.get("device_code").and_then(|v| v.as_str()) {
        Some(code) => code.trim().to_string(),
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Missing device_code in request body",
                "missing_device_code",
                &provider,
            );
        }
    };

    let _account_id = account_id;

    let pending_flow = state.pending_flows.get(&device_code);
    let flow = match pending_flow {
        Some(f) => f,
        None => {
            return make_error_response(
                StatusCode::NOT_FOUND,
                "Device code flow not found or expired",
                "flow_not_found",
                &provider,
            );
        }
    };

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Unknown provider",
                "unknown_provider",
                &provider,
            )
        }
    };

    let user_code = flow.user_code.clone().unwrap_or_default();
    let interval = provider_config
        .get_param("interval")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5);

    let token_response = if provider == "kilocode" {
        match device_code::kilocode_poll_for_token(&provider_config, &device_code).await {
            Ok(resp) => resp,
            Err(e) => {
                let pending = e.error == "authorization_pending" || e.error == "slow_down";
                return Json(json!({
                    "success": false,
                    "error": e.error,
                    "errorDescription": e.error_description,
                    "pending": pending,
                }))
                .into_response();
            }
        }
    } else {
        match device_code::poll_for_token(&provider_config, &device_code, &user_code, interval)
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let pending = e.error == "authorization_pending" || e.error == "slow_down";
                return Json(json!({
                    "success": false,
                    "error": e.error,
                    "errorDescription": e.error_description,
                    "pending": pending,
                }))
                .into_response();
            }
        }
    };

    // GitHub special: exchange OAuth token for Copilot token
    let final_token_response = if provider == "github" {
        match device_code::exchange_github_copilot_token(&token_response.access_token).await {
            Ok(copilot_token) => copilot_token,
            Err(e) => {
                return make_error_response(
                    StatusCode::BAD_REQUEST,
                    &format!(
                        "Copilot token exchange failed: {}",
                        e.error_description.unwrap_or_else(|| e.error.clone())
                    ),
                    "copilot_exchange_failed",
                    &provider,
                );
            }
        }
    } else {
        token_response
    };

    // Kimi device-flow parity (kimi.js): mint a stable deviceId at
    // login and persist it in providerSpecificData so refreshes and
    // usage fetches keep the same X-Msh-Device-Id across restarts.
    let mut extra_psd = std::collections::BTreeMap::new();
    if provider == "kimi" || provider == "kimi-coding" {
        let device_id = uuid::Uuid::new_v4().to_string();
        extra_psd.insert("deviceId".to_string(), json!(device_id));
    }

    if let Err(e) = store_connection(
        &state.db,
        &flow.account_id,
        &provider,
        &final_token_response,
        None,
    )
    .await
    {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to store connection: {}", e),
            "storage_error",
            &provider,
        );
    }

    if !extra_psd.is_empty() {
        if let Some(conn) = state
            .db
            .snapshot()
            .provider_connections
            .iter()
            .find(|c| c.provider == provider && c.id == flow.account_id)
            .cloned()
        {
            let _ = state
                .db
                .update(move |db| {
                    if let Some(target) =
                        db.provider_connections.iter_mut().find(|c| c.id == conn.id)
                    {
                        for (k, v) in extra_psd {
                            target.provider_specific_data.insert(k, v);
                        }
                    }
                })
                .await;
        }
    }

    Json(PollResponse {
        success: true,
        provider: provider.clone(),
        expires_in: final_token_response.expires_in.map(|e| e as u64),
        pending: Some(false),
        retry_after: None,
        message: Some("Authorization successful".to_string()),
    })
    .into_response()
}

// POST /api/oauth/:provider/refresh
pub async fn refresh_token(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let headers = request.headers();
    let api_key = match require_api_key_with_reload(headers, &state.db).await {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    let body_bytes = match axum::body::to_bytes(request.into_body(), 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid request body",
                "invalid_body",
                &provider,
            );
        }
    };
    let body: RefreshBody = match serde_json::from_slice(&body_bytes) {
        Ok(b) => b,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid JSON body",
                "invalid_body",
                &provider,
            );
        }
    };

    let snapshot = state.db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|conn| conn.provider == provider && conn.id.contains(&account_id))
        .cloned();

    let refresh_token = match body.refresh_token {
        Some(ref token) => token.clone(),
        None => connection
            .as_ref()
            .and_then(|c| c.refresh_token.clone())
            .unwrap_or_default(),
    };

    if refresh_token.is_empty() {
        return make_error_response(
            StatusCode::BAD_REQUEST,
            "No refresh token available",
            "no_refresh_token",
            &provider,
        );
    }

    // 9router parity (tokenRefresh/providers.js REFRESH_PROFILES): every
    // provider has its own refresh wire format — claude posts JSON to
    // api.anthropic.com, codex via refreshCodexToken. The generic
    // form-encoded grant with client_id "openproxy" only ever worked for
    // Auth0-style endpoints, so route through the per-provider dispatcher.
    let provider_specific_data = connection
        .as_ref()
        .map(|c| c.provider_specific_data.clone())
        .unwrap_or_default();
    let token_response = if let Some(configured) = connection.as_ref() {
        let observed = crate::oauth::token_refresh::connection_credential_generation(configured);
        let coordinated = match crate::oauth::token_refresh::CONNECTION_REFRESH_COORDINATOR
            .refresh_connection_with_token(
                state.db.clone(),
                &provider,
                &configured.id,
                observed,
                Some(refresh_token.clone()),
            )
            .await
        {
            Ok(result) => result.connection,
            Err(e) => {
                return make_error_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("Refresh failed: {}", e),
                    "refresh_failed",
                    &provider,
                );
            }
        };
        TokenResponse {
            access_token: coordinated.access_token.unwrap_or_default(),
            expires_in: coordinated.expires_in,
            refresh_token: coordinated.refresh_token,
            id_token: coordinated.id_token,
            token_type: coordinated.token_type,
            scope: coordinated.scope,
        }
    } else {
        // Bootstrap compatibility: there is no configured connection identity
        // to coordinate yet. The result is persisted immediately below.
        let refreshed = match crate::oauth::token_refresh::refresh_unconfigured_connection(
            &provider,
            &refresh_token,
            &provider_specific_data,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                return make_error_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("Refresh failed: {}", e),
                    "refresh_failed",
                    &provider,
                );
            }
        };
        let response = TokenResponse {
            access_token: refreshed.access_token,
            expires_in: refreshed.expires_in,
            refresh_token: refreshed.refresh_token,
            id_token: None,
            token_type: None,
            scope: None,
        };
        if let Err(e) = store_connection(&state.db, &account_id, &provider, &response, None).await {
            return make_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Failed to store connection: {}", e),
                "storage_error",
                &provider,
            );
        }
        response
    };

    Json(RefreshResponse {
        success: true,
        access_token: token_response.access_token.clone(),
        expires_in: token_response.expires_in.unwrap_or(3600) as u64,
        refresh_token: token_response.refresh_token.or(Some(refresh_token)),
    })
    .into_response()
}

// GET /api/oauth/:provider/status
pub async fn oauth_status(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key = match require_api_key_with_reload(&headers, &state.db).await {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    let snapshot = state.db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|conn| conn.provider == provider && conn.id.contains(&account_id));

    match connection {
        Some(conn) => {
            let needs_refresh = crate::oauth::needs_refresh(&conn.expires_at);
            Json(StatusResponse {
                provider: provider.clone(),
                connected: true,
                auth_type: conn.auth_type.clone(),
                expires_at: conn.expires_at.clone(),
                needs_refresh: Some(needs_refresh),
                scope: conn.scope.clone(),
            })
            .into_response()
        }
        None => Json(StatusResponse {
            provider: provider.clone(),
            connected: false,
            auth_type: "oauth".to_string(),
            expires_at: None,
            needs_refresh: None,
            scope: None,
        })
        .into_response(),
    }
}

/// POST /api/oauth/codex/bulk-import
/// Bulk import multiple codex (OAuth) account JSON objects in one call.
///
/// Body accepts any of:
///   - Array:    [{...}, {...}]
///   - Single:   {...}
///   - Wrapped:  { accounts: [{...}, ...] }
///
/// Each item must contain at least `accessToken`, directly or as
/// `tokens.access_token`. Missing email / chatgpt account info is best-effort
/// backfilled from the JWT (idToken or accessToken).
///
/// Tokens are NEVER echoed back in the response.
fn normalize_codex_import_item(
    mut item: serde_json::Map<String, Value>,
) -> Result<Value, &'static str> {
    if item
        .get("auth_mode")
        .is_some_and(|value| !value.is_null() && value.as_str() != Some("chatgpt"))
    {
        return Err("Unexpected auth_mode (expected chatgpt)");
    }

    let tokens = item
        .get("tokens")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    for (canonical, snake_case) in [
        ("accessToken", "access_token"),
        ("refreshToken", "refresh_token"),
        ("idToken", "id_token"),
    ] {
        let value = tokens
            .get(snake_case)
            .or_else(|| item.get(snake_case))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if let Some(value) = value {
            item.insert(canonical.to_string(), Value::String(value));
        }
    }

    if !item.contains_key("lastRefreshAt") {
        if let Some(value) = item
            .get("last_refresh")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        {
            item.insert("lastRefreshAt".to_string(), Value::String(value));
        }
    }

    let account_id = tokens
        .get("account_id")
        .or_else(|| item.get("account_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(account_id) = account_id {
        let provider_data = item
            .entry("providerSpecificData".to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(provider_data) = provider_data.as_object_mut() {
            provider_data
                .entry("chatgptAccountId".to_string())
                .or_insert(Value::String(account_id));
        }
    }

    Ok(Value::Object(item))
}

async fn codex_bulk_import(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 512 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return internal_error_response(error.to_string()),
    };
    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Invalid JSON body: {error}") })),
            )
                .into_response();
        }
    };

    // Normalize to array
    let accounts: Option<Vec<Value>> = if let Some(arr) = body.as_array() {
        Some(arr.clone())
    } else if let Some(arr) = body.get("accounts").and_then(Value::as_array) {
        Some(arr.clone())
    } else if body.is_object() {
        Some(vec![body.clone()])
    } else {
        None
    };
    let Some(accounts) = accounts else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "No accounts provided" })),
        )
            .into_response();
    };
    if accounts.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "No accounts provided" })),
        )
            .into_response();
    }

    let mut results: Vec<serde_json::Value> = Vec::with_capacity(accounts.len());
    let mut success = 0usize;
    let mut failed = 0usize;

    // SERIAL loop — createProviderConnection reads max(priority) and reorders
    // inside a transaction. Parallel calls would race on priority assignment.
    for (idx, raw) in accounts.iter().enumerate() {
        // Strip server-controlled fields
        let Some(raw_obj) = raw.as_object() else {
            failed += 1;
            results.push(json!({ "index": idx, "ok": false, "error": "Item is not an object" }));
            continue;
        };
        let mut item = raw_obj.clone();
        for key in ["id", "provider", "authType", "createdAt", "updatedAt"] {
            item.remove(key);
        }
        let normalized = match normalize_codex_import_item(item) {
            Ok(value) => value,
            Err(error) => {
                failed += 1;
                results.push(json!({ "index": idx, "ok": false, "error": error }));
                continue;
            }
        };
        let access_token = normalized
            .get("accessToken")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if access_token.is_empty() {
            failed += 1;
            results.push(json!({ "index": idx, "ok": false, "error": "Missing accessToken" }));
            continue;
        }

        // Backfill missing identity fields from JWT claims (idToken or accessToken).
        let mut psd_map: std::collections::BTreeMap<String, Value> = normalized
            .get("providerSpecificData")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let email = normalized
            .get("email")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let needs_backfill = email.is_none()
            || !psd_map.contains_key("chatgptAccountId")
            || !psd_map.contains_key("chatgptPlanType");
        let email = if needs_backfill {
            let source = normalized
                .get("idToken")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(&access_token);
            let (back_email, back_psd) = extract_codex_account_info(Some(source));
            // Merge caller PSD: backfill only fills gaps, caller keys win.
            for (k, v) in back_psd {
                psd_map.entry(k).or_insert(v);
            }
            email.or(back_email)
        } else {
            email
        };

        // Compute expiresAt from expiresIn if absent
        let expires_at = normalized
            .get("expiresAt")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                normalized
                    .get("expiresIn")
                    .and_then(Value::as_i64)
                    .filter(|n| *n > 0)
                    .map(|n| (chrono::Utc::now() + chrono::Duration::seconds(n)).to_rfc3339())
            });

        let mut connection = ProviderConnection {
            provider: "codex".to_string(),
            auth_type: "oauth".to_string(),
            email: email.clone(),
            access_token: Some(access_token),
            refresh_token: normalized
                .get("refreshToken")
                .and_then(Value::as_str)
                .map(str::to_string),
            id_token: normalized
                .get("idToken")
                .and_then(Value::as_str)
                .map(str::to_string),
            expires_at,
            test_status: Some(
                normalized
                    .get("testStatus")
                    .and_then(Value::as_str)
                    .unwrap_or("active")
                    .to_string(),
            ),
            is_active: Some(
                normalized
                    .get("isActive")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            ),
            provider_specific_data: psd_map,
            ..Default::default()
        };
        // JS bulk-import sets item.lastRefreshAt = now when absent.
        let last_refresh_at = normalized
            .get("lastRefreshAt")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        connection
            .extra
            .insert("lastRefreshAt".to_string(), Value::String(last_refresh_at));

        match create_imported_oauth_connection(&state, connection).await {
            Ok(conn) => {
                success += 1;
                results.push(json!({ "index": idx, "ok": true, "id": conn.id }));
            }
            Err(e) => {
                failed += 1;
                results.push(json!({ "index": idx, "ok": false, "error": e.to_string() }));
            }
        }
    }

    Json(json!({ "success": success, "failed": failed, "results": results })).into_response()
}

/// POST /api/oauth/codex/import-token
/// Import a ChatGPT access token (created from chatgpt.com settings)
/// as a provider connection, bypassing OAuth refresh flow.
///
/// Body: { accessToken: string, name?: string }
async fn codex_import_token(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let Some(access_token) = body.get("accessToken").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Access token is required" })),
        )
            .into_response();
    };
    if access_token.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Access token is required" })),
        )
            .into_response();
    }

    let token = access_token.trim().to_string();

    // Extract account info from the JWT (email, workspace, plan)
    let mut email: Option<String> = None;
    let mut provider_specific_data: std::collections::BTreeMap<String, Value> =
        std::collections::BTreeMap::from([(
            "authMethod".to_string(),
            Value::String("access_token".to_string()),
        )]);

    // Try decoding as JWT to extract email + workspace
    if let Some(payload) = decode_jwt_claims(&token) {
        let auth = payload
            .get("https://api.openai.com/auth")
            .and_then(Value::as_object);
        let profile = payload
            .get("https://api.openai.com/profile")
            .and_then(Value::as_object);
        email = profile
            .and_then(|p| p.get("email"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                payload
                    .get("email")
                    .or_else(|| payload.get("preferred_username"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });

        if let Some(account_id) = auth
            .and_then(|a| a.get("chatgpt_account_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            provider_specific_data.insert(
                "chatgptAccountId".to_string(),
                Value::String(account_id.to_string()),
            );
        }
        if let Some(plan_type) = auth
            .and_then(|a| a.get("chatgpt_plan_type"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            provider_specific_data.insert(
                "chatgptPlanType".to_string(),
                Value::String(plan_type.to_string()),
            );
        }
        // Store expiry info from JWT if available
        if let Some(exp) = payload.get("exp") {
            provider_specific_data.insert("jwtExp".to_string(), exp.clone());
        }
    }

    // Also try extractCodexAccountInfo via id_token-style extraction
    // (the access token itself may contain the same claims)
    if email.is_none() {
        let (back_email, back_psd) = extract_codex_account_info(Some(&token));
        if let Some(back_email) = back_email {
            email = Some(back_email);
        }
        for (k, v) in back_psd {
            provider_specific_data.entry(k).or_insert(v);
        }
    }

    let name = body
        .get("name")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            email
                .clone()
                .unwrap_or_else(|| "ChatGPT Access Token".to_string())
        });

    // Save to database as access_token authType (no refresh token)
    let connection = ProviderConnection {
        provider: "codex".to_string(),
        auth_type: "access_token".to_string(),
        name: Some(name.clone()),
        email: email.clone(),
        access_token: Some(token),
        expires_at: Some((chrono::Utc::now() + chrono::Duration::seconds(86_400)).to_rfc3339()),
        test_status: Some("active".to_string()),
        provider_specific_data,
        ..Default::default()
    };

    match create_imported_oauth_connection(&state, connection).await {
        Ok(connection) => {
            let workspace = connection
                .provider_specific_data
                .get("chatgptAccountId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let plan = connection
                .provider_specific_data
                .get("chatgptPlanType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Json(json!({
                "success": true,
                "connection": {
                    "id": connection.id,
                    "provider": connection.provider,
                    "email": connection.email,
                    "name": connection.name,
                    "workspace": if workspace.is_empty() { Value::Null } else { Value::String(workspace) },
                    "plan": if plan.is_empty() { Value::Null } else { Value::String(plan) }
                }
            }))
            .into_response()
        }
        Err(error) => internal_error_response(error.to_string()),
    }
}

/// POST /api/oauth/grok-cli/bulk-import
/// Bulk import multiple Grok CLI (OAuth/Device) account JSON objects in one call.
///
/// Body accepts any of:
///   - Array:    [{...}, {...}]
///   - Single:   {...}
///   - Wrapped:  { accounts: [{...}, ...] }
///
/// Each item accepts snake_case or camelCase:
///   access_token / accessToken
///   refresh_token / refreshToken
///   id_token / idToken
///   email
///   expires_in / expiresIn / expires_at / expiresAt
async fn grok_cli_bulk_import(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 512 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return internal_error_response(error.to_string()),
    };
    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Invalid JSON body: {error}") })),
            )
                .into_response();
        }
    };

    let accounts: Option<Vec<Value>> = if let Some(arr) = body.as_array() {
        Some(arr.clone())
    } else if let Some(arr) = body.get("accounts").and_then(Value::as_array) {
        Some(arr.clone())
    } else if body.is_object() {
        Some(vec![body.clone()])
    } else {
        None
    };
    let Some(accounts) = accounts else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "No accounts provided" })),
        )
            .into_response();
    };
    if accounts.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "No accounts provided" })),
        )
            .into_response();
    }

    let mut results: Vec<serde_json::Value> = Vec::with_capacity(accounts.len());
    let mut success = 0usize;
    let mut failed = 0usize;

    for (idx, raw) in accounts.iter().enumerate() {
        let outcome: Result<ProviderConnection, String> = (|| {
            let raw = raw.as_object().ok_or("Item is not an object")?;

            let access_token = raw
                .get("access_token")
                .or_else(|| raw.get("accessToken"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or("Missing access_token / accessToken")?
                .to_string();
            let refresh_token = raw
                .get("refresh_token")
                .or_else(|| raw.get("refreshToken"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let id_token = raw
                .get("id_token")
                .or_else(|| raw.get("idToken"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let mut email = raw.get("email").and_then(Value::as_str).map(str::to_string);

            if email.is_none() {
                email = id_token
                    .as_deref()
                    .and_then(|token| decode_xai_id_token_email(Some(token)))
                    .or_else(|| extract_email_from_access_token(&access_token));
            }

            let expires_at = raw
                .get("expires_at")
                .or_else(|| raw.get("expiresAt"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    raw.get("expires_in")
                        .or_else(|| raw.get("expiresIn"))
                        .and_then(Value::as_i64)
                        .filter(|n| *n > 0)
                        .map(|n| (chrono::Utc::now() + chrono::Duration::seconds(n)).to_rfc3339())
                });

            let mut psd: std::collections::BTreeMap<String, Value> =
                std::collections::BTreeMap::from([(
                    "authMethod".to_string(),
                    Value::String("device_code".to_string()),
                )]);
            if let Some(id_token) = &id_token {
                psd.insert("idToken".to_string(), Value::String(id_token.clone()));
            }
            if let Some(email) = &email {
                psd.insert("email".to_string(), Value::String(email.clone()));
            }
            if let Some(caller_psd) = raw.get("providerSpecificData").and_then(Value::as_object) {
                for (k, v) in caller_psd {
                    psd.insert(k.clone(), v.clone());
                }
            }

            Ok(ProviderConnection {
                provider: "grok-cli".to_string(),
                auth_type: "oauth".to_string(),
                email: email.clone(),
                display_name: raw
                    .get("displayName")
                    .or_else(|| raw.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                access_token: Some(access_token),
                refresh_token,
                id_token,
                expires_at,
                test_status: Some("active".to_string()),
                provider_specific_data: psd,
                ..Default::default()
            })
        })();

        match outcome {
            Err(err) => {
                failed += 1;
                results.push(json!({ "index": idx, "ok": false, "error": err }));
            }
            Ok(connection) => match create_imported_oauth_connection(&state, connection).await {
                Ok(created) => {
                    success += 1;
                    results.push(
                        json!({ "index": idx, "ok": true, "id": created.id, "email": created.email }),
                    );
                }
                Err(err) => {
                    failed += 1;
                    results.push(json!({ "index": idx, "ok": false, "error": err.to_string() }));
                }
            },
        }
    }

    Json(json!({
        "total": accounts.len(),
        "success": success,
        "failed": failed,
        "results": results
    }))
    .into_response()
}

/// Decode an xAI id token to an email (email | preferred_username | sub).
fn decode_xai_id_token_email(id_token: Option<&str>) -> Option<String> {
    let claims = decode_jwt_claims(id_token?)?;
    claims
        .get("email")
        .or_else(|| claims.get("preferred_username"))
        .or_else(|| claims.get("sub"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Extract an email from an access-token JWT payload (best effort).
fn extract_email_from_access_token(access_token: &str) -> Option<String> {
    decode_xai_id_token_email(Some(access_token))
}

/// POST /api/oauth/xiaomi-mimo/api-key
/// Import a Xiaomi MiMo API key manually (or from auto-import).
/// The key is validated against the models endpoint, then stored.
///
/// Body: { apiKey, uid?, baseUrl? }
async fn xiaomi_mimo_api_key_import(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return internal_error_response(error.to_string()),
    };
    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let api_key = body
        .get("apiKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(api_key) = api_key else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "API key is required" })),
        )
            .into_response();
    };
    if !api_key.starts_with("sk-") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid key format — expected sk- prefix" })),
        )
            .into_response();
    }
    let key = api_key.to_string();

    let uid = body
        .get("uid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let effective_base_url = body
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://api.xiaomimimo.com/v1")
        .trim_end_matches('/')
        .to_string();
    let mimo_pass_token = body
        .get("mimoPassToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mimo_user_id = body
        .get("mimoUserId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mimo_c_user_id = body
        .get("mimoCUserId")
        .and_then(Value::as_str)
        .map(str::to_string);

    // Validate the key against the models endpoint
    let mut validated = false;
    let mut model_count: i64 = 0;
    match tokio::time::timeout(
        Duration::from_secs(10),
        reqwest::Client::new()
            .get(format!("{effective_base_url}/models"))
            .header("Authorization", format!("Bearer {key}"))
            .header("X-Mimo-Source", "mimocode-cli")
            .send(),
    )
    .await
    {
        Ok(Ok(resp)) if resp.status().is_success() => {
            if let Ok(data) = resp.json::<Value>().await {
                model_count = data
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|arr| arr.len() as i64)
                    .unwrap_or(0);
                validated = true;
            }
        }
        // Network error — still allow import (key may be valid but network blocked)
        _ => {
            tracing::info!("[xiaomi-mimo] key validation failed, storing as untested");
        }
    }

    let expected_email = uid.as_deref().map(|uid| format!("{uid}@xiaomi"));

    // Dedup: if a connection with the same uid or same key already exists, update it
    let existing_id: Option<String> = state
        .db
        .snapshot()
        .provider_connections
        .iter()
        .find(|c| {
            c.provider == "xiaomi-mimo"
                && (expected_email
                    .as_deref()
                    .is_some_and(|email| c.email.as_deref() == Some(email))
                    || c.access_token.as_deref() == Some(key.as_str()))
        })
        .map(|c| c.id.clone());
    if let Some(existing_id) = existing_id {
        let update_result = state
            .db
            .update({
                let key = key.clone();
                let uid = uid.clone();
                let effective_base_url = effective_base_url.clone();
                let mimo_pass_token = mimo_pass_token.clone();
                let mimo_user_id = mimo_user_id.clone();
                let mimo_c_user_id = mimo_c_user_id.clone();
                let existing_id = existing_id.clone();
                move |db| {
                    if let Some(target) = db
                        .provider_connections
                        .iter_mut()
                        .find(|c| c.id == existing_id)
                    {
                        target.access_token = Some(key.clone());
                        let psd = &mut target.provider_specific_data;
                        if let Some(uid) = &uid {
                            psd.insert("uid".to_string(), Value::String(uid.clone()));
                        } else if !psd.contains_key("uid") {
                            psd.insert("uid".to_string(), Value::Null);
                        }
                        psd.insert(
                            "baseUrl".to_string(),
                            Value::String(effective_base_url.clone()),
                        );
                        // Per-account session credential — enables multi-account rotation.
                        if let Some(v) = &mimo_pass_token {
                            psd.insert("mimoPassToken".to_string(), Value::String(v.clone()));
                        } else if !psd.contains_key("mimoPassToken") {
                            psd.insert("mimoPassToken".to_string(), Value::Null);
                        }
                        if let Some(v) = &mimo_user_id {
                            psd.insert("mimoUserId".to_string(), Value::String(v.clone()));
                        } else if !psd.contains_key("mimoUserId") {
                            psd.insert("mimoUserId".to_string(), Value::Null);
                        }
                        if let Some(v) = &mimo_c_user_id {
                            psd.insert("mimoCUserId".to_string(), Value::String(v.clone()));
                        } else if !psd.contains_key("mimoCUserId") {
                            psd.insert("mimoCUserId".to_string(), Value::Null);
                        }
                        psd.insert("modelCount".to_string(), Value::Number(model_count.into()));
                        if validated {
                            target.test_status = Some("active".to_string());
                        }
                    }
                }
            })
            .await;
        if let Err(error) = update_result {
            return internal_error_response(error.to_string());
        }
        let updated = state
            .db
            .snapshot()
            .provider_connections
            .iter()
            .find(|c| c.id == existing_id)
            .cloned();
        if let Some(updated) = updated {
            return Json(json!({
                "success": true,
                "validated": validated,
                "modelCount": model_count,
                "updated": true,
                "connection": {
                    "id": updated.id,
                    "provider": updated.provider,
                    "email": updated.email,
                    "displayName": updated.display_name
                }
            }))
            .into_response();
        }
        return internal_error_response("Failed to update provider connection".to_string());
    }

    let mut psd = std::collections::BTreeMap::new();
    psd.insert(
        "uid".to_string(),
        uid.clone().map(Value::String).unwrap_or(Value::Null),
    );
    psd.insert(
        "baseUrl".to_string(),
        Value::String(effective_base_url.clone()),
    );
    psd.insert(
        "authMethod".to_string(),
        Value::String("api_key".to_string()),
    );
    psd.insert("provider".to_string(), Value::String("API Key".to_string()));
    psd.insert("modelCount".to_string(), Value::Number(model_count.into()));
    // Per-account session credential — enables multi-account rotation.
    psd.insert(
        "mimoPassToken".to_string(),
        mimo_pass_token.map(Value::String).unwrap_or(Value::Null),
    );
    psd.insert(
        "mimoUserId".to_string(),
        mimo_user_id.map(Value::String).unwrap_or(Value::Null),
    );
    psd.insert(
        "mimoCUserId".to_string(),
        mimo_c_user_id.map(Value::String).unwrap_or(Value::Null),
    );

    let connection = ProviderConnection {
        provider: "xiaomi-mimo".to_string(),
        auth_type: "api_key".to_string(),
        access_token: Some(key),
        refresh_token: None,
        // API keys don't expire on a fixed schedule; use a long horizon
        expires_at: Some((chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339()),
        email: expected_email.clone(),
        display_name: Some(
            uid.map(|uid| format!("Xiaomi {uid}"))
                .unwrap_or_else(|| "Xiaomi MiMo".to_string()),
        ),
        test_status: Some(if validated {
            "active".to_string()
        } else {
            "untested".to_string()
        }),
        provider_specific_data: psd,
        ..Default::default()
    };

    match create_imported_oauth_connection(&state, connection).await {
        Ok(connection) => Json(json!({
            "success": true,
            "validated": validated,
            "modelCount": model_count,
            "connection": {
                "id": connection.id,
                "provider": connection.provider,
                "email": connection.email,
                "displayName": connection.display_name
            }
        }))
        .into_response(),
        Err(error) => internal_error_response(format!("API key import failed: {error}")),
    }
}

/// Candidate paths for the Xiaomi MiMo Desktop auth.json
/// (MiMoCode / MiMo Desktop shared data dir, cross-platform XDG).
fn xiaomi_mimo_auth_candidates() -> Vec<PathBuf> {
    let home = cursor_home_dir();
    let mut paths = vec![home
        .join(".local")
        .join("share")
        .join("mimocode")
        .join("auth.json")];

    // Windows: also check USERPROFILE-based XDG
    if std::env::consts::OS == "windows" {
        let app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"));
        // Desktop's own storage (may have separate credentials in the future)
        paths.push(app_data.join("Xiaomi MiMo").join("auth.json"));
    }

    // macOS
    if std::env::consts::OS == "macos" {
        paths.push(
            home.join("Library")
                .join("Application Support")
                .join("mimocode")
                .join("auth.json"),
        );
    }

    paths
}

/// Read just the passToken + identity cookies from MiMo Desktop's cookie store.
///
/// Mirrors `readDesktopPassToken` in open-sse/shared/mimoAccount.js: copies the
/// Chromium cookie DB (exclusively locked while Desktop runs, so a copy failure
/// means null) and reads the `passToken`/`userId`/`cUserId` cookies for
/// `account.xiaomi.com`.
fn read_mimo_desktop_pass_token() -> Option<(String, Option<String>, Option<String>)> {
    let home = cursor_home_dir();
    let cookie_src = match std::env::consts::OS {
        "windows" => home
            .join("AppData")
            .join("Roaming")
            .join("Xiaomi MiMo")
            .join("Partitions")
            .join("xiaomi-account")
            .join("Network")
            .join("Cookies"),
        "macos" => home
            .join("Library")
            .join("Application Support")
            .join("Xiaomi MiMo")
            .join("Partitions")
            .join("xiaomi-account")
            .join("Network")
            .join("Cookies"),
        _ => home
            .join(".config")
            .join("Xiaomi MiMo")
            .join("Partitions")
            .join("xiaomi-account")
            .join("Network")
            .join("Cookies"),
    };
    if !cookie_src.is_file() {
        return None;
    }
    let tmp = std::env::temp_dir().join(format!(
        "openproxy-mimo-cookies-{}-{}.db",
        std::process::id(),
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    // Locked by a running Desktop — non-fatal, return null.
    if std::fs::copy(&cookie_src, &tmp).is_err() {
        return None;
    }
    let result = (|| {
        let conn =
            rusqlite::Connection::open_with_flags(&tmp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .ok()?;
        let mut stmt = conn
            .prepare("SELECT name, value FROM cookies WHERE host_key = '.account.xiaomi.com'")
            .ok()?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .ok()?
            .filter_map(|r| r.ok())
            .collect();
        let get = |name: &str| {
            rows.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };
        let pass_token = get("passToken")?;
        Some((pass_token, get("userId"), get("cUserId")))
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

/// GET /api/oauth/xiaomi-mimo/auto-import
/// Auto-detect Xiaomi MiMo credentials from local auth.json.
///
/// Sources (in priority order):
///   1. ~/.local/share/mimocode/auth.json  → xiaomi field
///   2. %APPDATA%/Xiaomi MiMo/...          → (future: Desktop keychain)
///
/// auth.json shape:
/// {
///   "xiaomi": {
///     "type": "api",
///     "key": "sk-xxxx",
///     "metadata": { "uid": "...", "base_url": "https://api.xiaomimimo.com/v1" }
///   }
/// }
async fn xiaomi_mimo_auto_import() -> Response {
    let candidates = xiaomi_mimo_auth_candidates();
    let mut auth_path: Option<PathBuf> = None;
    for candidate in &candidates {
        if std::fs::File::open(candidate).is_ok() {
            auth_path = Some(candidate.clone());
            break;
        }
    }
    let Some(auth_path) = auth_path else {
        let checked = candidates
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Json(json!({
            "found": false,
            "error": format!(
                "Xiaomi MiMo Desktop auth file not found. Checked:\n{checked}\n\nMake sure Xiaomi MiMo Desktop is installed and you are signed in."
            )
        }))
        .into_response();
    };

    // JS: file-read errors fall to the outer catch → HTTP 500.
    let raw = match std::fs::read_to_string(&auth_path) {
        Ok(raw) => raw,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "found": false, "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let auth: Value = match serde_json::from_slice(raw.as_bytes()) {
        Ok(auth) => auth,
        Err(_) => {
            return Json(json!({
                "found": false,
                "error": "auth.json is not valid JSON. Please sign in to Xiaomi MiMo Desktop again."
            }))
            .into_response();
        }
    };

    let xiaomi = auth.get("xiaomi");
    let key = xiaomi
        .and_then(|x| x.get("key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(key) = key else {
        return Json(json!({
            "found": false,
            "error": "No Xiaomi credentials found in auth.json. Please sign in to Xiaomi MiMo Desktop."
        }))
        .into_response();
    };
    if !key.starts_with("sk-") {
        return Json(json!({
            "found": false,
            "error": "Xiaomi key does not appear to be a valid API key (expected sk- prefix)."
        }))
        .into_response();
    }

    let metadata = xiaomi.and_then(|x| x.get("metadata"));
    let uid = metadata
        .and_then(|m| m.get("uid"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let base_url = metadata
        .and_then(|m| m.get("base_url"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://api.xiaomimimo.com/v1")
        .to_string();

    // Account-session passToken from Desktop's cookie store. Persisting it per
    // connection is what lets multiple Xiaomi accounts rotate independently.
    // (null while Desktop is running — its cookie DB is exclusively locked.)
    let (mimo_pass_token, mimo_user_id, mimo_c_user_id) = match read_mimo_desktop_pass_token() {
        Some((pass_token, user_id, c_user_id)) => (Some(pass_token), user_id, c_user_id),
        None => (None, None, None),
    };

    Json(json!({
        "found": true,
        "apiKey": key,
        "uid": uid,
        "baseUrl": base_url,
        "source": auth_path.to_string_lossy().to_string(),
        "mimoPassToken": mimo_pass_token,
        "mimoUserId": mimo_user_id,
        "mimoCUserId": mimo_c_user_id
    }))
    .into_response()
}

/// POST /api/oauth/xai/manual-code
/// Completes an xAI OAuth flow when the user pastes a bare authorization code
/// (or when the fixed-port redirect failed and they copy the code from xAI UI).
async fn xai_manual_code(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return internal_error_response(error.to_string()),
    };
    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let code = body
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let state_param = body
        .get("state")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let Some(code) = code else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing xAI authorization code" })),
        )
            .into_response();
    };
    let Some(state_param) = state_param else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "xAI OAuth session not found; restart the login flow and paste the code again" })),
        )
            .into_response();
    };

    let Some(session) = state.xai_proxy.get_session(&state_param).await else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "xAI OAuth session not found; restart the login flow and paste the code again" })),
        )
            .into_response();
    };

    match exchange_xai_compat(&code, &session.redirect_uri, &session.code_verifier).await {
        Ok(connection) => match create_imported_oauth_connection(&state, connection).await {
            Ok(saved) => {
                state.xai_proxy.clear_session(&state_param).await;
                state.xai_proxy.stop().await;
                let mut response_connection = serde_json::Map::from_iter([
                    ("id".to_string(), Value::String(saved.id)),
                    ("provider".to_string(), Value::String(saved.provider)),
                ]);
                if let Some(email) = saved.email {
                    response_connection.insert("email".to_string(), Value::String(email));
                }
                if let Some(display_name) = saved.display_name {
                    response_connection
                        .insert("displayName".to_string(), Value::String(display_name));
                }
                Json(json!({
                    "success": true,
                    "connection": Value::Object(response_connection),
                }))
                .into_response()
            }
            Err(error) => {
                state.xai_proxy.clear_session(&state_param).await;
                state.xai_proxy.stop().await;
                internal_error_response(error.to_string())
            }
        },
        Err(error) => {
            state.xai_proxy.clear_session(&state_param).await;
            state.xai_proxy.stop().await;
            internal_error_response(error)
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/oauth/codex/bulk-import", post(codex_bulk_import))
        .route("/api/oauth/codex/import-token", post(codex_import_token))
        .route(
            "/api/oauth/cursor/auto-import",
            get(cursor_auto_import_route),
        )
        .route(
            "/api/oauth/cursor/import",
            get(cursor_import_instructions_route).post(cursor_import_auth),
        )
        .route("/api/oauth/xai/manual-code", post(xai_manual_code))
        .route(
            "/api/oauth/grok-cli/bulk-import",
            post(grok_cli_bulk_import),
        )
        .route(
            "/api/oauth/xiaomi-mimo/api-key",
            post(xiaomi_mimo_api_key_import),
        )
        .route(
            "/api/oauth/xiaomi-mimo/auto-import",
            get(xiaomi_mimo_auto_import),
        )
        .route("/api/oauth/gitlab/pat", post(gitlab_pat_auth))
        .route("/api/oauth/{provider}/start", get(start_oauth_flow))
        .route("/api/oauth/{provider}/callback", get(oauth_callback))
        .route(
            "/api/oauth/{provider}/start-proxy",
            get(codex_start_proxy_compat),
        )
        .route(
            "/api/oauth/{provider}/poll-status",
            get(codex_poll_status_compat),
        )
        .route(
            "/api/oauth/{provider}/stop-proxy",
            get(codex_stop_proxy_compat),
        )
        .route(
            "/api/oauth/{provider}/authorize",
            get(authorize_oauth_compat),
        )
        .route(
            "/api/oauth/{provider}/exchange",
            post(exchange_oauth_compat),
        )
        .route("/api/oauth/{provider}/device_code", post(start_device_code))
        .route("/api/oauth/{provider}/poll", post(poll_device_code))
        .route("/api/oauth/{provider}/refresh", post(refresh_token))
        .route("/api/oauth/{provider}/status", get(oauth_status))
}

// ───────────────────────────────────────────────────────────────────────────
// Zed RSA native-app proxy (9router server.js zedProxy parity, bead .102).
// Singleton session; the callback GET http://127.0.0.1:<port>/?user_id=…&
// access_token=<RSA-encrypted> is decrypted with the session's private-key
// verifier and stored as a `zed` oauth connection.
// ───────────────────────────────────────────────────────────────────────────

const ZED_PROXY_TIMEOUT_MS: u64 = 600_000;
const ZED_PREFERRED_PORT: u16 = 58443;

#[derive(Clone)]
pub struct ZedProxyState {
    inner: Arc<Mutex<ZedProxyInner>>,
}

#[derive(Default)]
struct ZedProxyInner {
    server: Option<CodexProxyServer>,
    session: Option<ZedPendingLogin>,
    bound_port: Option<u16>,
}

struct ZedPendingLogin {
    state: String,
    private_key_verifier: String,
    status: String,
    created_at: i64,
    connection_id: Option<String>,
    email: Option<String>,
    error: Option<String>,
}

impl Default for ZedProxyState {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ZedProxyInner::default())),
        }
    }
}

impl ZedProxyState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start the local callback listener on the preferred port (58443),
    /// falling back to an OS-assigned port when busy.
    async fn start(&self, state: AppState, preferred_port: u16) -> Result<u16, String> {
        let mut inner = self.inner.lock().await;
        if let Some(port) = inner.bound_port {
            return Ok(port);
        }

        let listener = match TcpListener::bind(("127.0.0.1", preferred_port)).await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        };
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();

        let proxy_state = self.clone();
        let app_state = state.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let (mut stream, _) = match accepted {
                            Ok(p) => p,
                            Err(_) => break,
                        };
                        let proxy_state = proxy_state.clone();
                        let app_state = app_state.clone();
                        tokio::spawn(async move {
                            handle_zed_proxy_connection(proxy_state, app_state, &mut stream)
                                .await;
                        });
                    }
                }
            }
        });

        let proxy_state2 = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ZED_PROXY_TIMEOUT_MS)).await;
            proxy_state2.stop().await;
        });

        inner.server = Some(CodexProxyServer {
            shutdown_tx: Some(shutdown_tx),
        });
        inner.bound_port = Some(port);
        Ok(port)
    }

    async fn stop(&self) {
        let server = {
            let mut inner = self.inner.lock().await;
            inner.bound_port = None;
            inner.server.take()
        };
        if let Some(mut server) = server {
            if let Some(shutdown_tx) = server.shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }
        }
    }

    async fn get_session(
        &self,
        state: &str,
    ) -> Option<(String, String, Option<String>, Option<String>)> {
        let inner = self.inner.lock().await;
        inner
            .session
            .as_ref()
            .filter(|s| s.state == state)
            .map(|s| {
                (
                    s.status.clone(),
                    s.error.clone().unwrap_or_default(),
                    s.connection_id.clone(),
                    s.email.clone(),
                )
            })
    }

    fn register_session_locked(
        inner: &mut ZedProxyInner,
        state: &str,
        code_verifier: &str,
    ) -> bool {
        if state.trim().is_empty() || code_verifier.trim().is_empty() {
            return false;
        }
        inner.session = Some(ZedPendingLogin {
            state: state.to_string(),
            private_key_verifier: code_verifier.to_string(),
            status: "pending".into(),
            created_at: chrono::Utc::now().timestamp_millis(),
            connection_id: None,
            email: None,
            error: None,
        });
        true
    }
}

/// Accept one callback: parse user_id + access_token, decrypt with the
/// session's verifier, store the `zed` oauth connection, and answer HTML.
async fn handle_zed_proxy_connection(
    proxy_state: ZedProxyState,
    state: AppState,
    stream: &mut tokio::net::TcpStream,
) {
    use crate::oauth::zed_auth;

    let mut buffer = vec![0u8; 64 * 1024];
    let bytes_read = match stream.read(&mut buffer).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let parsed = match url::Url::parse(&format!("http://localhost{target}")) {
        Ok(u) => u,
        Err(_) => {
            write_http_response(
                stream,
                "400 Bad Request",
                &[("Content-Type", "text/plain; charset=utf-8".to_string())],
                "Invalid callback URL",
            )
            .await;
            return;
        }
    };

    if parsed.path() != "/" && parsed.path() != "/callback" {
        write_http_response(
            stream,
            "404 Not Found",
            &[("Content-Type", "text/plain; charset=utf-8".to_string())],
            "Not found",
        )
        .await;
        return;
    }

    // Snapshot + clear the pending session under one lock.
    let session = {
        let mut inner = proxy_state.inner.lock().await;
        inner.session.take()
    };
    let Some(session) = session else {
        write_http_response(
            stream,
            "200 OK",
            &[("Content-Type", "text/html; charset=utf-8".to_string())],
            "<html><body><h3>No active Zed login session</h3></body></html>",
        )
        .await;
        return;
    };

    let user_id = parsed
        .query_pairs()
        .find(|(k, _)| k == "user_id" || k == "userId")
        .map(|(_, v)| v.into_owned());
    let encrypted = parsed
        .query_pairs()
        .find(|(k, _)| matches!(k.as_ref(), "access_token" | "accessToken" | "token"))
        .map(|(_, v)| v.into_owned());

    let (Some(user_id), Some(encrypted)) = (user_id, encrypted) else {
        let mut inner = proxy_state.inner.lock().await;
        inner.session = Some(ZedPendingLogin {
            error: Some("Zed callback must include user_id and access_token".into()),
            ..session
        });
        write_http_response(
            stream,
            "200 OK",
            &[("Content-Type", "text/html; charset=utf-8".to_string())],
            "<html><body><h3>Zed login failed</h3><p>Missing user_id or access_token</p></body></html>",
        )
        .await;
        return;
    };

    let access_token =
        match zed_auth::decrypt_access_token(&encrypted, &session.private_key_verifier) {
            Ok(t) => t,
            Err(e) => {
                let mut inner = proxy_state.inner.lock().await;
                inner.session = Some(ZedPendingLogin {
                    error: Some(e.clone()),
                    ..session
                });
                write_http_response(
                    stream,
                    "200 OK",
                    &[("Content-Type", "text/html; charset=utf-8".to_string())],
                    &format!("<html><body><h3>Zed login failed</h3><p>{e}</p></body></html>"),
                )
                .await;
                return;
            }
        };

    // Best-effort user info for display name/email.
    let client = reqwest::Client::new();
    let auth = zed_auth::build_user_auth_header(&user_id, &access_token).unwrap_or_default();
    let mut email: Option<String> = None;
    if let Ok(info) = client
        .get(format!(
            "{}{}",
            crate::oauth::zed_auth::ZED_CLOUD_BASE_URL,
            "/client/users/me"
        ))
        .header("Accept", "application/json")
        .header("Authorization", auth)
        .send()
        .await
    {
        if let Ok(data) = info.json::<Value>().await {
            email = data.get("email").and_then(Value::as_str).map(String::from);
        }
    }

    // Persist the connection.
    let connection_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let psd_entry = ("userId".to_string(), Value::String(user_id.clone()));
    let store_result = state
        .db
        .update(move |db| {
            db.provider_connections
                .push(crate::types::ProviderConnection {
                    id: connection_id.clone(),
                    provider: "zed".into(),
                    auth_type: "oauth".into(),
                    is_active: Some(true),
                    created_at: Some(now.clone()),
                    updated_at: Some(now),
                    access_token: Some(access_token),
                    provider_specific_data: std::collections::BTreeMap::from([
                        psd_entry,
                        ("authMethod".to_string(), Value::String("oauth".into())),
                    ]),
                    test_status: Some("active".into()),
                    ..Default::default()
                });
        })
        .await;

    match store_result {
        Ok(_) => {
            let mut inner = proxy_state.inner.lock().await;
            inner.session = Some(ZedPendingLogin {
                status: "done".into(),
                connection_id: None,
                email: email.clone(),
                ..session
            });
            write_http_response(
                stream,
                "200 OK",
                &[("Content-Type", "text/html; charset=utf-8".to_string())],
                "<html><body><h3>Zed login successful</h3><p>You can close this window.</p></body></html>",
            )
            .await;
        }
        Err(e) => {
            let mut inner = proxy_state.inner.lock().await;
            inner.session = Some(ZedPendingLogin {
                error: Some(e.to_string()),
                ..session
            });
            write_http_response(
                stream,
                "200 OK",
                &[("Content-Type", "text/html; charset=utf-8".to_string())],
                &format!("<html><body><h3>Zed login failed</h3><p>{e}</p></body></html>"),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt_with_payload(payload: &serde_json::Value) -> String {
        let encode = |bytes: &[u8]| {
            let mut encoded = URL_SAFE_NO_PAD.encode(bytes);
            while !encoded.len().is_multiple_of(4) {
                encoded.push('=');
            }
            encoded
                .trim_end_matches('=')
                .replace('+', "-")
                .replace('/', "_")
        };
        format!(
            "{}.{}.sig",
            encode(br#"{"alg":"none"}"#),
            encode(payload.to_string().as_bytes())
        )
    }

    #[test]
    fn test_extract_codex_account_info_top_level_fallbacks() {
        // JS: chatgpt_account_id || payload.account_id, chatgpt_plan_type || payload.plan_type.
        let token =
            jwt_with_payload(&serde_json::json!({ "account_id": "acc-1", "plan_type": "plus" }));
        let (email, psd) = extract_codex_account_info(Some(&token));
        assert_eq!(email, None);
        assert_eq!(
            psd.get("chatgptAccountId"),
            Some(&serde_json::Value::String("acc-1".to_string()))
        );
        assert_eq!(
            psd.get("chatgptPlanType"),
            Some(&serde_json::Value::String("plus".to_string()))
        );
    }

    #[test]
    fn test_decode_xai_id_token_email_prefers_email() {
        let token = jwt_with_payload(&serde_json::json!({ "email": "a@x.ai" }));
        assert_eq!(
            decode_xai_id_token_email(Some(&token)),
            Some("a@x.ai".to_string())
        );
        assert_eq!(
            extract_email_from_access_token(&token),
            Some("a@x.ai".to_string())
        );
        assert_eq!(decode_xai_id_token_email(None), None);
        assert_eq!(decode_xai_id_token_email(Some("not-a-jwt")), None);
    }

    #[test]
    fn test_decode_xai_id_token_email_falls_back_to_sub() {
        let token = jwt_with_payload(&serde_json::json!({ "sub": "user-1" }));
        assert_eq!(
            decode_xai_id_token_email(Some(&token)),
            Some("user-1".to_string())
        );
    }

    #[test]
    fn test_extract_codex_account_info_reads_openai_auth_claims() {
        let token = jwt_with_payload(&serde_json::json!({
            "email": "c@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc-1",
                "chatgpt_plan_type": "plus"
            }
        }));
        let (email, psd) = extract_codex_account_info(Some(&token));
        assert_eq!(email, Some("c@example.com".to_string()));
        assert_eq!(
            psd.get("chatgptAccountId"),
            Some(&serde_json::Value::String("acc-1".to_string()))
        );
        assert_eq!(
            psd.get("chatgptPlanType"),
            Some(&serde_json::Value::String("plus".to_string()))
        );
    }

    #[test]
    fn test_normalize_codex_auth_json_import() {
        let item = serde_json::json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "id-token",
                "access_token": "access-token",
                "refresh_token": "refresh-token",
                "account_id": "account-1"
            },
            "last_refresh": "2026-01-01T00:00:00Z"
        })
        .as_object()
        .unwrap()
        .clone();

        let normalized = normalize_codex_import_item(item).unwrap();
        assert_eq!(normalized["idToken"], "id-token");
        assert_eq!(normalized["accessToken"], "access-token");
        assert_eq!(normalized["refreshToken"], "refresh-token");
        assert_eq!(
            normalized["providerSpecificData"]["chatgptAccountId"],
            "account-1"
        );
        assert_eq!(normalized["lastRefreshAt"], "2026-01-01T00:00:00Z");
    }

    #[test]
    fn test_xiaomi_mimo_auth_candidates_include_mimocode_path() {
        let paths: Vec<String> = xiaomi_mimo_auth_candidates()
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(paths.iter().any(|p| p.contains("mimocode")));
    }
}
