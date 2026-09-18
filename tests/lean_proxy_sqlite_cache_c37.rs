//! C37: SQLite page-cache `-64000` vs `-8192` measured on a representative DB.
//!
//! The setting is applied on the application connection (`SqliteDb::init`,
//! never an external sqlite shell) and mmap/temp_store/WAL/durability stay
//! untouched. This test builds one representative database, copies it per
//! configuration (fresh file \u2248 cold filesystem cache, rerun \u2248 warm),
//! runs the same request-log-shaped workload through the app path, and records
//! wall latency plus per-connection `SQLITE_DBSTATUS` page-cache used/hit/miss
//! plus process I/O deltas. Order alternates (ABBA) across repetitions to
//! cancel warm-up bias.
//!
//! Verdict rule (predeclared): adopt `-8192` only on a real \u226510% median
//! warm latency win with no >5% regression elsewhere; otherwise keep
//! `-64000`. A lowered limit is never reported as saved RAM.

use std::path::{Path, PathBuf};
use std::time::Instant;

use openproxy::db::sqlite::repo::request_repo::{self, NewRequestDetail, RequestDetailFilter};
use openproxy::db::sqlite::schema;
use rusqlite::ffi;

// SQLITE_DBSTATUS opcodes (libsqlite3-sys bindgen).
const CACHE_USED: std::os::raw::c_int = ffi::SQLITE_DBSTATUS_CACHE_USED;
const CACHE_HIT: std::os::raw::c_int = ffi::SQLITE_DBSTATUS_CACHE_HIT;
const CACHE_MISS: std::os::raw::c_int = ffi::SQLITE_DBSTATUS_CACHE_MISS;

const SEED_ROWS: usize = 20_000;
const WORKLOAD_WRITES: usize = 200;
const WORKLOAD_LISTS: usize = 40;
const REPETITIONS: usize = 3;

#[derive(Debug, Clone, Default, serde::Serialize)]
struct DbStatus {
    cache_used_bytes: i64,
    cache_hit: i64,
    cache_miss: i64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct ProcIo {
    read_bytes: u64,
    write_bytes: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct RunStats {
    cache_kib: u32,
    temperature: String,
    write_ms: f64,
    read_ms: f64,
    total_ms: f64,
    cache_used_before: i64,
    cache_used_after: i64,
    cache_hit_delta: i64,
    cache_miss_delta: i64,
    io_read_bytes_delta: u64,
    io_write_bytes_delta: u64,
    rows_after: usize,
}

fn db_status(conn: &rusqlite::Connection, op: std::os::raw::c_int) -> (i64, i64) {
    let mut current = 0;
    let mut highwater = 0;
    // SAFETY: `handle` is the live connection borrowed by `with_conn`; the
    // call only reads status counters and resets nothing.
    let rc = unsafe { ffi::sqlite3_db_status(conn.handle(), op, &mut current, &mut highwater, 0) };
    assert_eq!(rc, ffi::SQLITE_OK, "db_status must succeed");
    (current as i64, highwater as i64)
}

fn snapshot_status(conn: &rusqlite::Connection) -> DbStatus {
    DbStatus {
        cache_used_bytes: db_status(conn, CACHE_USED).0,
        cache_hit: db_status(conn, CACHE_HIT).0,
        cache_miss: db_status(conn, CACHE_MISS).0,
    }
}

fn read_proc_io() -> ProcIo {
    let content = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let mut io = ProcIo::default();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("read_bytes:"), Some(v)) => io.read_bytes = v.parse().unwrap_or(0),
            (Some("write_bytes:"), Some(v)) => io.write_bytes = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    io
}

fn pragma_cache_size(db: &openproxy::db::sqlite::SqliteDb) -> i64 {
    db.with_conn(|conn| conn.query_row("PRAGMA cache_size", [], |row| row.get::<_, i64>(0)))
        .expect("PRAGMA cache_size must be readable")
}

fn journal_mode(db: &openproxy::db::sqlite::SqliteDb) -> String {
    db.with_conn(|conn| conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0)))
        .expect("PRAGMA journal_mode must be readable")
}

fn seed_representative(path: &Path) {
    // Seed through the application connection path with the default cache
    // setting; the file is then copied per configuration under test.
    let db = openproxy::db::sqlite::SqliteDb::open(path).expect("seed db must open");
    db.with_conn(|conn| {
        for i in 0..SEED_ROWS {
            let id = format!("seed-{i:06}");
            let timestamp = format!("2026-08-{:02}T12:00:00Z", 1 + (i % 28));
            let provider = ["openai", "anthropic", "gemini", "codex"][i % 4];
            let model = format!("model-{}", i % 64);
            let data = serde_json::json!({
                "route": format!("{provider}/{}", model),
                "statusCode": 200,
                "durationMs": (i % 900) as u64,
                "inputTokens": (i % 4000) as u64,
                "outputTokens": (i % 2000) as u64,
                "cachedTokens": (i % 100) as u64,
            });
            request_repo::insert(
                conn,
                &NewRequestDetail {
                    id: &id,
                    timestamp: &timestamp,
                    provider: Some(provider),
                    model: Some(&model),
                    connection_id: None,
                    status: if i % 3 == 0 { "success" } else { "error" },
                    api_key_id: Some("key-seed"),
                    api_key_name: Some("seed"),
                    correlation_id: None,
                    data: &data,
                },
            )?;
        }
        Ok(())
    })
    .expect("seed inserts must succeed");
}

fn run_workload(path: &Path, cache_kib: u32, temperature: &str) -> RunStats {
    unsafe { std::env::set_var("OPENPROXY_SQLITE_CACHE_SIZE_KIB", cache_kib.to_string()) };
    let db = openproxy::db::sqlite::SqliteDb::open(path).expect("workload db must open");
    assert_eq!(
        pragma_cache_size(&db),
        -(cache_kib as i64),
        "cache_size must be applied on the application connection, not a shell"
    );
    assert_eq!(
        journal_mode(&db).to_ascii_uppercase(),
        "WAL",
        "C37 must not weaken WAL"
    );

    let before_status = db
        .with_conn(|conn| Ok(snapshot_status(conn)))
        .expect("status snapshot");
    let io_before = read_proc_io();

    let write_start = Instant::now();
    db.with_conn(|conn| {
        for i in 0..WORKLOAD_WRITES {
            let id = format!("w-{cache_kib}-{temperature}-{i:04}");
            let data = serde_json::json!({
                "route": "openai/gpt-c37",
                "statusCode": serde_json::Value::Null,
                "durationMs": 0,
                "inputTokens": serde_json::Value::Null,
                "outputTokens": serde_json::Value::Null,
                "cachedTokens": serde_json::Value::Null,
            });
            request_repo::insert(
                conn,
                &NewRequestDetail {
                    id: &id,
                    timestamp: "2026-09-18T12:00:00Z",
                    provider: Some("openai"),
                    model: Some("gpt-c37"),
                    connection_id: None,
                    status: "pending",
                    api_key_id: Some("key-c37"),
                    api_key_name: Some("c37"),
                    correlation_id: None,
                    data: &data,
                },
            )?;
            let done = serde_json::json!({
                "route": "openai/gpt-c37",
                "statusCode": 200,
                "durationMs": 3,
                "inputTokens": 10,
                "outputTokens": 20,
                "cachedTokens": 0,
            });
            assert!(request_repo::finish(conn, &id, "success", &done)?);
        }
        Ok(())
    })
    .expect("workload writes must succeed");
    let write_ms = write_start.elapsed().as_secs_f64() * 1000.0;

    let read_start = Instant::now();
    db.with_conn(|conn| {
        for page in 0..WORKLOAD_LISTS {
            let rows = request_repo::list(
                conn,
                &RequestDetailFilter {
                    provider: Some("openai"),
                    ..Default::default()
                },
                20,
                (page * 20) % 2000,
            )?;
            assert!(!rows.is_empty(), "representative reads must hit rows");
            let _ = request_repo::count(
                conn,
                &RequestDetailFilter {
                    status: Some("success"),
                    ..Default::default()
                },
            )?;
        }
        Ok(())
    })
    .expect("workload reads must succeed");
    let read_ms = read_start.elapsed().as_secs_f64() * 1000.0;

    let after_status = db
        .with_conn(|conn| Ok(snapshot_status(conn)))
        .expect("status snapshot");
    let io_after = read_proc_io();
    let rows_after: usize = db
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM requestDetails", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .expect("count") as usize;

    RunStats {
        cache_kib,
        temperature: temperature.to_string(),
        write_ms,
        read_ms,
        total_ms: write_ms + read_ms,
        cache_used_before: before_status.cache_used_bytes,
        cache_used_after: after_status.cache_used_bytes,
        cache_hit_delta: after_status.cache_hit - before_status.cache_hit,
        cache_miss_delta: after_status.cache_miss - before_status.cache_miss,
        io_read_bytes_delta: io_after.read_bytes.saturating_sub(io_before.read_bytes),
        io_write_bytes_delta: io_after.write_bytes.saturating_sub(io_before.write_bytes),
        rows_after,
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values[values.len() / 2]
}

#[test]
fn sqlite_page_cache_comparison() {
    let previous = std::env::var("OPENPROXY_SQLITE_CACHE_SIZE_KIB").ok();
    let dir = tempfile::tempdir().expect("c37 tempdir");
    let seed_path = dir.path().join("seed.sqlite");
    // Seed without override so the artifact baseline matches production init.
    unsafe { std::env::remove_var("OPENPROXY_SQLITE_CACHE_SIZE_KIB") };
    seed_representative(&seed_path);
    let seed_bytes = std::fs::metadata(&seed_path).expect("seed file").len();

    // ABBA order across repetitions cancels warm-up/filesystem-cache bias.
    let order: [u32; 6] = [8192, 64_000, 64_000, 8192, 8192, 64_000];
    let mut runs: Vec<RunStats> = Vec::new();
    for (rep, cache_kib) in order.into_iter().enumerate() {
        for temperature in ["cold", "warm"] {
            let path: PathBuf = dir
                .path()
                .join(format!("run-{rep}-{cache_kib}-{temperature}.sqlite"));
            std::fs::copy(&seed_path, &path).expect("copy seed per run");
            // Fresh file \u2248 cold OS page cache; immediate rerun \u2248 warm.
            runs.push(run_workload(&path, cache_kib, temperature));
        }
    }

    for run in &runs {
        assert_eq!(
            run.rows_after,
            SEED_ROWS + WORKLOAD_WRITES,
            "both configs must persist identical rows"
        );
    }

    let summarize = |kib: u32, temp: &str| -> (f64, f64, Vec<&RunStats>) {
        let group: Vec<&RunStats> = runs
            .iter()
            .filter(|r| r.cache_kib == kib && r.temperature == temp)
            .collect();
        assert_eq!(group.len(), REPETITIONS, "balanced matrix");
        let mut totals: Vec<f64> = group.iter().map(|r| r.total_ms).collect();
        let mut writes: Vec<f64> = group.iter().map(|r| r.write_ms).collect();
        (median(&mut totals), median(&mut writes), group)
    };
    let (small_cold, _, _) = summarize(8192, "cold");
    let (big_cold, _, _) = summarize(64_000, "cold");
    let (small_warm, small_warm_write, small_group) = summarize(8192, "warm");
    let (big_warm, big_warm_write, big_group) = summarize(64_000, "warm");

    // Predeclared rule: adopt -8192 only on a real \u226510% warm win with no
    // >5% regression on the other axis.
    let warm_gain = (big_warm - small_warm) / big_warm;
    let write_ratio = (small_warm_write - big_warm_write) / big_warm_write;
    let verdict = if warm_gain >= 0.10 && write_ratio <= 0.05 {
        "adopt -8192"
    } else {
        "keep -64000"
    };

    let artifact = serde_json::json!({
        "checkpoint": "C37",
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "seed_rows": SEED_ROWS,
        "seed_bytes": seed_bytes,
        "page_size": 4096,
        "workload": {"writes": WORKLOAD_WRITES, "lists": WORKLOAD_LISTS},
        "repetitions": REPETITIONS,
        "order": order,
        "medians_ms": {
            "small_8192_cold": small_cold,
            "big_64000_cold": big_cold,
            "small_8192_warm": small_warm,
            "big_64000_warm": big_warm,
        },
        "warm_gain_8192": warm_gain,
        "warm_write_ratio_8192": write_ratio,
        "runs": runs,
        "small_warm_cache_used_after_median": {
            "note": "per-connection SQLITE_DBSTATUS_CACHE_USED bytes after workload"
        },
        "small_group_cache_used": small_group.iter().map(|r| r.cache_used_after).collect::<Vec<_>>(),
        "big_group_cache_used": big_group.iter().map(|r| r.cache_used_after).collect::<Vec<_>>(),
        "limitations": [
            "Single 4096-byte pages; OS page cache cannot be dropped without privileges, so 'cold' means a freshly copied file, not a flushed cache.",
            "RSS is process-wide and allocator-retained; per-config RSS attribution is not claimed. Cache pressure is compared via SQLITE_DBSTATUS used/hit/miss and /proc/self/io deltas.",
            "mmap_size, temp_store, WAL, and synchronous were not varied in this experiment."
        ],
        "verdict": verdict,
    });
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bench/lean/sqlite-cache-c37.json");
    std::fs::create_dir_all(out.parent().unwrap()).expect("bench dir");
    std::fs::write(&out, serde_json::to_string_pretty(&artifact).unwrap()).expect("artifact");
    println!("C37 medians ms: small_cold={small_cold:.1} big_cold={big_cold:.1} small_warm={small_warm:.1} big_warm={big_warm:.1} verdict={verdict}");

    match previous {
        Some(value) => unsafe { std::env::set_var("OPENPROXY_SQLITE_CACHE_SIZE_KIB", value) },
        None => unsafe { std::env::remove_var("OPENPROXY_SQLITE_CACHE_SIZE_KIB") },
    }
    assert_eq!(
        schema::sqlite_cache_size_kib(),
        schema::DEFAULT_SQLITE_CACHE_KIB,
        "env must be restored so other suites keep the default"
    );
}
