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
async fn request_logs_return_only_structured_metadata() {
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
                    api_key_id: Some("private-key"),
                    api_key_name: Some("private-name"),
                    correlation_id: Some("private-correlation"),
                    data: &json!({
                        "route": "work",
                        "statusCode": 200,
                        "durationMs": 42,
                        "inputTokens": 10,
                        "outputTokens": 20,
                        "request": "secret prompt",
                        "response": "secret response"
                    }),
                },
            )
        })
        .unwrap();

    let response = openproxy::build_app(AppState::new(db))
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
    let serialized = String::from_utf8_lossy(&body);
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains("private"));
}
