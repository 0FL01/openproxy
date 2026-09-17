mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use openproxy::core::executor::{AntigravityExecutionRequest, AntigravityExecutor, ClientPool};
use openproxy::core::utils::antigravity_project::antigravity_project_id;
use openproxy::oauth::token_refresh::connection_credential_generation;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tokio::sync::Barrier;
use tokio::task::JoinSet;

fn connection(id: &str, token: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "antigravity".into(),
        auth_type: "oauth".into(),
        email: Some("shared@example.test".into()),
        is_active: Some(true),
        access_token: Some(token.into()),
        ..Default::default()
    }
}

fn executor(base_url: String) -> AntigravityExecutor {
    AntigravityExecutor::new(
        Arc::new(ClientPool::new()),
        Some(ProviderNode {
            id: "antigravity".into(),
            r#type: "antigravity".into(),
            base_url: Some(base_url),
            ..Default::default()
        }),
    )
    .expect("Antigravity executor")
}

fn request(credentials: ProviderConnection, index: usize) -> AntigravityExecutionRequest {
    AntigravityExecutionRequest {
        model: "gemini-2.5-pro".into(),
        body: json!({
            "model": "gemini-2.5-pro",
            "contents": [{"role": "user", "parts": [{"text": format!("turn-{index}")}]}]
        }),
        stream: false,
        credentials,
        proxy: None,
    }
}

#[tokio::test]
async fn concurrent_warm_generation_never_discovers_or_queues_project_metadata() {
    const REQUESTS: usize = 32;
    let upstream = MockUpstream::start(
        (0..REQUESTS).map(|_| ScriptedResponse::json(StatusCode::OK, r#"{"ok":true}"#)),
    )
    .await;
    let executor = executor(upstream.url("/configured"));
    let barrier = Arc::new(Barrier::new(REQUESTS));
    let mut tasks = JoinSet::new();

    for index in 0..REQUESTS {
        let executor = executor.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            tokio::time::timeout(
                Duration::from_millis(750),
                executor.execute_request(request(connection("c21-shared", "access-c21"), index)),
            )
            .await
            .expect("warm generation must not wait for project discovery")
            .expect("Antigravity generation")
        });
    }

    while let Some(result) = tasks.join_next().await {
        let response = result.expect("generation task");
        assert_eq!(response.response.status(), StatusCode::OK);
    }

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), REQUESTS);
    for recorded in requests {
        assert_eq!(recorded.path, "/configured/v1internal:generateContent");
        let body: Value = serde_json::from_slice(&recorded.body).expect("Antigravity body");
        assert_eq!(body["project"], "");
    }
    upstream.shutdown().await;
}

#[tokio::test]
async fn canonical_metadata_tracks_identity_and_delete_recreate_without_retained_state() {
    let temp = TempTestDb::new().await;
    let mut old = connection("same-id", "old-access");
    old.project_id = Some("projects/old".into());
    old.provider_specific_data = BTreeMap::from([
        ("projectId".into(), json!("projects/legacy")),
        ("unknown".into(), json!({"roundTrip": true})),
    ]);
    let old_generation = connection_credential_generation(&old);

    temp.db
        .update({
            let old = old.clone();
            move |db| db.provider_connections.push(old)
        })
        .await
        .expect("persist old connection");
    assert_eq!(
        antigravity_project_id(&temp.db.snapshot().provider_connections[0]).as_deref(),
        Some("projects/old")
    );

    let mut recreated = connection("same-id", "new-access");
    recreated
        .provider_specific_data
        .insert("unknown".into(), json!({"roundTrip": "still-present"}));
    temp.db
        .update({
            let recreated = recreated.clone();
            move |db| {
                db.provider_connections.retain(|item| item.id != "same-id");
                db.provider_connections.push(recreated);
            }
        })
        .await
        .expect("delete and recreate connection");

    let snapshot = temp.db.snapshot();
    let current = snapshot
        .provider_connections
        .iter()
        .find(|item| item.id == "same-id")
        .expect("recreated connection");
    assert!(antigravity_project_id(current).is_none());
    assert_ne!(connection_credential_generation(current), old_generation);
    assert_eq!(
        current.provider_specific_data.get("unknown"),
        Some(&json!({"roundTrip": "still-present"}))
    );

    let mut legacy = connection("legacy-id", "legacy-access");
    legacy
        .provider_specific_data
        .insert("projectId".into(), json!(" projects/legacy-only "));
    assert_eq!(
        antigravity_project_id(&legacy).as_deref(),
        Some("projects/legacy-only")
    );
}

#[test]
fn project_resolution_is_canonical_and_cache_free() {
    let source = include_str!("../src/core/executor/antigravity.rs");
    let execute = source
        .split("pub async fn execute_request(")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n\n#[cfg(test)]").next())
        .expect("AntigravityExecutor::execute_request source");

    for removed in [
        "get_project_id",
        "get_cached_project_id",
        "set_cached_project_id",
        "project_id_cache",
        "LOAD_CODE_ASSIST",
        "loadCodeAssist",
    ] {
        assert!(
            !execute.contains(removed),
            "generation still discovers or caches project metadata: {removed}"
        );
    }
    assert!(execute.contains("antigravity_project_id(&request.credentials)"));
    assert!(source.contains("pub async fn on_user_onboard"));

    let utils = include_str!("../src/core/utils/mod.rs");
    let executors = include_str!("../src/core/executor/mod.rs");
    let oauth = include_str!("../src/server/api/oauth.rs");
    assert!(!utils.contains("project_id_cache"));
    assert!(!executors.contains("project_id_cache"));
    assert!(!oauth.contains("invalidate_cached_project_id"));
    assert!(!Path::new("src/core/utils/project_id_cache.rs").exists());
    assert!(!Path::new("src/core/executor/project_id_cache.rs").exists());
}
