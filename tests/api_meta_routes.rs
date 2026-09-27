use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "api-meta-test-key";

async fn build_test_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "test-key-id".to_string(),
            name: "test".to_string(),
            key: TEST_KEY.to_string(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: Default::default(),
        }];
        state.settings.require_login = false;
    })
    .await
    .expect("seed auth");
    AppState::new(db)
}

async fn build_test_app() -> axum::Router {
    openproxy::build_app(build_test_state().await)
}

#[tokio::test]
async fn api_build_returns_compile_time_commit_or_null() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/build")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let commit = env!("OPENPROXY_BUILD_COMMIT");
    assert_eq!(
        json,
        serde_json::json!({"commit": (!commit.is_empty()).then_some(commit)})
    );
    if let Some(commit) = json["commit"].as_str() {
        assert!(matches!(commit.len(), 40 | 64));
        assert!(commit.bytes().all(|byte| byte.is_ascii_hexdigit()));
    } else {
        assert!(json["commit"].is_null());
    }
}

#[tokio::test]
async fn api_build_uses_dashboard_admin_auth_policy() {
    let state = build_test_state().await;
    state
        .db
        .update(|db| {
            db.settings.require_login = true;
            db.settings.password = Some("configured-password-hash".into());
        })
        .await
        .unwrap();
    let app = openproxy::build_app(state);
    for (key, status) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("invalid-key"), StatusCode::UNAUTHORIZED),
        (Some(TEST_KEY), StatusCode::OK),
    ] {
        let mut request = Request::builder().uri("/api/build");
        if let Some(key) = key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
}

#[tokio::test]
async fn api_health_returns_sidecar_compatible_payload() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], serde_json::json!(true));
    assert!(
        json["providers"].is_object(),
        "health carries providers summary: {json}"
    );
}

#[tokio::test]
async fn cloud_auth_route_is_served_by_rust() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/cloud/auth")
                .header("Authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["connections"], serde_json::json!([]));
    assert!(json["modelAliases"].is_object());
}

#[tokio::test]
async fn settings_proxy_test_route_rejects_missing_proxy_url() {
    let app = build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/proxy-test")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "ok": false, "error": "proxyUrl is required" })
    );
}
