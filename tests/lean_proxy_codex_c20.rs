mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::server::codex_catalog::CodexModelCatalog;
use openproxy::server::state::AppState;
use openproxy::types::{CustomModel, ProviderConnection, ProviderNode};
use serde_json::json;
use tokio::sync::Notify;
use tower::util::ServiceExt;

fn codex_connection(id: &str, priority: u32) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "codex".into(),
        auth_type: "oauth".into(),
        priority: Some(priority),
        is_active: Some(true),
        access_token: Some(format!("fixture-{id}-access")),
        ..Default::default()
    }
}

fn codex_node(endpoint: String) -> ProviderNode {
    ProviderNode {
        id: "codex".into(),
        r#type: "codex".into(),
        name: "Codex C20 loopback".into(),
        prefix: Some("cx".into()),
        api_type: Some("responses".into()),
        base_url: Some(endpoint),
        ..Default::default()
    }
}

fn catalog_payload(models: &[(&str, bool)]) -> String {
    json!({
        "models": models.iter().map(|(id, search)| json!({
            "slug": id,
            "display_name": id,
            "visibility": "list",
            "supported_in_api": true,
            "max_context_window": 272000,
            "supported_reasoning_levels": [{"effort":"high"}],
            "input_modalities": ["text"],
            "supports_search_tool": search
        })).collect::<Vec<_>>()
    })
    .to_string()
}

async fn state_with_catalog(
    catalog_url: String,
    generation_url: String,
    connections: Vec<ProviderConnection>,
    custom_models: Vec<CustomModel>,
) -> (TempTestDb, AppState) {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(move |db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![codex_node(generation_url)];
            db.provider_connections = connections;
            db.custom_models = custom_models;
        })
        .await
        .expect("seed C20 database");
    let mut state = AppState::new(test_db.db.clone());
    state.codex_models = Arc::new(CodexModelCatalog::with_models_url(catalog_url));
    (test_db, state)
}

async fn post_codex(app: &axum::Router, model: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": format!("codex/{model}"),
                        "messages": [{"role":"user","content":"published catalog"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .expect("C20 request"),
        )
        .await
        .expect("C20 response")
}

#[tokio::test]
async fn known_chat_uses_published_inventory_while_forced_refresh_is_held() {
    let release_refresh = Arc::new(Notify::new());
    let catalog = MockUpstream::start([
        ScriptedResponse::json(StatusCode::OK, catalog_payload(&[("gpt-known", true)])),
        ScriptedResponse::json(
            StatusCode::OK,
            catalog_payload(&[("gpt-known", true), ("gpt-new", false)]),
        )
        .holding_eof(release_refresh.clone()),
    ])
    .await;
    let generation = MockUpstream::start([ScriptedResponse::sse([concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n"
    )])])
    .await;
    let connection = codex_connection("codex-a", 1);
    let (_db, state) = state_with_catalog(
        catalog.url("/backend-api/codex/models"),
        generation.url("/backend-api/codex/responses"),
        vec![connection.clone()],
        Vec::new(),
    )
    .await;

    state
        .codex_models
        .refresh_connection(&state, &connection, true)
        .await
        .expect("seed published Codex inventory");
    let prior_identity = state.codex_models.published_snapshot_identity();
    let refresh_state = state.clone();
    let refresh_connection = connection.clone();
    let refresh = tokio::spawn(async move {
        refresh_state
            .codex_models
            .refresh_connection(&refresh_state, &refresh_connection, true)
            .await
    });
    catalog.wait_for_requests(2).await;

    let app = openproxy::build_app(state.clone());
    let response = tokio::time::timeout(Duration::from_millis(500), post_codex(&app, "gpt-known"))
        .await
        .expect("known Codex chat must not wait for catalog HTTP");
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    assert_eq!(generation.request_count().await, 1);
    assert!(state
        .codex_models
        .published_snapshot_identity()
        .ptr_eq(&prior_identity));

    release_refresh.notify_waiters();
    refresh
        .await
        .expect("join forced refresh")
        .expect("forced refresh succeeds");
    assert!(!state
        .codex_models
        .published_snapshot_identity()
        .ptr_eq(&prior_identity));
    assert!(state
        .codex_models
        .cached_supporters("gpt-new", &state.db.snapshot())
        .contains(&connection.id));
    catalog.shutdown().await;
    generation.shutdown().await;
}

#[tokio::test]
async fn failed_refresh_keeps_prior_publication_and_unknown_model_never_routes() {
    let catalog = MockUpstream::start([
        ScriptedResponse::json(StatusCode::OK, catalog_payload(&[("gpt-known", false)])),
        ScriptedResponse::json(StatusCode::SERVICE_UNAVAILABLE, "catalog unavailable"),
    ])
    .await;
    let generation =
        MockUpstream::start([ScriptedResponse::sse(["data: must-not-be-requested\n\n"])]).await;
    let connection = codex_connection("codex-b", 1);
    let (_db, state) = state_with_catalog(
        catalog.url("/backend-api/codex/models"),
        generation.url("/backend-api/codex/responses"),
        vec![connection.clone()],
        Vec::new(),
    )
    .await;
    state
        .codex_models
        .refresh_connection(&state, &connection, true)
        .await
        .expect("seed Codex inventory");
    let prior_identity = state.codex_models.published_snapshot_identity();
    let stale = state
        .codex_models
        .refresh_connection(&state, &connection, true)
        .await
        .expect("failed refresh serves prior publication");
    assert!(stale.warning.is_some());
    assert!(state
        .codex_models
        .published_snapshot_identity()
        .ptr_eq(&prior_identity));

    let app = openproxy::build_app(state.clone());
    let response = post_codex(&app, "gpt-unknown").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(generation.request_count().await, 0);
    catalog.shutdown().await;
    generation.shutdown().await;
}

#[tokio::test]
async fn configuration_reconciliation_removes_inactive_and_tracks_explicit_custom_models() {
    let catalog = MockUpstream::start([]).await;
    let generation = MockUpstream::start([]).await;
    let mut connection = codex_connection("codex-config", 1);
    connection
        .provider_specific_data
        .insert("enabledModels".into(), json!(["gpt-explicit"]));
    let custom = CustomModel {
        provider_alias: "cx".into(),
        id: "gpt-custom".into(),
        r#type: "llm".into(),
        name: Some("Custom".into()),
        extra: BTreeMap::new(),
    };
    let (test_db, state) = state_with_catalog(
        catalog.url("/backend-api/codex/models"),
        generation.url("/backend-api/codex/responses"),
        vec![connection.clone()],
        vec![custom],
    )
    .await;
    let snapshot = state.db.snapshot();
    assert_eq!(
        state
            .codex_models
            .cached_supporters("gpt-explicit", &snapshot),
        std::collections::HashSet::from([connection.id.clone()])
    );
    assert_eq!(
        state
            .codex_models
            .cached_supporters("gpt-custom", &snapshot),
        std::collections::HashSet::from([connection.id.clone()])
    );
    assert!(state
        .codex_models
        .cached_supporters("gpt-unknown", &snapshot)
        .is_empty());

    test_db
        .db
        .update(|db| {
            db.provider_connections[0].is_active = Some(false);
            db.custom_models.clear();
        })
        .await
        .expect("disable Codex connection");
    let updated = state.db.snapshot();
    assert!(state
        .codex_models
        .cached_supporters("gpt-explicit", &updated)
        .is_empty());
    assert!(state.codex_models.union_active(&updated).models.is_empty());

    test_db
        .db
        .update(|db| db.provider_connections.clear())
        .await
        .expect("delete Codex connection");
    let deleted = state.db.snapshot();
    assert!(state
        .codex_models
        .cached_supporters("gpt-explicit", &deleted)
        .is_empty());
    assert!(state.codex_models.union_active(&deleted).models.is_empty());
    assert_eq!(catalog.request_count().await, 0);
    assert_eq!(generation.request_count().await, 0);
    catalog.shutdown().await;
    generation.shutdown().await;
}

#[test]
fn generation_paths_have_no_codex_catalog_refresh_wait_or_discovery_fallback() {
    let catalog = include_str!("../src/server/codex_catalog.rs");
    let chat = include_str!("../src/server/api/chat.rs");
    let v1_models = include_str!("../src/server/api/v1_models.rs");
    let main = include_str!("../src/main.rs");

    assert!(catalog.contains("ArcSwap<CodexCatalogSnapshot>"));
    assert!(catalog.contains("pub fn cached_supporters"));
    assert!(catalog.contains("pub fn union_active"));
    assert!(!catalog.contains("entry.lock().await"));
    let generation = chat
        .split("async fn forward_with_provider_fallback")
        .nth(1)
        .and_then(|tail| tail.split("fn select_connection(").next())
        .expect("chat generation source");
    assert!(!generation.contains("models_for_connection"));
    assert!(generation.contains("published_for_connection"));
    assert!(generation.contains("cached_supporters"));
    assert!(v1_models.contains("provider_id != \"codex\""));
    assert!(main.contains("spawn_codex_catalog_refresh"));
}
