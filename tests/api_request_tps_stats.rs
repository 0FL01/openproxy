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
    assert_eq!(scope["groups"][0]["model"], "GPT%_A");
    assert_eq!(scope["groups"][0]["windows"]["1h"]["medianTps"], 20.0);
    assert_eq!(
        scope["groups"][0]["windows"]["1h"]["successfulAttemptCount"],
        2
    );
    assert_eq!(scope["groups"][1]["model"], "gpt%_a");
    let key_scope = fixture
        .stats("?provider=p&model=GPT%25_&apiKeyId=%20consumer-a%20")
        .await;
    assert_eq!(key_scope["pagination"]["totalItems"], 1);
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
    // Status and date filters belong to raw logs; they cannot change TPS scope.
    let status_ignored = fixture
        .stats("?status=error&startDate=2099-01-01&endDate=2000-01-01")
        .await;
    assert_eq!(status_ignored["groups"], empty["groups"]);
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
    let huge = fixture
        .stats(&format!("?page={}&pageSize=100", usize::MAX))
        .await;
    assert_eq!(huge["groups"], json!([]));
    assert_eq!(huge["pagination"]["totalItems"], 5);
    assert_eq!(huge["pagination"]["page"].as_u64(), Some(usize::MAX as u64));
    let missing = fixture.stats("?provider=missing").await;
    assert_eq!(missing["groups"], json!([]));
    assert_eq!(missing["pagination"]["totalPages"], 0);
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
    assert_keys(&payload, &["asOf", "windows", "groups", "pagination"]);
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
