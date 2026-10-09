use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::sqlite::repo::request_repo::{self, NewRequestDetail};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "request-logs-test-key";

#[tokio::test]
async fn request_logs_return_metadata_and_filter_results() {
    let temp = tempdir().unwrap();
    let db = Arc::new(Db::load_from(temp.path()).await.unwrap());
    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "test-key-id".into(),
            name: "test".into(),
            key: TEST_KEY.into(),
            is_active: Some(true),
            ..Default::default()
        }];
        state.settings.require_login = false;
    })
    .await
    .unwrap();
    db.sqlite
        .with_conn(|conn| {
            request_repo::insert(
                conn,
                &NewRequestDetail {
                    id: "request-1",
                    timestamp: "2026-09-15T12:00:00Z",
                    provider: Some("openai"),
                    model: Some("gpt-5"),
                    connection_id: Some("private-connection"),
                    status: "success",
                    api_key_id: Some("consumer-key-id"),
                    api_key_name: Some("OpenCode"),
                    correlation_id: Some("private-correlation"),
                    data: &json!({
                        "route": "work",
                        "statusCode": 200,
                        "durationMs": 42,
                        "inputTokens": 10,
                        "outputTokens": 20,
                        "codexCache": {"accountHmac": "private-fingerprint"},
                        "chatSession": {"version": 1, "hmac": "private-chat-fingerprint", "source": "x-session-id"},
                        "upstreamTps": {"version": 1, "generatedOutputTokens": 7, "elapsedMicros": 250000, "endKind": "protocol_terminal", "privateExtra": "secret"},
                        "request": "secret prompt",
                        "response": "secret response",
                        "errorCode": "invalid_request_error",
                        "errorMessage": "Bad request Authorization: Bearer diagnostic-secret"
                    }),
                },
            )
            .and_then(|_| {
                request_repo::insert(
                    conn,
                    &NewRequestDetail {
                        id: "request-2",
                        timestamp: "2026-09-15T11:00:00Z",
                        provider: Some("openai"),
                        model: Some("claude-3-5-sonnet"),
                        connection_id: Some("private-connection"),
                        status: "error",
                        api_key_id: Some("other-key-id"),
                        api_key_name: Some("Other"),
                        correlation_id: Some("private-correlation"),
                        data: &json!({
                            "route": "work",
                            "statusCode": 500,
                            "durationMs": 42
                        }),
                    },
                )
            })
        })
        .unwrap();

    let response = openproxy::build_app(AppState::new(db.clone()))
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/request-logs?page=1&pageSize=20")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["requests"][0]["requestId"], "request-1");
    assert_eq!(payload["requests"][0]["route"], "work");
    assert_eq!(payload["requests"][0]["inputTokens"], 10);
    assert_eq!(payload["requests"][0]["apiKeyId"], "consumer-key-id");
    assert!(!payload.to_string().contains("codexCache"));
    assert!(!payload.to_string().contains("private-fingerprint"));
    assert_eq!(payload["requests"][0]["apiKeyName"], "OpenCode");
    assert_eq!(payload["requests"][0]["errorCode"], "invalid_request_error");
    assert_eq!(
        payload["requests"][0]["errorMessage"],
        "Bad request Authorization: [REDACTED]"
    );
    assert_eq!(payload["requests"][0]["tokensPerSecond"], 28.0);
    assert_eq!(payload["requests"][0]["generatedOutputTokens"], 7);
    assert_eq!(payload["requests"][0]["upstreamDurationMs"], 250.0);
    assert!(payload["requests"][1]["tokensPerSecond"].is_null());
    for forbidden in [
        "chatSession",
        "hmac",
        "source",
        "correlationId",
        "connectionId",
        "upstreamTps",
        "elapsedMicros",
        "privateExtra",
    ] {
        assert!(!payload.to_string().contains(forbidden));
    }
    let serialized = String::from_utf8_lossy(&body);
    assert!(!serialized.contains(TEST_KEY));
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains("diagnostic-secret"));
    assert!(!serialized.contains("private"));

    let response = openproxy::build_app(AppState::new(db.clone()))
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/request-logs?page=1&pageSize=20&apiKeyId=consumer-key-id")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["pagination"]["totalItems"], 1);
    assert_eq!(payload["requests"][0]["requestId"], "request-1");

    let response = openproxy::build_app(AppState::new(db))
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/request-logs?page=1&pageSize=20&model=gpt")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["pagination"]["totalItems"], 1);
    assert_eq!(payload["requests"].as_array().unwrap().len(), 1);
    assert_eq!(payload["requests"][0]["requestId"], "request-1");
    assert_eq!(payload["requests"][0]["model"], "gpt-5");
}

#[tokio::test]
async fn request_logs_tps_requires_valid_final_inputs_and_success_preserving_zero() {
    let temp = tempdir().unwrap();
    let db = Arc::new(Db::load_from(temp.path()).await.unwrap());
    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "management-key".into(),
            name: "test".into(),
            key: TEST_KEY.into(),
            is_active: Some(true),
            ..Default::default()
        }];
        state.settings.require_login = false;
    })
    .await
    .unwrap();
    let valid = json!({"version": 1, "generatedOutputTokens": 3, "elapsedMicros": 1500000, "endKind": "json_body"});
    let mut cases = vec![(
        "fraction",
        "success",
        valid.clone(),
        Some((3u64, 1500.0, 2.0)),
    )];
    cases.push((
        "sub-ms",
        "success",
        json!({"version":1,"generatedOutputTokens":1,"elapsedMicros":125,"endKind":"json_body"}),
        Some((1, 0.125, 8000.0)),
    ));
    cases.push(("fractional-tps", "success", json!({"version":1,"generatedOutputTokens":7,"elapsedMicros":3000000,"endKind":"protocol_terminal"}), Some((7, 3000.0, 7.0 / 3.0))));
    for kind in ["protocol_terminal", "json_body", "clean_eof"] {
        let mut observation = valid.clone();
        observation["endKind"] = json!(kind);
        observation["generatedOutputTokens"] = json!(0);
        cases.push((kind, "success", observation, Some((0, 1500.0, 0.0))));
    }
    cases.push(("max-u64", "success", json!({"version":1,"generatedOutputTokens":u64::MAX,"elapsedMicros":1000000,"endKind":"clean_eof"}), Some((u64::MAX, 1000.0, u64::MAX as f64))));
    for (name, field, value) in [
        ("version", "version", json!(2)),
        ("version-string", "version", json!("1")),
        ("version-float", "version", json!(1.0)),
        ("end-kind", "endKind", json!("provisional")),
        ("elapsed-zero", "elapsedMicros", json!(0)),
        ("elapsed-negative", "elapsedMicros", json!(-1)),
        ("elapsed-float", "elapsedMicros", json!(1.5)),
        ("tokens-null", "generatedOutputTokens", json!(null)),
        ("tokens-negative", "generatedOutputTokens", json!(-1)),
        ("tokens-float", "generatedOutputTokens", json!(1.5)),
        ("tokens-integral-float", "generatedOutputTokens", json!(1.0)),
        ("tokens-string", "generatedOutputTokens", json!("3")),
    ] {
        let mut observation = valid.clone();
        observation[field] = value;
        cases.push((name, "success", observation, None));
    }
    for field in [
        "version",
        "endKind",
        "elapsedMicros",
        "generatedOutputTokens",
    ] {
        let mut observation = valid.clone();
        observation.as_object_mut().unwrap().remove(field);
        cases.push((field, "success", observation, None));
    }
    for status in ["pending", "error", "interrupted"] {
        cases.push((status, status, valid.clone(), None));
    }
    cases.push(("legacy", "success", json!(null), None));
    db.sqlite.with_conn(|conn| {
        for (index, (_, status, observation, _)) in cases.iter().enumerate() {
            request_repo::insert(conn, &NewRequestDetail {
                id: &format!("case-{index}"), timestamp: "2026-10-03T12:00:00Z", provider: Some("test"), model: Some("test"), connection_id: Some("private-connection"), status, api_key_id: Some("consumer"), api_key_name: Some("client"), correlation_id: Some("private-correlation"),
                data: &json!({"durationMs": 1, "outputTokens": 999, "upstreamTps": observation, "chatSession": {"hmac": "private-session"}, "arbitrary": "private-data"}),
            })?;
        }
        Ok(())
    }).unwrap();
    let response = openproxy::build_app(AppState::new(db))
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?pageSize=100")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["requests"].as_array().unwrap().len(), cases.len());
    for (index, (name, _, _, expected)) in cases.iter().enumerate() {
        let row = payload["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["requestId"] == format!("case-{index}"))
            .unwrap();
        if let Some((tokens, ms, tps)) = expected {
            assert_eq!(
                row["generatedOutputTokens"].as_u64(),
                Some(*tokens),
                "{name}"
            );
            assert_eq!(row["upstreamDurationMs"].as_f64(), Some(*ms), "{name}");
            assert_eq!(row["tokensPerSecond"].as_f64(), Some(*tps), "{name}");
        } else {
            for field in [
                "generatedOutputTokens",
                "upstreamDurationMs",
                "tokensPerSecond",
            ] {
                assert!(row[field].is_null(), "{name}: {field}");
            }
        }
        let allowed = [
            "requestId",
            "timestamp",
            "route",
            "provider",
            "model",
            "status",
            "statusCode",
            "errorCode",
            "errorMessage",
            "durationMs",
            "inputTokens",
            "outputTokens",
            "cachedTokens",
            "apiKeyId",
            "apiKeyName",
            "tokensPerSecond",
            "generatedOutputTokens",
            "upstreamDurationMs",
        ];
        assert_eq!(row.as_object().unwrap().len(), allowed.len() - 2);
        assert!(row.get("errorCode").is_none());
        assert!(row.get("errorMessage").is_none());
        assert!(row
            .as_object()
            .unwrap()
            .keys()
            .all(|key| allowed.contains(&key.as_str())));
    }
    assert!(!payload.to_string().contains("private"));
}
