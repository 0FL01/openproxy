//! C40: the backup export buffer must not outlive the backup write.
//!
//! `spawn_auto_backup` in `src/main.rs` exports the whole in-memory snapshot
//! as JSON bytes once per hour. Those bytes must be scoped to the write and
//! dropped before the hourly sleep — never retained in the idle task. Backup
//! format, retention, and file-count behavior are unchanged.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::lean_harness::TempTestDb;
use openproxy::db::backups::{BackupManager, BackupReason};

/// Extract the `spawn_auto_backup` function body from `src/main.rs`.
fn auto_backup_fn() -> String {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let main = std::fs::read_to_string(root.join("src/main.rs")).expect("read src/main.rs");
    let start = main
        .find("fn spawn_auto_backup")
        .expect("spawn_auto_backup must exist");
    let tail = &main[start..];
    // The function ends at the first line starting at column 0 with `}`.
    let mut end = tail.len();
    for (offset, line) in tail.lines().enumerate() {
        if offset > 0 && line == "}" {
            end = tail
                .lines()
                .take(offset + 1)
                .map(|line| line.len() + 1)
                .sum();
            break;
        }
    }
    tail[..end].to_string()
}

#[test]
fn export_buffer_does_not_span_the_hourly_sleep() {
    let body = auto_backup_fn();
    // The export binding must exist (no behavior change: still exports).
    let export_pos = body
        .find("db.export_db()")
        .expect("still exports the snapshot");
    // Exactly one hourly sleep per loop iteration: the old export-failure arm
    // had its own sleep, which also meant two different retention shapes.
    let sleeps = body.matches("60 * 60").count();
    assert_eq!(
        sleeps, 1,
        "one hourly sleep per iteration; export/write outcomes share it"
    );
    let sleep_pos = body.find("60 * 60").expect("hourly sleep must exist");
    assert!(export_pos < sleep_pos, "export must precede the sleep");
    // Between the export and the sleep there must be a scope close at the
    // loop-body indent (12 spaces): the buffer's block ends before sleeping.
    let between = &body[export_pos..sleep_pos];
    assert!(
        between.lines().any(|line| line == "            }"),
        "export buffer scope must close before the hourly sleep"
    );
    // No `continue` that would bypass the single sleep (old failure path).
    assert!(
        !body.contains("continue;"),
        "no early continue: every path reaches the same hourly sleep"
    );
    // Format/retention/throttle behavior untouched.
    for required in [
        "BackupManager::new(&db.data_dir)",
        "BackupReason::Auto",
        "create_from_json",
        "DISABLE_AUTO_BACKUP",
    ] {
        assert!(body.contains(required), "backup behavior kept: {required}");
    }
}

#[tokio::test]
async fn backup_success_round_trip_through_scoped_buffer() {
    let test_db = TempTestDb::new().await;
    // Mirror the C40 loop iteration: export, write inside a scope, drop,
    // then (without sleeping an hour here) assert the artifact restores.
    let backup_id = {
        let (json_bytes, _filename) = test_db.db.export_db().expect("export succeeds");
        assert!(!json_bytes.is_empty(), "export must produce bytes");
        let mgr = BackupManager::new(test_db.path());
        let info = mgr
            .create_from_json(BackupReason::Auto, &json_bytes)
            .await
            .expect("write succeeds")
            .expect("auto backup not throttled on first write");
        info.id.clone()
    };
    let mgr = BackupManager::new(test_db.path());
    let restored = mgr.read_backup(&backup_id).await.expect("backup restores");
    let snapshot = test_db.db.snapshot();
    assert_eq!(
        restored.api_keys.len(),
        snapshot.api_keys.len(),
        "restored backup must match the snapshot"
    );
    let listed = mgr.list().await.expect("list works");
    assert!(listed.iter().any(|info| info.id == backup_id));
}

#[tokio::test]
async fn backup_write_error_is_explicit_and_stateless() {
    // A data dir that is a file (not a directory) makes the write fail.
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("not-a-dir");
    std::fs::write(&file_path, b"occupied").expect("occupy path");
    let payload = serde_json::json!({"snapshot": "c40"})
        .to_string()
        .into_bytes();
    // A fresh manager per attempt: the write error must come from the
    // filesystem, not from retained throttle/partial state (the same manager
    // would answer the immediate repeat with a throttled Ok(None)).
    for _ in 0..2 {
        let mgr = BackupManager::new(&file_path);
        let attempt = mgr.create_from_json(BackupReason::Auto, &payload).await;
        assert!(
            attempt.is_err(),
            "unwritable backup dir must error, not panic"
        );
    }
}

#[tokio::test]
async fn cancelled_backup_iteration_drops_its_buffer() {
    struct DropFlag(Vec<u8>, Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.1.store(true, Ordering::Release);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(tokio::sync::Notify::new());
    let waiter = gate.clone();
    // Same shape as one C40 loop iteration: a large buffer held across an
    // await (the backup write), then dropped at scope end.
    let task = tokio::spawn({
        let dropped = dropped.clone();
        async move {
            {
                let buffer = DropFlag(vec![7u8; 8 * 1024 * 1024], dropped);
                waiter.notified().await;
                // Touch the buffer so it is live across the await.
                assert_eq!(buffer.0.len(), 8 * 1024 * 1024);
            }
            tokio::time::sleep(Duration::from_secs(60 * 60)).await;
        }
    });
    tokio::task::yield_now().await;
    task.abort();
    let _ = task.await;
    assert!(
        dropped.load(Ordering::Acquire),
        "aborting the backup future must release the export-sized buffer"
    );
    gate.notify_waiters();
}
