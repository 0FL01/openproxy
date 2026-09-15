use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, CustomModel};
use serde_json::json;
use tempfile::tempdir;
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

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn authorized_request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

#[tokio::test]
async fn models_custom_get_returns_wrapped_models() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.custom_models.push(CustomModel {
                provider_alias: "oa".into(),
                id: "gpt-custom".into(),
                r#type: "llm".into(),
                name: Some("Custom".into()),
                extra: BTreeMap::new(),
            });
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/models/custom",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "models": [{
                "providerAlias": "oa",
                "id": "gpt-custom",
                "type": "llm",
                "name": "Custom"
            }]
        })
    );
}

#[tokio::test]
async fn models_custom_post_requires_provider_alias_and_id() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::POST,
            "/api/models/custom",
            Body::from(r#"{"providerAlias":"","id":"gpt-custom"}"#),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "providerAlias and id required" }));
}

#[tokio::test]
async fn models_custom_post_returns_added_true_then_false_for_duplicate() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());

    let first = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/models/custom",
            Body::from(r#"{"providerAlias":"oa","id":"gpt-custom","type":"llm","name":"Custom"}"#),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(first).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "added": true }));

    let second = app
        .oneshot(authorized_request(
            Method::POST,
            "/api/models/custom",
            Body::from(r#"{"providerAlias":"oa","id":"gpt-custom","type":"llm","name":"Other"}"#),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(second).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true, "added": false }));

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.custom_models.len(), 1);
    assert_eq!(snapshot.custom_models[0].name.as_deref(), Some("Custom"));
}

#[tokio::test]
async fn custom_model_discovery_preserves_metadata_and_hides_disabled_rows() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let metadata = json!({
        "limit": {"context": 628000, "input": 500000, "output": 128000},
        "modalities": {"input": ["text", "image"], "output": ["text"]},
        "reasoning": true,
        "tool_call": true,
        "variants": {"high": {"reasoningEffort": "high"}}
    });
    let response = app.clone().oneshot(authorized_request(
        Method::POST, "/api/models/custom",
        Body::from(json!({"providerAlias": "proxy", "id": "new/model", "name": "New Model", "opencode": metadata}).to_string()),
    )).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        state.db.snapshot().custom_models[0].extra["opencode"],
        metadata
    );

    let response = app
        .clone()
        .oneshot(authorized_request(Method::GET, "/v1/models", Body::empty()))
        .await
        .unwrap();
    let (_, body) = response_json(response).await;
    let model = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "proxy/new/model")
        .unwrap();
    let mut expected = metadata.clone();
    expected["name"] = json!("New Model");
    expected["source"] = json!("proxy");
    assert_eq!(model["opencode"], expected);

    // Use the dashboard's actual disable API, not a synthetic internal state.
    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/models/disabled",
            Body::from(json!({"providerAlias": "proxy", "ids": ["new/model"]}).to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .oneshot(authorized_request(Method::GET, "/v1/models", Body::empty()))
        .await
        .unwrap();
    let (_, body) = response_json(response).await;
    assert!(!body["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["id"] == "proxy/new/model"));
}

#[tokio::test]
async fn custom_model_metadata_rejects_invalid_limits_and_transport_overrides() {
    let app = openproxy::build_app(app_state().await);
    for metadata in [
        json!({"limit": {"context": 0}}),
        json!({"provider": {"api": "https://example.invalid"}}),
    ] {
        let response = app
            .clone()
            .oneshot(authorized_request(
                Method::POST,
                "/api/models/custom",
                Body::from(
                    json!({"providerAlias": "proxy", "id": "invalid", "opencode": metadata})
                        .to_string(),
                ),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[tokio::test]
async fn models_custom_delete_requires_provider_alias_and_id() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/models/custom?providerAlias=oa",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "providerAlias and id required" }));
}

#[tokio::test]
async fn models_custom_delete_query_removes_matching_model_only() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.custom_models.push(CustomModel {
                provider_alias: "oa".into(),
                id: "gpt-custom".into(),
                r#type: "llm".into(),
                name: Some("Custom".into()),
                extra: BTreeMap::new(),
            });
            db.custom_models.push(CustomModel {
                provider_alias: "oa".into(),
                id: "gpt-custom".into(),
                r#type: "embedding".into(),
                name: Some("Embedding".into()),
                extra: BTreeMap::new(),
            });
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/models/custom?providerAlias=oa&id=gpt-custom&type=llm",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true }));

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.custom_models.len(), 1);
    assert_eq!(snapshot.custom_models[0].r#type, "embedding");
}
