use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeDelta, Utc};
use jsonwebtoken::{encode, EncodingKey, Header};
use openproxy::db::sqlite::repo::request_repo::{self, NewRequestDetail};
use openproxy::db::Db;
use openproxy::server::auth::{jwt_secret, DashboardClaims};
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::{json, Value};
use tower::util::ServiceExt;

const TEST_KEY: &str = "tps-stats-test-management-key";

struct Fixture {
    _directory: tempfile::TempDir,
    state: AppState,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::load_from(directory.path()).await.unwrap());
        db.update(|state| {
            state.api_keys = vec![
                ApiKey {
                    id: "management".into(),
                    key: TEST_KEY.into(),
                    is_active: Some(true),
                    ..Default::default()
                },
                ApiKey {
                    id: "inactive".into(),
                    key: "inactive-tps-test-key".into(),
                    is_active: Some(false),
                    ..Default::default()
                },
            ];
            state.settings.require_login = true;
        })
        .await
        .unwrap();
        Self {
            _directory: directory,
            state: AppState::new(db),
        }
    }

    fn insert(
        &self,
        id: &str,
        provider: Option<&str>,
        model: Option<&str>,
        key: &str,
        status: &str,
        data: Value,
    ) {
        self.state
            .db
            .sqlite
            .with_conn(|conn| {
                request_repo::insert(
                    conn,
                    &NewRequestDetail {
                        id,
                        timestamp: &(Utc::now() - TimeDelta::minutes(30)).to_rfc3339(),
                        provider,
                        model,
                        connection_id: Some("private-connection"),
                        status,
                        api_key_id: Some(key),
                        api_key_name: Some("private-key-name"),
                        correlation_id: Some("private-correlation"),
                        data: &data,
                    },
                )
            })
            .unwrap();
    }

    fn age(&self, id: &str, hours: i64) {
        self.state
            .db
            .sqlite
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE requestDetails SET timestamp=?1 WHERE id=?2",
                    rusqlite::params![(Utc::now() - TimeDelta::hours(hours)).to_rfc3339(), id],
                )?;
                Ok(())
            })
            .unwrap();
    }

    async fn request(&self, uri: &str, header: Option<(&str, &str)>) -> (StatusCode, Value) {
        let mut request = Request::builder().uri(uri);
        if let Some((name, value)) = header {
            request = request.header(name, value);
        }
        let response = openproxy::build_app(self.state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn stats(&self, query: &str) -> Value {
        let (status, payload) = self
            .request(
                &format!("/api/request-logs/tps-stats{query}"),
                Some(("x-api-key", TEST_KEY)),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{payload}");
        payload
    }
}

fn observation(tokens: u64) -> Value {
    json!({
        "upstreamTps": {"version":1,"endKind":"clean_eof","generatedOutputTokens":tokens,"elapsedMicros":1_000_000,"privateExtra":"private-data"},
        "chatSession":{"hmac":"private-session"},
        "request":"private-prompt", "response":"private-response",
        "outputTokens":999,"durationMs":1,
    })
}

fn assert_keys(value: &Value, expected: &[&str]) {
    let object = value.as_object().unwrap();
    assert_eq!(object.len(), expected.len(), "{value}");
    assert!(
        object.keys().all(|key| expected.contains(&key.as_str())),
        "{value}"
    );
}

fn hidden_query(providers: Value) -> String {
    format!(
        "?hiddenProviders={}",
        urlencoding::encode(&providers.to_string())
    )
}

fn identities(payload: &Value) -> Vec<Value> {
    payload["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| json!([group["provider"], group["model"]]))
        .collect()
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
async fn filters_are_equalities_and_literal_case_sensitive_model_substrings_success_only() {
    let fixture = Fixture::new().await;
    for (id, provider, model, key, status, rate) in [
        ("a", "p", "GPT%_A", "consumer-a", "success", 10),
        ("b", "p", "GPT%_A", "consumer-b", "success", 30),
        ("c", "p", "GPTxx_A", "consumer-a", "success", 50),
        ("d", "other-p", "GPT%_A", "consumer-a", "success", 90),
        ("e", "p", "gpt%_a", "consumer-a", "success", 70),
        ("f", "p", "GPT%_A", "consumer-a", "error", 1000),
        ("g", "p", "failure-only", "consumer-a", "pending", 1000),
    ] {
        fixture.insert(
            id,
            Some(provider),
            Some(model),
            key,
            status,
            observation(rate),
        );
    }
    let scope = fixture.stats("?provider=%20p%20&model=%20%25_%20").await;
    assert_eq!(scope["pagination"]["totalItems"], 2);
    assert_eq!(scope["availableProviders"], json!(["p"]));
    assert_eq!(
        identities(&scope),
        vec![json!(["p", "gpt%_a"]), json!(["p", "GPT%_A"])]
    );
    let gpt = group(&scope, Some("p"), Some("GPT%_A"));
    assert_eq!(gpt["windows"]["1h"]["medianTps"], 20.0);
    assert_eq!(gpt["windows"]["1h"]["successfulAttemptCount"], 2);
    let key_scope = fixture
        .stats("?provider=p&model=GPT%25_&apiKeyId=%20consumer-a%20")
        .await;
    assert_eq!(key_scope["pagination"]["totalItems"], 1);
    assert_eq!(key_scope["availableProviders"], json!(["p"]));
    assert_eq!(key_scope["groups"][0]["windows"]["1h"]["medianTps"], 10.0);
    assert_eq!(key_scope["groups"][0]["windows"]["1h"]["sampleCount"], 1);
    assert_eq!(
        fixture.stats("?provider=P").await["pagination"]["totalItems"],
        0
    );
    assert_eq!(
        fixture.stats("?apiKeyId=consumer").await["pagination"]["totalItems"],
        0
    );
    assert_eq!(
        fixture.stats("?model=Gpt").await["pagination"]["totalItems"],
        0
    );
    let empty = fixture.stats("?provider=%20&model=%20&apiKeyId=%20").await;
    assert_eq!(empty["pagination"]["totalItems"], 4);
    assert_eq!(empty["availableProviders"], json!(["other-p", "p"]));
    // Status and date filters belong to raw logs; they cannot change TPS scope.
    let status_ignored = fixture
        .stats("?status=error&startDate=2099-01-01&endDate=2000-01-01")
        .await;
    assert_eq!(status_ignored["groups"], empty["groups"]);
    assert_eq!(
        status_ignored["availableProviders"],
        empty["availableProviders"]
    );
    let (status, raw) = fixture
        .request(
            "/api/request-logs?status=error&pageSize=1",
            Some(("x-api-key", TEST_KEY)),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(raw["pagination"]["totalItems"], 1);
    assert_eq!(raw["requests"][0]["requestId"], "f");
    assert_eq!(fixture.stats("").await["groups"], empty["groups"]);
}

#[tokio::test]
async fn nullable_groups_paging_defaults_empty_pages_and_huge_page_are_deterministic() {
    let fixture = Fixture::new().await;
    for (id, provider, model) in [
        ("nn", None, None),
        ("ne", None, Some("")),
        ("en", Some(""), None),
        ("ee", Some(""), Some("")),
        ("aa", Some("a"), Some("a")),
    ] {
        fixture.insert(id, provider, model, "consumer", "success", observation(0));
    }
    let all = fixture.stats("").await;
    assert_eq!(
        all["pagination"],
        json!({"page":1,"pageSize":20,"totalItems":5,"totalPages":1})
    );
    let second = fixture.stats("?page=2&pageSize=2").await;
    assert_eq!(
        second["pagination"],
        json!({"page":2,"pageSize":2,"totalItems":5,"totalPages":3})
    );
    assert_eq!(
        second["groups"],
        json!([all["groups"][2], all["groups"][3]])
    );
    assert_eq!(all["groups"][0]["provider"], Value::Null);
    assert_eq!(all["groups"][1]["model"], "");
    assert_eq!(all["groups"][2]["provider"], "");
    assert_eq!(all["groups"][2]["model"], Value::Null);
    assert_eq!(all["availableProviders"], json!([null, "", "a"]));
    assert_eq!(second["availableProviders"], all["availableProviders"]);
    let huge = fixture
        .stats(&format!("?page={}&pageSize=100", usize::MAX))
        .await;
    assert_eq!(huge["groups"], json!([]));
    assert_eq!(huge["pagination"]["totalItems"], 5);
    assert_eq!(huge["availableProviders"], all["availableProviders"]);
    assert_eq!(huge["pagination"]["page"].as_u64(), Some(usize::MAX as u64));
    let missing = fixture.stats("?provider=missing").await;
    assert_eq!(missing["groups"], json!([]));
    assert_eq!(missing["pagination"]["totalPages"], 0);
    assert_eq!(missing["availableProviders"], json!([]));
    for query in ["?page=0", "?pageSize=0", "?pageSize=101"] {
        assert_eq!(
            fixture
                .request(
                    &format!("/api/request-logs/tps-stats{query}"),
                    Some(("x-api-key", TEST_KEY))
                )
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn priority_window_paging_and_exclusions_keep_complete_historical_provider_options() {
    let fixture = Fixture::new().await;
    for (id, provider, model, data) in [
        ("null-unknown", None, "unknown", json!({})),
        ("empty-day", Some(""), "day-only", observation(7)),
        ("a-unknown", Some("a"), "unknown", json!({})),
        ("a-zero", Some("a"), "zero", observation(0)),
        ("z-a", Some("z"), "a", observation(1)),
        ("z-b", Some("z"), "b", observation(100)),
        ("a-six", Some("a"), "six-slow", observation(5)),
        ("z-six", Some("z"), "six-fast", observation(1000)),
        ("z-day", Some("z"), "day-fast", observation(2000)),
        ("z-unknown", Some("z"), "unknown", json!({})),
    ] {
        fixture.insert(id, provider, Some(model), "consumer", "success", data);
    }
    fixture.age("empty-day", 8);
    fixture.age("a-six", 3);
    fixture.age("z-six", 3);
    fixture.age("z-day", 8);
    for (id, provider, model) in [
        ("a-six-unknown", "a", "six-slow"),
        ("z-six-unknown", "z", "six-fast"),
        ("z-day-unknown", "z", "day-fast"),
        ("empty-day-unknown", "", "day-only"),
    ] {
        fixture.insert(
            id,
            Some(provider),
            Some(model),
            "consumer",
            "success",
            json!({"outputTokens":999,"durationMs":1}),
        );
    }
    fixture.insert(
        "old",
        Some("old"),
        Some("m"),
        "consumer",
        "success",
        observation(1),
    );
    fixture.age("old", 25);
    fixture.insert(
        "failed",
        Some("failed-only"),
        Some("m"),
        "consumer",
        "error",
        observation(1),
    );

    let expected = [
        json!(["z", "b"]),
        json!(["z", "a"]),
        json!(["a", "zero"]),
        json!(["z", "six-fast"]),
        json!(["a", "six-slow"]),
        json!(["z", "day-fast"]),
        json!(["", "day-only"]),
        json!([null, "unknown"]),
        json!(["a", "unknown"]),
        json!(["z", "unknown"]),
    ];
    for page in 1..=6 {
        let payload = fixture.stats(&format!("?page={page}&pageSize=2")).await;
        let offset = ((page - 1) * 2).min(expected.len());
        assert_eq!(
            identities(&payload),
            expected[offset..(offset + 2).min(expected.len())]
        );
        assert_eq!(
            payload["pagination"],
            json!({"page":page,"pageSize":2,"totalItems":10,"totalPages":5})
        );
        assert_eq!(payload["availableProviders"], json!([null, "", "a", "z"]));
    }
    let all = fixture.stats("").await;
    let day = group(&all, Some(""), Some("day-only"));
    assert_eq!(day["windows"]["1h"]["sampleCount"], 0);
    assert_eq!(day["windows"]["6h"]["sampleCount"], 0);
    assert_eq!(day["windows"]["24h"]["sampleCount"], 1);
    assert_eq!(day["windows"]["1h"]["successfulAttemptCount"], 1);
    assert_eq!(day["windows"]["24h"]["successfulAttemptCount"], 2);
    assert_eq!(
        group(&all, Some("a"), Some("zero"))["windows"]["24h"]["medianTps"],
        0.0
    );
    let query = hidden_query(json!(["z", "", "z"]));
    let first = fixture.stats(&format!("{query}&pageSize=2")).await;
    assert_eq!(
        identities(&first),
        vec![json!(["a", "zero"]), json!(["a", "six-slow"])]
    );
    assert_eq!(
        first["pagination"],
        json!({"page":1,"pageSize":2,"totalItems":4,"totalPages":2})
    );
    let second = fixture.stats(&format!("{query}&pageSize=2&page=2")).await;
    assert_eq!(
        identities(&second),
        vec![json!([null, "unknown"]), json!(["a", "unknown"])]
    );
    assert_eq!(second["availableProviders"], json!([null, "", "a", "z"]));
    let out_of_range = fixture.stats(&format!("{query}&pageSize=2&page=3")).await;
    assert_eq!(out_of_range["groups"], json!([]));
    assert_eq!(
        out_of_range["availableProviders"],
        first["availableProviders"]
    );
    let all_hidden = fixture
        .stats(&hidden_query(json!([null, "", "a", "z"])))
        .await;
    assert_eq!(all_hidden["groups"], json!([]));
    assert_eq!(
        all_hidden["pagination"],
        json!({"page":1,"pageSize":20,"totalItems":0,"totalPages":0})
    );
    assert_eq!(
        all_hidden["availableProviders"],
        first["availableProviders"]
    );
    let model_scope = fixture
        .stats(&format!("{query}&model=unknown&apiKeyId=consumer"))
        .await;
    assert_eq!(
        identities(&model_scope),
        vec![json!([null, "unknown"]), json!(["a", "unknown"])]
    );
    assert_eq!(model_scope["availableProviders"], json!([null, "a", "z"]));
    let provider_scope = fixture.stats(&format!("{query}&provider=z")).await;
    assert_eq!(provider_scope["groups"], json!([]));
    assert_eq!(provider_scope["availableProviders"], json!(["z"]));
}

#[tokio::test]
async fn exact_even_fractional_medians_and_first_window_ties_rank_across_pages() {
    let fixture = Fixture::new().await;
    for (id, provider, tokens, micros) in [
        ("fraction-a", "a", 7, 3_000_000),
        ("fraction-z-1", "z", 7, 3_000_000),
        ("fraction-z-2", "z", 1167, 500_000_000),
    ] {
        let mut data = observation(tokens);
        data["upstreamTps"]["elapsedMicros"] = json!(micros);
        fixture.insert(
            id,
            Some(provider),
            Some("fractional"),
            "consumer",
            "success",
            data,
        );
    }
    for (model, hours, lower_hours) in [("one", 0, Some(3)), ("six", 3, Some(8)), ("day", 8, None)]
    {
        for (provider, lower_rate) in [("tie-a", 0), ("tie-z", 1000)] {
            let id = format!("{provider}-{model}");
            fixture.insert(
                &id,
                Some(provider),
                Some(model),
                "consumer",
                "success",
                observation(1),
            );
            if hours != 0 {
                fixture.age(&id, hours);
            }
            if let Some(lower_hours) = lower_hours {
                let lower_id = format!("{id}-lower");
                fixture.insert(
                    &lower_id,
                    Some(provider),
                    Some(model),
                    "consumer",
                    "success",
                    observation(lower_rate),
                );
                fixture.age(&lower_id, lower_hours);
            }
        }
    }
    let expected = [
        json!(["z", "fractional"]),
        json!(["a", "fractional"]),
        json!(["tie-a", "one"]),
        json!(["tie-z", "one"]),
        json!(["tie-a", "six"]),
        json!(["tie-z", "six"]),
        json!(["tie-a", "day"]),
        json!(["tie-z", "day"]),
    ];
    for page in 1..=5 {
        let payload = fixture.stats(&format!("?page={page}&pageSize=2")).await;
        let offset = ((page - 1) * 2).min(expected.len());
        assert_eq!(
            identities(&payload),
            expected[offset..(offset + 2).min(expected.len())]
        );
        assert_eq!(payload["pagination"]["totalItems"], 8);
        assert_eq!(
            payload["availableProviders"],
            json!(["a", "tie-a", "tie-z", "z"])
        );
    }
    let all = fixture.stats("").await;
    let low = group(&all, Some("a"), Some("fractional"))["windows"]["1h"]["medianTps"]
        .as_f64()
        .unwrap();
    let high = group(&all, Some("z"), Some("fractional"))["windows"]["1h"]["medianTps"]
        .as_f64()
        .unwrap();
    assert_eq!(low, 7.0 / 3.0);
    assert_eq!(high, (7.0 / 3.0 + 2.334) / 2.0);
    assert!(high > low);
    assert_eq!(format!("{low:.2}"), format!("{high:.2}"));
    assert_eq!(
        group(&all, Some("z"), Some("fractional"))["windows"]["1h"]["sampleCount"],
        2
    );
    for (model, lower_window) in [("one", "6h"), ("six", "24h")] {
        assert!(
            group(&all, Some("tie-a"), Some(model))["windows"][lower_window]["medianTps"]
                .as_f64()
                .unwrap()
                < group(&all, Some("tie-z"), Some(model))["windows"][lower_window]["medianTps"]
                    .as_f64()
                    .unwrap()
        );
    }
}

#[tokio::test]
async fn exact_search_model_is_omitted_for_every_provider_without_changing_rows_or_raw_logs() {
    let fixture = Fixture::new().await;
    for (id, provider) in [
        ("search-null", None),
        ("search-empty", Some("")),
        ("search-only", Some("search-only")),
        ("search-mixed", Some("mixed")),
    ] {
        let mut data = observation(99);
        if id == "search-only" {
            data["irrelevant"] = json!("x".repeat(4 * 128 * 1024));
        }
        fixture.insert(
            id,
            provider,
            Some("standalone-web-search"),
            "consumer",
            "success",
            data,
        );
    }
    for (id, provider, model, rate) in [
        ("case", "case", "Standalone-web-search", 4),
        ("prefix", "prefix", "vendor/standalone-web-search", 3),
        ("suffix", "suffix", "standalone-web-search-extra", 2),
        ("substring", "substring", "xstandalone-web-searchx", 1),
        ("generation", "mixed", "m", 0),
    ] {
        fixture.insert(
            id,
            Some(provider),
            Some(model),
            "consumer",
            "success",
            observation(rate),
        );
    }
    let snapshot = || {
        fixture
            .state
            .db
            .sqlite
            .with_conn(|conn| {
                let mut rows = conn.prepare("SELECT id, data FROM requestDetails ORDER BY id")?;
                let rows = rows
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((
                    rows,
                    request_repo::list(conn, &Default::default(), 100, 0)?,
                    conn.total_changes(),
                ))
            })
            .unwrap()
    };
    let before = snapshot();
    let all = fixture.stats("").await;
    let expected = [
        json!(["case", "Standalone-web-search"]),
        json!(["prefix", "vendor/standalone-web-search"]),
        json!(["suffix", "standalone-web-search-extra"]),
        json!(["substring", "xstandalone-web-searchx"]),
        json!(["mixed", "m"]),
    ];
    assert_eq!(identities(&all), expected);
    assert_eq!(
        all["availableProviders"],
        json!(["case", "mixed", "prefix", "substring", "suffix"])
    );
    assert_eq!(all["pagination"]["totalItems"], 5);
    for page in 1..=4 {
        let payload = fixture.stats(&format!("?page={page}&pageSize=2")).await;
        let offset = ((page - 1) * 2).min(expected.len());
        assert_eq!(
            identities(&payload),
            expected[offset..(offset + 2).min(expected.len())]
        );
        assert_eq!(payload["pagination"]["totalItems"], 5);
        assert_eq!(payload["availableProviders"], all["availableProviders"]);
    }
    let search_scope = fixture
        .stats("?model=standalone-web-search&apiKeyId=consumer")
        .await;
    assert_eq!(search_scope["pagination"]["totalItems"], 3);
    assert_eq!(
        search_scope["availableProviders"],
        json!(["prefix", "substring", "suffix"])
    );
    let excluded = fixture.stats(&hidden_query(json!(["case", "mixed"]))).await;
    assert_eq!(excluded["pagination"]["totalItems"], 3);
    assert_eq!(excluded["availableProviders"], all["availableProviders"]);
    let only_search = fixture.stats("?provider=search-only").await;
    assert_eq!(only_search["groups"], json!([]));
    assert_eq!(only_search["availableProviders"], json!([]));
    let (status, raw) = fixture
        .request(
            "/api/request-logs?pageSize=100",
            Some(("x-api-key", TEST_KEY)),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(raw["pagination"]["totalItems"], 9);
    for id in ["search-null", "search-empty", "search-only", "search-mixed"] {
        let row = raw["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["requestId"] == id)
            .unwrap();
        assert_eq!(row["model"], "standalone-web-search");
        assert_eq!(row["tokensPerSecond"], 99.0);
        assert_eq!(row["generatedOutputTokens"], 99);
        assert_eq!(row["upstreamDurationMs"], 1000.0);
    }
    assert_eq!(snapshot(), before);
}

#[tokio::test]
async fn exclusions_are_exact_nullable_untrimmed_ids_and_options_follow_key_and_model_scope() {
    let fixture = Fixture::new().await;
    for (id, provider) in [
        ("null", None),
        ("empty", Some("")),
        ("literal", Some("null")),
        ("spaced", Some(" p ")),
        ("plain", Some("p")),
    ] {
        fixture.insert(
            id,
            provider,
            Some("match"),
            "key-a",
            "success",
            observation(1),
        );
    }
    fixture.insert(
        "other-key",
        Some("only-key-b"),
        Some("match"),
        "key-b",
        "success",
        observation(1),
    );
    fixture.insert(
        "other-model",
        Some("other-model"),
        Some("elsewhere"),
        "key-a",
        "success",
        observation(1),
    );
    let scope = "&apiKeyId=key-a&model=match";
    let baseline = fixture.stats("?apiKeyId=key-a&model=match").await;
    assert_eq!(
        baseline["availableProviders"],
        json!([null, "", " p ", "null", "p"])
    );
    for (hidden, remaining) in [
        (json!([]), json!([null, "", " p ", "null", "p"])),
        (json!(["unmatched"]), json!([null, "", " p ", "null", "p"])),
        (json!([null]), json!(["", " p ", "null", "p"])),
        (json!([""]), json!([null, " p ", "null", "p"])),
        (json!(["null"]), json!([null, "", " p ", "p"])),
        (json!([" p ", " p "]), json!([null, "", "null", "p"])),
        (json!(["p"]), json!([null, "", " p ", "null"])),
    ] {
        let payload = fixture
            .stats(&format!("{}{scope}", hidden_query(hidden)))
            .await;
        let providers: Vec<_> = payload["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|group| group["provider"].clone())
            .collect();
        assert_eq!(json!(providers), remaining);
        assert_eq!(payload["pagination"]["totalItems"], providers.len());
        assert_eq!(
            payload["availableProviders"],
            baseline["availableProviders"]
        );
    }
    let key_b = fixture
        .stats(&format!(
            "{}&apiKeyId=key-b&model=match",
            hidden_query(json!(["only-key-b"]))
        ))
        .await;
    assert_eq!(key_b["groups"], json!([]));
    assert_eq!(key_b["availableProviders"], json!(["only-key-b"]));
    let no_match = fixture
        .stats(&format!(
            "{}&apiKeyId=key-a&model=missing",
            hidden_query(json!([null]))
        ))
        .await;
    assert_eq!(no_match["availableProviders"], json!([]));
}

#[tokio::test]
async fn invalid_exclusions_are_generic_json_400_and_limits_count_utf8_bytes() {
    let fixture = Fixture::new().await;
    let invalid = [
        "".to_owned(),
        "[".to_owned(),
        "[null,]".to_owned(),
        "null".to_owned(),
        "{}".to_owned(),
        "true".to_owned(),
        "\"private-invalid\"".to_owned(),
        "[123]".to_owned(),
        "[true]".to_owned(),
        "[[null]]".to_owned(),
        "[{\"private-invalid\":1}]".to_owned(),
        json!(["x".repeat(4 * 1024 + 1)]).to_string(),
        json!(["é".repeat(2049)]).to_string(),
    ];
    let mut expected = None;
    for raw in invalid {
        let (status, payload) = fixture
            .request(
                &format!(
                    "/api/request-logs/tps-stats?hiddenProviders={}",
                    urlencoding::encode(&raw)
                ),
                Some(("x-api-key", TEST_KEY)),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_keys(&payload, &["error"]);
        assert!(payload["error"].is_string());
        assert!(!payload.to_string().contains("private-invalid"));
        if let Some(expected) = &expected {
            assert_eq!(&payload, expected);
        } else {
            expected = Some(payload);
        }
    }
    for raw in [
        json!(["x".repeat(4 * 1024)]).to_string(),
        json!(["é".repeat(2048)]).to_string(),
    ] {
        let payload = fixture
            .stats(&format!("?hiddenProviders={}", urlencoding::encode(&raw)))
            .await;
        assert_eq!(payload["groups"], json!([]));
        assert_eq!(payload["availableProviders"], json!([]));
    }
}

#[tokio::test]
async fn projection_is_strict_metadata_whitelist_and_reader_changes_no_rows_or_schema() {
    let fixture = Fixture::new().await;
    fixture.insert(
        "observed",
        Some("p"),
        Some("m"),
        "consumer",
        "success",
        observation(0),
    );
    fixture.insert(
        "legacy",
        Some("p"),
        Some("m"),
        "consumer",
        "success",
        json!({"outputTokens":999,"durationMs":1,"arbitrary":"private-data"}),
    );
    fixture.insert(
        "malformed",
        Some("p"),
        Some("m"),
        "consumer",
        "success",
        json!(null),
    );
    fixture
        .state
        .db
        .sqlite
        .with_conn(|conn| {
            conn.execute(
                "UPDATE requestDetails SET data='{broken' WHERE id='malformed'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let snapshot = || {
        fixture
            .state
            .db
            .sqlite
            .with_conn(|conn| {
                let rows = request_repo::list(conn, &Default::default(), 100, 0)?;
                let mut schema =
                    conn.prepare("SELECT name, sql FROM sqlite_master ORDER BY name")?;
                let schema = schema
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let malformed: String = conn.query_row(
                    "SELECT data FROM requestDetails WHERE id='malformed'",
                    [],
                    |row| row.get(0),
                )?;
                Ok((rows, schema, malformed, conn.total_changes()))
            })
            .unwrap()
    };
    let before = snapshot();
    let payload = fixture.stats("").await;
    assert_eq!(snapshot(), before);
    let excluded = fixture.stats(&hidden_query(json!(["p"]))).await;
    assert_eq!(excluded["groups"], json!([]));
    assert_eq!(excluded["availableProviders"], json!(["p"]));
    assert_eq!(snapshot(), before);
    assert_keys(
        &payload,
        &[
            "asOf",
            "windows",
            "groups",
            "pagination",
            "availableProviders",
        ],
    );
    assert_eq!(payload["availableProviders"], json!(["p"]));
    assert_keys(&payload["windows"], &["1h", "6h", "24h"]);
    assert_keys(
        &payload["pagination"],
        &["page", "pageSize", "totalItems", "totalPages"],
    );
    assert_keys(&payload["groups"][0], &["provider", "model", "windows"]);
    assert_keys(&payload["groups"][0]["windows"], &["1h", "6h", "24h"]);
    for window in ["1h", "6h", "24h"] {
        assert_keys(&payload["windows"][window], &["start", "end"]);
        assert_eq!(payload["windows"][window]["end"], payload["asOf"]);
        let cell = &payload["groups"][0]["windows"][window];
        assert_keys(
            cell,
            &["medianTps", "sampleCount", "successfulAttemptCount"],
        );
        assert_eq!(cell["medianTps"], 0.0);
        assert_eq!(cell["sampleCount"], 1);
        assert_eq!(cell["successfulAttemptCount"], 3);
    }
    let text = payload.to_string();
    for forbidden in [
        "private",
        TEST_KEY,
        "consumer",
        "connectionId",
        "apiKeyId",
        "apiKeyName",
        "correlationId",
        "upstreamTps",
        "chatSession",
        "requestId",
        "outputTokens",
        "durationMs",
        "arbitrary",
    ] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
}

#[tokio::test]
async fn configured_auth_matches_logs_and_busy_permit_is_immediate_without_blocking_raw_logs() {
    let fixture = Fixture::new().await;
    let cookie = format!(
        "auth_token={}",
        encode(
            &Header::default(),
            &DashboardClaims {
                authenticated: true,
                exp: (Utc::now().timestamp() + 3600) as usize,
                jti: None
            },
            &EncodingKey::from_secret(jwt_secret().as_bytes()),
        )
        .unwrap()
    );
    for header in [
        None,
        Some(("cookie", "auth_token=invalid")),
        Some(("x-api-key", "invalid")),
        Some(("x-api-key", "inactive-tps-test-key")),
    ] {
        for uri in ["/api/request-logs", "/api/request-logs/tps-stats"] {
            assert_eq!(
                fixture.request(uri, header).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
    }
    for header in [
        Some(("x-api-key", TEST_KEY)),
        Some(("cookie", cookie.as_str())),
    ] {
        assert_eq!(
            fixture
                .request("/api/request-logs/tps-stats", header)
                .await
                .0,
            StatusCode::OK
        );
    }
    let permit = fixture
        .state
        .request_tps_stats_reader
        .clone()
        .try_acquire_owned()
        .unwrap();
    let (status, payload) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        fixture.request("/api/request-logs/tps-stats", Some(("x-api-key", TEST_KEY))),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        payload,
        json!({"error":{"code":"tps_stats_unavailable","message":"TPS statistics are temporarily unavailable"}})
    );
    assert_eq!(
        fixture.request("/api/request-logs/tps-stats", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture
            .request("/api/request-logs", Some(("x-api-key", TEST_KEY)))
            .await
            .0,
        StatusCode::OK
    );
    drop(permit);
    fixture
        .state
        .db
        .update(|state| state.settings.require_login = false)
        .await
        .unwrap();
    for uri in ["/api/request-logs", "/api/request-logs/tps-stats"] {
        assert_eq!(fixture.request(uri, None).await.0, StatusCode::OK);
    }
}

#[tokio::test]
async fn oversized_relevant_data_and_reader_errors_have_only_generic_unavailable_response() {
    let fixture = Fixture::new().await;
    fixture.insert(
        "oversized",
        Some("p"),
        Some("m"),
        "consumer",
        "success",
        json!({"private": "x".repeat(128 * 1024)}),
    );
    let expected = json!({"error":{"code":"tps_stats_unavailable","message":"TPS statistics are temporarily unavailable"}});
    let (status, payload) = fixture
        .request("/api/request-logs/tps-stats", Some(("x-api-key", TEST_KEY)))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(payload, expected);
    let (status, payload) = fixture
        .request(
            &format!("/api/request-logs/tps-stats{}", hidden_query(json!(["p"]))),
            Some(("x-api-key", TEST_KEY)),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(payload, expected);
    assert_eq!(fixture.stats("?provider=other").await["groups"], json!([]));
    fixture
        .state
        .db
        .sqlite
        .with_conn(|conn| conn.execute_batch("DROP INDEX idx_rd_ts"))
        .unwrap();
    let (status, payload) = fixture
        .request("/api/request-logs/tps-stats", Some(("x-api-key", TEST_KEY)))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(payload, expected);
}
