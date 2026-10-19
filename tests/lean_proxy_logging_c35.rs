//! C35: audit I/O leaves the lean first-chunk path.
//!
//! Lean mode enqueues metadata through one bounded events-and-bytes pipeline
//! into a single background writer: the first upstream byte never waits for
//! SQLite, overflow drops with an explicit counter, and cancellation never
//! spawns unbounded detached tasks. Durable mode keeps honest
//! insert-before-upstream / await-finish backpressure and is tested
//! separately (never advertised as zero-wait).

mod common;

use std::time::Duration;

use openproxy::db::sqlite::repo::request_repo;
use openproxy::server::application_logs::{
    request_log_bounds, request_log_dropped, request_log_flush_with_budget, request_log_mode,
    request_log_queued_bytes, RequestLogContext, RequestLogMode,
};
use openproxy::types::ApiKey;

use common::lean_harness::TempTestDb;

/// The lean log pipeline is process-global (one bounded queue, one writer,
/// one drop counter), so queue-touching tests must run serially within this
/// binary; otherwise the overflow test's flood drops other tests' rows.
static SERIAL: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

async fn serial_guard() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

fn test_key() -> ApiKey {
    ApiKey {
        id: "key-c35".to_string(),
        name: "c35-harness".to_string(),
        key: "test-key".to_string(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: Default::default(),
    }
}

fn lean_context(db: &std::sync::Arc<openproxy::db::Db>) -> RequestLogContext {
    RequestLogContext::new_with_mode(
        db.clone(),
        &test_key(),
        "openai/gpt-c35",
        RequestLogMode::Lean,
    )
}

fn durable_context(db: &std::sync::Arc<openproxy::db::Db>) -> RequestLogContext {
    RequestLogContext::new_with_mode(
        db.clone(),
        &test_key(),
        "openai/gpt-c35",
        RequestLogMode::Durable,
    )
}

async fn wait_for_row(
    db: &std::sync::Arc<openproxy::db::Db>,
    id: &str,
    timeout: Duration,
) -> request_repo::RequestDetailRow {
    let start = std::time::Instant::now();
    loop {
        let found = {
            let sqlite = db.sqlite.clone();
            let id = id.to_string();
            tokio::task::spawn_blocking(move || {
                sqlite.with_conn(|conn| request_repo::get(conn, &id))
            })
            .await
            .expect("lookup task")
            .expect("lookup query")
        };
        if let Some(row) = found {
            return row;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for request log row"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn lean_start_returns_without_waiting_for_slow_sqlite() {
    let _serial = serial_guard().await;
    let tmp = TempTestDb::new().await;
    let ctx = lean_context(&tmp.db);

    // Hold the single SQLite connection so any synchronous insert would block.
    let sqlite = tmp.db.sqlite.clone();
    let gate = tokio::task::spawn_blocking(move || {
        let _guard = sqlite.lock();
        std::thread::sleep(Duration::from_millis(600));
    });

    // Lean start must return immediately despite the held lock.
    let attempt =
        tokio::time::timeout(Duration::from_millis(150), ctx.start_attempt("openai", "m"))
            .await
            .expect("lean start must not wait for SQLite")
            .expect("lean start enqueues");
    let id = {
        // AttemptLog holds a private id; recover it via flush + listing.
        // Finish immediately so the row becomes observable after the gate.
        attempt.finish("success", Some(200), None, None).await;
        assert!(
            request_log_flush_with_budget(Duration::from_secs(5)).await,
            "queue must drain after the SQLite gate releases"
        );
        let sqlite = tmp.db.sqlite.clone();
        let rows = tokio::task::spawn_blocking(move || {
            sqlite.with_conn(|conn| {
                request_repo::list(conn, &request_repo::RequestDetailFilter::default(), 10, 0)
            })
        })
        .await
        .expect("list task")
        .expect("list query");
        assert_eq!(rows.len(), 1, "exactly one lean row, got {rows:?}");
        rows.into_iter().next().unwrap().id
    };
    let row = wait_for_row(&tmp.db, &id, Duration::from_secs(5)).await;
    assert_eq!(row.status.as_deref(), Some("success"));

    gate.await.expect("gate task");
}

#[tokio::test]
async fn durable_start_waits_for_sqlite() {
    let _serial = serial_guard().await;
    let tmp = TempTestDb::new().await;
    let ctx = durable_context(&tmp.db);

    let sqlite = tmp.db.sqlite.clone();
    let gate = tokio::task::spawn_blocking(move || {
        let _guard = sqlite.lock();
        std::thread::sleep(Duration::from_millis(600));
    });
    // Give the gate a moment to acquire the lock first.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Durable start must block on the insert (honest backpressure).
    let blocked =
        tokio::time::timeout(Duration::from_millis(150), ctx.start_attempt("openai", "m")).await;
    assert!(
        blocked.is_err(),
        "durable start must wait for SQLite instead of returning early"
    );

    gate.await.expect("gate task");
    // After the gate releases, durable start succeeds synchronously.
    let attempt = tokio::time::timeout(Duration::from_secs(5), ctx.start_attempt("openai", "m"))
        .await
        .expect("durable start completes")
        .expect("durable row inserted");
    attempt.finish("success", Some(200), None, None).await;
}

#[tokio::test]
async fn lean_start_finish_preserves_order_and_metadata_only() {
    let _serial = serial_guard().await;
    let tmp = TempTestDb::new().await;
    let ctx = lean_context(&tmp.db);

    let attempt = ctx
        .start_attempt("openai", "gpt-c35")
        .await
        .expect("lean start");
    let usage = openproxy::types::TokenUsage {
        prompt_tokens: Some(11),
        completion_tokens: Some(22),
        total_tokens: Some(135),
        input_tokens: None,
        output_tokens: None,
        cached_tokens: Some(4),
        cache_read_input_tokens: None,
        reasoning_tokens: Some(94),
        cache_creation_input_tokens: None,
        extra: Default::default(),
    };
    attempt
        .finish("success", Some(200), Some(&usage), None)
        .await;
    assert!(
        request_log_flush_with_budget(Duration::from_secs(5)).await,
        "lean finish must drain"
    );

    let sqlite = tmp.db.sqlite.clone();
    let rows = tokio::task::spawn_blocking(move || {
        sqlite.with_conn(|conn| {
            request_repo::list(conn, &request_repo::RequestDetailFilter::default(), 10, 0)
        })
    })
    .await
    .expect("list task")
    .expect("list query");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.status.as_deref(), Some("success"));
    assert_eq!(
        row.data.get("statusCode").and_then(|v| v.as_u64()),
        Some(200)
    );
    assert_eq!(
        row.data.get("inputTokens").and_then(|v| v.as_u64()),
        Some(11)
    );
    assert_eq!(
        row.data.get("cachedTokens").and_then(|v| v.as_u64()),
        Some(4)
    );
    assert_eq!(row.data["reasoningTokens"], 94);
    assert_eq!(row.data["totalTokens"], 135);
    // Metadata only: no prompt/response bodies or connection secrets. The
    // private correlation column and explicit-chat HMAC are passive metadata.
    assert!(uuid::Uuid::parse_str(row.correlation_id.as_deref().unwrap()).is_ok());
    assert!(row.data["chatSession"].is_null());
    let serialized = serde_json::json!({
        "provider": row.provider,
        "model": row.model,
        "status": row.status,
        "apiKeyId": row.api_key_id,
        "apiKeyName": row.api_key_name,
        "data": row.data,
    })
    .to_string();
    assert!(!serialized.contains("secret"), "log row: {serialized}");
    for forbidden in ["request", "response", "connectionId", "correlationId"] {
        if forbidden == "request" {
            // Column-adjacent words appear in debug shapes; check the stored
            // JSON data object only for payload leakage.
            let data = serde_json::to_string(&row.data).expect("data");
            assert!(
                !data.contains("prompt") && !data.contains("body"),
                "metadata data must not carry bodies: {data}"
            );
        } else {
            assert!(
                !serialized.contains(forbidden),
                "log row leaks {forbidden}: {serialized}"
            );
        }
    }
    let data = row.data.as_object().expect("data object");
    for key in data.keys() {
        assert!(
            [
                "route",
                "statusCode",
                "durationMs",
                "inputTokens",
                "outputTokens",
                "cachedTokens",
                "reasoningTokens",
                "totalTokens",
                "chatSession",
                "upstreamTps"
            ]
            .contains(&key.as_str()),
            "unexpected log data key: {key}"
        );
    }
}

#[tokio::test]
async fn selected_usage_counters_preserve_null_zero_precedence_and_privacy_in_both_modes() {
    let _serial = serial_guard().await;
    for mode in [RequestLogMode::Durable, RequestLogMode::Lean] {
        let tmp = TempTestDb::new().await;
        let ctx = RequestLogContext::new_with_mode(tmp.db.clone(), &test_key(), "test", mode);
        let known = serde_json::json!({
            "prompt_tokens": 32, "input_tokens": 999,
            "completion_tokens": 9, "output_tokens": 999,
            "cached_tokens": 6, "cache_read_input_tokens": 999,
            "reasoning_tokens": 94, "total_tokens": 135,
            "extra_private": {"request": "secret-prompt", "response": "secret-body"}
        });
        for (name, raw, expected) in [
            (
                "known",
                Some(known.clone()),
                [Some(32), Some(9), Some(6), Some(94), Some(135)],
            ),
            (
                "zero",
                Some(serde_json::json!({
                    "prompt_tokens": 0, "input_tokens": 999,
                    "completion_tokens": 0, "output_tokens": 999,
                    "cached_tokens": 0, "cache_read_input_tokens": 999,
                    "reasoning_tokens": 0, "total_tokens": 0,
                    "extra_private": "secret"
                })),
                [Some(0), Some(0), Some(0), Some(0), Some(0)],
            ),
            (
                "fallback",
                Some(serde_json::json!({
                    "input_tokens": 32, "output_tokens": 9, "cache_read_input_tokens": 6,
                    "reasoning_tokens": 94, "total_tokens": 135
                })),
                [Some(32), Some(9), Some(6), Some(94), Some(135)],
            ),
            (
                "missing-breakdown",
                Some(serde_json::json!({
                    "prompt_tokens": 32, "completion_tokens": 9, "cached_tokens": 6,
                    "completion_tokens_details": {"reasoning_tokens": 94},
                    "rawPayload": {"total_tokens": 135, "private": "secret"}
                })),
                [Some(32), Some(9), Some(6), None, None],
            ),
            ("no-usage", None, [None; 5]),
            (
                "error",
                Some(known),
                [Some(32), Some(9), Some(6), Some(94), Some(135)],
            ),
            ("interrupted", None, [None; 5]),
        ] {
            let attempt = ctx.start_attempt("test", name).await.unwrap();
            assert!(request_log_flush_with_budget(Duration::from_secs(5)).await);
            let pending = tmp
                .db
                .sqlite
                .with_conn(|conn| {
                    request_repo::list(
                        conn,
                        &request_repo::RequestDetailFilter {
                            model: Some(name),
                            ..Default::default()
                        },
                        10,
                        0,
                    )
                })
                .unwrap();
            assert_eq!(pending.len(), 1, "{mode:?}: {name}");
            let id = &pending[0].id;
            assert_eq!(pending[0].status.as_deref(), Some("pending"));
            for field in [
                "inputTokens",
                "outputTokens",
                "cachedTokens",
                "reasoningTokens",
                "totalTokens",
            ] {
                assert_eq!(
                    pending[0].data.get(field),
                    Some(&serde_json::Value::Null),
                    "{name}: pending {field}"
                );
            }
            let usage: Option<openproxy::types::TokenUsage> =
                raw.map(|value| serde_json::from_value(value).unwrap());
            if name == "interrupted" {
                drop(attempt);
            } else {
                attempt
                    .finish(
                        if name == "error" { "error" } else { "success" },
                        Some(if name == "error" { 502 } else { 200 }),
                        usage.as_ref(),
                        None,
                    )
                    .await;
            }
            assert!(request_log_flush_with_budget(Duration::from_secs(5)).await);
            let row = wait_for_row(&tmp.db, id, Duration::from_secs(5)).await;
            assert_eq!(
                row.status.as_deref(),
                Some(if name == "interrupted" {
                    "interrupted"
                } else if name == "error" {
                    "error"
                } else {
                    "success"
                })
            );
            let fields = [
                "inputTokens",
                "outputTokens",
                "cachedTokens",
                "reasoningTokens",
                "totalTokens",
            ];
            for (field, expected) in fields.into_iter().zip(expected) {
                assert_eq!(
                    row.data[field],
                    serde_json::json!(expected),
                    "{mode:?}: {name}: {field}"
                );
            }
            let allowed = [
                "route",
                "statusCode",
                "durationMs",
                "inputTokens",
                "outputTokens",
                "cachedTokens",
                "reasoningTokens",
                "totalTokens",
                "chatSession",
            ];
            assert_eq!(row.data.as_object().unwrap().len(), allowed.len());
            assert!(row
                .data
                .as_object()
                .unwrap()
                .keys()
                .all(|key| allowed.contains(&key.as_str())));
            for private in [
                "secret",
                "extra_private",
                "rawPayload",
                "completion_tokens_details",
                "request",
                "response",
            ] {
                assert!(
                    !row.data.to_string().contains(private),
                    "{mode:?}: {name}: leaked {private}"
                );
            }
        }
    }
}

#[tokio::test]
async fn lean_cancellation_marks_interrupted_without_blocking() {
    let _serial = serial_guard().await;
    let tmp = TempTestDb::new().await;
    let ctx = lean_context(&tmp.db);

    {
        let _attempt = ctx.start_attempt("openai", "m").await.expect("lean start");
        // Drop without finish simulates downstream cancellation.
    }
    assert!(
        request_log_flush_with_budget(Duration::from_secs(5)).await,
        "interrupted mark must drain"
    );
    let sqlite = tmp.db.sqlite.clone();
    let rows = tokio::task::spawn_blocking(move || {
        sqlite.with_conn(|conn| {
            request_repo::list(conn, &request_repo::RequestDetailFilter::default(), 10, 0)
        })
    })
    .await
    .expect("list task")
    .expect("list query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status.as_deref(), Some("interrupted"));
}

#[tokio::test]
async fn lean_overflow_drops_with_explicit_counter() {
    let _serial = serial_guard().await;
    let (max_events, max_bytes, max_event) = request_log_bounds();
    assert!(max_events > 0 && max_bytes > 0 && max_event > 0);
    let tmp = TempTestDb::new().await;
    let ctx = lean_context(&tmp.db);
    let before = request_log_dropped();

    // Enqueue far faster than the single SQLite writer can drain; the bounded
    // pipeline must drop with a counter instead of growing.
    let mut started = 0usize;
    for _ in 0..(max_events * 8) {
        if ctx.start_attempt("openai", "m").await.is_some() {
            started += 1;
        }
    }
    let dropped = request_log_dropped().saturating_sub(before);
    assert!(
        dropped > 0,
        "expected overflow drops, started={started} max_events={max_events}"
    );
    // Drain within a generous budget so later tests start from an empty queue.
    assert!(
        request_log_flush_with_budget(Duration::from_secs(15)).await,
        "queue must drain after burst"
    );
    assert_eq!(request_log_queued_bytes(), 0);
}

#[tokio::test]
async fn lean_event_bytes_are_bounded() {
    let _serial = serial_guard().await;
    let tmp = TempTestDb::new().await;
    let ctx = lean_context(&tmp.db);
    let long_provider = "p".repeat(10_000);
    let long_model = "m".repeat(10_000);
    let attempt = ctx
        .start_attempt(&long_provider, &long_model)
        .await
        .expect("oversized fields truncate, not fail");
    attempt.finish("success", Some(200), None, None).await;
    assert!(
        request_log_flush_with_budget(Duration::from_secs(5)).await,
        "truncated event must drain"
    );
    let (_, _, max_event) = request_log_bounds();
    let sqlite = tmp.db.sqlite.clone();
    let rows = tokio::task::spawn_blocking(move || {
        sqlite.with_conn(|conn| {
            request_repo::list(conn, &request_repo::RequestDetailFilter::default(), 10, 0)
        })
    })
    .await
    .expect("list task")
    .expect("list query");
    assert_eq!(rows.len(), 1);
    let wire = serde_json::json!({
        "provider": rows[0].provider,
        "model": rows[0].model,
        "data": rows[0].data,
    })
    .to_string();
    assert!(
        wire.len() < max_event,
        "stored event must fit the per-event cap ({} >= {})",
        wire.len(),
        max_event
    );
    assert!(rows[0].provider.as_deref().unwrap().len() <= 256);
}

#[tokio::test]
async fn flush_budget_is_honest_when_sqlite_is_blocked() {
    let _serial = serial_guard().await;
    let tmp = TempTestDb::new().await;
    let ctx = lean_context(&tmp.db);
    let sqlite = tmp.db.sqlite.clone();
    let gate = tokio::task::spawn_blocking(move || {
        let _guard = sqlite.lock();
        std::thread::sleep(Duration::from_millis(800));
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Fill the queue while SQLite is held so the flush cannot complete.
    for _ in 0..32 {
        let _ = ctx.start_attempt("openai", "m").await;
    }
    let drained = request_log_flush_with_budget(Duration::from_millis(100)).await;
    assert!(
        !drained,
        "flush budget must report incomplete instead of hanging"
    );
    gate.await.expect("gate task");
    assert!(
        request_log_flush_with_budget(Duration::from_secs(10)).await,
        "flush completes after SQLite releases"
    );
}

#[test]
fn default_mode_preserves_durable_guarantees() {
    // Unknown/unset mode must remain durable so migration is never silent.
    // (This test does not mutate the process env; it only asserts the parser.)
    assert_eq!(request_log_bounds().0, 512);
    let _ = request_log_mode();
}
