mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use futures_util::StreamExt;
use once_cell::sync::Lazy;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;
use tokio::sync::{Mutex, Notify};
use tower::util::ServiceExt;

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

fn compatible_node(id: &str, base_url: String) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: id.into(),
        prefix: Some(id.into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

fn connection(id: &str, provider: &str, priority: u32) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        priority: Some(priority),
        api_key: Some(format!("key-{id}")),
        default_model: Some("gpt-c13".into()),
        ..Default::default()
    }
}

async fn c13_app(
    provider: &str,
    base_url: String,
    connections: Vec<ProviderConnection>,
) -> (TempTestDb, axum::Router) {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![compatible_node(provider, base_url)];
            db.provider_connections = connections;
        })
        .await
        .expect("seed C13 database");
    let app = openproxy::build_app(AppState::new(test_db.db.clone()));
    (test_db, app)
}

async fn post_chat(app: &axum::Router, provider: &str, stream: bool) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": format!("{provider}/gpt-c13"),
                        "messages": [{"role": "user", "content": "bounded"}],
                        "stream": stream
                    })
                    .to_string(),
                ))
                .expect("C13 request"),
        )
        .await
        .expect("C13 response")
}

#[tokio::test]
async fn status_matrix_has_exact_account_bound_and_preserves_final_error() {
    let cases = [
        (StatusCode::BAD_REQUEST, 1usize),
        (StatusCode::UNAUTHORIZED, 3),
        (StatusCode::FORBIDDEN, 3),
        (StatusCode::TOO_MANY_REQUESTS, 3),
        (StatusCode::INTERNAL_SERVER_ERROR, 3),
        (StatusCode::BAD_GATEWAY, 3),
        (StatusCode::SERVICE_UNAVAILABLE, 3),
        (StatusCode::GATEWAY_TIMEOUT, 3),
    ];

    for (status, expected_attempts) in cases {
        let scripts = (0..3)
            .map(|index| {
                ScriptedResponse::json(
                    status,
                    format!(
                        "{{\"error\":{{\"message\":\"c13-{}-{index}\"}}}}",
                        status.as_u16()
                    ),
                )
                .with_header("retry-after", format!("{}", 20 + index))
            })
            .collect::<Vec<_>>();
        let upstream = MockUpstream::start(scripts).await;
        let provider = format!("c13-{}", status.as_u16());
        let connections = (1..=3)
            .map(|index| connection(&format!("account-{index}"), &provider, index))
            .collect();
        let (_db, app) = c13_app(&provider, upstream.url("/v1"), connections).await;

        let response = tokio::time::timeout(
            Duration::from_millis(750),
            post_chat(&app, &provider, false),
        )
        .await
        .expect("request-scoped planner must not sleep");
        assert_eq!(response.status(), status);
        let retry_after = response
            .headers()
            .get("retry-after")
            .unwrap()
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert!(
            (18 + expected_attempts..=19 + expected_attempts).contains(&retry_after),
            "unexpected final Retry-After for {status}: {retry_after}"
        );
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("C13 error body");
        assert!(
            String::from_utf8_lossy(&body).contains(&format!("-{}", expected_attempts - 1)),
            "final body did not come from the final permitted account: {}",
            String::from_utf8_lossy(&body)
        );
        assert_eq!(upstream.request_count().await, expected_attempts);
        let observed = upstream.requests().await;
        let auth = observed
            .iter()
            .map(|request| {
                request
                    .headers
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>();
        let expected = (1..=expected_attempts)
            .map(|index| format!("Bearer key-account-{index}"))
            .collect::<Vec<_>>();
        assert_eq!(auth, expected);
        upstream.shutdown().await;
    }
}

struct EnvGuard(&'static str);

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: this integration binary serializes access with ENV_LOCK.
        unsafe { std::env::remove_var(self.0) };
    }
}

#[tokio::test]
async fn one_planner_owned_auth_recovery_is_counted_and_persisted() {
    let _lock = ENV_LOCK.lock().await;
    let generation = MockUpstream::start([
        ScriptedResponse::json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"expired"}}"#,
        ),
        ScriptedResponse::json(
            StatusCode::OK,
            r#"{"id":"chatcmpl-c13","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
        ),
        ScriptedResponse::json(StatusCode::OK, r#"{"must":"not run"}"#),
    ])
    .await;
    let tokens = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        r#"{"access_token":"new-c13-access","refresh_token":"new-c13-refresh","expires_in":3600}"#,
    )])
    .await;
    // SAFETY: this integration binary serializes access with ENV_LOCK.
    unsafe { std::env::set_var("OPENPROXY_XAI_TOKEN_URL", tokens.url("/oauth/token")) };
    let _env = EnvGuard("OPENPROXY_XAI_TOKEN_URL");

    let mut account = connection("xai-c13", "xai", 1);
    account.auth_type = "oauth".into();
    account.api_key = None;
    account.access_token = Some("old-c13-access".into());
    account.refresh_token = Some("old-c13-refresh".into());
    let (test_db, app) = c13_app("xai", generation.url("/v1"), vec![account]).await;

    let response = post_chat(&app, "xai", false).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(generation.request_count().await, 2);
    assert_eq!(tokens.request_count().await, 1);
    let requests = generation.requests().await;
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer old-c13-access"
    );
    assert_eq!(
        requests[1].headers.get("authorization").unwrap(),
        "Bearer new-c13-access"
    );
    let saved = test_db
        .db
        .snapshot()
        .provider_connections
        .iter()
        .find(|connection| connection.id == "xai-c13")
        .cloned()
        .expect("saved C13 account");
    assert_eq!(saved.access_token.as_deref(), Some("new-c13-access"));
    assert_eq!(saved.refresh_token.as_deref(), Some("new-c13-refresh"));

    generation.shutdown().await;
    tokens.shutdown().await;
}

#[tokio::test]
async fn downstream_commit_or_cancellation_never_starts_another_account() {
    let first_delta =
        "data: {\"id\":\"chatcmpl-c13\",\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n";
    let hold_eof = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let upstream = MockUpstream::start([
        ScriptedResponse::sse([first_delta])
            .holding_eof(hold_eof)
            .notifying_on_body_drop(dropped.clone()),
        ScriptedResponse::json(StatusCode::OK, r#"{"must":"not run"}"#),
    ])
    .await;
    let provider = "c13-stream";
    let (_db, app) = c13_app(
        provider,
        upstream.url("/v1"),
        vec![
            connection("stream-a", provider, 1),
            connection("stream-b", provider, 2),
        ],
    )
    .await;

    let response = post_chat(&app, provider, true).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_millis(500), body.next())
        .await
        .expect("committed first chunk")
        .expect("first body item")
        .expect("valid first body item");
    assert!(String::from_utf8_lossy(&first).contains("first"));
    drop(body);
    tokio::time::timeout(Duration::from_secs(1), dropped.notified())
        .await
        .expect("cancellation drops committed upstream body");
    assert_eq!(upstream.request_count().await, 1);
    upstream.shutdown().await;
}

#[test]
fn source_has_one_recovery_owner_and_no_hidden_generation_multiplier() {
    let chat = include_str!("../src/server/api/chat.rs");
    let default = include_str!("../src/core/executor/default.rs");
    let mimo = include_str!("../src/core/executor/mimo_free.rs");
    let kiro = include_str!("../src/core/executor/kiro.rs");

    assert_eq!(chat.matches("dispatch_oauth_refresh(").count(), 1);
    assert!(chat.contains("GenerationAttemptBudget::new"));
    assert!(chat.contains("matches!(status.as_u16(), 400 | 422)"));
    for removed in [
        "try_refresh_credentials",
        "refresh_with_retry",
        "may_try_distinct_url",
        "for (url_index, url) in urls",
    ] {
        assert!(
            !default.contains(removed),
            "DefaultExecutor still owns {removed}"
        );
    }
    assert!(!mimo.contains("retry_response"));
    assert!(mimo.contains("this executor never repeats"));
    assert!(kiro.contains("execute_request_with_budget"));
    assert!(kiro.contains("GenerationAttemptBudget::try_acquire"));
    for forbidden in [
        "filter_available_accounts(",
        "is_account_unavailable(",
        "apply_error_state(",
        "tokio::time::sleep(",
    ] {
        assert!(
            !chat.contains(forbidden),
            "request planner restored forbidden policy: {forbidden}"
        );
    }
}
