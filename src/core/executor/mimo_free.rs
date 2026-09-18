//! MimoFree executor.
//!
//! Dedicated executor for the `mimo-free` provider with a Bootstrap-JWT
//! authentication flow (9router mimo-free.js parity):
//!
//! 1. POST `/api/free-ai/bootstrap` with `{ client: fingerprint }` (SHA-256 of
//!    `hostname|platform|arch|cpu|username`) → receive a JWT token.
//! 2. Use the JWT as a Bearer token for subsequent chat completions, sending
//!    `X-Mimo-Source: mimocode-cli-free` and `x-session-affinity`.
//!
//! The JWT is cached in-memory (per fingerprint) with its `exp` claim minus a
//! 300s buffer (fallback TTL 3000s). A 401/403 invalidates the cached JWT so a
//! later client-owned request can bootstrap again; this executor never repeats
//! the generation request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::{
    diagnostic_body_limit, read_reqwest_body, read_reqwest_diagnostic, success_body_limit,
    ClientPool, TransportKind, UpstreamResponse,
};

// ── Constants ──────────────────────────────────────────────────────────────

/// Base URL for the MimoFree bootstrap (device authorize) endpoint.
/// 9router registry uses api.xiaomimimo.com free-ai surface.
const MIMO_BOOTSTRAP_URL: &str = "https://api.xiaomimimo.com/api/free-ai/bootstrap";

/// Base URL for chat completions (9router registry).
const MIMO_CHAT_URL: &str = "https://api.xiaomimimo.com/api/free-ai/openai/chat";

/// Rotating Chrome User-Agent strings — the exact 3 strings from 9router
/// mimo-free.js USER_AGENTS (Chrome/131.0.0.0 x3).
const CHROME_USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
];

/// JWT expiry buffer: treat the token as expired 300s before its real exp.
const JWT_EXPIRY_BUFFER_MS: u64 = 300_000;

/// Fallback TTL for cached JWTs without a parseable `exp` claim.
const JWT_FALLBACK_TTL_MS: u64 = 3_000_000;

/// Default timeout for the bootstrap POST request.
const BOOTSTRAP_TIMEOUT_SECS: u64 = 15;

// ── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MimoFreeExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct MimoFreeExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transport: TransportKind,
}

impl std::fmt::Debug for MimoFreeExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MimoFreeExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transport", &self.transport)
            .finish()
    }
}

#[derive(Debug)]
pub enum MimoFreeExecutorError {
    MissingCredentials(String),
    BootstrapFailed(String),
    BootstrapAuthFailed(String),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    Request(reqwest::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    InvalidUri(hyper::http::uri::InvalidUri),
    InvalidRequest(hyper::http::Error),
}

impl From<reqwest::Error> for MimoFreeExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for MimoFreeExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<serde_json::Error> for MimoFreeExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for MimoFreeExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for MimoFreeExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<hyper::http::uri::InvalidUri> for MimoFreeExecutorError {
    fn from(error: hyper::http::uri::InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<hyper::http::Error> for MimoFreeExecutorError {
    fn from(error: hyper::http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl std::fmt::Display for MimoFreeExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(p) => write!(f, "Missing credentials for mimo-free: {}", p),
            Self::BootstrapFailed(e) => write!(f, "MimoFree bootstrap failed: {}", e),
            Self::BootstrapAuthFailed(e) => write!(f, "MimoFree bootstrap auth failed: {}", e),
            Self::InvalidHeader(e) => write!(f, "Invalid header: {}", e),
            Self::Request(e) => write!(f, "Request error: {}", e),
            Self::Serialize(e) => write!(f, "Serialization error: {}", e),
            Self::HyperClientInit(e) => write!(f, "Hyper client init error: {}", e),
            Self::Hyper(e) => write!(f, "Hyper error: {}", e),
            Self::InvalidUri(e) => write!(f, "Invalid URI: {}", e),
            Self::InvalidRequest(e) => write!(f, "Invalid request: {}", e),
        }
    }
}

impl std::error::Error for MimoFreeExecutorError {}

// ── JWT bootstrap response shape ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct BootstrapResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    jwt: Option<String>,
}

/// Parse the `exp` claim (seconds) from a JWT's base64url payload segment,
/// returning milliseconds since the epoch. None when unparseable (9router
/// parseJwtExp fallback behavior).
fn parse_jwt_expiry(jwt: &str) -> Option<u64> {
    let mut parts = jwt.split('.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    // base64url → base64
    let padded = payload_b64.replace('-', "+").replace('_', "/");
    let mut b64 = padded;
    while b64.len() % 4 != 0 {
        b64.push('=');
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .ok()?;
    let payload: Value = serde_json::from_slice(&bytes).ok()?;
    let exp = payload.get("exp").and_then(Value::as_u64)?;
    Some(exp.saturating_mul(1000))
}

// ── Executor ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MimoFreeExecutor {
    pool: Arc<ClientPool>,
    /// In-memory JWT cache: `device_fingerprint -> (jwt_token, expiry_instant)`.
    /// A `None` capacity means the cache entry has no defined expiry.
    jwt_cache: Arc<Mutex<HashMap<String, JwtCacheEntry>>>,
    /// Round-robin counter for Chrome UA rotation.
    ua_counter: Arc<Mutex<usize>>,
}

#[derive(Debug, Clone)]
struct JwtCacheEntry {
    token: String,
    expires_at: Option<Instant>,
}

impl MimoFreeExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self {
            pool,
            jwt_cache: Arc::new(Mutex::new(HashMap::new())),
            ua_counter: Arc::new(Mutex::new(0)),
        }
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    // ── UA rotation ────────────────────────────────────────────────────────

    /// Pick the next Chrome User-Agent string in round-robin order.
    fn next_user_agent(&self) -> &'static str {
        let mut counter = self.ua_counter.lock().expect("ua_counter lock");
        let idx = *counter % CHROME_USER_AGENTS.len();
        *counter += 1;
        CHROME_USER_AGENTS[idx]
    }

    // ── Session affinity ────────────────────────────────────────────────────

    /// Generate a session affinity id: `ses_` + 24 chars of `[a-z0-9]`
    /// (9router generateSessionId).
    fn generate_session_id() -> String {
        const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut bytes = [0u8; 24];
        for b in bytes.iter_mut() {
            let idx = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
                ^ std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u32)
                    .unwrap_or(0))
                % CHARSET.len() as u32;
            *b = CHARSET[idx as usize];
        }
        format!("ses_{}", String::from_utf8_lossy(&bytes))
    }

    // ── Fingerprint derivation ──────────────────────────────────────────────

    /// Read the machine hostname (9router seeds the fingerprint with
    /// `os.hostname()`). Mirrors the crate's machine_id helper.
    fn read_hostname() -> String {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .or_else(|_| std::env::var("HOST"))
            .unwrap_or_default()
    }

    /// Derive the device fingerprint: SHA-256 of
    /// `hostname|platform|arch|cpu|username` (9router generateFingerprint).
    /// Stable per machine so the JWT cache hits across restarts.
    fn derive_fingerprint(_credentials: &ProviderConnection) -> String {
        let hostname = Self::read_hostname();
        let platform = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let cpu = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default();
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let seed = format!("{hostname}|{platform}|{arch}|{cpu}|{username}");
        let mut hasher = Sha256::new();
        hasher.update(seed.as_bytes());
        hex::encode(hasher.finalize())
    }

    // ── Bootstrap JWT ──────────────────────────────────────────────────────

    /// Bootstrap a JWT token by POSTing the SHA-256 device fingerprint to
    /// `/v1/device/authorize`.
    ///
    /// Returns the JWT token string on success.
    async fn bootstrap_jwt(&self, fingerprint: &str) -> Result<String, MimoFreeExecutorError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(BOOTSTRAP_TIMEOUT_SECS))
            .build()
            .map_err(|e| MimoFreeExecutorError::BootstrapFailed(e.to_string()))?;

        // 9router: POST { client: generateFingerprint() }
        let payload = serde_json::json!({
            "client": fingerprint,
        });

        let response = client
            .post(MIMO_BOOTSTRAP_URL)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(USER_AGENT, HeaderValue::from_static(self.next_user_agent()))
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = String::from_utf8_lossy(
                &read_reqwest_diagnostic(response, diagnostic_body_limit())
                    .await
                    .bytes,
            )
            .into_owned();
            return Err(MimoFreeExecutorError::BootstrapAuthFailed(format!(
                "bootstrap returned HTTP {}: {}",
                status.as_u16(),
                body_text
            )));
        }

        let body = read_reqwest_body(response, success_body_limit())
            .await
            .map_err(|error| MimoFreeExecutorError::BootstrapFailed(error.to_string()))?;
        let bootstrap: BootstrapResponse = serde_json::from_slice(&body).map_err(|e| {
            MimoFreeExecutorError::BootstrapFailed(format!("JSON parse error: {}", e))
        })?;

        // Accept either `token`, `access_token`, or `jwt` field.
        let jwt = bootstrap
            .token
            .or(bootstrap.access_token)
            .or(bootstrap.jwt)
            .ok_or_else(|| {
                MimoFreeExecutorError::BootstrapFailed(
                    "bootstrap response did not contain a token field".to_string(),
                )
            })?;

        tracing::debug!("mimo-free: bootstrapped JWT token (len={})", jwt.len());

        // Cache with expiry derived from the JWT `exp` claim (minus a 300s
        // buffer), falling back to a 3000s TTL (9router parseJwtExp).
        let expires_at = parse_jwt_expiry(&jwt).map(|exp_ms| {
            Instant::now()
                + std::time::Duration::from_millis(exp_ms.saturating_sub(JWT_EXPIRY_BUFFER_MS))
        });
        let fallback = Instant::now() + std::time::Duration::from_millis(JWT_FALLBACK_TTL_MS);
        {
            let mut cache = self.jwt_cache.lock().expect("jwt_cache lock");
            cache.insert(
                fingerprint.to_string(),
                JwtCacheEntry {
                    token: jwt.clone(),
                    expires_at: expires_at.or(Some(fallback)),
                },
            );
        }

        Ok(jwt)
    }

    /// Retrieve a valid JWT from cache or bootstrap a new one.
    async fn get_or_bootstrap_jwt(
        &self,
        fingerprint: &str,
    ) -> Result<String, MimoFreeExecutorError> {
        // Check cache for a non-expired entry.
        {
            let cache = self.jwt_cache.lock().expect("jwt_cache lock");
            if let Some(entry) = cache.get(fingerprint) {
                match entry.expires_at {
                    Some(expiry) if Instant::now() < expiry => {
                        return Ok(entry.token.clone());
                    }
                    None => {
                        // No expiry set — treat as still valid.
                        return Ok(entry.token.clone());
                    }
                    _ => {}
                }
            }
        }

        // Cache miss or expired — bootstrap.
        self.bootstrap_jwt(fingerprint).await
    }

    /// Invalidate the cached JWT for a given fingerprint (used on 401/403).
    fn invalidate_jwt(&self, fingerprint: &str) {
        let mut cache = self.jwt_cache.lock().expect("jwt_cache lock");
        cache.remove(fingerprint);
        tracing::debug!("mimo-free: invalidated JWT cache for fingerprint");
    }

    // ── Headers ─────────────────────────────────────────────────────────────

    fn build_headers(
        jwt: &str,
        stream: bool,
        user_agent: &str,
        session_id: &str,
    ) -> Result<HeaderMap, MimoFreeExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_str(user_agent)?);

        let auth = format!("Bearer {jwt}");
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&auth)?);

        // 9router buildHeaders: X-Mimo-Source + x-session-affinity.
        headers.insert(
            "X-Mimo-Source",
            HeaderValue::from_static("mimocode-cli-free"),
        );
        headers.insert("x-session-affinity", HeaderValue::from_str(session_id)?);

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }

        Ok(headers)
    }

    // ── Execute ─────────────────────────────────────────────────────────────

    pub async fn execute_request(
        &self,
        request: MimoFreeExecutionRequest,
    ) -> Result<MimoFreeExecutorResponse, MimoFreeExecutorError> {
        // Derive device fingerprint.
        let fingerprint = Self::derive_fingerprint(&request.credentials);

        // Bootstrap or retrieve JWT.
        let jwt = self.get_or_bootstrap_jwt(&fingerprint).await?;

        // Generate a session affinity ID.
        let session_id = Self::generate_session_id();

        // Pick a Chrome UA for this request.
        let user_agent = self.next_user_agent();

        let url = MIMO_CHAT_URL.to_string();
        let body_bytes = serde_json::to_vec(&request.body)?;
        let headers = Self::build_headers(&jwt, request.stream, user_agent, &session_id)?;

        let client = self.pool.get("mimo-free", request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;

        if matches!(response.status().as_u16(), 401 | 403) {
            // C13: invalidate stale derived credentials, but preserve the raw
            // response. The request-scoped planner is the only generation
            // recovery owner and this no-refresh-token provider cannot retry
            // the same request invisibly.
            self.invalidate_jwt(&fingerprint);
        }

        Ok(MimoFreeExecutorResponse {
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
    fn test_generate_session_id() {
        let id = MimoFreeExecutor::generate_session_id();
        assert!(id.starts_with("ses_"), "session id should start with ses_");
        // 9router: ses_ + 24 lowercase alnum chars
        assert_eq!(id.len(), 4 + 24, "ses_ prefix + 24 chars");
        let body = &id[4..];
        assert!(
            body.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "session body must be [a-z0-9], got: {body}"
        );
    }

    #[test]
    fn test_derive_fingerprint() {
        let creds = ProviderConnection {
            api_key: Some("my-seed".to_string()),
            ..Default::default()
        };
        let fp = MimoFreeExecutor::derive_fingerprint(&creds);
        // SHA-256 hex is 64 chars.
        assert_eq!(fp.len(), 64);

        // Same machine seed should produce same fingerprint.
        let fp2 = MimoFreeExecutor::derive_fingerprint(&creds);
        assert_eq!(fp, fp2);
    }

    #[test]
    fn test_parse_jwt_expiry() {
        use base64::Engine;
        // exp: 2_000_000_000 (seconds) in a real JWT-shaped payload.
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"exp":2000000000,"sub":"x"}"#);
        let jwt = format!("{header}.{payload}.sig");
        assert_eq!(parse_jwt_expiry(&jwt), Some(2_000_000_000u64 * 1000));

        // Unparseable → None (fallback TTL path).
        assert_eq!(parse_jwt_expiry("not-a-jwt"), None);
        assert_eq!(parse_jwt_expiry("a.b"), None);
    }

    #[test]
    fn test_next_user_agent_rotation() {
        let executor = MimoFreeExecutor::new(Arc::new(crate::core::executor::ClientPool::new()));
        let ua1 = executor.next_user_agent();
        let ua2 = executor.next_user_agent();
        let ua3 = executor.next_user_agent();
        let ua4 = executor.next_user_agent();

        // All should be one of the CHROME_USER_AGENTS.
        assert!(CHROME_USER_AGENTS.contains(&ua1));
        assert!(CHROME_USER_AGENTS.contains(&ua2));
        assert!(CHROME_USER_AGENTS.contains(&ua3));
        // After wrapping around, ua4 should equal ua1 again.
        assert_eq!(ua1, ua4, "round-robin should wrap after 3 UAs");
    }

    #[test]
    fn test_build_headers() {
        let headers =
            MimoFreeExecutor::build_headers("test-jwt", true, "test-ua", "ses_test-session")
                .unwrap();
        assert_eq!(
            headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()),
            Some("Bearer test-jwt")
        );
        assert_eq!(
            headers.get(USER_AGENT).and_then(|v| v.to_str().ok()),
            Some("test-ua")
        );
        assert_eq!(
            headers
                .get("x-session-affinity")
                .and_then(|v| v.to_str().ok()),
            Some("ses_test-session")
        );
        assert_eq!(
            headers.get("X-Mimo-Source").and_then(|v| v.to_str().ok()),
            Some("mimocode-cli-free")
        );
        assert_eq!(
            headers.get(ACCEPT).and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
    }

    #[test]
    fn test_jwt_cache() {
        let executor = MimoFreeExecutor::new(Arc::new(crate::core::executor::ClientPool::new()));

        let fp = "test-fingerprint".to_string();

        // Insert into cache.
        {
            let mut cache = executor.jwt_cache.lock().unwrap();
            cache.insert(
                fp.clone(),
                JwtCacheEntry {
                    token: "cached-token".to_string(),
                    expires_at: Some(Instant::now() + std::time::Duration::from_secs(3600)),
                },
            );
        }

        // Retrieve should succeed.
        let cache = executor.jwt_cache.lock().unwrap();
        let entry = cache.get(&fp).unwrap();
        assert_eq!(entry.token, "cached-token");
        assert!(Instant::now() < entry.expires_at.unwrap());
    }

    #[test]
    fn test_invalidate_jwt() {
        let executor = MimoFreeExecutor::new(Arc::new(crate::core::executor::ClientPool::new()));

        let fp = "test-fingerprint".to_string();

        // Insert into cache.
        {
            let mut cache = executor.jwt_cache.lock().unwrap();
            cache.insert(
                fp.clone(),
                JwtCacheEntry {
                    token: "test".to_string(),
                    expires_at: None,
                },
            );
            assert!(cache.contains_key(&fp));
        }

        executor.invalidate_jwt(&fp);

        let cache = executor.jwt_cache.lock().unwrap();
        assert!(!cache.contains_key(&fp));
    }
}
