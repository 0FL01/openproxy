//! The removed subscription integration cannot execute or mutate saved data.

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use common::lean_harness::{MockUpstream, TempTestDb};
use common::test_api_key;
use openproxy::core::executor::{
    ApiKeyExecutionRequest, ApiKeyExecutor, ClientPool, ProviderExecutorConfig, UnifiedExecutor,
};
use openproxy::core::model::catalog::provider_catalog;
use openproxy::server::state::AppState;
use openproxy::types::{CustomModel, ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn request(method: Method, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn retired_oauth_surfaces_never_authorize_exchange_import_or_refresh() {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| db.api_keys = vec![test_api_key()])
        .await
        .unwrap();
    let state = AppState::new(test_db.db.clone());
    let app = openproxy::build_app(state.clone());
    for provider in ["claude", "cc", "anthropic", "claude-agent"] {
        for (method, action) in [
            (Method::GET, "authorize"),
            (Method::GET, "start"),
            (Method::GET, "status"),
            (Method::POST, "exchange"),
            (Method::POST, "refresh"),
        ] {
            let response = app
                .clone()
                .oneshot(request(
                    method,
                    &format!("/api/oauth/{provider}/{action}"),
                    json!({}),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::GONE, "{provider}/{action}");
            let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(value.get("authUrl").is_none());
            assert!(value.get("accessToken").is_none());
        }
    }
    let response = app
        .oneshot(request(
            Method::POST,
            "/api/oauth/claude/import-token",
            json!({"accessToken":"sentinel-retired-token"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(state.db.snapshot().provider_connections.is_empty());
    state.signal_shutdown();
}

#[tokio::test]
async fn saved_subscription_entries_are_inert_and_survive_round_trip_unchanged() {
    let upstream = MockUpstream::start([]).await;
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            for provider in ["claude", "cc", "anthropic"] {
                db.provider_connections.push(ProviderConnection {
                    id: format!("legacy-{provider}"),
                    provider: provider.into(),
                    auth_type: "oauth".into(),
                    name: Some(format!("saved-{provider}")),
                    is_active: Some(true),
                    access_token: Some("sentinel-retired-access".into()),
                    refresh_token: Some("sentinel-retired-refresh".into()),
                    expires_at: Some("2000-01-01T00:00:00Z".into()),
                    runtime_transport: Some(openproxy::types::RuntimeTransport {
                        base_url: Some(upstream.url("/v1/messages")),
                    }),
                    provider_specific_data: std::collections::BTreeMap::from([
                        ("baseUrl".into(), json!(upstream.url(""))),
                        ("subscription".into(), json!("legacy-plan")),
                        ("unknown-value".into(), json!({"keep": true})),
                    ]),
                    ..Default::default()
                });
            }
            db.provider_nodes.push(ProviderNode {
                id: "claude".into(),
                r#type: "anthropic-compatible".into(),
                prefix: Some("cc".into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            });
            db.custom_models.push(CustomModel {
                provider_alias: "saved-claude".into(),
                id: "legacy-model".into(),
                name: None,
                r#type: "llm".into(),
                extra: Default::default(),
            });
            db.settings.extra.insert(
                "claudeQuotaSnapshot:legacy-claude".into(),
                json!({"quotas":{"session (5h)":{"used":42}},"observedAt":"2026-10-07T00:00:00Z"}),
            );
            db.settings
                .provider_context_limits
                .insert("claude".into(), 700_000);
        })
        .await
        .unwrap();
    // Compare actual persisted representations, including SQLite's existing
    // ordering and nullable-timestamp normalization, not seed-only defaults.
    test_db.db.reload_snapshot().await.unwrap();
    let before = serde_json::to_value(&test_db.db.snapshot().provider_connections).unwrap();
    let custom_before = serde_json::to_value(&test_db.db.snapshot().custom_models).unwrap();
    let state = AppState::new(test_db.db.clone());
    let app = openproxy::build_app(state.clone());
    for provider in ["claude", "cc", "anthropic"] {
        let response = app.clone().oneshot(request(Method::POST, "/v1/chat/completions", json!({"model":format!("{provider}/claude-sonnet-4-6"),"messages":[{"role":"user","content":"fixture"}],"stream":false}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE, "inference {provider}");
        // The translator has its own direct reqwest sender and header preview;
        // neither may revive/export a retired subscription credential.
        for (uri, payload) in [
            (
                "/api/translator/send",
                json!({"provider":provider,"model":"fixture","body":{"messages":[],"stream":false}}),
            ),
            (
                "/api/translator/translate",
                json!({"step":3,"provider":provider,"model":"fixture","body":{"messages":[]}}),
            ),
        ] {
            let response = app
                .clone()
                .oneshot(request(Method::POST, uri, payload))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::GONE, "{provider} {uri}");
            let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("sentinel-retired-access"));
        }
        for (method, uri) in [
            (
                Method::GET,
                format!("/api/providers/legacy-{provider}/models"),
            ),
            (
                Method::POST,
                format!("/api/providers/legacy-{provider}/test"),
            ),
            (
                Method::POST,
                format!("/api/providers/legacy-{provider}/import-models"),
            ),
            (Method::GET, format!("/api/usage/legacy-{provider}")),
        ] {
            let response = app
                .clone()
                .oneshot(request(method, &uri, json!({})))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::GONE, "{uri}");
        }
    }
    openproxy::oauth::background_refresh::run_tick(&state).await;
    assert_eq!(upstream.request_count().await, 0);
    test_db.db.reload_snapshot().await.unwrap();
    let after = test_db.db.snapshot();
    assert_eq!(
        serde_json::to_value(&after.provider_connections).unwrap(),
        before
    );
    assert_eq!(
        serde_json::to_value(&after.custom_models).unwrap(),
        custom_before
    );
    assert_eq!(after.settings.provider_context_limits["claude"], 700_000);
    assert_eq!(
        after.settings.extra["claudeQuotaSnapshot:legacy-claude"]["quotas"]["session (5h)"]["used"],
        42
    );
    state.signal_shutdown();
    upstream.shutdown().await;
}

#[test]
fn native_catalog_never_reuses_the_retired_subscription_inventory() {
    let catalog = provider_catalog();
    assert!(catalog.provider_info("claude").is_none());
    assert!(catalog.static_alias_for_provider("claude").is_none());
    assert!(catalog.models_for_alias("cc").is_none());
    assert!(catalog.provider_info("claude-agent").is_some());
    assert!(catalog.models_for_alias("claude-agent").unwrap().is_empty());
    assert!(catalog.provider_info("anthropic").is_some());
    assert!(catalog
        .models_for_alias("anthropic")
        .is_some_and(|models| !models.is_empty()));
}

#[test]
fn public_generic_executor_also_rejects_subscription_and_native_sources() {
    for provider in ["claude", "cc", "claude-agent", "anthropic"] {
        let executor = UnifiedExecutor::new(
            provider,
            ProviderExecutorConfig::anthropic("http://127.0.0.1:1"),
            std::sync::Arc::new(ClientPool::new()),
        );
        let connection = ProviderConnection {
            provider: provider.into(),
            auth_type: "oauth".into(),
            access_token: Some("sentinel-retired-access".into()),
            api_key: Some("sentinel-retired-key".into()),
            ..Default::default()
        };
        let error = executor.build_headers(&connection, false).unwrap_err();
        assert!(error.to_string().contains(if provider == "claude-agent" {
            "native v2"
        } else {
            "retired"
        }));
    }
}

#[tokio::test]
async fn public_api_key_executor_never_falls_back_to_subscription_tokens() {
    let upstream = MockUpstream::start([]).await;
    for provider in ["claude", "cc", "claude-agent", "anthropic"] {
        let executor = ApiKeyExecutor::new(provider, &upstream.url("/v1"), "x-api-key");
        let result = executor
            .execute(ApiKeyExecutionRequest {
                model: "fixture".into(),
                body: json!({"messages": []}),
                stream: false,
                credentials: ProviderConnection {
                    provider: provider.into(),
                    auth_type: "oauth".into(),
                    access_token: Some("sentinel-retired-token".into()),
                    ..Default::default()
                },
                proxy: None,
            })
            .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains(if provider == "claude-agent" {
            "native v2"
        } else {
            "retired"
        }));
    }
    assert_eq!(upstream.request_count().await, 0);
    upstream.shutdown().await;
}

#[tokio::test]
async fn native_models_read_saved_safe_inventory_without_spawning_or_importing_copies() {
    let test_db = TempTestDb::new().await;
    test_db.db.update(|db| {
        db.api_keys = vec![test_api_key()];
        db.provider_connections.push(ProviderConnection {
            id: "native".into(), provider: "claude-agent".into(), auth_type: "external".into(),
            provider_specific_data: std::collections::BTreeMap::from([("claudeAgentModels".into(), json!([
                {"id":"opus","name":"Native Opus","kind":"llm","contextWindow":1000000,"efforts":["low","max","not-an-effort"],"privateMetadata":"sentinel-native-private"},
                {"id":"bad model","name":"Invalid ID"}
            ]))]), ..Default::default()
        });
        db.custom_models.push(CustomModel { provider_alias: "claude-agent".into(), id: "user-model".into(), name: Some("User model".into()), r#type: "llm".into(), extra: Default::default() });
    }).await.unwrap();
    let state = AppState::new(test_db.db.clone());
    let before = serde_json::to_value(&state.db.snapshot().provider_connections).unwrap();
    let app = openproxy::build_app(state.clone());
    let response = app
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/providers/native/models",
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        json!({"provider":"claude-agent","connectionId":"native","models":[{"id":"opus","name":"Native Opus","kind":"llm","contextWindow":1000000,"efforts":["low","max"]}]})
    );
    let response = app
        .oneshot(request(
            Method::POST,
            "/api/providers/native/import-models",
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::to_value(&state.db.snapshot().provider_connections).unwrap(),
        before
    );
    assert_eq!(state.db.snapshot().custom_models.len(), 1);
    assert_eq!(state.db.snapshot().custom_models[0].id, "user-model");
    assert!(
        !test_db.path().join("claude-agent").exists(),
        "model discovery/import rejection must not start Claude Code"
    );
    state.signal_shutdown();
}

#[tokio::test]
async fn malformed_native_markers_never_enter_http_or_oauth_connection_testing() {
    let upstream = MockUpstream::start([]).await;
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            for auth_type in ["apikey", "oauth"] {
                db.provider_connections.push(ProviderConnection {
                    id: format!("native-{auth_type}"),
                    provider: "claude-agent".into(),
                    auth_type: auth_type.into(),
                    api_key: Some("sentinel-native-key".into()),
                    access_token: Some("sentinel-native-token".into()),
                    use_connection_proxy: Some(true),
                    proxy_url: Some(upstream.url("")),
                    ..Default::default()
                });
            }
        })
        .await
        .unwrap();
    let state = AppState::new(test_db.db.clone());
    let app = openproxy::build_app(state.clone());
    for auth_type in ["apikey", "oauth"] {
        let response = app
            .clone()
            .oneshot(request(
                Method::POST,
                &format!("/api/providers/native-{auth_type}/test"),
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(upstream.request_count().await, 0);
    assert!(!test_db.path().join("claude-agent").exists());
    state.signal_shutdown();
    upstream.shutdown().await;
}
