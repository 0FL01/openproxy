//! Claude usage endpoint serves passively observed header snapshots.
//!
//! Claude quota is no longer fetched from `/api/oauth/usage`: it is
//! observed from `anthropic-ratelimit-unified-*` headers on live
//! generation traffic and persisted under
//! `settings.extra["claudeQuotaSnapshot:<connectionId>"]`. These tests pin
//! the dashboard contract: snapshot rows render, missing snapshots say so,
//! and no upstream call is ever made.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, AppDb, ProviderConnection};
use serde_json::json;
use tower::util::ServiceExt;

fn active_key(key: &str) -> ApiKey {
    ApiKey {
        id: format!("{key}-id"),
        name: "Local".into(),
        key: key.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
    }
}

async fn app_state_with_claude_connection(
    snapshot: Option<serde_json::Value>,
) -> (AppState, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(directory.path()).await.expect("db"));
    let mut app_db: AppDb = (*db.snapshot()).clone();
    app_db.api_keys.push(active_key("usage-mgmt-key"));
    app_db.provider_connections.push(ProviderConnection {
        id: "claude-conn-1".into(),
        provider: "claude".into(),
        auth_type: "oauth".into(),
        access_token: Some("stored-access-token".into()),
        ..Default::default()
    });
    if let Some(snapshot) = snapshot {
        app_db
            .settings
            .extra
            .insert("claudeQuotaSnapshot:claude-conn-1".into(), snapshot);
    }
    db.update(move |state| *state = app_db)
        .await
        .expect("publish");
    (AppState::new(db), directory)
}

async fn usage_response(state: &AppState) -> (StatusCode, serde_json::Value) {
    let app = openproxy::build_app(state.clone());
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/usage/claude-conn-1")
        .header("authorization", "Bearer usage-mgmt-key")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.expect("oneshot");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    let json = serde_json::from_slice(&bytes).unwrap_or(json!(null));
    (status, json)
}

#[tokio::test]
async fn claude_usage_serves_passive_header_snapshot() {
    let snapshot = json!({
        "quotas": {
            "session (5h)": {
                "used": 42.0, "total": 100.0, "remaining": 58.0,
                "remainingPercentage": 58.0, "unlimited": false,
                "resetAt": "2026-10-07T00:00:00Z"
            },
            "weekly (7d)": {
                "used": 15.0, "total": 100.0, "remaining": 85.0,
                "remainingPercentage": 85.0, "unlimited": false
            }
        },
        "observedAt": "2026-10-06T22:00:00Z"
    });
    let (state, _directory) = app_state_with_claude_connection(Some(snapshot)).await;
    let (status, json) = usage_response(&state).await;
    assert_eq!(status, StatusCode::OK);
    let quotas = json["quotas"].as_object().expect("quotas rendered");
    assert_eq!(quotas.len(), 2);
    assert_eq!(quotas["session (5h)"]["used"], 42.0);
    assert_eq!(quotas["weekly (7d)"]["remainingPercentage"], 85.0);
    let message = json["message"].as_str().expect("message");
    assert!(
        message.contains("2026-10-06T22:00:00Z"),
        "observation timestamp surfaced: {message}"
    );
}

#[tokio::test]
async fn claude_usage_without_snapshot_names_the_live_traffic_source() {
    let (state, _directory) = app_state_with_claude_connection(None).await;
    let (status, json) = usage_response(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["quotas"].as_object().is_none_or(|q| q.is_empty()));
    let message = json["message"].as_str().expect("message");
    assert!(
        message.contains("first request"),
        "must point at the passive source: {message}"
    );
}
