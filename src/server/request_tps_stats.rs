//! Exact, on-demand rolling TPS medians from saved successful attempts.
//! No aggregate writes, reader pool, cache, or legacy TPS reconstruction.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, TimeDelta, Utc};
use rusqlite::{params, types::ValueRef, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::OwnedSemaphorePermit;

use super::application_logs::completed_tps;
use super::state::AppState;

// Engineering budgets, not throughput guarantees. The retained
// budget includes vector capacities, literal keys, and conservative map/node
// overhead. JSON is bounded separately and discarded after each relevant row.
const MAX_RETAINED_BYTES: usize = 32 * 1024 * 1024;
const MAX_CANDIDATES: usize = 1_000_000;
const MAX_JSON_BYTES: usize = 128 * 1024;
// Capture writes <=256-byte labels; allow substantially larger imported labels
// without permitting unbounded keys. Never truncate stored identities.
const MAX_LABEL_BYTES: usize = 4 * 1024;
// RFC3339 needs far less; this also permits long fractional precision.
const MAX_TIMESTAMP_BYTES: usize = 128;
// Allow for the sparsely populated root's preallocated entry slots as well as
// per-entry structs, tree nodes, and allocator overhead in subsequent groups.
const MAP_ROOT_OVERHEAD_BYTES: usize = 2 * 1024;
const GROUP_OVERHEAD_BYTES: usize = 512;
// Additional response/selection vectors and label copies do not share the
// map's node allowance. Include conservative allocator overhead for each.
const ALLOCATION_OVERHEAD_BYTES: usize = 32;
// Cooperative between-row check, NOT a hard deadline for SQLite/disk I/O.
const MAX_SCAN_DURATION: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
struct Limits {
    retained_bytes: usize,
    candidates: usize,
    json_bytes: usize,
    label_bytes: usize,
    timestamp_bytes: usize,
    scan_duration: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            retained_bytes: MAX_RETAINED_BYTES,
            candidates: MAX_CANDIDATES,
            json_bytes: MAX_JSON_BYTES,
            label_bytes: MAX_LABEL_BYTES,
            timestamp_bytes: MAX_TIMESTAMP_BYTES,
            scan_duration: MAX_SCAN_DURATION,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct StatsQuery {
    page: Option<usize>,
    page_size: Option<usize>,
    provider: Option<String>,
    model: Option<String>,
    api_key_id: Option<String>,
    hidden_providers: Option<String>,
    #[serde(skip)]
    excluded_providers: Vec<Option<String>>,
}

impl StatsQuery {
    fn normalize(mut self) -> Result<Self, &'static str> {
        self.page = Some(self.page.unwrap_or(1));
        self.page_size = Some(self.page_size.unwrap_or(20));
        if self.page == Some(0) || !(1..=100).contains(&self.page_size.unwrap()) {
            return Err("page must be >= 1 and pageSize must be between 1 and 100");
        }
        if let Some(raw) = self.hidden_providers.take() {
            const INVALID: &str =
                "hiddenProviders must be a JSON array of string or null IDs within the allowed limits";
            if raw.len() > MAX_JSON_BYTES {
                return Err(INVALID);
            }
            let mut providers: Vec<Option<String>> =
                serde_json::from_str(&raw).map_err(|_| INVALID)?;
            if providers
                .iter()
                .flatten()
                .any(|provider| provider.len() > MAX_LABEL_BYTES)
            {
                return Err(INVALID);
            }
            // Exact nullable identities: no trimming, coercion, or catalog lookup.
            providers.sort_unstable();
            providers.dedup();
            self.excluded_providers = providers;
        }
        let clean = |value: Option<String>| {
            value
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        self.provider = clean(self.provider);
        self.model = clean(self.model);
        self.api_key_id = clean(self.api_key_id);
        Ok(self)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TpsStatsPayload {
    as_of: String,
    windows: Windows<WindowBounds>,
    groups: Vec<GroupStats>,
    available_providers: Vec<Option<String>>,
    pagination: Pagination,
}

#[derive(Serialize)]
struct Windows<T> {
    #[serde(rename = "1h")]
    one: T,
    #[serde(rename = "6h")]
    six: T,
    #[serde(rename = "24h")]
    day: T,
}

#[derive(Serialize)]
struct WindowBounds {
    start: String,
    end: String,
}

#[derive(Serialize)]
struct GroupStats {
    provider: Option<String>,
    model: Option<String>,
    windows: Windows<WindowStats>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowStats {
    median_tps: Option<f64>,
    sample_count: usize,
    successful_attempt_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Pagination {
    page: usize,
    page_size: usize,
    total_items: usize,
    total_pages: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unavailable;

impl From<rusqlite::Error> for Unavailable {
    fn from(_: rusqlite::Error) -> Self {
        Self
    }
}

fn unavailable_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": {"code": "tps_stats_unavailable", "message": "TPS statistics are temporarily unavailable"}})),
    )
        .into_response()
}

pub(super) async fn get_tps_stats(
    State(state): State<AppState>,
    Query(query): Query<StatsQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::api::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    let Ok(permit) = state.request_tps_stats_reader.clone().try_acquire_owned() else {
        return unavailable_response();
    };
    let query = match query.normalize() {
        Ok(query) => query,
        Err(message) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response()
        }
    };
    let Some(path) = state.db.sqlite.path().map(Path::to_path_buf) else {
        return unavailable_response();
    };
    let as_of = Utc::now();
    match spawn_reader(permit, move || {
        read_stats(&path, &query, as_of, Limits::default())
    })
    .await
    {
        Ok(Ok(payload)) => Json(payload).into_response(),
        _ => unavailable_response(),
    }
}

// Ownership lives in this closure through actual completion, even when the
// awaiting HTTP task is aborted and its JoinHandle is dropped.
fn spawn_reader(
    permit: OwnedSemaphorePermit,
    read: impl FnOnce() -> Result<TpsStatsPayload, Unavailable> + Send + 'static,
) -> tokio::task::JoinHandle<Result<TpsStatsPayload, Unavailable>> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        read()
    })
}

#[derive(Default)]
struct Accumulator {
    successes: [usize; 3],
    samples: [Vec<f64>; 3],
}

type GroupKey = (Option<String>, Option<String>);
type Groups = BTreeMap<GroupKey, Accumulator>;

struct Budget {
    used: usize,
    limit: usize,
}

impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), Unavailable> {
        let used = self.used.checked_add(bytes).ok_or(Unavailable)?;
        if used > self.limit {
            return Err(Unavailable);
        }
        self.used = used;
        Ok(())
    }

    fn push(&mut self, samples: &mut Vec<f64>, rate: f64) -> Result<(), Unavailable> {
        if samples.len() == samples.capacity() {
            let old = samples.capacity();
            let available = (self.limit - self.used) / size_of::<f64>();
            let growth = old.max(4).min(available);
            if growth == 0 {
                return Err(Unavailable);
            }
            // Reserve only after checking the prospective retained capacity.
            samples.try_reserve_exact(growth).map_err(|_| Unavailable)?;
            self.charge((samples.capacity() - old) * size_of::<f64>())?;
        }
        samples.push(rate);
        Ok(())
    }

    fn reserve<T>(&mut self, values: &mut Vec<T>, capacity: usize) -> Result<(), Unavailable> {
        let old = values.capacity();
        if capacity <= old {
            return Ok(());
        }
        let bytes = (capacity - old)
            .checked_mul(size_of::<T>())
            .and_then(|bytes| {
                bytes.checked_add(if old == 0 {
                    ALLOCATION_OVERHEAD_BYTES
                } else {
                    0
                })
            })
            .ok_or(Unavailable)?;
        self.charge(bytes)?;
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| Unavailable)?;
        // Account actual capacity if an allocator gives us more than requested.
        self.charge(
            (values.capacity() - capacity)
                .checked_mul(size_of::<T>())
                .ok_or(Unavailable)?,
        )
    }

    fn clone_label(&mut self, label: &Option<String>) -> Result<Option<String>, Unavailable> {
        let Some(label) = label else {
            return Ok(None);
        };
        self.charge(
            label
                .capacity()
                .checked_add(ALLOCATION_OVERHEAD_BYTES)
                .ok_or(Unavailable)?,
        )?;
        let copy = label.clone();
        self.charge(copy.capacity().saturating_sub(label.capacity()))?;
        Ok(Some(copy))
    }

    fn push_provider(
        &mut self,
        providers: &mut Vec<Option<String>>,
        provider: &Option<String>,
    ) -> Result<(), Unavailable> {
        if providers.len() == providers.capacity() {
            let old = providers.capacity();
            self.reserve(providers, old.checked_add(old.max(4)).ok_or(Unavailable)?)?;
        }
        let provider = self.clone_label(provider)?;
        providers.push(provider);
        Ok(())
    }

    fn push_page_key(
        &mut self,
        keys: &mut Vec<GroupKey>,
        key: &GroupKey,
        capacity: usize,
    ) -> Result<(), Unavailable> {
        if keys.len() >= capacity {
            return Err(Unavailable);
        }
        self.reserve(keys, capacity)?;
        let provider = self.clone_label(&key.0)?;
        let model = self.clone_label(&key.1)?;
        keys.push((provider, model));
        Ok(())
    }
}

// Date-only bounds deliberately pad BOTH UTC dates by 24h. Comparing a full
// RFC3339 string against these dates includes every legal offset and fraction;
// only exact parsed instants below decide membership. No SQL sort or UDF.
const SCAN_SQL: &str = "SELECT timestamp, status, apiKeyId, provider, model, data
    FROM requestDetails INDEXED BY idx_rd_ts
    WHERE timestamp >= ?1 AND timestamp < ?2";

fn scan_groups(
    path: &Path,
    query: &StatsQuery,
    as_of: DateTime<Utc>,
    limits: Limits,
) -> Result<(Groups, usize), Unavailable> {
    let started = Instant::now();
    let starts = [1, 6, 24].map(|hours| as_of - TimeDelta::hours(hours));
    let coarse_start = (starts[2] - TimeDelta::hours(24)).date_naive().to_string();
    let coarse_end = (as_of + TimeDelta::hours(24))
        .date_naive()
        .succ_opt()
        .ok_or(Unavailable)?
        .to_string();
    let mut groups = Groups::new();
    let mut budget = Budget {
        used: 0,
        limit: limits.retained_bytes,
    };
    // Parsed exclusions are bounded by the JSON input limit, and remain owned
    // by the query during the scan. Include their retained capacities too.
    if query.excluded_providers.capacity() != 0 {
        budget.charge(
            query.excluded_providers.capacity() * size_of::<Option<String>>()
                + ALLOCATION_OVERHEAD_BYTES,
        )?;
        for provider in query.excluded_providers.iter().flatten() {
            budget.charge(provider.capacity() + ALLOCATION_OVERHEAD_BYTES)?;
        }
    }
    {
        // This connection never calls SqliteDb::open/init or inherits the
        // application's 64MiB page cache, mmap, temp-store, or busy settings.
        let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        reader.busy_timeout(Duration::ZERO)?;
        let mut statement = reader.prepare(SCAN_SQL)?;
        let mut rows = statement.query(params![coarse_start, coarse_end])?;
        let mut candidates = 0;
        while let Some(row) = rows.next()? {
            candidates += 1;
            if candidates > limits.candidates || started.elapsed() >= limits.scan_duration {
                return Err(Unavailable);
            }
            // Count every coarse candidate, including failures, filter misses
            // and padded dates. Prove irrelevance using borrowed metadata before
            // fetching/copying/parsing JSON; huge irrelevant JSON is harmless to
            // our parser budget. SQLite's own internal allocations are separate.
            if row.get_ref(1)? != ValueRef::Text(b"success") {
                continue;
            }
            if !matches_equality(row.get_ref(2)?, query.api_key_id.as_deref())
                || !matches_equality(row.get_ref(3)?, query.provider.as_deref())
                || !matches_model(row.get_ref(4)?, query.model.as_deref())
            {
                continue;
            }
            let timestamp = bounded_text(row.get_ref(0)?, limits.timestamp_bytes)?;
            let Some(timestamp) = timestamp
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc))
            else {
                continue;
            };
            if timestamp < starts[2] || timestamp >= as_of {
                continue;
            }
            let provider = bounded_text(row.get_ref(3)?, limits.label_bytes)?;
            let model = bounded_text(row.get_ref(4)?, limits.label_bytes)?;
            let data = bounded_text(row.get_ref(5)?, limits.json_bytes)?.ok_or(Unavailable)?;
            // Malformed JSON is a saved success with unknown TPS, just as in
            // raw logs. Oversize/indeterminate data is refused, never truncated.
            let value = serde_json::from_str::<Value>(data).unwrap_or(Value::Null);
            let rate = completed_tps(Some("success"), &value)
                .map(|(tokens, micros)| tokens as f64 * 1_000_000.0 / micros as f64);
            let borrowed_key = (provider.map(str::to_owned), model.map(str::to_owned));
            // Keys are capped above; charge only retained identities, not the
            // bounded per-row lookup key. BTreeMap keeps literal nullable order.
            if !groups.contains_key(&borrowed_key) {
                budget.charge(
                    if groups.is_empty() {
                        MAP_ROOT_OVERHEAD_BYTES
                    } else {
                        0
                    } + GROUP_OVERHEAD_BYTES
                        + borrowed_key.0.as_ref().map_or(0, String::capacity)
                        + borrowed_key.1.as_ref().map_or(0, String::capacity),
                )?;
            }
            let group = groups.entry(borrowed_key).or_default();
            for (index, start) in starts.iter().enumerate() {
                if timestamp >= *start {
                    group.successes[index] += 1;
                    if let Some(rate) = rate {
                        budget.push(&mut group.samples[index], rate)?;
                    }
                }
            }
        }
    } // Cursor, statement, snapshot, and local reader close BEFORE medians.

    Ok((groups, budget.used))
}

fn read_stats(
    path: &Path,
    query: &StatsQuery,
    as_of: DateTime<Utc>,
    limits: Limits,
) -> Result<TpsStatsPayload, Unavailable> {
    let starts = [1, 6, 24].map(|hours| as_of - TimeDelta::hours(hours));
    let (mut groups, retained) = scan_groups(path, query, as_of, limits)?;
    let mut budget = Budget {
        used: retained,
        limit: limits.retained_bytes,
    };
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);
    let offset = page.saturating_sub(1).saturating_mul(page_size);
    let mut available_providers = Vec::new();
    let mut previous_provider = None;
    let mut page_keys = Vec::new();
    let mut total_items = 0;
    let mut visible_index = 0;
    // Two literal-map passes form a global measured/unmeasured partition without
    // sorting the population or cloning sample vectors. Options come from ALL
    // groups in the first pass, before exclusion, and are already null-first.
    for measured in [true, false] {
        for (key, group) in &groups {
            if measured && previous_provider != Some(&key.0) {
                budget.push_provider(&mut available_providers, &key.0)?;
                previous_provider = Some(&key.0);
            }
            if query.excluded_providers.binary_search(&key.0).is_ok() {
                continue;
            }
            if measured {
                total_items += 1;
            }
            if group.samples[2].is_empty() == measured {
                continue;
            }
            if visible_index >= offset && page_keys.len() < page_size {
                budget.push_page_key(&mut page_keys, key, groups.len().min(page_size))?;
            }
            visible_index += 1;
        }
    }
    let mut page_groups = Vec::new();
    budget.reserve(&mut page_groups, page_keys.len())?;
    for key in page_keys {
        let group = groups.remove(&key).ok_or(Unavailable)?;
        let (provider, model) = key;
        let [one, six, day] = group.samples;
        page_groups.push(GroupStats {
            provider,
            model,
            windows: Windows {
                one: window_stats(one, group.successes[0]),
                six: window_stats(six, group.successes[1]),
                day: window_stats(day, group.successes[2]),
            },
        });
    }
    let end = as_of.to_rfc3339();
    let bounds = |index: usize| WindowBounds {
        start: starts[index].to_rfc3339(),
        end: end.clone(),
    };
    Ok(TpsStatsPayload {
        as_of: end.clone(),
        windows: Windows {
            one: bounds(0),
            six: bounds(1),
            day: bounds(2),
        },
        groups: page_groups,
        available_providers,
        pagination: Pagination {
            page,
            page_size,
            total_items,
            total_pages: total_items.div_ceil(page_size),
        },
    })
}

fn matches_equality(value: ValueRef<'_>, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| value == ValueRef::Text(filter.as_bytes()))
}

fn matches_model(value: ValueRef<'_>, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| match value {
        ValueRef::Text(bytes) => bytes
            .windows(filter.len())
            .any(|part| part == filter.as_bytes()),
        _ => false,
    })
}

fn bounded_text(value: ValueRef<'_>, limit: usize) -> Result<Option<&str>, Unavailable> {
    match value {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) if bytes.len() <= limit => std::str::from_utf8(bytes)
            .map(Some)
            .map_err(|_| Unavailable),
        _ => Err(Unavailable),
    }
}

fn window_stats(mut samples: Vec<f64>, successful_attempt_count: usize) -> WindowStats {
    let sample_count = samples.len();
    samples.sort_unstable_by(f64::total_cmp);
    let median_tps = if sample_count == 0 {
        None
    } else if sample_count % 2 == 0 {
        Some((samples[sample_count / 2 - 1] + samples[sample_count / 2]) / 2.0)
    } else {
        Some(samples[sample_count / 2])
    };
    WindowStats {
        median_tps,
        sample_count,
        successful_attempt_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    struct Fixture {
        directory: tempfile::TempDir,
        writer: Connection,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let writer = Connection::open(directory.path().join("stats.sqlite")).unwrap();
            writer
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     CREATE TABLE requestDetails (
                         timestamp TEXT, status TEXT, apiKeyId TEXT,
                         provider TEXT, model TEXT, data TEXT);
                     CREATE INDEX idx_rd_ts ON requestDetails(timestamp DESC);",
                )
                .unwrap();
            Self { directory, writer }
        }

        fn insert(
            &self,
            timestamp: &str,
            status: &str,
            provider: Option<&str>,
            model: Option<&str>,
            data: &str,
        ) {
            self.writer
                .execute(
                    "INSERT INTO requestDetails VALUES (?1,?2,'consumer',?3,?4,?5)",
                    params![timestamp, status, provider, model, data],
                )
                .unwrap();
        }

        fn read(&self, query: StatsQuery, limits: Limits) -> Result<Value, Unavailable> {
            read_stats(
                &self.directory.path().join("stats.sqlite"),
                &query.normalize().unwrap(),
                as_of(),
                limits,
            )
            .map(|result| serde_json::to_value(result).unwrap())
        }
    }

    fn as_of() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-12T12:00:00.500Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn observation(tokens: u64, micros: u64) -> String {
        json!({"upstreamTps":{"version":1,"endKind":"json_body","generatedOutputTokens":tokens,"elapsedMicros":micros}}).to_string()
    }

    fn group<'a>(payload: &'a Value, provider: Option<&str>, model: Option<&str>) -> &'a Value {
        payload["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|group| group["provider"] == json!(provider) && group["model"] == json!(model))
            .unwrap()
    }

    #[tokio::test]
    async fn hidden_provider_raw_json_byte_boundary_and_safe_400() {
        // The HTTP URI parser refuses URLs this large before routing; exercise
        // the handler directly so its independent JSON bound stays verified.
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::Db::load_from(directory.path()).await.unwrap());
        db.update(|data| data.settings.require_login = false)
            .await
            .unwrap();
        let state = AppState::new(db);
        for bytes in [MAX_JSON_BYTES, MAX_JSON_BYTES + 1] {
            let raw = format!("{}[]", " ".repeat(bytes - 2));
            let response = get_tps_stats(
                State(state.clone()),
                Query(StatsQuery {
                    hidden_providers: Some(raw),
                    ..Default::default()
                }),
                HeaderMap::new(),
            )
            .await;
            assert_eq!(
                response.status(),
                if bytes == MAX_JSON_BYTES {
                    StatusCode::OK
                } else {
                    StatusCode::BAD_REQUEST
                }
            );
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let payload: Value = serde_json::from_slice(&body).unwrap();
            if bytes == MAX_JSON_BYTES {
                assert_eq!(payload["availableProviders"], json!([]));
            } else {
                assert_eq!(
                    payload,
                    json!({"error": "hiddenProviders must be a JSON array of string or null IDs within the allowed limits"})
                );
            }
        }
        let query = StatsQuery {
            hidden_providers: Some(r#"["null",null,"",null," p ","null"]"#.into()),
            ..Default::default()
        }
        .normalize()
        .unwrap();
        assert_eq!(
            query.excluded_providers,
            vec![
                None,
                Some("".into()),
                Some(" p ".into()),
                Some("null".into())
            ]
        );
        assert!(query.hidden_providers.is_none());
    }

    #[test]
    fn half_open_boundaries_offsets_precision_invalid_times_and_counts() {
        let fixture = Fixture::new();
        for timestamp in [
            "2026-10-12T11:00:00.500Z",       // inclusive 1h
            "2026-10-12T11:00:00.499999999Z", // 6h only
            "2026-10-12T06:00:00.500+00:00",  // inclusive 6h
            "2026-10-12T05:00:00.499Z",       // 24h only
            "2026-10-11T12:00:00.500Z",       // inclusive 24h
            "2026-10-11T12:00:00.499Z",       // excluded
            "2026-10-12T12:00:00.500Z",       // excluded asOf
            "2026-10-12T12:00:00.499999999Z", // included fraction
            "2026-10-12T14:30:00.500+03:00",  // 11:30 UTC
            "2026-10-12T07:30:00.500-04:00",  // 11:30 UTC
            "2026-10-12T99:99:99Z",           // invalid
            "2026-10-12invalid",              // invalid
        ] {
            fixture.insert(
                timestamp,
                "success",
                Some("p"),
                Some("m"),
                &observation(7, 3_000_000),
            );
        }
        fixture.insert(
            "2026-10-12T11:30:00Z",
            "error",
            Some("p"),
            Some("m"),
            &observation(999, 1),
        );
        let payload = fixture
            .read(StatsQuery::default(), Limits::default())
            .unwrap();
        assert_eq!(payload["asOf"], "2026-10-12T12:00:00.500+00:00");
        for (window, expected_count, hours) in [("1h", 4, 1), ("6h", 6, 6), ("24h", 8, 24)] {
            let cell = &payload["groups"][0]["windows"][window];
            assert_eq!(cell["sampleCount"], expected_count, "{window}");
            assert_eq!(cell["successfulAttemptCount"], expected_count, "{window}");
            assert_eq!(cell["medianTps"].as_f64(), Some(7.0 / 3.0));
            assert_eq!(payload["windows"][window]["end"], payload["asOf"]);
            assert_eq!(
                payload["windows"][window]["start"],
                (as_of() - TimeDelta::hours(hours)).to_rfc3339()
            );
        }
    }

    #[test]
    fn coarse_dates_include_extreme_rfc3339_offsets() {
        let fixture = Fixture::new();
        // Legal RFC3339 offsets can be almost 24h in either direction.
        for timestamp in [
            "2026-10-10T12:01:00.500-23:59", // exact 24h lower bound
            "2026-10-13T11:58:59.999+23:59", // just before asOf
        ] {
            fixture.insert(
                timestamp,
                "success",
                Some("p"),
                Some("m"),
                &observation(1, 1),
            );
        }
        let payload = fixture
            .read(StatsQuery::default(), Limits::default())
            .unwrap();
        assert_eq!(payload["groups"][0]["windows"]["24h"]["sampleCount"], 2);
        assert_eq!(payload["groups"][0]["windows"]["1h"]["sampleCount"], 1);
    }

    #[test]
    fn ordinary_medians_unknown_successes_zero_and_nullable_identities() {
        let fixture = Fixture::new();
        for tokens in [0, 1, 9] {
            fixture.insert(
                "2026-10-12T11:30:00Z",
                "success",
                None,
                None,
                &observation(tokens, 1_000_000),
            );
        }
        for tokens in [0, 1, 2, 10] {
            fixture.insert(
                "2026-10-12T11:30:00Z",
                "success",
                Some(""),
                None,
                &observation(tokens, 1_000_000),
            );
        }
        for (provider, model, data) in [
            (None, Some(""), "{malformed"),
            (Some(""), Some(""), r#"{"outputTokens":99,"durationMs":1}"#),
        ] {
            fixture.insert("2026-10-12T04:00:00Z", "success", provider, model, data);
        }
        let payload = fixture
            .read(StatsQuery::default(), Limits::default())
            .unwrap();
        let groups = payload["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 4);
        assert_eq!(
            group(&payload, None, None)["windows"]["1h"]["medianTps"],
            1.0
        );
        assert_eq!(
            group(&payload, Some(""), None)["windows"]["1h"]["medianTps"],
            1.5
        );
        for provider in [None, Some("")] {
            let unknown = group(&payload, provider, Some(""));
            assert!(unknown["windows"]["24h"]["medianTps"].is_null());
            assert_eq!(unknown["windows"]["24h"]["sampleCount"], 0);
            assert_eq!(unknown["windows"]["24h"]["successfulAttemptCount"], 1);
            assert_eq!(unknown["windows"]["6h"]["successfulAttemptCount"], 0);
        }
        fixture.insert(
            "2026-10-12T11:30:00Z",
            "success",
            Some("zero"),
            Some("m"),
            &observation(0, 1),
        );
        let zero = fixture
            .read(
                StatsQuery {
                    provider: Some("zero".into()),
                    ..Default::default()
                },
                Limits::default(),
            )
            .unwrap();
        assert_eq!(zero["groups"][0]["windows"]["1h"]["medianTps"], 0.0);
    }

    #[test]
    fn aggregate_validator_matches_raw_projection_for_u64_floats_and_malformed_json() {
        let fixture = Fixture::new();
        let valid: Value = serde_json::from_str(&observation(7, 3_000_000)).unwrap();
        let mut cases = vec![
            valid.clone(),
            json!({"outputTokens":999,"durationMs":1}),
            json!(null),
        ];
        for (field, value) in [
            ("version", json!(1.0)),
            ("version", json!(2)),
            ("generatedOutputTokens", json!(1.0)),
            ("generatedOutputTokens", json!(1.5)),
            ("generatedOutputTokens", json!(-1)),
            ("generatedOutputTokens", json!("1")),
            ("generatedOutputTokens", json!(u64::MAX)),
            ("elapsedMicros", json!(1.0)),
            ("elapsedMicros", json!(0)),
            ("elapsedMicros", json!(u64::MAX)),
            ("endKind", json!("partial")),
        ] {
            let mut data = valid.clone();
            data["upstreamTps"][field] = value;
            cases.push(data);
        }
        for kind in ["protocol_terminal", "clean_eof", "json_body"] {
            let mut data = valid.clone();
            data["upstreamTps"]["endKind"] = json!(kind);
            cases.push(data);
        }
        for (index, data) in cases.iter().enumerate() {
            fixture.insert(
                "2026-10-12T11:30:00Z",
                "success",
                Some("p"),
                Some(&format!("{index:02}")),
                &data.to_string(),
            );
        }
        fixture.insert(
            "2026-10-12T11:30:00Z",
            "success",
            Some("p"),
            Some("malformed"),
            "{bad",
        );
        let payload = fixture
            .read(StatsQuery::default(), Limits::default())
            .unwrap();
        for (index, data) in cases.iter().enumerate() {
            let expected = completed_tps(Some("success"), data)
                .map(|(tokens, micros)| tokens as f64 * 1_000_000.0 / micros as f64);
            let cell = &group(&payload, Some("p"), Some(&format!("{index:02}")))["windows"]["1h"];
            assert_eq!(cell["sampleCount"], usize::from(expected.is_some()));
            assert_eq!(cell["medianTps"].as_f64(), expected);
        }
        let cell = &group(&payload, Some("p"), Some("malformed"))["windows"]["1h"];
        assert_eq!(cell["sampleCount"], 0);
        assert_eq!(cell["successfulAttemptCount"], 1);
    }

    #[test]
    fn all_coarse_candidates_count_toward_ceiling_and_huge_irrelevant_json_is_skipped() {
        let fixture = Fixture::new();
        let huge = "x".repeat(MAX_JSON_BYTES + 1);
        for (time, status, provider) in [
            ("2026-10-12T11:30:00Z", "error", "p"),
            ("2026-10-12T11:30:00Z", "success", "other"),
            ("2026-10-10T23:00:00Z", "success", "p"),
            ("2026-10-12invalid", "success", "p"),
        ] {
            fixture.insert(time, status, Some(provider), Some("m"), &huge);
        }
        let query = || StatsQuery {
            provider: Some("p".into()),
            ..Default::default()
        };
        let limits = Limits {
            candidates: 4,
            json_bytes: 1,
            ..Limits::default()
        };
        let payload = fixture.read(query(), limits).unwrap();
        assert_eq!(payload["pagination"]["totalItems"], 0);
        assert_eq!(
            fixture.read(
                query(),
                Limits {
                    candidates: 3,
                    ..limits
                }
            ),
            Err(Unavailable)
        );
        fixture.insert(
            "2026-10-12T11:30:00Z",
            "success",
            Some("p"),
            Some("m"),
            &huge,
        );
        assert_eq!(fixture.read(query(), Limits::default()), Err(Unavailable));
    }

    #[test]
    fn exact_json_field_capacity_and_cooperative_duration_budgets_refuse_whole_result() {
        let fixture = Fixture::new();
        let data = observation(1, 1_000_000);
        fixture.insert(
            "2026-10-12T11:30:00Z",
            "success",
            Some("p"),
            Some("m"),
            &data,
        );
        let scan_bytes =
            MAP_ROOT_OVERHEAD_BYTES + GROUP_OVERHEAD_BYTES + 2 + 3 * 4 * size_of::<f64>();
        let provider_list_bytes = 4 * size_of::<Option<String>>()
            + ALLOCATION_OVERHEAD_BYTES
            + 1
            + ALLOCATION_OVERHEAD_BYTES;
        let page_key_bytes =
            size_of::<GroupKey>() + ALLOCATION_OVERHEAD_BYTES + 2 * (1 + ALLOCATION_OVERHEAD_BYTES);
        let page_bytes = size_of::<GroupStats>() + ALLOCATION_OVERHEAD_BYTES;
        let exact = scan_bytes + provider_list_bytes + page_key_bytes + page_bytes;
        assert!(fixture
            .read(
                StatsQuery::default(),
                Limits {
                    json_bytes: data.len(),
                    retained_bytes: exact,
                    label_bytes: 1,
                    ..Limits::default()
                }
            )
            .is_ok());
        for limits in [
            Limits {
                json_bytes: data.len() - 1,
                ..Limits::default()
            },
            Limits {
                label_bytes: 0,
                ..Limits::default()
            },
            Limits {
                timestamp_bytes: 1,
                ..Limits::default()
            },
            Limits {
                retained_bytes: GROUP_OVERHEAD_BYTES + 1,
                ..Limits::default()
            },
            Limits {
                retained_bytes: exact - 1,
                ..Limits::default()
            },
            // Cannot hold even the first complete f64 observation.
            Limits {
                retained_bytes: MAP_ROOT_OVERHEAD_BYTES + GROUP_OVERHEAD_BYTES + 2 + 7,
                ..Limits::default()
            },
            Limits {
                scan_duration: Duration::ZERO,
                ..Limits::default()
            },
        ] {
            assert_eq!(
                fixture.read(StatsQuery::default(), limits),
                Err(Unavailable)
            );
        }
        fixture
            .writer
            .execute("UPDATE requestDetails SET model = x'80'", [])
            .unwrap();
        assert_eq!(
            fixture.read(StatsQuery::default(), Limits::default()),
            Err(Unavailable)
        );
    }

    #[test]
    fn budget_tracks_actual_vector_capacity_through_growth() {
        let mut budget = Budget { used: 0, limit: 64 };
        let mut samples = Vec::new();
        for index in 0..8 {
            budget.push(&mut samples, index as f64).unwrap();
            assert_eq!(budget.used, samples.capacity() * size_of::<f64>());
        }
        assert_eq!(budget.push(&mut samples, 9.0), Err(Unavailable));
        assert_eq!(samples.len(), 8);
    }

    #[test]
    fn provider_list_and_page_key_capacities_have_independent_checked_budgets() {
        let provider = Some("literal".to_owned());
        let model = Some("model".to_owned());
        let provider_bytes = 4 * size_of::<Option<String>>()
            + ALLOCATION_OVERHEAD_BYTES
            + provider.as_ref().unwrap().capacity()
            + ALLOCATION_OVERHEAD_BYTES;
        let mut budget = Budget {
            used: 0,
            limit: provider_bytes,
        };
        let mut providers = Vec::new();
        budget.push_provider(&mut providers, &provider).unwrap();
        assert_eq!(providers, vec![provider.clone()]);
        assert_eq!(
            budget.used,
            providers.capacity() * size_of::<Option<String>>()
                + ALLOCATION_OVERHEAD_BYTES
                + providers[0].as_ref().unwrap().capacity()
                + ALLOCATION_OVERHEAD_BYTES
        );
        assert_eq!(
            budget.push_provider(&mut providers, &provider),
            Err(Unavailable)
        );
        assert_eq!(providers.len(), 1);
        let mut too_small = Budget {
            used: 0,
            limit: provider_bytes - 1,
        };
        let mut refused = Vec::new();
        assert_eq!(
            too_small.push_provider(&mut refused, &provider),
            Err(Unavailable)
        );
        assert!(refused.is_empty());

        let key = (provider, model);
        let key_bytes = 3 * size_of::<GroupKey>()
            + ALLOCATION_OVERHEAD_BYTES
            + key.0.as_ref().unwrap().capacity()
            + key.1.as_ref().unwrap().capacity()
            + 2 * ALLOCATION_OVERHEAD_BYTES;
        let mut budget = Budget {
            used: 0,
            limit: key_bytes,
        };
        let mut keys = Vec::new();
        budget.push_page_key(&mut keys, &key, 3).unwrap();
        assert_eq!(keys, vec![key.clone()]);
        assert_eq!(
            budget.used,
            keys.capacity() * size_of::<GroupKey>()
                + ALLOCATION_OVERHEAD_BYTES
                + keys[0].0.as_ref().unwrap().capacity()
                + keys[0].1.as_ref().unwrap().capacity()
                + 2 * ALLOCATION_OVERHEAD_BYTES
        );
        assert_eq!(budget.push_page_key(&mut keys, &key, 3), Err(Unavailable));
        assert_eq!(keys.len(), 1);
        let mut too_small = Budget {
            used: 0,
            limit: key_bytes - 1,
        };
        let mut refused = Vec::new();
        assert_eq!(
            too_small.push_page_key(&mut refused, &key, 3),
            Err(Unavailable)
        );
        assert!(refused.is_empty());
    }

    #[test]
    fn provider_options_are_budgeted_even_when_no_page_groups_can_be_returned() {
        let fixture = Fixture::new();
        fixture.insert(
            "2026-10-12T11:30:00Z",
            "success",
            Some("p"),
            Some("m"),
            "{}",
        );
        let path = fixture.directory.path().join("stats.sqlite");
        for query in [
            StatsQuery {
                page: Some(2),
                ..Default::default()
            },
            StatsQuery {
                hidden_providers: Some(r#"["p"]"#.into()),
                ..Default::default()
            },
        ] {
            let query = query.normalize().unwrap();
            let (_, scan_bytes) = scan_groups(&path, &query, as_of(), Limits::default()).unwrap();
            let list_bytes = 4 * size_of::<Option<String>>()
                + ALLOCATION_OVERHEAD_BYTES
                + 1
                + ALLOCATION_OVERHEAD_BYTES;
            let payload = read_stats(
                &path,
                &query,
                as_of(),
                Limits {
                    retained_bytes: scan_bytes + list_bytes,
                    ..Limits::default()
                },
            )
            .unwrap();
            assert!(payload.groups.is_empty());
            assert_eq!(payload.available_providers, vec![Some("p".into())]);
            assert!(matches!(
                read_stats(
                    &path,
                    &query,
                    as_of(),
                    Limits {
                        retained_bytes: scan_bytes + list_bytes - 1,
                        ..Limits::default()
                    }
                ),
                Err(Unavailable)
            ));
        }
    }

    #[test]
    fn measured_partition_is_global_literal_order_with_zero_and_day_only_samples() {
        let fixture = Fixture::new();
        for (provider, model, measured, tokens, timestamp) in [
            (None, "unknown", false, 0, "2026-10-12T11:30:00Z"),
            (Some(""), "day-only", true, 7, "2026-10-12T04:00:00Z"),
            (Some("a"), "unknown", false, 0, "2026-10-12T11:30:00Z"),
            (Some("a"), "zero", true, 0, "2026-10-12T11:30:00Z"),
            (Some("z"), "a", true, 100, "2026-10-12T11:30:00Z"),
            (Some("z"), "b", true, 1, "2026-10-12T11:30:00Z"),
            (Some("z"), "unknown", false, 0, "2026-10-12T11:30:00Z"),
        ] {
            fixture.insert(
                timestamp,
                "success",
                provider,
                Some(model),
                &if measured {
                    observation(tokens, 1_000_000)
                } else {
                    "{}".into()
                },
            );
        }
        let expected = [
            json!(["", "day-only"]),
            json!(["a", "zero"]),
            json!(["z", "a"]),
            json!(["z", "b"]),
            json!([null, "unknown"]),
            json!(["a", "unknown"]),
            json!(["z", "unknown"]),
        ];
        for page in 1..=5 {
            let payload = fixture
                .read(
                    StatsQuery {
                        page: Some(page),
                        page_size: Some(2),
                        ..Default::default()
                    },
                    Limits::default(),
                )
                .unwrap();
            let identities: Vec<_> = payload["groups"]
                .as_array()
                .unwrap()
                .iter()
                .map(|group| json!([group["provider"], group["model"]]))
                .collect();
            let offset = ((page - 1) * 2).min(expected.len());
            assert_eq!(
                identities,
                expected[offset..(offset + 2).min(expected.len())]
            );
            assert_eq!(payload["pagination"]["totalItems"], 7);
            assert_eq!(payload["pagination"]["totalPages"], 4);
            assert_eq!(payload["availableProviders"], json!([null, "", "a", "z"]));
        }
        let payload = fixture
            .read(StatsQuery::default(), Limits::default())
            .unwrap();
        let day = group(&payload, Some(""), Some("day-only"));
        assert_eq!(day["windows"]["24h"]["sampleCount"], 1);
        assert_eq!(day["windows"]["6h"]["sampleCount"], 0);
        assert_eq!(
            group(&payload, Some("a"), Some("zero"))["windows"]["24h"]["medianTps"],
            0.0
        );
    }

    #[test]
    fn scan_plan_is_timestamp_indexed_without_temporary_sort() {
        let fixture = Fixture::new();
        let mut plan = fixture
            .writer
            .prepare(&format!("EXPLAIN QUERY PLAN {SCAN_SQL}"))
            .unwrap();
        let details = plan
            .query_map(["2026-10-10", "2026-10-13"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            details.iter().any(|detail| detail
                .contains("SEARCH requestDetails USING INDEX idx_rd_ts")
                && detail.contains("timestamp>?")
                && detail.contains("timestamp<?")),
            "{details:?}"
        );
        assert!(!details.iter().any(|detail| detail.contains("TEMP B-TREE")));
    }

    #[tokio::test]
    async fn aborted_await_keeps_permit_until_blocking_reader_really_finishes() {
        let fixture = Fixture::new();
        let path = fixture.directory.path().join("stats.sqlite");
        let semaphore = Arc::new(Semaphore::new(1));
        let permit = semaphore.clone().try_acquire_owned().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let request = tokio::spawn(async move {
            spawn_reader(permit, move || {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                read_stats(&path, &StatsQuery::default(), as_of(), Limits::default())
            })
            .await
        });
        started_rx.await.unwrap();
        request.abort();
        assert!(matches!(request.await, Err(error) if error.is_cancelled()));
        assert!(semaphore.clone().try_acquire_owned().is_err());
        let finished = semaphore.clone().acquire_owned();
        finish_tx.send(()).unwrap();
        let _permit = tokio::time::timeout(Duration::from_secs(5), finished)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn sqlite_busy_and_read_errors_refuse_without_partial_statistics() {
        let fixture = Fixture::new();
        fixture.insert(
            "2026-10-12T11:30:00Z",
            "success",
            Some("p"),
            Some("m"),
            &observation(1, 1),
        );
        fixture
            .writer
            .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE")
            .unwrap();
        assert_eq!(
            fixture.read(StatsQuery::default(), Limits::default()),
            Err(Unavailable)
        );
        fixture.writer.execute_batch("ROLLBACK").unwrap();
        assert!(fixture
            .read(StatsQuery::default(), Limits::default())
            .is_ok());
        fixture
            .writer
            .execute_batch("DROP INDEX idx_rd_ts")
            .unwrap();
        assert_eq!(
            fixture.read(StatsQuery::default(), Limits::default()),
            Err(Unavailable)
        );
    }

    /// Parent-run engineering probe: same scan, default budgets, file-backed
    /// WAL, varied rates, historical rows outside the cursor, concurrent writes.
    /// Run alone with --ignored --nocapture; this is not a capacity guarantee.
    #[test]
    #[ignore = "parent-run resource/concurrent-write measurement"]
    fn measure_indexed_scan_with_concurrent_writes() {
        use std::sync::atomic::{AtomicBool, Ordering};

        fn rss_kib() -> Option<usize> {
            // Linux-only measurement; absent on other targets, not a gate there.
            std::fs::read_to_string("/proc/self/status")
                .ok()?
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        }

        fn measure(fixture: &Fixture, name: &str, candidates: usize) {
            let path = fixture.directory.path().join("stats.sqlite");
            let reader =
                Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let cache_size: i64 = reader
                .query_row("PRAGMA cache_size", [], |row| row.get(0))
                .unwrap();
            drop(reader);
            let stop = Arc::new(AtomicBool::new(false));
            let writer_stop = stop.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let writer_path = path.clone();
            let writer = std::thread::spawn(move || {
                let writer = Connection::open(writer_path).unwrap();
                writer.busy_timeout(Duration::from_millis(100)).unwrap();
                let mut latencies = Vec::new();
                ready_tx.send(()).unwrap();
                while !writer_stop.load(Ordering::Acquire) {
                    let started = Instant::now();
                    writer.execute("INSERT INTO requestDetails VALUES ('2020-01-01T00:00:00Z','pending',NULL,NULL,NULL,'{}')", []).unwrap();
                    writer
                        .execute(
                            "UPDATE requestDetails SET status='success', data='{}' WHERE rowid=?1",
                            [writer.last_insert_rowid()],
                        )
                        .unwrap();
                    latencies.push(started.elapsed().as_secs_f64() * 1_000.0);
                    std::thread::sleep(Duration::from_millis(2));
                }
                latencies
            });
            ready_rx.recv().unwrap();
            let before_rss = rss_kib();
            let started = Instant::now();
            // Actual endpoint worker, including sorting only returned page groups.
            let result = read_stats(&path, &StatsQuery::default(), as_of(), Limits::default());
            let total_ms = started.elapsed().as_secs_f64() * 1_000.0;
            let after_rss = rss_kib();
            stop.store(true, Ordering::Release);
            let mut writes = writer.join().unwrap();
            writes.sort_unstable_by(f64::total_cmp);
            let write_max = writes.last().copied().unwrap_or_default();
            let write_median = writes.get(writes.len() / 2).copied().unwrap_or_default();
            let rss_delta_kib = before_rss
                .zip(after_rss)
                .map(|(before, after)| after.saturating_sub(before));
            // A second scan records accounted capacities, independently of RSS.
            match scan_groups(&path, &StatsQuery::default(), as_of(), Limits::default()) {
                Ok((groups, retained)) => {
                    assert!(result.is_ok());
                    assert_eq!(
                        groups
                            .values()
                            .map(|group| group.successes[2])
                            .sum::<usize>(),
                        candidates
                    );
                    assert!(retained <= MAX_RETAINED_BYTES);
                    eprintln!("TPS_PROBE name={name} candidates={candidates} groups={} retained_bytes={retained} total_ms={total_ms:.1} cache_size={cache_size} rss_delta_kib={rss_delta_kib:?} writes={} write_pair_median_ms={write_median:.3} write_pair_max_ms={write_max:.3}", groups.len(), writes.len());
                }
                Err(Unavailable) => {
                    assert!(result.is_err());
                    eprintln!("TPS_PROBE name={name} candidates={candidates} unavailable=true total_ms={total_ms:.1} cache_size={cache_size} rss_delta_kib={rss_delta_kib:?} writes={} write_pair_median_ms={write_median:.3} write_pair_max_ms={write_max:.3}", writes.len());
                }
            }
        }

        let mut fixture = Fixture::new();
        {
            let tx = fixture.writer.transaction().unwrap();
            tx.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<100000)
                INSERT INTO requestDetails SELECT '2020-01-01T00:00:00Z','success','consumer','historic','m','{}' FROM n").unwrap();
            tx.commit().unwrap();
        }
        let observations: Vec<_> = (0..101)
            .map(|tokens| observation(tokens, 3_000_000))
            .collect();
        let mut previous = 0;
        for candidates in [25_000, 100_000, 250_000, 1_000_000] {
            {
                let tx = fixture.writer.transaction().unwrap();
                let mut insert = tx.prepare("INSERT INTO requestDetails VALUES ('2026-10-12T11:30:00Z','success','consumer',?1,?2,?3)").unwrap();
                for index in previous..candidates {
                    insert
                        .execute(params![
                            format!("p-{}", index % 10),
                            format!("m-{}", index % 100),
                            observations[index % 101]
                        ])
                        .unwrap();
                }
                drop(insert);
                tx.commit().unwrap();
            }
            previous = candidates;
            measure(&fixture, "typical", candidates);
        }

        // Combined extremes: maximum permitted JSON/labels and many groups;
        // both accepted work and the retained-memory refusal are observable.
        let mut fixture = Fixture::new();
        let mut data = observation(7, 3_000_000);
        data.pop();
        data.push_str(",\"padding\":\"");
        data.push_str(&"x".repeat(MAX_JSON_BYTES - data.len() - 2));
        data.push_str("\"}");
        assert_eq!(data.len(), MAX_JSON_BYTES);
        for (start, end) in [(0, 1000), (1000, 4500)] {
            let tx = fixture.writer.transaction().unwrap();
            let mut insert = tx.prepare("INSERT INTO requestDetails VALUES ('2026-10-12T11:30:00Z','success','consumer',?1,?2,?3)").unwrap();
            for index in start..end {
                let provider = format!("{index:04}{}", "p".repeat(MAX_LABEL_BYTES - 4));
                let model = "m".repeat(MAX_LABEL_BYTES);
                insert.execute(params![provider, model, data]).unwrap();
            }
            drop(insert);
            tx.commit().unwrap();
            measure(&fixture, "max-fields", end);
        }
    }
}
