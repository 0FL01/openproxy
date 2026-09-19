mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
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

async fn post_codex_responses(app: &axum::Router, model: &str) -> axum::response::Response {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json");
    app.clone()
        .oneshot(
            request
                .body(Body::from(
                    json!({
                        "model": format!("codex/{model}"),
                        "input": "stream failure",
                        "stream": true
                    })
                    .to_string(),
                ))
                .expect("C20 Responses request"),
        )
        .await
        .expect("C20 Responses response")
}

async fn post_mcp_search(app: &axum::Router, response_length: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-11-25")
                .body(Body::from(
                    json!({
                        "jsonrpc":"2.0",
                        "id":"search-1",
                        "method":"tools/call",
                        "params":{
                            "name":"search",
                            "arguments":{
                                "query":"current Rust release",
                                "response_length":response_length
                            }
                        }
                    })
                    .to_string(),
                ))
                .expect("MCP search request"),
        )
        .await
        .expect("MCP search response")
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
async fn mcp_search_uses_standalone_index_account_fallback_and_all_lengths() {
    let catalog = MockUpstream::start([]).await;
    let search = MockUpstream::start([
        ScriptedResponse::json(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error":{"message":"first account limited"}}).to_string(),
        ),
        ScriptedResponse::json(
            StatusCode::OK,
            json!({
                "output":"Rust is current. \u{e200}cite\u{e202}turn0search0\u{e201}",
                "results":[{"ref_id":"turn0search0","title":"Rust","url":"https://www.rust-lang.org/","snippet":"Rust language"}]
            })
            .to_string(),
        ),
        ScriptedResponse::json(
            StatusCode::OK,
            json!({
                "results":[{"title":"MCP","url":"https://modelcontextprotocol.io/","snippet":"Protocol documentation"}]
            })
            .to_string(),
        ),
        ScriptedResponse::json(
            StatusCode::OK,
            json!({
                "results":[{"title":"OpenAI models","url":"https://developers.openai.com/api/docs/models","snippet":"Official model catalog"}]
            })
            .to_string(),
        ),
    ])
    .await;
    let mut first = codex_connection("codex-mcp-a", 1);
    first.default_model = Some("gpt-5.5".into());
    first
        .provider_specific_data
        .insert("chatgptAccountId".into(), json!("account-mcp-a"));
    let mut second = codex_connection("codex-mcp-b", 2);
    second.default_model = Some("gpt-5.5".into());
    second
        .provider_specific_data
        .insert("chatgptAccountId".into(), json!("account-mcp-b"));
    let (_db, state) = state_with_catalog(
        catalog.url("/backend-api/codex/models"),
        search.url("/backend-api/codex/responses"),
        vec![first, second],
        Vec::new(),
    )
    .await;

    let app = openproxy::build_app(state);
    let response = post_mcp_search(&app, "short").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(body["id"], "search-1");
    assert_eq!(
        body["result"]["content"][0]["text"],
        "Rust is current. [1]\n\nSources:\n1. Rust: https://www.rust-lang.org/"
    );
    assert!(body["result"].get("structuredContent").is_none());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &to_bytes(
                post_mcp_search(&app, "medium").await.into_body(),
                1024 * 1024,
            )
            .await
            .unwrap()
        )
        .unwrap()["result"]["content"][0]["text"],
        "1. MCP\n   URL: https://modelcontextprotocol.io/\n   Protocol documentation"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &to_bytes(
                post_mcp_search(&app, "long").await.into_body(),
                1024 * 1024,
            )
            .await
            .unwrap()
        )
        .unwrap()["result"]["content"][0]["text"],
        "1. OpenAI models\n   URL: https://developers.openai.com/api/docs/models\n   Official model catalog"
    );

    assert_eq!(catalog.request_count().await, 0);
    let requests = search.requests().await;
    assert_eq!(requests.len(), 4);
    let expected_lengths = ["short", "short", "medium", "long"];
    let expected_accounts = ["a", "b", "a", "a"];
    for ((request, expected_length), expected_account) in
        requests.iter().zip(expected_lengths).zip(expected_accounts)
    {
        assert_eq!(request.path, "/backend-api/codex/alpha/search");
        let upstream: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(upstream["model"], "gpt-4o");
        assert_eq!(
            upstream["commands"]["search_query"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            upstream["commands"]["search_query"][0]["q"],
            "current Rust release"
        );
        assert_eq!(upstream["commands"]["response_length"], expected_length);
        assert!(upstream.get("tools").is_none());
        assert!(upstream.get("tool_choice").is_none());
        let authorization = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_ne!(authorization, "Bearer test-key");
        assert_eq!(
            authorization,
            format!("Bearer fixture-codex-mcp-{expected_account}-access")
        );
        assert_eq!(
            request
                .headers
                .get("chatgpt-account-id")
                .and_then(|value| value.to_str().ok()),
            Some(format!("account-mcp-{expected_account}").as_str())
        );
    }
    catalog.shutdown().await;
    search.shutdown().await;
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

#[tokio::test]
async fn post_commit_transport_failure_emits_sequenced_responses_error_once() {
    let catalog = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        catalog_payload(&[("gpt-known", false)]),
    )])
    .await;
    let generation = MockUpstream::start([ScriptedResponse::sse([concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"sequence_number\":4,",
        "\"response\":{\"id\":\"resp_c20\",\"created_at\":1,\"model\":\"gpt-known\"}}\n\n"
    )])
    .failing_after_chunks()])
    .await;
    let connection = codex_connection("codex-stream-error", 1);
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

    let response = post_codex_responses(&openproxy::build_app(state), "gpt-known").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("bounded Responses SSE");
    let output = String::from_utf8(body.to_vec()).expect("Responses SSE is UTF-8");

    assert!(output.contains("event: response.created"), "{output}");
    assert_eq!(output.matches("event: error").count(), 1, "{output}");
    let error_data = output
        .split("event: error\n")
        .nth(1)
        .and_then(|tail| tail.lines().find_map(|line| line.strip_prefix("data: ")))
        .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .expect("valid Responses error data");
    assert_eq!(error_data["type"], "error");
    assert_eq!(error_data["sequence_number"], 5);
    assert!(error_data["code"].is_string());
    assert!(error_data["message"].is_string());
    assert_eq!(error_data["param"], serde_json::Value::Null);
    assert!(!output.contains("response.completed"), "{output}");
    assert!(!output.contains("data: [DONE]"), "{output}");
    assert_eq!(generation.request_count().await, 1);
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
    assert!(generation.contains("cached_supporters"));
    assert!(v1_models.contains("provider_id != \"codex\""));
    assert!(main.contains("spawn_codex_catalog_refresh"));
}
