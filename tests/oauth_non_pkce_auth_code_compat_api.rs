#![allow(clippy::await_holding_lock)]
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::{engine::general_purpose::STANDARD, Engine};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
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
            key: "nonpkce-mgmt-key".into(),
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
        .header("authorization", "Bearer nonpkce-mgmt-key")
        .body(Body::empty())
        .unwrap()
}

fn post_request(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("authorization", "Bearer nonpkce-mgmt-key")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
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

fn cline_code(payload: serde_json::Value) -> String {
    STANDARD.encode(payload.to_string())
}

#[tokio::test]
async fn antigravity_authorize_matches_openproxy_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/antigravity/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code");
    assert_eq!(json["redirectUri"], "http://localhost:4624/callback");
    assert_eq!(json["callbackPath"], "/callback");

    let state = json["state"].as_str().expect("state");
    assert_eq!(
        json["authUrl"],
        format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.email+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.profile+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcclog+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fexperimentsandconfigs&state={state}&access_type=offline&prompt=consent"
        )
    );
}

#[tokio::test]
async fn cline_authorize_matches_openproxy_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/cline/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code");
    assert_eq!(json["redirectUri"], "http://localhost:4624/callback");
    assert_eq!(json["callbackPath"], "/callback");
    assert_eq!(
        json["authUrl"],
        "https://api.cline.bot/api/v1/auth/authorize?client_type=extension&callback_url=http%3A%2F%2Flocalhost%3A4624%2Fcallback&redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback"
    );
}

#[tokio::test]
async fn antigravity_exchange_matches_openproxy_and_saves_connection() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_TOKEN_URL",
        &format!("{}/token", server.uri()),
    );
    let _user_info_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_USER_INFO_URL",
        &format!("{}/userinfo", server.uri()),
    );
    let _load_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_LOAD_CODE_ASSIST_ENDPOINT",
        &format!("{}/v1internal:loadCodeAssist", server.uri()),
    );
    let _onboard_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_ONBOARD_USER_ENDPOINT",
        &format!("{}/v1internal:onboardUser", server.uri()),
    );

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains(
            "client_id=1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com",
        ))
        .and(body_string_contains("code=auth-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "antigravity-access",
            "refresh_token": "antigravity-refresh",
            "expires_in": 5400,
            "scope": "scope-antigravity"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(query_param("alt", "json"))
        .and(header("authorization", "Bearer antigravity-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "antigravity@example.com"
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1internal:loadCodeAssist"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "cloudaicompanionProject": { "id": "ag-project" },
            "allowedTiers": [
                { "id": "legacy-tier", "isDefault": false },
                { "id": "preferred-tier", "isDefault": true }
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1internal:onboardUser"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "done": true
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/antigravity/exchange",
            json!({
                "code": "auth-code",
                "redirectUri": "http://localhost:4624/callback",
                "codeVerifier": "pkce-verifier"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "antigravity");
    assert_eq!(json["connection"]["email"], "antigravity@example.com");

    let requests = server.received_requests().await.expect("requests");
    let load_request = requests
        .iter()
        .find(|request| request.url.path() == "/v1internal:loadCodeAssist")
        .expect("load request");
    assert_eq!(
        load_request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer antigravity-access")
    );
    assert_eq!(
        load_request
            .headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            .unwrap_or(""),
        "antigravity/cli/1.1.5 (aidev_client; os_type=darwin; arch=arm64; auth_method=consumer)"
    );
    assert!(load_request.headers.get("x-goog-api-client").is_none());
    assert!(load_request.headers.get("client-metadata").is_none());
    assert!(load_request.headers.get("x-request-source").is_none());
    let load_body: serde_json::Value =
        serde_json::from_slice(&load_request.body).expect("load request body");
    assert_eq!(
        load_body,
        json!({
            "metadata": {
                "ideType": "ANTIGRAVITY"
            }
        })
    );

    let snapshot = state.db.snapshot();
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "antigravity");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.email.as_deref(), Some("antigravity@example.com"));
    assert_eq!(
        connection.access_token.as_deref(),
        Some("antigravity-access")
    );
    assert_eq!(
        connection.refresh_token.as_deref(),
        Some("antigravity-refresh")
    );
    assert_eq!(connection.scope.as_deref(), Some("scope-antigravity"));
    assert_eq!(connection.project_id.as_deref(), Some("ag-project"));
    assert_eq!(connection.test_status.as_deref(), Some("active"));
}

#[tokio::test]
async fn antigravity_project_discovery_failure_is_saved_explicitly() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_TOKEN_URL",
        &format!("{}/token", server.uri()),
    );
    let _user_info_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_USER_INFO_URL",
        &format!("{}/userinfo", server.uri()),
    );
    let _load_url = EnvVarGuard::set(
        "OPENPROXY_ANTIGRAVITY_LOAD_CODE_ASSIST_ENDPOINT",
        &format!("{}/v1internal:loadCodeAssist", server.uri()),
    );

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "antigravity-error-access",
            "refresh_token": "antigravity-error-refresh",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "project-error@example.com"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:loadCodeAssist"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": "temporarily unavailable"
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let response = openproxy::build_app(state.clone())
        .oneshot(post_request(
            "/api/oauth/antigravity/exchange",
            json!({
                "code": "auth-code",
                "redirectUri": "http://localhost:4624/callback",
                "codeVerifier": "pkce-verifier"
            }),
        ))
        .await
        .expect("Antigravity exchange response");
    let (status, body) = response_json(response).await;
    // CLI hard-cut: empty project after loadCodeAssist failure → fail fast,
    // no degraded connection is persisted.
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        body.get("error").is_some(),
        "expected error body, got {body}"
    );

    let snapshot = state.db.snapshot();
    assert!(
        snapshot
            .provider_connections
            .iter()
            .find(|connection| connection.provider == "antigravity")
            .is_none(),
        "failed CLI discovery must not persist a connection"
    );
}

#[tokio::test]
async fn cline_exchange_accepts_base64_code_without_pkce() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/cline/exchange",
            json!({
                "code": cline_code(json!({
                    "accessToken": "cline-access",
                    "refreshToken": "cline-refresh",
                    "email": "cline@example.com",
                    "firstName": "Cli",
                    "lastName": "Ne",
                    "expiresAt": "2035-01-02T03:04:05Z"
                })),
                "redirectUri": "http://localhost:4624/callback"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "cline");
    assert_eq!(json["connection"]["email"], "cline@example.com");

    let snapshot = state.db.snapshot();
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "cline");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.email.as_deref(), Some("cline@example.com"));
    assert_eq!(connection.access_token.as_deref(), Some("cline-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("cline-refresh"));
    assert_eq!(
        connection.provider_specific_data.get("firstName"),
        Some(&json!("Cli"))
    );
    assert_eq!(
        connection.provider_specific_data.get("lastName"),
        Some(&json!("Ne"))
    );
    assert!(connection.expires_at.is_some());
    assert_eq!(connection.test_status.as_deref(), Some("active"));
}
