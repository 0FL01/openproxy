mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::lean_harness::TempTestDb;
use common::test_api_key;
use openproxy::db::Db;
use openproxy::server::api::quota_auto_ping::{
    quota_auto_ping_enabled, reconcile_quota_auto_ping, run_quota_auto_ping_tick,
    spawn_quota_auto_ping_if_enabled,
};
use openproxy::server::state::AppState;
use openproxy::types::ProviderConnection;
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn oauth_connection(provider: &str, id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "oauth".into(),
        is_active: Some(true),
        access_token: Some(format!("{provider}-access-placeholder")),
        refresh_token: Some(format!("{provider}-refresh-placeholder")),
        ..Default::default()
    }
}

fn glm_connection(id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "glm".into(),
        auth_type: "api_key".into(),
        is_active: Some(true),
        api_key: Some("glm-key-placeholder".into()),
        ..Default::default()
    }
}

fn auto_ping_value(ids: &[&str]) -> Value {
    let connections = ids
        .iter()
        .map(|id| ((*id).to_string(), Value::Bool(true)))
        .collect::<serde_json::Map<String, Value>>();
    json!({"enabled": true, "connections": connections})
}

async fn wait_for_active(state: &AppState, expected: bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if state.quota_auto_ping.is_active() == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("auto-ping lifecycle transition");
}

async fn patch_settings(app: &axum::Router, payload: Value) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/settings")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("settings request"),
        )
        .await
        .expect("settings response")
        .status()
}

#[tokio::test]
async fn empty_configuration_has_no_worker_or_control_plane_requests() {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.provider_connections = vec![
                oauth_connection("claude", "claude-1"),
                oauth_connection("codex", "codex-1"),
                glm_connection("glm-1"),
            ];
        })
        .await
        .expect("seed connections");

    let state = AppState::new(test_db.db.clone());
    assert!(!quota_auto_ping_enabled(test_db.db.snapshot().as_ref()));
    assert!(!spawn_quota_auto_ping_if_enabled(state.clone()));
    assert!(!state.quota_auto_ping.is_active());

    let tick = run_quota_auto_ping_tick(&state).await;
    assert_eq!(tick["targets"], 0);
    assert_eq!(tick["results"], json!([]));
    assert!(!state.quota_auto_ping.is_active());
    state.quota_auto_ping.shutdown().await;
}

#[tokio::test]
async fn settings_patch_starts_one_worker_and_last_disable_stops_it() {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_connections = vec![glm_connection("glm-1")];
        })
        .await
        .expect("seed settings app");
    let state = AppState::new(test_db.db.clone());
    let app = openproxy::build_app(state.clone());

    assert_eq!(
        patch_settings(&app, json!({"glmAutoPing": auto_ping_value(&["glm-1"])})).await,
        StatusCode::OK
    );
    wait_for_active(&state, true).await;
    assert!(!spawn_quota_auto_ping_if_enabled(state.clone()));
    assert_eq!(
        test_db.db.snapshot().settings.extra["glmAutoPing"]["connections"]["glm-1"],
        true
    );

    assert_eq!(
        patch_settings(
            &app,
            json!({"glmAutoPing": {"enabled": false, "connections": {"glm-1": false}}})
        )
        .await,
        StatusCode::OK
    );
    wait_for_active(&state, false).await;
    assert!(!quota_auto_ping_enabled(test_db.db.snapshot().as_ref()));
    state.quota_auto_ping.shutdown().await;

    let reloaded = Arc::new(
        Db::load_from(test_db.path())
            .await
            .expect("reload database"),
    );
    assert_eq!(
        reloaded.snapshot().settings.extra["glmAutoPing"]["connections"]["glm-1"],
        false
    );
    let restarted = AppState::new(reloaded);
    assert!(!spawn_quota_auto_ping_if_enabled(restarted.clone()));
    assert!(!restarted.quota_auto_ping.is_active());
    restarted.quota_auto_ping.shutdown().await;
}

#[tokio::test]
async fn multiple_providers_share_one_worker_and_shutdown_preserves_markers() {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            let mut claude = oauth_connection("claude", "claude-1");
            claude
                .extra
                .insert("lastPingedResetKey".into(), json!("2026-09-18T12:00:00Z"));
            claude
                .extra
                .insert("lastPingAt".into(), json!("2026-09-18T12:00:01Z"));
            let mut codex = oauth_connection("codex", "codex-1");
            codex.extra.insert(
                "codexAutoPingPending".into(),
                json!({
                    "quotaKey": "session",
                    "generationKey": "codex:session:fixture",
                    "triggerReason": "scheduled_reset",
                    "resetAt": "2026-09-18T12:00:00Z",
                    "detectedAt": "2026-09-18T11:59:59Z"
                }),
            );
            db.provider_connections = vec![claude, codex, glm_connection("glm-1")];
            db.settings
                .extra
                .insert("claudeAutoPing".into(), auto_ping_value(&["claude-1"]));
            db.settings
                .extra
                .insert("codexAutoPing".into(), auto_ping_value(&["codex-1"]));
            db.settings
                .extra
                .insert("glmAutoPing".into(), auto_ping_value(&["glm-1"]));
        })
        .await
        .expect("seed multi-provider opt-ins");

    let state = AppState::new(test_db.db.clone());
    assert!(quota_auto_ping_enabled(test_db.db.snapshot().as_ref()));
    assert!(spawn_quota_auto_ping_if_enabled(state.clone()));
    wait_for_active(&state, true).await;
    assert!(!reconcile_quota_auto_ping(&state));

    state.quota_auto_ping.shutdown().await;
    wait_for_active(&state, false).await;
    assert!(!spawn_quota_auto_ping_if_enabled(state.clone()));

    let reloaded = Arc::new(
        Db::load_from(test_db.path())
            .await
            .expect("reload database"),
    );
    let snapshot = reloaded.snapshot();
    assert_eq!(
        snapshot.settings.extra["claudeAutoPing"]["connections"]["claude-1"],
        true
    );
    assert_eq!(
        snapshot.settings.extra["codexAutoPing"]["connections"]["codex-1"],
        true
    );
    assert_eq!(
        snapshot.settings.extra["glmAutoPing"]["connections"]["glm-1"],
        true
    );
    let claude = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == "claude-1")
        .expect("reloaded Claude connection");
    assert_eq!(claude.extra["lastPingedResetKey"], "2026-09-18T12:00:00Z");
    let codex = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == "codex-1")
        .expect("reloaded Codex connection");
    assert_eq!(
        codex.extra["codexAutoPingPending"]["generationKey"],
        "codex:session:fixture"
    );
}

#[test]
fn source_has_no_default_idle_worker_and_keeps_proactive_refresh_separate() {
    let source = include_str!("../src/server/api/quota_auto_ping.rs");
    let main = include_str!("../src/main.rs");
    let proactive = include_str!("../src/oauth/background_refresh.rs");

    assert!(source.contains("pub fn quota_auto_ping_enabled"));
    assert!(source.contains("pub fn spawn_quota_auto_ping_if_enabled"));
    assert!(source.contains("shutdown_signal.notified()"));
    assert!(source.contains("lifecycle.wake.notified()"));
    assert!(!source.contains("pub fn spawn_quota_auto_ping(state"));
    assert!(!main.contains("spawn_quota_auto_ping(state.clone())"));
    assert!(main.contains("spawn_quota_auto_ping_if_enabled(state.clone())"));
    assert!(main.contains("state.quota_auto_ping.shutdown().await"));
    assert!(main.contains("spawn_background_token_refresh(state.clone().into())"));
    assert!(proactive.contains("pub fn spawn_background_token_refresh"));
}
