mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use openproxy::core::executor::{AntigravityExecutionRequest, AntigravityExecutor, ClientPool};
use openproxy::oauth::antigravity_onboarding::AntigravityOnboardingCoordinator;
use openproxy::oauth::token_refresh::connection_credential_generation;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;
use tokio::sync::{Barrier, Notify};

fn connection(id: &str, token: &str) -> ProviderConnection {
    let mut connection = ProviderConnection {
        id: id.into(),
        provider: "antigravity".into(),
        auth_type: "oauth".into(),
        is_active: Some(true),
        access_token: Some(token.into()),
        refresh_token: Some(format!("refresh-{token}")),
        project_id: Some(format!("projects/{id}")),
        test_status: Some("active".into()),
        ..Default::default()
    };
    connection
        .provider_specific_data
        .insert("tierId".into(), json!("tier-c22"));
    connection
}

async fn seed(test_db: &TempTestDb, connection: ProviderConnection) {
    test_db
        .db
        .update(move |db| db.provider_connections.push(connection))
        .await
        .expect("seed C22 connection");
}

async fn wait_until_idle(coordinator: &AntigravityOnboardingCoordinator) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while coordinator.active_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("C22 coordinator returns to idle");
}

#[tokio::test]
async fn hundred_generation_requests_never_start_onboarding() {
    let scripts = (0..100).map(|_| ScriptedResponse::json(StatusCode::OK, r#"{"ok":true}"#));
    let upstream = MockUpstream::start(scripts).await;
    let node = ProviderNode {
        id: "antigravity".into(),
        r#type: "antigravity".into(),
        name: "C22 generation".into(),
        base_url: Some(upstream.url("/generation")),
        ..Default::default()
    };
    let executor = Arc::new(
        AntigravityExecutor::new(Arc::new(ClientPool::new()), Some(node))
            .expect("C22 Antigravity executor"),
    );
    let start = Arc::new(Barrier::new(101));
    let mut tasks = Vec::new();
    for index in 0..100 {
        let executor = executor.clone();
        let start = start.clone();
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            executor
                .execute_request(AntigravityExecutionRequest {
                    model: "gemini-2.5-pro".into(),
                    body: json!({
                        "model": "gemini-2.5-pro",
                        "contents": [{"role":"user","parts":[{"text":format!("chat-{index}")}]}]
                    }),
                    stream: false,
                    credentials: connection("c22-chat", "chat-token"),
                    proxy: None,
                })
                .await
                .expect("C22 generation response")
        }));
    }
    start.wait().await;
    for task in tasks {
        let result = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("generation does not wait for onboarding")
            .expect("join C22 generation");
        assert_eq!(result.response.status(), StatusCode::OK);
    }
    assert_eq!(upstream.request_count().await, 100);
    assert!(upstream
        .requests()
        .await
        .iter()
        .all(|request| request.path == "/generation/v1internal:generateContent"));

    let source = include_str!("../src/core/executor/antigravity.rs");
    let execute = source
        .split("pub async fn execute_request(")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n\n#[cfg(test)]").next())
        .expect("Antigravity execute source");
    for removed in [
        "on_user_onboard",
        "onboardUser",
        "tokio::spawn",
        "ONBOARD_USER",
    ] {
        assert!(
            !execute.contains(removed),
            "generation still starts onboarding: {removed}"
        );
    }
    upstream.shutdown().await;
}

#[tokio::test]
async fn configured_generation_singleflights_and_failure_can_be_retried() {
    let release = Arc::new(Notify::new());
    let upstream = MockUpstream::start([
        ScriptedResponse::json(StatusCode::OK, r#"{"done":true}"#).waiting_for(release.clone()),
        ScriptedResponse::json(StatusCode::BAD_REQUEST, "bad setup"),
        ScriptedResponse::json(StatusCode::OK, r#"{"done":true}"#),
    ])
    .await;
    let test_db = TempTestDb::new().await;
    let configured = connection("c22-singleflight", "token-one");
    let generation = connection_credential_generation(&configured);
    seed(&test_db, configured.clone()).await;
    let coordinator = Arc::new(AntigravityOnboardingCoordinator::with_config(
        upstream.url("/onboard"),
        1,
        Duration::from_millis(1),
    ));
    let pool = Arc::new(ClientPool::new());
    let start = Arc::new(Barrier::new(33));
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let coordinator = coordinator.clone();
        let db = test_db.db.clone();
        let pool = pool.clone();
        let start = start.clone();
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            coordinator
                .ensure(db, pool, "c22-singleflight", generation)
                .await
        }));
    }
    start.wait().await;
    upstream.wait_for_requests(1).await;
    assert_eq!(coordinator.active_count(), 1);
    assert_eq!(upstream.request_count().await, 1);
    release.notify_waiters();
    for task in tasks {
        task.await
            .expect("join onboarding waiter")
            .expect("shared success");
    }
    wait_until_idle(&coordinator).await;
    assert_eq!(upstream.request_count().await, 1);

    // A changed canonical generation is a new explicit setup lifecycle. The
    // first attempt fails visibly; a later explicit call may retry once.
    let changed = test_db
        .db
        .update(|db| {
            let connection = db
                .provider_connections
                .iter_mut()
                .find(|connection| connection.id == "c22-singleflight")
                .unwrap();
            connection.access_token = Some("token-two".into());
        })
        .await
        .expect("rotate C22 generation");
    let changed = changed
        .provider_connections
        .iter()
        .find(|connection| connection.id == "c22-singleflight")
        .unwrap()
        .clone();
    let changed_generation = connection_credential_generation(&changed);
    let error = coordinator
        .ensure(
            test_db.db.clone(),
            pool.clone(),
            &changed.id,
            changed_generation,
        )
        .await
        .expect_err("400 onboarding is explicit failure");
    assert!(error.contains("HTTP 400"));
    wait_until_idle(&coordinator).await;
    let after_failure = test_db.db.snapshot();
    let after_failure = after_failure
        .provider_connections
        .iter()
        .find(|connection| connection.id == changed.id)
        .unwrap();
    assert_eq!(after_failure.test_status.as_deref(), Some("error"));
    assert_eq!(after_failure.access_token.as_deref(), Some("token-two"));
    assert_eq!(
        after_failure.refresh_token.as_deref(),
        Some("refresh-token-one")
    );

    coordinator
        .ensure(test_db.db.clone(), pool, &changed.id, changed_generation)
        .await
        .expect("later explicit retry succeeds");
    wait_until_idle(&coordinator).await;
    assert_eq!(upstream.request_count().await, 3);
    let after_success = test_db.db.snapshot();
    let after_success = after_success
        .provider_connections
        .iter()
        .find(|connection| connection.id == changed.id)
        .unwrap();
    assert_eq!(after_success.test_status.as_deref(), Some("active"));
    assert_eq!(after_success.last_error, None);
    upstream.shutdown().await;
}

#[tokio::test]
async fn deletion_and_shutdown_cancel_and_release_active_work() {
    let release_delete = Arc::new(Notify::new());
    let release_shutdown = Arc::new(Notify::new());
    let upstream = MockUpstream::start([
        ScriptedResponse::json(StatusCode::OK, r#"{"done":true}"#)
            .waiting_for(release_delete.clone()),
        ScriptedResponse::json(StatusCode::OK, r#"{"done":true}"#)
            .waiting_for(release_shutdown.clone()),
    ])
    .await;
    let test_db = TempTestDb::new().await;
    let first = connection("c22-delete", "delete-token");
    let first_generation = connection_credential_generation(&first);
    seed(&test_db, first).await;
    let coordinator = Arc::new(AntigravityOnboardingCoordinator::with_config(
        upstream.url("/onboard"),
        2,
        Duration::from_millis(1),
    ));
    let pool = Arc::new(ClientPool::new());
    let task = {
        let coordinator = coordinator.clone();
        let db = test_db.db.clone();
        let pool = pool.clone();
        tokio::spawn(async move {
            coordinator
                .ensure(db, pool, "c22-delete", first_generation)
                .await
        })
    };
    upstream.wait_for_requests(1).await;
    test_db
        .db
        .update(|db| {
            db.provider_connections
                .retain(|connection| connection.id != "c22-delete")
        })
        .await
        .expect("delete C22 connection");
    coordinator.cancel_connection("c22-delete");
    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("delete cancels onboarding")
        .expect("join delete onboarding")
        .expect_err("deleted onboarding fails");
    assert!(error.contains("cancelled") || error.contains("no longer configured"));
    wait_until_idle(&coordinator).await;
    assert!(test_db.db.snapshot().provider_connections.is_empty());
    release_delete.notify_waiters();

    let second = connection("c22-shutdown", "shutdown-token");
    let second_generation = connection_credential_generation(&second);
    seed(&test_db, second).await;
    let task = {
        let coordinator = coordinator.clone();
        let db = test_db.db.clone();
        let pool = pool.clone();
        tokio::spawn(async move {
            coordinator
                .ensure(db, pool, "c22-shutdown", second_generation)
                .await
        })
    };
    upstream.wait_for_requests(2).await;
    tokio::time::timeout(Duration::from_secs(1), coordinator.shutdown())
        .await
        .expect("shutdown drains onboarding");
    let error = task
        .await
        .expect("join shutdown onboarding")
        .expect_err("shutdown cancels waiter");
    assert!(error.contains("cancelled"));
    assert_eq!(coordinator.active_count(), 0);
    assert!(coordinator
        .ensure(test_db.db.clone(), pool, "c22-shutdown", second_generation,)
        .await
        .unwrap_err()
        .contains("shutting down"));
    release_shutdown.notify_waiters();
    upstream.shutdown().await;
}
