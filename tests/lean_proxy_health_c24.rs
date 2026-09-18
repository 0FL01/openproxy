mod common;

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::core::health::{
    health_checks_enabled, run_health_tick, spawn_health_daemon_if_enabled, DEGRADED_UNTIL_KEY,
    HEALTH_CHECKED_AT_KEY, HEALTH_STATUS_KEY,
};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ProviderConnection;
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn api_key_connection(id: &str, base_url: &str) -> ProviderConnection {
    let mut connection = ProviderConnection {
        id: id.into(),
        provider: "openai-compatible-c24".into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        api_key: Some("c24-placeholder-key".into()),
        ..Default::default()
    };
    connection
        .provider_specific_data
        .insert("baseUrl".into(), json!(base_url));
    connection
}

async fn json_response(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("health request"),
        )
        .await
        .expect("health response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("health body");
    serde_json::from_slice(&bytes).expect("health JSON")
}

#[tokio::test]
async fn periodic_probe_requires_literal_true_and_default_creates_no_worker() {
    let upstream = MockUpstream::start([ScriptedResponse::json(
        StatusCode::SERVICE_UNAVAILABLE,
        r#"{"error":"down"}"#,
    )])
    .await;
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.provider_connections = vec![api_key_connection("c24-opt-in", &upstream.url("/v1"))];
        })
        .await
        .expect("seed health connection");

    let state = AppState::new(test_db.db.clone());
    assert!(!health_checks_enabled(&test_db.db.snapshot().settings));
    assert!(!spawn_health_daemon_if_enabled(state.clone()));
    let skipped = run_health_tick(&state).await;
    assert_eq!(skipped["skipped"], true);
    assert_eq!(upstream.request_count().await, 0);

    test_db
        .db
        .update(|db| {
            db.settings
                .extra
                .insert("healthCheckEnabled".into(), json!(false));
        })
        .await
        .expect("set explicit false");
    assert!(!health_checks_enabled(&test_db.db.snapshot().settings));
    assert_eq!(run_health_tick(&state).await["skipped"], true);
    assert_eq!(upstream.request_count().await, 0);

    test_db
        .db
        .update(|db| {
            db.settings
                .extra
                .insert("healthCheckEnabled".into(), json!(true));
        })
        .await
        .expect("set explicit true");
    assert!(health_checks_enabled(&test_db.db.snapshot().settings));
    let result = run_health_tick(&state).await;
    assert_eq!(result["probed"], 1);
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}

#[tokio::test]
async fn liveness_ignores_dead_upstream_and_reports_unknown_or_stale_diagnostics() {
    let test_db = TempTestDb::new().await;
    let mut historical = api_key_connection("c24-stale", "http://127.0.0.1:9/v1");
    historical
        .extra
        .insert(HEALTH_STATUS_KEY.into(), json!("healthy"));
    historical.extra.insert(
        HEALTH_CHECKED_AT_KEY.into(),
        json!((Utc::now() - Duration::hours(1)).to_rfc3339()),
    );
    historical.extra.insert(
        DEGRADED_UNTIL_KEY.into(),
        json!((Utc::now() + Duration::hours(1)).to_rfc3339()),
    );
    test_db
        .db
        .update(|db| {
            db.provider_connections = vec![
                api_key_connection("c24-unknown", "http://127.0.0.1:9/v1"),
                historical,
            ];
        })
        .await
        .expect("seed stale diagnostics");

    let state = AppState::new(test_db.db.clone());
    assert!(!spawn_health_daemon_if_enabled(state.clone()));
    let app = openproxy::build_app(state);

    let public = json_response(&app, "/health").await;
    assert_eq!(public["status"], "ok");
    assert_eq!(public["providers"]["connections"], 2);
    assert_eq!(public["providers"]["healthy"], 0);
    assert_eq!(public["providers"]["degraded"], 0);
    assert_eq!(public["providers"]["unknown"], 1);
    assert_eq!(public["providers"]["stale"], 2);
    assert!(public["providers"]["lastCheckedAt"].is_string());

    let api = json_response(&app, "/api/health").await;
    assert_eq!(api["ok"], true);
    assert_eq!(api["providers"]["healthy"], 0);
    assert_eq!(api["providers"]["degraded"], 0);
}

#[tokio::test]
async fn manual_connection_test_publishes_timestamped_diagnostic() {
    let upstream = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        r#"{"object":"list","data":[]}"#,
    )])
    .await;
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_connections = vec![api_key_connection("c24-manual", &upstream.url("/v1"))];
        })
        .await
        .expect("seed manual diagnostic");

    let app = openproxy::build_app(AppState::new(test_db.db.clone()));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/providers/c24-manual/test")
                .header("authorization", "Bearer test-key")
                .body(Body::empty())
                .expect("manual test request"),
        )
        .await
        .expect("manual test response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("manual test body");
    let payload: Value = serde_json::from_slice(&body).expect("manual test JSON");
    assert_eq!(payload["valid"], true);

    let snapshot = test_db.db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == "c24-manual")
        .expect("manual connection remains");
    assert!(connection.last_tested.is_some());
    assert_eq!(
        connection.extra.get(HEALTH_STATUS_KEY),
        Some(&json!("healthy"))
    );
    assert!(connection.extra.contains_key(HEALTH_CHECKED_AT_KEY));

    let health = json_response(&app, "/api/health").await;
    assert_eq!(health["providers"]["connections"], 1);
    assert_eq!(health["providers"]["healthy"], 1);
    assert_eq!(health["providers"]["unknown"], 0);
    assert_eq!(health["providers"]["stale"], 0);
    assert!(health["providers"]["lastCheckedAt"].is_string());
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}

#[tokio::test]
async fn legacy_health_values_round_trip_and_breaker_state_is_absent() {
    let test_db = TempTestDb::new().await;
    let future = (Utc::now() + Duration::hours(2)).to_rfc3339();
    let mut connection = api_key_connection("c24-legacy", "http://127.0.0.1:9/v1");
    connection
        .extra
        .insert(HEALTH_STATUS_KEY.into(), json!("unavailable"));
    connection
        .extra
        .insert(HEALTH_CHECKED_AT_KEY.into(), json!("2026-09-18T12:00:00Z"));
    connection
        .extra
        .insert(DEGRADED_UNTIL_KEY.into(), json!(future.clone()));
    connection
        .extra
        .insert("unknownHealthField".into(), json!({"kept": true}));

    test_db
        .db
        .update(|db| {
            db.settings
                .extra
                .insert("healthCheckEnabled".into(), json!(true));
            db.provider_connections = vec![connection];
        })
        .await
        .expect("persist legacy values");

    let reloaded = Arc::new(
        Db::load_from(test_db.path())
            .await
            .expect("reload database"),
    );
    let snapshot = reloaded.snapshot();
    assert_eq!(snapshot.settings.extra["healthCheckEnabled"], json!(true));
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.extra[HEALTH_STATUS_KEY], json!("unavailable"));
    assert_eq!(connection.extra[DEGRADED_UNTIL_KEY], json!(future));
    assert_eq!(
        connection.extra["unknownHealthField"],
        json!({"kept": true})
    );

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!root.join("src/core/circuit_breaker.rs").exists());
    let core = include_str!("../src/core/mod.rs");
    let state = include_str!("../src/server/state.rs");
    let main = include_str!("../src/main.rs");
    let chat = include_str!("../src/server/api/chat.rs");
    assert!(!core.contains("circuit_breaker"));
    assert!(!state.contains("CircuitBreaker"));
    assert!(main.contains("spawn_health_daemon_if_enabled"));
    assert!(!main.contains("spawn_health_daemon(state.clone())"));
    assert!(!chat.contains("is_account_degraded("));
    assert!(!chat.contains("is_model_degraded("));
    assert!(root.join("src/server/auth/login_limiter.rs").exists());
    assert!(root.join("src/server/auth/mod.rs").exists());
    assert!(root.join("src/core/tls/mod.rs").exists());
}
