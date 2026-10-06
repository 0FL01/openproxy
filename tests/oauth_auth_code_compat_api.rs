#![allow(clippy::await_holding_lock)]
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_json, body_string_contains, method, path};
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
        // Management key: oauth routes sit in the admin tier now that
        // requireLogin defaults to true (9router parity).
        state.api_keys.push(openproxy::types::ApiKey {
            id: "mgmt-1".into(),
            name: "Management".into(),
            key: TEST_MGMT_KEY.into(),
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

const TEST_MGMT_KEY: &str = "oauth-mgmt-test-key";

fn get_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("authorization", concat!("Bearer ", "oauth-mgmt-test-key"))
        .body(Body::empty())
        .unwrap()
}

fn post_request(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("authorization", concat!("Bearer ", "oauth-mgmt-test-key"))
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

fn is_base64url_no_pad(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn make_codex_id_token(email: &str, account_id: &str, plan_type: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_plan_type": plan_type
            }
        })
        .to_string(),
    );
    format!("{header}.{payload}.sig")
}

#[tokio::test]
async fn claude_authorize_matches_openproxy_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/claude/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code_pkce");
    assert_eq!(json["redirectUri"], "http://localhost:4624/callback");
    assert_eq!(json["callbackPath"], "/callback");
    assert!(json.get("fixedPort").is_none());

    let state = json["state"].as_str().expect("state");
    let code_verifier = json["codeVerifier"].as_str().expect("code verifier");
    let code_challenge = json["codeChallenge"].as_str().expect("code challenge");
    assert_eq!(state.len(), 43);
    assert_eq!(code_verifier.len(), 43);
    assert_eq!(code_challenge.len(), 43);
    assert!(is_base64url_no_pad(state));
    assert!(is_base64url_no_pad(code_verifier));
    assert!(is_base64url_no_pad(code_challenge));

    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    assert_eq!(code_challenge, expected_challenge);
    assert_eq!(
        json["authUrl"],
        format!(
            "https://platform.claude.com/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback&scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference+user%3Asessions%3Aclaude_code&code_challenge={code_challenge}&code_challenge_method=S256&state={state}"
        )
    );
}

#[tokio::test]
async fn codex_authorize_matches_openproxy_response_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/codex/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code_pkce");
    assert_eq!(json["redirectUri"], "http://localhost:1455/auth/callback");
    assert_eq!(json["callbackPath"], "/auth/callback");
    assert_eq!(json["fixedPort"], 1455);

    let state = json["state"].as_str().expect("state");
    let code_verifier = json["codeVerifier"].as_str().expect("code verifier");
    let code_challenge = json["codeChallenge"].as_str().expect("code challenge");
    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    assert_eq!(code_challenge, expected_challenge);
    assert_eq!(
        json["authUrl"],
        format!(
            "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_EMoamEEZ73f0CkXaXp7hrann&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&scope=openid%20profile%20email%20offline_access&code_challenge={code_challenge}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=codex_cli_rs&state={state}"
        )
    );
}

#[tokio::test]
async fn exchange_compat_rejects_missing_required_fields() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(post_request("/api/oauth/claude/exchange", json!({})))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Missing required fields" }));
}

#[tokio::test]
async fn claude_exchange_matches_openproxy_and_saves_connection() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CLAUDE_TOKEN_URL",
        &format!("{}/v1/oauth/token", server.uri()),
    );
    let _profile_url = EnvVarGuard::set(
        "OPENPROXY_CLAUDE_PROFILE_URL",
        &format!("{}/api/oauth/profile", server.uri()),
    );

    // Live wire pins (MITM 2026-10-06): the token exchange rides the axios
    // identity; the profile validation runs as claude-code/<version> with
    // the axios accept + Cache-Control: no-cache and no anthropic-* headers.
    // wiremock splits header values on commas, so multi-segment values are
    // pinned via `headers(key, vec![...])`.
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(wiremock::matchers::header("user-agent", "axios/1.15.2"))
        .and(wiremock::matchers::headers(
            "accept",
            vec!["application/json", "text/plain", "*/*"],
        ))
        .and(body_json(json!({
            "code": "auth-code",
            "state": "fragment-state",
            "grant_type": "authorization_code",
            "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            "redirect_uri": "http://localhost:4624/callback",
            "code_verifier": "pkce-verifier"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "claude-access",
            "refresh_token": "claude-refresh",
            "expires_in": 3600,
            "scope": "org:create_api_key user:profile user:inference"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/oauth/profile"))
        .and(wiremock::matchers::header(
            "user-agent",
            "claude-code/2.1.289",
        ))
        .and(wiremock::matchers::headers(
            "accept",
            vec!["application/json", "text/plain", "*/*"],
        ))
        .and(wiremock::matchers::header("cache-control", "no-cache"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            // Live CLI zod shape: account.display_name / account.full_name.
            "account": {
                "uuid": "u-1",
                "email": "op@test.dev",
                "display_name": "Op User",
                "full_name": "Op Full User"
            },
            "organization": { "uuid": "o-1" }
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/claude/exchange",
            json!({
                "code": "auth-code#fragment-state",
                "redirectUri": "http://localhost:4624/callback",
                "codeVerifier": "pkce-verifier",
                "state": "body-state"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "claude");
    assert_eq!(json["connection"]["email"], "op@test.dev");

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "claude");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.name.as_deref(), Some("op@test.dev"));
    assert_eq!(connection.email.as_deref(), Some("op@test.dev"));
    assert_eq!(connection.access_token.as_deref(), Some("claude-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("claude-refresh"));
    assert_eq!(
        connection.scope.as_deref(),
        Some("org:create_api_key user:profile user:inference")
    );
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert!(connection.expires_at.is_some());
    // Live account.* fallbacks: display_name must enrich from the
    // account.display_name field of the live zod shape.
    assert_eq!(connection.display_name.as_deref(), Some("Op User"));
    assert_eq!(connection.email.as_deref(), Some("op@test.dev"));

    // Repeat login with the same account must upsert, not duplicate.
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("auth-code-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "claude-access-2",
            "refresh_token": "claude-refresh-2",
            "expires_in": 3600,
            "scope": "org:create_api_key user:profile user:inference"
        })))
        .mount(&server)
        .await;

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/claude/exchange",
            json!({
                "code": "auth-code-2#fragment-state",
                "redirectUri": "http://localhost:4624/callback",
                "codeVerifier": "pkce-verifier",
                "state": "body-state"
            }),
        ))
        .await
        .unwrap();
    let (status, _) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    assert_eq!(
        snapshot.provider_connections[0].access_token.as_deref(),
        Some("claude-access-2")
    );
}

#[tokio::test]
async fn codex_exchange_matches_openproxy_and_maps_id_token() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CODEX_TOKEN_URL",
        &format!("{}/oauth/token", server.uri()),
    );
    let id_token = make_codex_id_token("me@example.com", "acct_123", "plus");

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains(
            "client_id=app_EMoamEEZ73f0CkXaXp7hrann",
        ))
        .and(body_string_contains("code=auth-code"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .and(body_string_contains("code_verifier=pkce-verifier"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "codex-access",
            "refresh_token": "codex-refresh",
            "expires_in": 7200,
            "id_token": id_token
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/codex/exchange",
            json!({
                "code": "auth-code",
                "redirectUri": "http://localhost:1455/auth/callback",
                "codeVerifier": "pkce-verifier"
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "codex");
    assert_eq!(json["connection"]["email"], "me@example.com");
    assert!(json["connection"].get("displayName").is_none());

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "codex");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.name.as_deref(), Some("me@example.com"));
    assert_eq!(connection.email.as_deref(), Some("me@example.com"));
    assert_eq!(connection.access_token.as_deref(), Some("codex-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("codex-refresh"));
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert_eq!(
        connection.provider_specific_data.get("chatgptAccountId"),
        Some(&json!("acct_123"))
    );
    assert_eq!(
        connection.provider_specific_data.get("chatgptPlanType"),
        Some(&json!("plus"))
    );
}

#[tokio::test]
async fn claude_import_token_accepts_scope_limited_setup_token() {
    // Live MITM 2026-10-06: every genuine `claude setup-token` token gets
    // 403 oauth_scope_insufficient from the profile endpoint (its grant is
    // inference-only) and the CLI logs in anyway. Import must accept it.
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _profile_url = EnvVarGuard::set(
        "OPENPROXY_CLAUDE_PROFILE_URL",
        &format!("{}/api/oauth/profile", server.uri()),
    );

    Mock::given(method("GET"))
        .and(path("/api/oauth/profile"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {
                "type": "permission_error",
                "message": "insufficient scope",
                "details": {
                    "required_scopes": ["user:profile", "user:office"],
                    "error_code": "oauth_scope_insufficient"
                }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/claude/import-token",
            json!({ "accessToken": "sk-ant-oat01-setup-token", "name": "CICD token" }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "scope-limited token must import: {json}"
    );
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "claude");
    assert_eq!(json["connection"]["name"], "CICD token");
    assert_eq!(json["connection"]["email"], Value::Null);

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(
        connection.access_token.as_deref(),
        Some("sk-ant-oat01-setup-token")
    );
    assert_eq!(connection.refresh_token, None);
    assert_eq!(connection.expires_at, None);
    assert_eq!(connection.email, None);
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    server.verify().await;
}

#[tokio::test]
async fn claude_import_token_rejects_invalid_token_with_400() {
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _profile_url = EnvVarGuard::set(
        "OPENPROXY_CLAUDE_PROFILE_URL",
        &format!("{}/api/oauth/profile", server.uri()),
    );

    Mock::given(method("GET"))
        .and(path("/api/oauth/profile"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "type": "authentication_error", "message": "invalid token" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state);
    let response = app
        .oneshot(post_request(
            "/api/oauth/claude/import-token",
            json!({ "accessToken": "sk-ant-oat01-typo-token" }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].as_str().unwrap().contains("rejected"));
    server.verify().await;
}

#[tokio::test]
async fn claude_import_token_maps_broken_body_to_400() {
    // R1 acceptance: broken body is a client problem (400), not a 500.
    let _lock = ENV_LOCK.lock().unwrap();
    let state = app_state().await;
    let app = openproxy::build_app(state);
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/oauth/claude/import-token")
        .header("authorization", concat!("Bearer ", "oauth-mgmt-test-key"))
        .header("content-type", "application/json")
        .body(Body::from("{\"accessToken\":"))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn claude_import_token_preserves_existing_proxy_pool_binding() {
    // D1 regression: importing a setup token over an existing interactive
    // OAuth connection for the same email must not wipe user-managed
    // provider_specific_data (e.g. a bound proxyPoolId). The import carries
    // no data of its own, so the existing map must survive. Refresh token
    // and expiry ARE replaced — that is the documented import semantic.
    let _lock = ENV_LOCK.lock().unwrap();
    let server = MockServer::start().await;
    let _profile_url = EnvVarGuard::set(
        "OPENPROXY_CLAUDE_PROFILE_URL",
        &format!("{}/api/oauth/profile", server.uri()),
    );

    Mock::given(method("GET"))
        .and(path("/api/oauth/profile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user": { "email": "owner@example.com", "name": "Owner" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = app_state().await;
    // Seed an interactive OAuth connection with a bound proxy pool.
    state
        .db
        .update(|db| {
            db.provider_connections
                .push(openproxy::types::ProviderConnection {
                    id: "existing-1".into(),
                    provider: "claude".into(),
                    auth_type: "oauth".into(),
                    name: Some("Interactive login".into()),
                    email: Some("owner@example.com".into()),
                    display_name: Some("Owner".into()),
                    access_token: Some("old-access".into()),
                    refresh_token: Some("keep-out-refresh".into()),
                    expires_at: Some("2099-01-01T00:00:00Z".into()),
                    test_status: Some("active".into()),
                    provider_specific_data: [("proxyPoolId".to_string(), json!("pool-7"))]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                });
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/claude/import-token",
            json!({ "accessToken": "sk-ant-oat01-new-setup-token" }),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "{json}");

    let snapshot = state.db.snapshot();
    assert_eq!(
        snapshot.provider_connections.len(),
        1,
        "upsert, not duplicate"
    );
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.id, "existing-1");
    assert_eq!(
        connection.provider_specific_data.get("proxyPoolId"),
        Some(&json!("pool-7")),
        "user proxy pool binding must survive a data-less import"
    );
    // Import semantics: the fresh setup token replaces the credential set.
    assert_eq!(
        connection.access_token.as_deref(),
        Some("sk-ant-oat01-new-setup-token")
    );
    assert_eq!(connection.refresh_token, None);
    assert_eq!(connection.email.as_deref(), Some("owner@example.com"));
    server.verify().await;
}
