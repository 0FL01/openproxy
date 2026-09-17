mod common;

use std::sync::{mpsc, Arc};
use std::time::Duration;

use common::lean_harness::TempTestDb;
use openproxy::db::Db;
use openproxy::types::{ModelAliasTarget, ProviderConnection};
use tokio::sync::{oneshot, Barrier};

fn connection(id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "openai".into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        api_key: Some(format!("key-{id}")),
        ..Default::default()
    }
}

#[tokio::test]
async fn no_op_update_returns_the_original_snapshot_arc() {
    let test_db = TempTestDb::new().await;
    let before = test_db.db.snapshot();

    let returned = test_db.db.update(|_| {}).await.expect("no-op update");
    let published = test_db.db.snapshot();

    assert!(Arc::ptr_eq(&before, &returned));
    assert!(Arc::ptr_eq(&before, &published));
}

#[tokio::test]
async fn real_update_publishes_only_after_sqlite_commit() {
    let test_db = TempTestDb::new().await;
    let before = test_db.db.snapshot();

    let returned = test_db
        .db
        .update(|db| {
            db.model_aliases.insert(
                "c15-real".into(),
                ModelAliasTarget::Path("openai/gpt-c15".into()),
            );
        })
        .await
        .expect("real update");
    let published = test_db.db.snapshot();

    assert!(!Arc::ptr_eq(&before, &returned));
    assert!(Arc::ptr_eq(&returned, &published));
    assert_eq!(
        published.model_aliases.get("c15-real"),
        Some(&ModelAliasTarget::Path("openai/gpt-c15".into()))
    );

    let reloaded = Db::load_from(test_db.path())
        .await
        .expect("reload committed update");
    assert_eq!(
        reloaded.snapshot().model_aliases.get("c15-real"),
        published.model_aliases.get("c15-real")
    );
}

#[tokio::test]
async fn sqlite_failure_does_not_publish_and_a_retry_can_succeed() {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .sqlite_handle()
        .with_conn(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER c15_reject_connection
                 BEFORE INSERT ON providerConnections
                 BEGIN SELECT RAISE(ABORT, 'c15 injected failure'); END;",
            )
        })
        .expect("install failure trigger");
    let before = test_db.db.snapshot();

    let error = test_db
        .db
        .update(|db| db.provider_connections.push(connection("rejected")))
        .await
        .expect_err("trigger must reject update");
    assert!(error
        .to_string()
        .contains("SQLite incremental write failed"));
    assert!(Arc::ptr_eq(&before, &test_db.db.snapshot()));
    let stored_count: i64 = test_db
        .db
        .sqlite_handle()
        .with_conn(|connection| {
            connection.query_row("SELECT COUNT(*) FROM providerConnections", [], |row| {
                row.get(0)
            })
        })
        .expect("count persisted connections");
    assert_eq!(stored_count, 0);

    test_db
        .db
        .sqlite_handle()
        .with_conn(|connection| connection.execute_batch("DROP TRIGGER c15_reject_connection"))
        .expect("remove failure trigger");
    test_db
        .db
        .update(|db| db.provider_connections.push(connection("accepted")))
        .await
        .expect("retry after rollback");
    assert_eq!(test_db.db.snapshot().provider_connections[0].id, "accepted");
}

#[tokio::test]
async fn concurrent_writers_are_linear_and_snapshot_readers_remain_lock_free() {
    const WRITERS: usize = 32;

    let test_db = TempTestDb::new().await;
    let barrier = Arc::new(Barrier::new(WRITERS + 1));
    let mut writers = Vec::new();
    for index in 0..WRITERS {
        let db = Arc::clone(&test_db.db);
        let barrier = Arc::clone(&barrier);
        writers.push(tokio::spawn(async move {
            barrier.wait().await;
            db.update(move |state| {
                state.model_aliases.insert(
                    format!("c15-{index}"),
                    ModelAliasTarget::Path(format!("openai/model-{index}")),
                );
            })
            .await
            .expect("serialized writer");
        }));
    }

    barrier.wait().await;
    let mut last_seen = 0;
    while writers.iter().any(|writer| !writer.is_finished()) {
        let count = test_db.db.snapshot().model_aliases.len();
        assert!(count >= last_seen, "published snapshots must be monotonic");
        last_seen = count;
        tokio::task::yield_now().await;
    }
    for writer in writers {
        writer.await.expect("join writer");
    }
    assert_eq!(test_db.db.snapshot().model_aliases.len(), WRITERS);

    let reloaded = Db::load_from(test_db.path())
        .await
        .expect("reload concurrent writes");
    assert_eq!(reloaded.snapshot().model_aliases.len(), WRITERS);
}

#[tokio::test]
async fn cancelling_a_started_update_cannot_diverge_sqlite_and_snapshot() {
    let test_db = TempTestDb::new().await;
    let sqlite = test_db.db.sqlite.clone();
    let (locked_tx, locked_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let lock_thread = std::thread::spawn(move || {
        sqlite
            .with_conn(|_| {
                locked_tx.send(()).expect("report held SQLite lock");
                release_rx.recv().expect("release SQLite lock");
                Ok(())
            })
            .expect("hold SQLite connection");
    });
    locked_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("SQLite connection lock acquired");

    let db = Arc::clone(&test_db.db);
    let (updater_tx, updater_rx) = oneshot::channel();
    let update_task = tokio::spawn(async move {
        db.update(move |state| {
            state
                .provider_connections
                .push(connection("cancelled-caller"));
            let _ = updater_tx.send(());
        })
        .await
    });
    updater_rx.await.expect("updater ran");
    tokio::time::sleep(Duration::from_millis(100)).await;
    update_task.abort();
    assert!(
        update_task
            .await
            .expect_err("caller must be cancelled")
            .is_cancelled(),
        "outer update task was cancelled while persistence was blocked"
    );

    release_tx.send(()).expect("release persistence");
    lock_thread.join().expect("join SQLite lock thread");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if test_db
                .db
                .snapshot()
                .provider_connections
                .iter()
                .any(|candidate| candidate.id == "cancelled-caller")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("detached persistence publishes committed snapshot");

    let reloaded = Db::load_from(test_db.path())
        .await
        .expect("reload cancellation result");
    let persisted = reloaded.snapshot();
    let published = test_db.db.snapshot();
    assert_eq!(persisted.provider_connections.len(), 1);
    assert_eq!(published.provider_connections.len(), 1);
    assert_eq!(persisted.provider_connections[0].id, "cancelled-caller");
    assert_eq!(
        persisted.provider_connections[0].api_key,
        published.provider_connections[0].api_key
    );

    test_db
        .db
        .update(|state| {
            state.model_aliases.insert(
                "after-cancel".into(),
                ModelAliasTarget::Path("openai/after-cancel".into()),
            );
        })
        .await
        .expect("writer lock released after detached commit");
}
