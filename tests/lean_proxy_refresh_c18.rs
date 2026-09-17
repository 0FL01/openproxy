mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::lean_harness::TempTestDb;
use openproxy::oauth::token_refresh::{
    connection_credential_generation, ConnectionRefreshCoordinator, RefreshResult,
};
use openproxy::types::ProviderConnection;
use tokio::sync::{Barrier, Semaphore};

fn oauth_connection(id: &str, access: &str, refresh: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "codex".into(),
        auth_type: "oauth".into(),
        is_active: Some(true),
        access_token: Some(access.into()),
        refresh_token: Some(refresh.into()),
        ..Default::default()
    }
}

async fn seed_connection(test_db: &TempTestDb, connection: ProviderConnection) {
    test_db
        .db
        .update(move |state| state.provider_connections.push(connection))
        .await
        .expect("seed OAuth connection");
}

fn canonical(test_db: &TempTestDb, id: &str) -> ProviderConnection {
    test_db
        .db
        .snapshot()
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
        .expect("canonical connection")
}

async fn wait_until_idle(coordinator: &ConnectionRefreshCoordinator) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while coordinator.active_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("refresh coordinator becomes idle");
}

#[tokio::test]
async fn twenty_thousand_historical_generations_leave_no_state_or_provider_calls() {
    const HISTORICAL_GENERATIONS: usize = 20_000;

    let test_db = TempTestDb::new().await;
    seed_connection(
        &test_db,
        oauth_connection("history", "canonical-access", "canonical-refresh"),
    )
    .await;
    let coordinator = Arc::new(ConnectionRefreshCoordinator::new());
    let provider_calls = Arc::new(AtomicUsize::new(0));

    for index in 0..HISTORICAL_GENERATIONS {
        let historical = oauth_connection(
            "history",
            &format!("historical-access-{index}"),
            &format!("historical-refresh-{index}"),
        );
        let observed = connection_credential_generation(&historical);
        let calls = Arc::clone(&provider_calls);
        let result = coordinator
            .refresh(
                Arc::clone(&test_db.db),
                "codex",
                "history",
                observed,
                move |_| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err("historical generation must not call provider".into())
                },
            )
            .await
            .expect("stale generation receives canonical credentials");
        assert!(!result.refreshed);
        assert_eq!(
            result.connection.refresh_token.as_deref(),
            Some("canonical-refresh")
        );
        wait_until_idle(&coordinator).await;
    }

    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    assert_eq!(coordinator.active_count(), 0);
}

#[tokio::test]
async fn mass_waiter_cancellation_keeps_one_operation_until_publish_then_returns_idle() {
    const WAITERS: usize = 512;

    let test_db = TempTestDb::new().await;
    seed_connection(
        &test_db,
        oauth_connection("cancel-many", "old-access", "old-refresh"),
    )
    .await;
    let observed = connection_credential_generation(&canonical(&test_db, "cancel-many"));
    let coordinator = Arc::new(ConnectionRefreshCoordinator::new());
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(Barrier::new(WAITERS + 1));
    let release = Arc::new(Semaphore::new(0));
    let mut waiters = Vec::with_capacity(WAITERS);

    for _ in 0..WAITERS {
        let db = Arc::clone(&test_db.db);
        let coordinator = Arc::clone(&coordinator);
        let provider_calls = Arc::clone(&provider_calls);
        let start = Arc::clone(&start);
        let release = Arc::clone(&release);
        waiters.push(tokio::spawn(async move {
            start.wait().await;
            coordinator
                .refresh(db, "codex", "cancel-many", observed, move |_| async move {
                    provider_calls.fetch_add(1, Ordering::SeqCst);
                    release
                        .acquire()
                        .await
                        .expect("release provider response")
                        .forget();
                    Ok(RefreshResult {
                        access_token: "new-access".into(),
                        refresh_token: Some("new-refresh".into()),
                        expires_in: Some(3600),
                    })
                })
                .await
        }));
    }

    start.wait().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while provider_calls.load(Ordering::SeqCst) != 1 || coordinator.active_count() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("one shared provider operation starts");
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }

    for waiter in &waiters {
        waiter.abort();
    }
    for waiter in waiters {
        assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
    }

    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(coordinator.active_count(), 1);
    release.add_permits(1);

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = canonical(&test_db, "cancel-many");
            if current.access_token.as_deref() == Some("new-access")
                && coordinator.active_count() == 0
            {
                assert_eq!(current.refresh_token.as_deref(), Some("new-refresh"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached refresh publishes after mass waiter cancellation");
    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn source_has_no_token_keyed_or_completed_refresh_cache() {
    let source = include_str!("../src/oauth/token_refresh.rs");
    let oauth_module = include_str!("../src/oauth/mod.rs");
    for forbidden in [
        "RefreshDedup",
        "DedupEntry",
        "make_dedup_key",
        "dedup_refresh",
        "GLOBAL_REFRESH_DEDUP",
        "REFRESH_RESULT_TTL",
        "cached_result",
        "OnceCell",
    ] {
        assert!(
            !source.contains(forbidden),
            "legacy refresh cache symbol remains: {forbidden}"
        );
    }

    assert!(!oauth_module.contains("pub mod refresh;"));
    assert!(source.contains("active: Mutex<HashMap<ConnectionRefreshKey"));
    assert!(source.contains("active.remove(&key)"));
    assert!(source.contains("refresh_with_retry(||"));
}
