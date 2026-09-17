mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use common::lean_harness::TempTestDb;
use openproxy::oauth::token_refresh::{
    connection_credential_generation, ConnectionRefreshCoordinator, RefreshResult,
};
use openproxy::types::ProviderConnection;
use tokio::sync::{oneshot, Semaphore};

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

async fn seed_connections(test_db: &TempTestDb, connections: Vec<ProviderConnection>) {
    test_db
        .db
        .update(move |state| state.provider_connections.extend(connections))
        .await
        .expect("seed OAuth connections");
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

async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while counter.load(Ordering::SeqCst) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("counter reached expected value");
}

#[tokio::test]
async fn one_hundred_same_generation_callers_share_one_refresh_and_late_stale_reads_current() {
    const CALLERS: usize = 100;

    let test_db = TempTestDb::new().await;
    seed_connections(
        &test_db,
        vec![oauth_connection("same", "access-old", "refresh-old")],
    )
    .await;
    let observed = connection_credential_generation(&canonical(&test_db, "same"));
    let coordinator = Arc::new(ConnectionRefreshCoordinator::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Semaphore::new(0));

    let mut tasks = Vec::new();
    for _ in 0..CALLERS {
        let db = Arc::clone(&test_db.db);
        let coordinator = Arc::clone(&coordinator);
        let calls = Arc::clone(&calls);
        let release = Arc::clone(&release);
        tasks.push(tokio::spawn(async move {
            coordinator
                .refresh(db, "codex", "same", observed, move |canonical| async move {
                    assert_eq!(canonical.refresh_token.as_deref(), Some("refresh-old"));
                    calls.fetch_add(1, Ordering::SeqCst);
                    release.acquire().await.expect("release refresh").forget();
                    Ok(RefreshResult {
                        access_token: "access-new".into(),
                        refresh_token: Some("refresh-new".into()),
                        expires_in: Some(3600),
                    })
                })
                .await
        }));
    }

    wait_for_count(&calls, 1).await;
    release.add_permits(1);
    for task in tasks {
        let result = task
            .await
            .expect("join same-generation waiter")
            .expect("refresh");
        assert_eq!(
            result.connection.access_token.as_deref(),
            Some("access-new")
        );
        assert_eq!(
            result.connection.refresh_token.as_deref(),
            Some("refresh-new")
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(coordinator.active_count(), 0);

    let late_calls = Arc::new(AtomicUsize::new(0));
    let result = coordinator
        .refresh(Arc::clone(&test_db.db), "codex", "same", observed, {
            let late_calls = Arc::clone(&late_calls);
            move |_| async move {
                late_calls.fetch_add(1, Ordering::SeqCst);
                Err("stale caller must not issue HTTP".into())
            }
        })
        .await
        .expect("late stale caller receives canonical credentials");
    assert!(!result.refreshed);
    assert_eq!(
        result.connection.access_token.as_deref(),
        Some("access-new")
    );
    assert_eq!(late_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn different_connections_do_not_block_each_other() {
    let test_db = TempTestDb::new().await;
    seed_connections(
        &test_db,
        vec![
            oauth_connection("blocked", "a-old", "same-refresh-token"),
            oauth_connection("free", "b-old", "same-refresh-token"),
        ],
    )
    .await;
    let blocked_generation = connection_credential_generation(&canonical(&test_db, "blocked"));
    let free_generation = connection_credential_generation(&canonical(&test_db, "free"));
    let coordinator = Arc::new(ConnectionRefreshCoordinator::new());
    let blocked_gate = Arc::new(Semaphore::new(0));
    let (started_tx, started_rx) = oneshot::channel();

    let blocked_task = {
        let coordinator = Arc::clone(&coordinator);
        let db = Arc::clone(&test_db.db);
        let blocked_gate = Arc::clone(&blocked_gate);
        tokio::spawn(async move {
            coordinator
                .refresh(
                    db,
                    "codex",
                    "blocked",
                    blocked_generation,
                    move |_| async move {
                        let _ = started_tx.send(());
                        blocked_gate
                            .acquire()
                            .await
                            .expect("release blocked")
                            .forget();
                        Ok(RefreshResult::access_only("a-new".into()))
                    },
                )
                .await
        })
    };
    started_rx.await.expect("blocked refresh started");

    let free_result = tokio::time::timeout(
        Duration::from_millis(500),
        coordinator.refresh(
            Arc::clone(&test_db.db),
            "codex",
            "free",
            free_generation,
            |_| async { Ok(RefreshResult::access_only("b-new".into())) },
        ),
    )
    .await
    .expect("different connection must not wait")
    .expect("free refresh");
    assert_eq!(
        free_result.connection.access_token.as_deref(),
        Some("b-new")
    );

    blocked_gate.add_permits(1);
    let blocked_result = blocked_task
        .await
        .expect("join blocked refresh")
        .expect("blocked refresh");
    assert_eq!(
        blocked_result.connection.access_token.as_deref(),
        Some("a-new")
    );
    assert_eq!(coordinator.active_count(), 0);
}

#[tokio::test]
async fn failed_waiter_group_shares_error_then_next_call_may_retry() {
    const WAITERS: usize = 32;

    let test_db = TempTestDb::new().await;
    seed_connections(
        &test_db,
        vec![oauth_connection("failure", "old", "refresh")],
    )
    .await;
    let observed = connection_credential_generation(&canonical(&test_db, "failure"));
    let coordinator = Arc::new(ConnectionRefreshCoordinator::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Semaphore::new(0));
    let mut tasks = Vec::new();

    for _ in 0..WAITERS {
        let coordinator = Arc::clone(&coordinator);
        let db = Arc::clone(&test_db.db);
        let calls = Arc::clone(&calls);
        let release = Arc::clone(&release);
        tasks.push(tokio::spawn(async move {
            coordinator
                .refresh(db, "codex", "failure", observed, move |_| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    release.acquire().await.expect("release failure").forget();
                    Err("Refresh request returned HTTP 400: invalid_grant".into())
                })
                .await
        }));
    }

    wait_for_count(&calls, 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    release.add_permits(1);
    for task in tasks {
        let error = task
            .await
            .expect("join failed waiter")
            .expect_err("waiter shares failure");
        assert_eq!(error, "Refresh request returned HTTP 400: invalid_grant");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        canonical(&test_db, "failure").access_token.as_deref(),
        Some("old")
    );

    let retry = coordinator
        .refresh(Arc::clone(&test_db.db), "codex", "failure", observed, {
            let calls = Arc::clone(&calls);
            move |_| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(RefreshResult::access_only("after-failure".into()))
            }
        })
        .await
        .expect("next independent call may retry");
    assert!(retry.refreshed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn persistence_error_is_shared_without_publishing_and_can_be_retried() {
    let test_db = TempTestDb::new().await;
    seed_connections(
        &test_db,
        vec![oauth_connection("persist", "old", "refresh")],
    )
    .await;
    let observed = connection_credential_generation(&canonical(&test_db, "persist"));
    test_db
        .db
        .sqlite_handle()
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER c16_reject_refresh
                 BEFORE UPDATE ON providerConnections
                 BEGIN SELECT RAISE(ABORT, 'c16 injected persistence failure'); END;",
            )
        })
        .expect("install persistence failure trigger");
    let coordinator = Arc::new(ConnectionRefreshCoordinator::new());
    let calls = Arc::new(AtomicUsize::new(0));

    let error = coordinator
        .refresh(Arc::clone(&test_db.db), "codex", "persist", observed, {
            let calls = Arc::clone(&calls);
            move |_| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(RefreshResult::access_only("rejected".into()))
            }
        })
        .await
        .expect_err("SQLite trigger rejects persistence");
    assert!(error.contains("persist refreshed credentials"));
    assert_eq!(
        canonical(&test_db, "persist").access_token.as_deref(),
        Some("old")
    );
    assert_eq!(coordinator.active_count(), 0);

    test_db
        .db
        .sqlite_handle()
        .with_conn(|connection| connection.execute_batch("DROP TRIGGER c16_reject_refresh"))
        .expect("drop persistence failure trigger");
    let result = coordinator
        .refresh(Arc::clone(&test_db.db), "codex", "persist", observed, {
            let calls = Arc::clone(&calls);
            move |_| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(RefreshResult::access_only("accepted".into()))
            }
        })
        .await
        .expect("retry after persistence repair");
    assert!(result.refreshed);
    assert_eq!(result.connection.access_token.as_deref(), Some("accepted"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn caller_cancellation_after_upstream_issue_still_persists_rotated_pair() {
    let test_db = TempTestDb::new().await;
    seed_connections(
        &test_db,
        vec![oauth_connection("cancel", "old", "refresh-old")],
    )
    .await;
    let observed = connection_credential_generation(&canonical(&test_db, "cancel"));
    let coordinator = Arc::new(ConnectionRefreshCoordinator::new());

    let sqlite = test_db.db.sqlite.clone();
    let (locked_tx, locked_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let lock_thread = std::thread::spawn(move || {
        sqlite
            .with_conn(|_| {
                locked_tx.send(()).expect("report SQLite lock");
                release_rx.recv().expect("release SQLite lock");
                Ok(())
            })
            .expect("hold SQLite connection");
    });
    locked_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("SQLite connection locked");

    let (issued_tx, issued_rx) = oneshot::channel();
    let caller = {
        let coordinator = Arc::clone(&coordinator);
        let db = Arc::clone(&test_db.db);
        tokio::spawn(async move {
            coordinator
                .refresh(db, "codex", "cancel", observed, move |_| async move {
                    let _ = issued_tx.send(());
                    Ok(RefreshResult {
                        access_token: "cancel-access-new".into(),
                        refresh_token: Some("cancel-refresh-new".into()),
                        expires_in: None,
                    })
                })
                .await
        })
    };
    issued_rx.await.expect("provider issued rotated pair");
    tokio::time::sleep(Duration::from_millis(50)).await;
    caller.abort();
    assert!(caller.await.expect_err("caller cancelled").is_cancelled());

    release_tx.send(()).expect("release SQLite persistence");
    lock_thread.join().expect("join SQLite lock thread");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = canonical(&test_db, "cancel");
            if current.access_token.as_deref() == Some("cancel-access-new")
                && coordinator.active_count() == 0
            {
                assert_eq!(current.refresh_token.as_deref(), Some("cancel-refresh-new"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("detached operation persists after caller cancellation");
}

#[tokio::test]
async fn delete_recreate_generation_wins_and_shutdown_drains_active_operation() {
    let test_db = TempTestDb::new().await;
    seed_connections(
        &test_db,
        vec![oauth_connection("recreate", "old", "refresh-old")],
    )
    .await;
    let observed = connection_credential_generation(&canonical(&test_db, "recreate"));
    let coordinator = Arc::new(ConnectionRefreshCoordinator::new());
    let gate = Arc::new(Semaphore::new(0));
    let (started_tx, started_rx) = oneshot::channel();

    let caller = {
        let coordinator = Arc::clone(&coordinator);
        let db = Arc::clone(&test_db.db);
        let gate = Arc::clone(&gate);
        tokio::spawn(async move {
            coordinator
                .refresh(db, "codex", "recreate", observed, move |_| async move {
                    let _ = started_tx.send(());
                    gate.acquire()
                        .await
                        .expect("release recreated refresh")
                        .forget();
                    Ok(RefreshResult {
                        access_token: "stale-upstream-access".into(),
                        refresh_token: Some("stale-upstream-refresh".into()),
                        expires_in: None,
                    })
                })
                .await
        })
    };
    started_rx.await.expect("refresh read original generation");

    test_db
        .db
        .update(|state| {
            state
                .provider_connections
                .retain(|connection| connection.id != "recreate");
            state.provider_connections.push(oauth_connection(
                "recreate",
                "recreated-access",
                "recreated-refresh",
            ));
        })
        .await
        .expect("delete and recreate connection identity");

    let shutdown = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move { coordinator.shutdown().await })
    };
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished(), "shutdown waits for active refresh");
    gate.add_permits(1);

    let result = caller
        .await
        .expect("join recreated caller")
        .expect("recreated canonical result");
    assert!(!result.refreshed);
    assert_eq!(
        result.connection.access_token.as_deref(),
        Some("recreated-access")
    );
    assert_eq!(
        result.connection.refresh_token.as_deref(),
        Some("recreated-refresh")
    );
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .expect("shutdown drains")
        .expect("join shutdown");
    assert_eq!(coordinator.active_count(), 0);
}

#[test]
fn coordinator_source_has_connection_key_and_no_completed_cache() {
    let source = include_str!("../src/oauth/token_refresh.rs");
    let coordinator = source
        .split("// Connection-scoped refresh coordination (C16)")
        .nth(1)
        .expect("coordinator section")
        .split("// Refresh lead times")
        .next()
        .expect("coordinator end");

    assert!(coordinator.contains("connection_id: String"));
    assert!(coordinator.contains("provider: String"));
    assert!(coordinator.contains("active.remove(&key)"));
    assert!(!coordinator.contains("cached_result"));
    assert!(!coordinator.contains("REFRESH_RESULT_TTL_MS"));
    assert!(!coordinator.contains("make_dedup_key"));
    assert!(!coordinator.contains("old_token"));

    for removed in [
        "RefreshDedup",
        "DedupEntry",
        "make_dedup_key",
        "dedup_refresh",
        "GLOBAL_REFRESH_DEDUP",
        "REFRESH_RESULT_TTL",
        "cached_result",
        "OnceCell",
    ] {
        assert!(!source.contains(removed), "legacy cache remains: {removed}");
    }
}
