mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::core::model::models_dev::ModelsDevCatalog;
use openproxy::server::state::AppState;
use openproxy::types::{CustomModel, ProviderConnection};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::util::ServiceExt;

fn models_fixture(zen_id: &str, context: u32) -> Value {
    json!({
        "opencode": {
            "npm": "@ai-sdk/openai-compatible",
            "models": {
                zen_id: {
                    "id": zen_id,
                    "name": format!("C19 {zen_id}"),
                    "provider": {"npm": "@ai-sdk/openai"},
                    "cost": {"input": 0, "output": 0},
                    "limit": {"context": context, "input": context - 1000, "output": 1000},
                    "reasoning": true,
                    "tool_call": true
                }
            }
        },
        "opencode-go": {
            "npm": "@ai-sdk/openai-compatible",
            "models": {}
        }
    })
}

fn opencode_connection(enabled: &[&str]) -> ProviderConnection {
    ProviderConnection {
        id: "c19-opencode".into(),
        provider: "opencode-zen".into(),
        auth_type: "none".into(),
        is_active: Some(true),
        priority: Some(1),
        provider_specific_data: BTreeMap::from([("enabledModels".into(), json!(enabled))]),
        ..Default::default()
    }
}

async fn post_unknown_chat(app: &axum::Router) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "opencode-zen/not-in-published-snapshot",
                        "messages": [{"role": "user", "content": "hello"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .expect("C19 chat request"),
        )
        .await
        .expect("C19 chat response")
}

#[tokio::test]
async fn hung_refresh_does_not_block_generation_catalog_read() {
    let release = Arc::new(Notify::new());
    let refreshed = models_fixture("new-model", 700_000);
    let upstream =
        MockUpstream::start([
            ScriptedResponse::json(StatusCode::OK, refreshed.to_string())
                .waiting_for(release.clone()),
        ])
        .await;
    let catalog = Arc::new(
        ModelsDevCatalog::from_json_with_endpoint(
            models_fixture("old-model", 600_000),
            upstream.url("/api.json"),
        )
        .expect("seed C19 catalog"),
    );

    let temp = TempTestDb::new().await;
    temp.db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_connections = vec![opencode_connection(&["old-model"])];
        })
        .await
        .expect("seed C19 database");
    let mut state = AppState::new(temp.db.clone());
    state.models_dev = catalog.clone();
    let app = openproxy::build_app(state);

    let refresh_catalog = catalog.clone();
    let refresh = tokio::spawn(async move { refresh_catalog.refresh().await });
    upstream.wait_for_requests(1).await;

    let before = catalog.load();
    assert!(before.find("opencode-zen", "old-model").is_some());
    let response = tokio::time::timeout(Duration::from_millis(250), post_unknown_chat(&app))
        .await
        .expect("generation must not wait for models.dev refresh");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read C19 error");
    assert!(String::from_utf8_lossy(&body).contains("published models.dev snapshot"));
    assert!(Arc::ptr_eq(&before, &catalog.load()));

    release.notify_waiters();
    let after = refresh
        .await
        .expect("join C19 refresh")
        .expect("refresh C19 catalog");
    assert!(after.find("opencode-zen", "new-model").is_some());
    assert!(catalog.load().find("opencode-zen", "old-model").is_none());
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}

#[tokio::test]
async fn failed_refresh_preserves_prior_arc_and_bundled_cold_start_is_nonempty() {
    let upstream = MockUpstream::start([ScriptedResponse::json(
        StatusCode::SERVICE_UNAVAILABLE,
        "catalog unavailable",
    )])
    .await;
    let catalog = ModelsDevCatalog::from_json_with_endpoint(
        models_fixture("stable-model", 500_000),
        upstream.url("/api.json"),
    )
    .expect("seed stable C19 catalog");
    let before = catalog.load();
    let error = catalog.refresh().await.expect_err("refresh should fail");
    assert!(error.contains("HTTP 503"));
    assert!(Arc::ptr_eq(&before, &catalog.load()));
    assert!(catalog
        .load()
        .find("opencode-zen", "stable-model")
        .is_some());
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;

    let bundled = ModelsDevCatalog::default().load();
    assert!(
        bundled
            .models("opencode-zen")
            .is_some_and(|models| !models.is_empty()),
        "cold start must have bundled OpenCode Zen metadata"
    );
    assert!(
        bundled
            .models("opencode-go")
            .is_some_and(|models| !models.is_empty()),
        "cold start must have bundled OpenCode Go metadata"
    );
}

#[tokio::test]
async fn published_snapshot_preserves_enabled_custom_disabled_and_advertised_metadata() {
    let temp = TempTestDb::new().await;
    temp.db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_connections = vec![opencode_connection(&[
                "remote-model",
                "disabled-model",
                "custom-model",
            ])];
            db.custom_models = vec![CustomModel {
                provider_alias: "opencode-zen".into(),
                id: "custom-model".into(),
                r#type: "llm".into(),
                name: Some("C19 Custom".into()),
                extra: BTreeMap::from([(
                    "opencode".into(),
                    json!({
                        "limit": {"context": 300000, "input": 250000, "output": 50000},
                        "tool_call": true
                    }),
                )]),
            }];
            db.extra.insert(
                "disabledModels".into(),
                json!({"opencode-zen": ["disabled-model"]}),
            );
            db.settings
                .provider_context_limits
                .insert("opencode-zen".into(), 450_000);
        })
        .await
        .expect("seed C19 model configuration");

    let catalog = ModelsDevCatalog::from_json_with_endpoint(
        json!({
            "opencode": {
                "npm": "@ai-sdk/openai-compatible",
                "models": {
                    "remote-model": {
                        "id": "remote-model", "name": "C19 Remote",
                        "provider": {"npm": "@ai-sdk/openai"},
                        "cost": {"input": 0, "output": 0},
                        "limit": {"context": 600000, "input": 550000, "output": 50000}
                    },
                    "disabled-model": {
                        "id": "disabled-model", "name": "C19 Disabled",
                        "cost": {"input": 0, "output": 0}
                    }
                }
            },
            "opencode-go": {"npm": "@ai-sdk/openai-compatible", "models": {}}
        }),
        "http://127.0.0.1:9/never-called",
    )
    .expect("seed C19 metadata");
    let mut state = AppState::new(temp.db.clone());
    state.models_dev = Arc::new(catalog);
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "Bearer test-key")
                .body(Body::empty())
                .expect("C19 models request"),
        )
        .await
        .expect("C19 models response");
    assert_eq!(response.status(), StatusCode::OK);
    let payload: Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .expect("read C19 models"),
    )
    .expect("parse C19 models");
    let rows = payload["data"].as_array().expect("models data");
    assert!(!rows
        .iter()
        .any(|row| row["id"] == "opencode-zen/disabled-model"));

    let remote = rows
        .iter()
        .find(|row| row["id"] == "opencode-zen/remote-model")
        .expect("published remote model");
    assert_eq!(remote["opencode"]["source"], "opencode-zen");
    assert_eq!(remote["opencode"]["limit"]["context"], 450_000);
    assert_eq!(remote["opencode"]["limit"]["input"], 450_000);

    let custom = rows
        .iter()
        .find(|row| row["id"] == "opencode-zen/custom-model")
        .expect("configured custom model");
    assert_eq!(custom["opencode"]["name"], "C19 Custom");
    assert_eq!(custom["opencode"]["source"], "opencode-zen");
    assert_eq!(custom["opencode"]["limit"]["context"], 300_000);
    assert_eq!(custom["opencode"]["tool_call"], true);
}

#[test]
fn source_guard_keeps_remote_catalog_out_of_readers() {
    let model_source = include_str!("../src/core/model/models_dev.rs");
    let chat_source = include_str!("../src/server/api/chat.rs");
    let catalog_source = include_str!("../src/server/api/mod.rs");
    let v1_source = include_str!("../src/server/api/v1_models.rs");
    let provider_source = include_str!("../src/server/api/provider_models.rs");

    assert!(model_source.contains("published: ArcSwap<ModelsDevSnapshot>"));
    assert!(model_source.contains("pub fn load(&self)"));
    assert!(!model_source.contains("struct CacheState"));
    assert!(!chat_source.contains("models_dev\n            .snapshot()"));
    assert!(chat_source.contains("let models = state.models_dev.load();"));
    assert!(catalog_source.contains("let snapshot = state.models_dev.load();"));
    assert!(v1_source.contains("Some(state.models_dev.load())"));
    assert!(provider_source.contains("models_dev.refresh_if_stale().await"));
}
