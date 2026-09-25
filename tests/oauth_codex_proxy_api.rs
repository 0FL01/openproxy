#![allow(clippy::await_holding_lock)]
use base64::Engine;
use openproxy::core::tls::ensure_rustls_provider;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ProviderConnection;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

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

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        // Management key: oauth proxy routes sit in the admin tier now that
        // requireLogin defaults to true (9router parity).
        state.api_keys.push(openproxy::types::ApiKey {
            id: "mgmt-1".into(),
            name: "Management".into(),
            key: "codex-proxy-mgmt-key".into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: Default::default(),
        });
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn get_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("authorization", "Bearer codex-proxy-mgmt-key")
        .body(Body::empty())
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

async fn stop_proxy(app: &axum::Router) {
    let _ = app
        .clone()
        .oneshot(get_request("/api/oauth/codex/stop-proxy"))
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn codex_start_proxy_registers_server_side_session_and_poll_status() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let app = openproxy::build_app(app_state().await);
    stop_proxy(&app).await;

    let response = app
        .clone()
        .oneshot(get_request(
            "/api/oauth/codex/start-proxy?app_port=4624&state=state-1&code_verifier=verifier-1&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "serverSide": true }));

    let response = app
        .clone()
        .oneshot(get_request("/api/oauth/codex/poll-status?state=state-1"))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "status": "pending" }));

    stop_proxy(&app).await;
}

#[tokio::test]
async fn codex_proxy_fallback_redirects_to_app_callback() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let app = openproxy::build_app(app_state().await);
    stop_proxy(&app).await;

    let response = app
        .clone()
        .oneshot(get_request("/api/oauth/codex/start-proxy?app_port=4624"))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "serverSide": false }));

    ensure_rustls_provider();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client
        .get("http://127.0.0.1:1455/auth/callback?code=legacy-code&state=legacy-state")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:4624/callback?code=legacy-code&state=legacy-state")
    );

    stop_proxy(&app).await;
}

#[tokio::test]
async fn codex_proxy_server_side_callback_exchanges_and_clears_session() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CODEX_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"))
        .and(body_string_contains("code=auth-code"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .and(body_string_contains("code_verifier=proxy-verifier"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "codex-access",
            "refresh_token": "codex-refresh",
            "expires_in": 3600,
            "id_token": "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6ImNvZGV4QGV4YW1wbGUuY29tIn0.sig"
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    stop_proxy(&app).await;

    let response = app
        .clone()
        .oneshot(get_request(
            "/api/oauth/codex/start-proxy?app_port=4624&state=proxy-state&code_verifier=proxy-verifier&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "serverSide": true }));

    let response =
        reqwest::get("http://127.0.0.1:1455/auth/callback?code=auth-code&state=proxy-state")
            .await
            .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("Authentication Successful"));

    let response = app
        .clone()
        .oneshot(get_request(
            "/api/oauth/codex/poll-status?state=proxy-state",
        ))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "done");
    assert_eq!(json["email"], "codex@example.com");
    assert!(json["connectionId"].as_str().is_some());

    let response = app
        .clone()
        .oneshot(get_request(
            "/api/oauth/codex/poll-status?state=proxy-state",
        ))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "status": "unknown" }));

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "codex");
    assert_eq!(connection.email.as_deref(), Some("codex@example.com"));
    assert_eq!(connection.access_token.as_deref(), Some("codex-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("codex-refresh"));

    stop_proxy(&app).await;
}

#[tokio::test]
async fn codex_refresh_uses_oauth_token_url_override_and_preserves_safe_error_code() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_rustls_provider();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CODEX_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(wiremock::matchers::header(
            "content-type",
            "application/json",
        ))
        .and(wiremock::matchers::body_json(json!({
            "grant_type": "refresh_token",
            "refresh_token": "sentinel-refresh-token",
            "client_id": "app_EMoamEEZ73f0CkXaXp7hrann"
        })))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {
                "code": "refresh_token_reused",
                "message": "sensitive upstream detail must not escape"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = openproxy::oauth::token_refresh::dispatch_oauth_refresh(
        "codex",
        "sentinel-refresh-token",
        &Default::default(),
    )
    .await
    .expect_err("mock endpoint rejects the reused refresh token");

    assert!(error.contains("HTTP 401"), "unexpected error: {error}");
    assert!(
        error.contains("refresh_token_reused"),
        "unexpected error: {error}"
    );
    assert!(!error.contains("sentinel-refresh-token"));
    assert!(!error.contains("sensitive upstream detail"));
    server.verify().await;
}

#[tokio::test]
async fn codex_request_refresh_rotates_once_and_reuses_fresh_credentials() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_rustls_provider();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CODEX_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(wiremock::matchers::body_json(json!({
            "grant_type": "refresh_token",
            "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",
            "refresh_token": "old-refresh"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let expiry = chrono::Utc::now().timestamp() + 60;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(json!({ "exp": expiry }).to_string());
    let connection = ProviderConnection {
        id: "codex-request-refresh".into(),
        provider: "codex".into(),
        auth_type: "oauth".into(),
        access_token: Some(format!("header.{payload}.signature")),
        refresh_token: Some("old-refresh".into()),
        ..Default::default()
    };
    let directory = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(directory.path()).await.expect("db"));
    db.update(|state| state.provider_connections.push(connection.clone()))
        .await
        .expect("store connection");

    let refreshed =
        openproxy::oauth::token_refresh::codex_connection_for_request(db.clone(), connection).await;
    assert_eq!(refreshed.access_token.as_deref(), Some("new-access"));
    assert_eq!(refreshed.refresh_token.as_deref(), Some("new-refresh"));
    assert!(refreshed
        .provider_specific_data
        .contains_key("lastRefreshAt"));

    let next =
        openproxy::oauth::token_refresh::codex_connection_for_request(db.clone(), refreshed).await;
    assert_eq!(next.access_token.as_deref(), Some("new-access"));
    assert_eq!(
        db.snapshot().provider_connections[0]
            .refresh_token
            .as_deref(),
        Some("new-refresh")
    );
    server.verify().await;
}
