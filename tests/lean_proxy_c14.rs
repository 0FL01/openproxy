mod common;

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

const REQUEST_COUNT: usize = 16;

fn successful_response() -> ScriptedResponse {
    ScriptedResponse::json(
        StatusCode::OK,
        r#"{"id":"chatcmpl-c14","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
    )
}

fn node(base_url: String) -> ProviderNode {
    ProviderNode {
        id: "legacy-node".into(),
        r#type: "openai-compatible".into(),
        name: "C14 loopback".into(),
        prefix: Some("legacy".into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

fn connection(id: &str, priority: u32, key: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "legacy-node".into(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        priority: Some(priority),
        is_active: Some(true),
        default_model: Some("gpt-4.1".into()),
        api_key: Some(key.into()),
        ..Default::default()
    }
}

fn add_legacy_diagnostics(connection: &mut ProviderConnection, future: &str) {
    connection.test_status = Some("unavailable".into());
    connection.last_error = Some("legacy diagnostic".into());
    connection.last_error_at = Some("2026-09-17T12:00:00Z".into());
    connection.rate_limited_until = Some(future.into());
    connection.error_code = Some("legacy_error".into());
    connection.backoff_level = Some(7);
    connection.consecutive_errors = Some(9);
    connection
        .extra
        .insert("modelLock_gpt-4.1".into(), Value::String(future.into()));
    connection
        .extra
        .insert("degradedUntil".into(), Value::String(future.into()));
}

async fn chat_request(app: &axum::Router) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "legacy/gpt-4.1",
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .expect("chat request"),
        )
        .await
        .expect("chat response")
}

#[tokio::test]
async fn successful_generation_ignores_and_preserves_legacy_diagnostics_without_db_update() {
    let upstream =
        MockUpstream::start(std::iter::repeat_with(successful_response).take(REQUEST_COUNT)).await;
    let test_db = TempTestDb::new().await;
    let future = (Utc::now() + Duration::hours(1)).to_rfc3339();
    let mut preferred = connection("preferred", 1, "preferred-key");
    add_legacy_diagnostics(&mut preferred, &future);
    let expected = preferred.clone();

    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![node(upstream.url("/v1"))];
            db.provider_connections = vec![preferred];
            for index in 0..256 {
                db.provider_connections.push(ProviderConnection {
                    id: format!("unrelated-{index}"),
                    provider: "unrelated".into(),
                    auth_type: "apikey".into(),
                    is_active: Some(true),
                    api_key: Some(format!("unused-{index}")),
                    ..Default::default()
                });
            }
        })
        .await
        .expect("seed C14 database");

    let before = test_db.db.snapshot();
    let state = AppState::new(test_db.db.clone());
    let app = openproxy::build_app(state);

    for _ in 0..REQUEST_COUNT {
        let response = chat_request(&app).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("read successful body");
        assert!(String::from_utf8_lossy(&body).contains("chatcmpl-c14"));
    }

    let after = test_db.db.snapshot();
    assert!(
        Arc::ptr_eq(&before, &after),
        "successful headers must not publish an AppDb clone"
    );
    let actual = after
        .provider_connections
        .iter()
        .find(|candidate| candidate.id == "preferred")
        .expect("preferred connection remains");
    assert_eq!(
        actual, &expected,
        "diagnostic fields remain round-trippable"
    );

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), REQUEST_COUNT);
    assert!(requests
        .iter()
        .all(|request| request.headers["authorization"] == "Bearer preferred-key"));
    upstream.shutdown().await;
}

#[tokio::test]
async fn explicit_clear_remains_narrow_and_does_not_reinterpret_other_diagnostics() {
    let test_db = TempTestDb::new().await;
    let future = (Utc::now() + Duration::hours(1)).to_rfc3339();
    let mut legacy = connection("legacy", 1, "legacy-key");
    add_legacy_diagnostics(&mut legacy, &future);
    legacy.extra.insert(
        "modelLock_other-model".into(),
        Value::String(future.clone()),
    );

    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_connections = vec![legacy];
        })
        .await
        .expect("seed explicit-clear database");

    let app = openproxy::build_app(AppState::new(test_db.db.clone()));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/models/availability")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "action": "clearCooldown",
                        "provider": "legacy-node",
                        "model": "gpt-4.1"
                    })
                    .to_string(),
                ))
                .expect("clear request"),
        )
        .await
        .expect("clear response");
    assert_eq!(response.status(), StatusCode::OK);

    let snapshot = test_db.db.snapshot();
    let actual = &snapshot.provider_connections[0];
    assert_eq!(actual.extra.get("modelLock_gpt-4.1"), Some(&Value::Null));
    assert_eq!(
        actual.extra.get("modelLock_other-model"),
        Some(&Value::String(future.clone()))
    );
    assert_eq!(
        actual.extra.get("degradedUntil"),
        Some(&Value::String(future.clone()))
    );
    assert_eq!(actual.rate_limited_until.as_deref(), Some(future.as_str()));
    assert_eq!(actual.error_code.as_deref(), Some("legacy_error"));
    assert_eq!(actual.consecutive_errors, Some(9));
    assert_eq!(actual.test_status.as_deref(), Some("active"));
    assert_eq!(actual.last_error, None);
    assert_eq!(actual.backoff_level, Some(0));
}
