mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{HeaderMap, Request, StatusCode};
use common::{test_api_key, test_connection};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::server::xai_catalog::XaiModelCatalog;
use openproxy::types::{CustomModel, ModelAliasTarget, ProviderConnection};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::util::ServiceExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn connection(id: &str, key: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        api_key: Some(key.into()),
        ..test_connection("xai")
    }
}

fn language_models() -> Value {
    json!({"models": [
        {
            "id": "grok-fixture-vision", "aliases": ["grok-fixture-latest"],
            "input_modalities": ["text", "image"], "output_modalities": ["text"],
            "capabilities": {"reasoning_effort": ["low", "high"]}
        },
        {"id": "grok-fixture-multi-agent", "aliases": ["grok-excluded-alias"]}
    ]})
}

fn model_contexts() -> Value {
    json!({"data": [
        {"id": "grok-fixture-vision", "context_length": 256000},
        {"id": "grok-enrichment-only", "context_length": 999999}
    ]})
}

async fn mount_inventory(server: &MockServer, key: &str, language: Value, contexts: Value) {
    Mock::given(method("GET"))
        .and(path("/language-models"))
        .and(header("authorization", format!("Bearer {key}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(language))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", format!("Bearer {key}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(contexts))
        .mount(server)
        .await;
}

async fn test_state(
    server: &MockServer,
    connections: Vec<ProviderConnection>,
) -> (TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::load_from(dir.path()).await.unwrap());
    db.update(move |snapshot| {
        snapshot.settings.require_login = true;
        snapshot.api_keys = vec![test_api_key()];
        snapshot.provider_connections = connections;
    })
    .await
    .unwrap();
    let mut state = AppState::new(db);
    state.xai_models = Arc::new(XaiModelCatalog::with_urls(
        format!("{}/language-models", server.uri()),
        format!("{}/models", server.uri()),
    ));
    (dir, state)
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, HeaderMap, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", "Bearer test-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, serde_json::from_slice(&body).unwrap())
}

fn xai_catalog_rows(body: &Value) -> &[Value] {
    body["providerModels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["alias"] == "xai")
        .unwrap()["models"]
        .as_array()
        .unwrap()
}

fn ids(body: &Value) -> Vec<&str> {
    body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn all_projections_share_chat_inventory_and_preserve_static_media() {
    let server = MockServer::start().await;
    mount_inventory(&server, "fixture-key", language_models(), model_contexts()).await;
    let (_dir, state) = test_state(&server, vec![connection("xai-first", "fixture-key")]).await;
    let before = state.db.snapshot();
    let app = openproxy::build_app(state.clone());

    let (status, _, catalog) = get(&app, "/api/catalog").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["xaiCatalog"]["status"], "ready");
    let rows = xai_catalog_rows(&catalog);
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().any(|row| row["kind"] == "image"));
    assert!(rows.iter().any(|row| row["kind"] == "video"));
    assert!(!rows.iter().any(|row| row["id"] == "grok-4"));
    assert!(!rows.iter().any(|row| row["id"] == "grok-enrichment-only"));
    assert!(!rows.iter().any(|row| row["id"] == "grok-excluded-alias"));

    let (_, _, dashboard) = get(&app, "/api/models").await;
    let row = dashboard["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["fullModel"] == "xai/grok-fixture-latest")
        .unwrap();
    assert_eq!(row["contextWindow"], 256000);
    assert_eq!(row["caps"], json!({"vision": true, "reasoning": true}));
    assert_eq!(row["reasoningEfforts"], json!(["low", "high"]));

    let (_, _, public) = get(&app, "/v1/models").await;
    assert_eq!(
        ids(&public),
        ["xai/grok-fixture-latest", "xai/grok-fixture-vision"]
    );
    for row in public["data"].as_array().unwrap() {
        assert_eq!(row["context_length"], 256000);
        assert_eq!(row["opencode"]["source"], "xai");
        assert_eq!(row["opencode"]["attachment"], true);
        assert_eq!(row["opencode"]["reasoning"], true);
        assert_eq!(
            row["opencode"]["modalities"]["input"],
            json!(["text", "image"])
        );
        assert_eq!(
            row["opencode"]["variants"]["high"]["reasoningEffort"],
            "high"
        );
    }
    let (_, _, info) = get(&app, "/v1/models/info?model=xai/grok-fixture-latest").await;
    assert_eq!(info["data"]["contextWindow"], 256000);
    assert_eq!(info["data"]["vision"], true);
    assert_eq!(info["data"]["opencode"]["source"], "xai");

    // The connection endpoint is the explicit shared force-refresh surface.
    let (status, _, connection_models) = get(&app, "/api/providers/xai-first/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(connection_models["provider"], "xai");
    assert_eq!(connection_models["connectionId"], "xai-first");
    assert_eq!(connection_models["models"].as_array().unwrap().len(), 2);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        4,
        "TTL shares one initial refresh; explicit GET forces one more"
    );
    assert!(requests.iter().all(|request| request.method == "GET"));
    let after = state.db.snapshot();
    assert_eq!(after.provider_connections, before.provider_connections);
    assert!(
        after.custom_models.is_empty(),
        "discovery must not import custom rows"
    );
}

#[tokio::test]
async fn selected_account_is_first_active_db_row_not_priority_or_union() {
    let server = MockServer::start().await;
    mount_inventory(
        &server,
        "first-key",
        json!({"models": [{"id": "grok-first"}]}),
        json!({"data": []}),
    )
    .await;
    mount_inventory(
        &server,
        "second-key",
        json!({"models": [{"id": "grok-second"}]}),
        json!({"data": []}),
    )
    .await;
    let mut inactive = connection("inactive", "inactive-key");
    inactive.is_active = Some(false);
    let mut first = connection("first", "first-key");
    first.priority = Some(99);
    let mut second = connection("second", "second-key");
    second.priority = Some(1);
    let (_dir, state) = test_state(&server, vec![inactive, first, second]).await;
    let app = openproxy::build_app(state);
    assert_eq!(
        get(&app, "/api/providers/second/models").await.0,
        StatusCode::OK
    );
    let (_, _, public) = get(&app, "/v1/models").await;
    assert_eq!(ids(&public), ["xai/grok-first"]);
    let (_, _, catalog) = get(&app, "/api/catalog").await;
    assert!(xai_catalog_rows(&catalog)
        .iter()
        .any(|row| row["id"] == "grok-first"));
    assert!(!xai_catalog_rows(&catalog)
        .iter()
        .any(|row| row["id"] == "grok-second"));
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn cold_failure_is_public_503_but_admin_aggregates_remain_useful() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/language-models"))
        .respond_with(ResponseTemplate::new(500).set_body_string("private-upstream-body-fixture"))
        .mount(&server)
        .await;
    let (_dir, state) = test_state(
        &server,
        vec![connection("xai", "fixture-key"), test_connection("openai")],
    )
    .await;
    let app = openproxy::build_app(state);
    let (status, headers, public) = get(&app, "/v1/models").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers["access-control-allow-origin"], "*");
    assert!(headers.contains_key("retry-after"));
    assert_eq!(public["error"]["code"], "xai_catalog_unavailable");
    assert!(!public.to_string().contains("private-upstream-body-fixture"));
    let (status, _, catalog) = get(&app, "/api/catalog").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["xaiCatalog"]["status"], "unavailable");
    assert_eq!(
        xai_catalog_rows(&catalog).len(),
        2,
        "static media is not a chat fallback"
    );
    assert!(catalog["providerModels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["alias"] == "openai" && !row["models"].as_array().unwrap().is_empty()));
    let (status, _, dashboard) = get(&app, "/api/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dashboard["xaiCatalog"]["status"], "unavailable");
    assert!(dashboard["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["provider"] == "openai"));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "failed automatic retry is throttled"
    );
}

#[tokio::test]
async fn explicit_enabled_custom_alias_and_disabled_configuration_survives_without_discovery() {
    let server = MockServer::start().await;
    let mut selected = connection("xai", "fixture-key");
    selected.default_model = Some("grok-explicit-multi-agent".into());
    selected
        .provider_specific_data
        .insert("prefix".into(), json!("personal"));
    selected.provider_specific_data.insert(
        "enabledModels".into(),
        json!(["xai/grok-explicit-multi-agent", "grok-disabled"]),
    );
    let (_dir, state) = test_state(&server, vec![selected]).await;
    state
        .db
        .update(|snapshot| {
            snapshot.custom_models = vec![
                CustomModel {
                    provider_alias: "xai".into(),
                    id: "grok-custom-multi-agent".into(),
                    name: Some("My saved model".into()),
                    r#type: "llm".into(),
                    extra: BTreeMap::new(),
                },
                CustomModel {
                    provider_alias: "xai".into(),
                    id: "grok-disabled".into(),
                    r#type: "llm".into(),
                    name: None,
                    extra: BTreeMap::new(),
                },
            ];
            snapshot.model_aliases.insert(
                "saved-alias".into(),
                ModelAliasTarget::Path("xai/grok-alias-multi-agent".into()),
            );
            snapshot
                .extra
                .insert("disabledModels".into(), json!({"xai": ["grok-disabled"]}));
        })
        .await
        .unwrap();
    let before = serde_json::to_value(&*state.db.snapshot()).unwrap();
    let app = openproxy::build_app(state.clone());
    let (status, _, public) = get(&app, "/v1/models").await;
    assert_eq!(status, StatusCode::OK);
    let listed = ids(&public);
    assert!(listed.contains(&"personal/grok-explicit-multi-agent"));
    assert!(listed.contains(&"personal/grok-custom-multi-agent"));
    assert!(listed.contains(&"personal/grok-alias-multi-agent"));
    assert!(!listed.iter().any(|id| id.ends_with("/grok-disabled")));
    let row = public["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "personal/grok-custom-multi-agent")
        .unwrap();
    assert_eq!(row["opencode"]["name"], "My saved model");
    assert_eq!(row["opencode"]["source"], "xai");
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(serde_json::to_value(&*state.db.snapshot()).unwrap(), before);
}

#[tokio::test]
async fn nonempty_raw_enabled_array_is_an_override_even_without_normalized_ids() {
    let server = MockServer::start().await;
    let mut selected = connection("xai", "fixture-key");
    selected
        .provider_specific_data
        .insert("enabledModels".into(), json!(["  ", 7, null]));
    let (_dir, state) = test_state(&server, vec![selected]).await;
    let app = openproxy::build_app(state);
    let (status, _, public) = get(&app, "/v1/models").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ids(&public).is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn valid_empty_inventory_does_not_trigger_generic_discovery_or_static_chat_fallback() {
    let server = MockServer::start().await;
    mount_inventory(
        &server,
        "fixture-key",
        json!({"models": []}),
        json!({"data": [{"id": "grok-not-authoritative"}]}),
    )
    .await;
    let mut selected = connection("xai", "fixture-key");
    selected
        .provider_specific_data
        .insert("enabledModels".into(), json!([]));
    let (_dir, state) = test_state(&server, vec![selected]).await;
    let app = openproxy::build_app(state);
    for _ in 0..2 {
        let (status, _, public) = get(&app, "/v1/models").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ids(&public).is_empty());
    }
    let (_, _, catalog) = get(&app, "/api/catalog").await;
    assert_eq!(catalog["xaiCatalog"]["status"], "ready");
    assert_eq!(xai_catalog_rows(&catalog).len(), 2);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn disabled_custom_row_cannot_reappear_when_it_becomes_automatic() {
    let server = MockServer::start().await;
    mount_inventory(&server, "fixture-key", language_models(), model_contexts()).await;
    let (_dir, state) = test_state(&server, vec![connection("xai", "fixture-key")]).await;
    state
        .db
        .update(|snapshot| {
            snapshot.custom_models.push(CustomModel {
                provider_alias: "xai".into(),
                id: "grok-fixture-vision".into(),
                r#type: "llm".into(),
                name: Some("Personal display name".into()),
                extra: BTreeMap::from([("opencode".into(), json!({"limit": {"context": 8192}}))]),
            });
            snapshot.extra.insert(
                "disabledModels".into(),
                json!({"xai": ["grok-fixture-vision"]}),
            );
        })
        .await
        .unwrap();
    let app = openproxy::build_app(state.clone());
    let (_, _, public) = get(&app, "/v1/models").await;
    assert!(!ids(&public).contains(&"xai/grok-fixture-vision"));
    state
        .db
        .update(|snapshot| {
            snapshot.extra.remove("disabledModels");
        })
        .await
        .unwrap();
    let (_, _, public) = get(&app, "/v1/models").await;
    let rows = public["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == "xai/grok-fixture-vision")
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["opencode"]["name"], "Personal display name");
    assert_eq!(rows[0]["opencode"]["limit"]["context"], 8192);
    assert_eq!(rows[0]["opencode"]["source"], "xai");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn failed_force_refresh_preserves_last_good_with_shared_warning() {
    let server = MockServer::start().await;
    mount_inventory(&server, "fixture-key", language_models(), model_contexts()).await;
    let (_dir, state) = test_state(&server, vec![connection("xai", "fixture-key")]).await;
    let app = openproxy::build_app(state);
    assert_eq!(get(&app, "/v1/models").await.0, StatusCode::OK);
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/language-models"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let (status, _, refresh) = get(&app, "/api/providers/xai/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refresh["models"].as_array().unwrap().len(), 2);
    assert!(refresh["warning"]
        .as_str()
        .is_some_and(|warning| !warning.is_empty()));
    let (_, _, catalog) = get(&app, "/api/catalog").await;
    assert_eq!(catalog["xaiCatalog"]["status"], "stale");
    let (_, _, dashboard) = get(&app, "/api/models").await;
    assert_eq!(dashboard["xaiCatalog"]["status"], "stale");
    let (status, _, public) = get(&app, "/v1/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&public).len(), 2);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn optional_context_failure_keeps_confirmed_capabilities_without_invented_limits() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/language-models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(language_models()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let (_dir, state) = test_state(&server, vec![connection("xai", "fixture-key")]).await;
    let app = openproxy::build_app(state);
    let (status, _, public) = get(&app, "/v1/models").await;
    assert_eq!(status, StatusCode::OK);
    for model in public["data"].as_array().unwrap() {
        assert!(model.get("context_length").is_none());
        assert!(model["opencode"].get("limit").is_none());
        assert_eq!(model["opencode"]["reasoning"], true);
        assert_eq!(model["opencode"]["attachment"], true);
    }
    let (_, _, catalog) = get(&app, "/api/catalog").await;
    assert!(catalog["xaiCatalog"]["warning"].is_string());
}

#[tokio::test]
async fn missing_capabilities_remain_unknown_and_confirmed_text_input_is_not_vision() {
    let server = MockServer::start().await;
    mount_inventory(&server, "fixture-key", json!({"models": [
        {"id": "grok-confirmed-text", "input_modalities": ["text"], "output_modalities": ["text"], "capabilities": null},
        {"id": "grok-unknown-facts"}
    ]}), json!({"data": []})).await;
    let (_dir, state) = test_state(&server, vec![connection("xai", "fixture-key")]).await;
    let app = openproxy::build_app(state);
    let (status, _, public) = get(&app, "/v1/models").await;
    assert_eq!(status, StatusCode::OK);
    for model in public["data"].as_array().unwrap() {
        assert!(model["opencode"].get("reasoning").is_none());
        assert!(model["opencode"].get("tool_call").is_none());
        assert!(model["opencode"].get("variants").is_none());
        if model["id"] == "xai/grok-confirmed-text" {
            assert_eq!(model["opencode"]["attachment"], false);
        } else {
            assert!(model["opencode"].get("attachment").is_none());
        }
    }
    let (_, _, info) = get(&app, "/v1/models/info?model=xai/grok-unknown-facts").await;
    assert!(info["data"].get("reasoning").is_none());
    assert!(info["data"].get("tools").is_none());
    assert!(info["data"].get("vision").is_none());
}

#[tokio::test]
async fn credential_rotation_during_discovery_cannot_return_a_partial_aggregate() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/language-models"))
        .and(header("authorization", "Bearer old-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(150))
                .set_body_json(language_models()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(model_contexts()))
        .mount(&server)
        .await;
    let (_dir, state) = test_state(
        &server,
        vec![connection("xai", "old-key"), test_connection("openai")],
    )
    .await;
    let app = openproxy::build_app(state.clone());
    let in_flight = tokio::spawn({
        let app = app.clone();
        async move { get(&app, "/v1/models").await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    state
        .db
        .update(|snapshot| {
            snapshot.provider_connections[0].api_key = Some("new-key".into());
        })
        .await
        .unwrap();
    let (status, headers, body) = in_flight.await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(headers.contains_key("retry-after"));
    assert!(body.get("data").is_none());
    assert!(!body.to_string().contains("grok-fixture-vision"));
    mount_inventory(
        &server,
        "new-key",
        json!({"models": [{"id": "grok-new-identity"}]}),
        json!({"data": []}),
    )
    .await;
    let (status, _, body) = get(&app, "/v1/models").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ids(&body).contains(&"xai/grok-new-identity"));
    assert!(!ids(&body).contains(&"xai/grok-fixture-vision"));
}

#[tokio::test]
async fn discovery_is_authenticated_before_any_catalog_request() {
    let server = MockServer::start().await;
    let (_dir, state) = test_state(&server, vec![connection("xai", "fixture-key")]).await;
    let app = openproxy::build_app(state);
    for uri in [
        "/v1/models",
        "/v1/models/info?model=xai/grok-fixture",
        "/api/catalog",
        "/api/models",
        "/api/providers/xai/models",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn no_active_account_omits_diagnostic_and_preserves_media_registration() {
    let server = MockServer::start().await;
    let (_dir, state) = test_state(&server, vec![test_connection("openai")]).await;
    let app = openproxy::build_app(state);
    let (_, _, catalog) = get(&app, "/api/catalog").await;
    assert!(catalog.get("xaiCatalog").is_none());
    assert_eq!(xai_catalog_rows(&catalog).len(), 2);
    let (_, _, dashboard) = get(&app, "/api/models").await;
    assert!(dashboard.get("xaiCatalog").is_none());
    assert!(server.received_requests().await.unwrap().is_empty());
}
