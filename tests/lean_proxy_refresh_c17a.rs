mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use once_cell::sync::Lazy;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::json;
use tokio::sync::{Mutex, Notify};
use tower::util::ServiceExt;

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

struct EnvGuard(&'static str);

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: every test in this integration binary holds ENV_LOCK.
        unsafe { std::env::remove_var(self.0) };
    }
}

fn oauth_connection(id: &str, access: &str, refresh: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "xai".into(),
        auth_type: "oauth".into(),
        is_active: Some(true),
        priority: Some(1),
        access_token: Some(access.into()),
        refresh_token: Some(refresh.into()),
        default_model: Some("grok-c17a".into()),
        ..Default::default()
    }
}

fn compatible_node(base_url: String) -> ProviderNode {
    ProviderNode {
        id: "xai".into(),
        r#type: "openai-compatible".into(),
        name: "xai".into(),
        prefix: Some("xai".into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

async fn c17a_app(
    generation_base_url: String,
    connection: ProviderConnection,
) -> (TempTestDb, axum::Router) {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(move |db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![compatible_node(generation_base_url)];
            db.provider_connections = vec![connection];
        })
        .await
        .expect("seed C17A database");
    let app = openproxy::build_app(AppState::new(test_db.db.clone()));
    (test_db, app)
}

async fn post_chat(app: axum::Router) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer test-key")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "model": "xai/grok-c17a",
                    "messages": [{"role": "user", "content": "coordinate"}],
                    "stream": false
                })
                .to_string(),
            ))
            .expect("C17A request"),
    )
    .await
    .expect("C17A response")
}

fn success_body(index: usize) -> String {
    format!(
        r#"{{"id":"chatcmpl-c17a-{index}","object":"chat.completion","choices":[{{"index":0,"message":{{"role":"assistant","content":"ok"}},"finish_reason":"stop"}}]}}"#
    )
}

fn canonical_connection(test_db: &TempTestDb, id: &str) -> ProviderConnection {
    test_db
        .db
        .snapshot()
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
        .expect("canonical C17A connection")
}

#[tokio::test]
async fn parallel_foreground_401s_share_one_refresh_and_count_one_followup_each() {
    const CALLERS: usize = 16;
    let _env_lock = ENV_LOCK.lock().await;
    let release_refresh = Arc::new(Notify::new());

    let generation_scripts = (0..CALLERS)
        .map(|_| {
            ScriptedResponse::json(
                StatusCode::UNAUTHORIZED,
                r#"{"error":{"code":"invalid_token","message":"expired"}}"#,
            )
        })
        .chain(
            (0..CALLERS).map(|index| ScriptedResponse::json(StatusCode::OK, success_body(index))),
        )
        .collect::<Vec<_>>();
    let generation = MockUpstream::start(generation_scripts).await;
    let tokens = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        r#"{"access_token":"access-c17a-new","refresh_token":"refresh-c17a-new","expires_in":3600}"#,
    )
    .waiting_for(release_refresh.clone())])
    .await;
    // SAFETY: ENV_LOCK serializes process-environment access in this binary.
    unsafe { std::env::set_var("OPENPROXY_XAI_TOKEN_URL", tokens.url("/oauth/token")) };
    let _env = EnvGuard("OPENPROXY_XAI_TOKEN_URL");

    let (test_db, app) = c17a_app(
        generation.url("/v1"),
        oauth_connection("xai-c17a-parallel", "access-c17a-old", "refresh-c17a-old"),
    )
    .await;

    let mut callers = Vec::new();
    for _ in 0..CALLERS {
        callers.push(tokio::spawn(post_chat(app.clone())));
    }
    generation.wait_for_requests(CALLERS).await;
    tokens.wait_for_requests(1).await;
    assert_eq!(tokens.request_count().await, 1);
    release_refresh.notify_waiters();

    for caller in callers {
        let response = tokio::time::timeout(Duration::from_secs(3), caller)
            .await
            .expect("parallel foreground request completed")
            .expect("join foreground request");
        assert_eq!(response.status(), StatusCode::OK);
        let _ = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("read foreground response");
    }

    assert_eq!(tokens.request_count().await, 1);
    assert_eq!(generation.request_count().await, CALLERS * 2);
    let requests = generation.requests().await;
    let old = requests
        .iter()
        .filter(|request| {
            request
                .headers
                .get("authorization")
                .is_some_and(|value| value.to_str().ok() == Some("Bearer access-c17a-old"))
        })
        .count();
    let new = requests
        .iter()
        .filter(|request| {
            request
                .headers
                .get("authorization")
                .is_some_and(|value| value.to_str().ok() == Some("Bearer access-c17a-new"))
        })
        .count();
    assert_eq!((old, new), (CALLERS, CALLERS));
    let canonical = canonical_connection(&test_db, "xai-c17a-parallel");
    assert_eq!(canonical.access_token.as_deref(), Some("access-c17a-new"));
    assert_eq!(canonical.refresh_token.as_deref(), Some("refresh-c17a-new"));

    generation.shutdown().await;
    tokens.shutdown().await;
}

#[tokio::test]
async fn invalid_grant_is_once_per_request_and_non_token_403_does_not_refresh() {
    let _env_lock = ENV_LOCK.lock().await;

    let invalid_generation = MockUpstream::start([
        ScriptedResponse::json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"code":"invalid_token"}}"#,
        ),
        ScriptedResponse::json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"code":"invalid_token"}}"#,
        ),
    ])
    .await;
    let invalid_tokens = MockUpstream::start([
        ScriptedResponse::json(StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#),
        ScriptedResponse::json(StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#),
    ])
    .await;
    // SAFETY: ENV_LOCK serializes process-environment access in this binary.
    unsafe {
        std::env::set_var(
            "OPENPROXY_XAI_TOKEN_URL",
            invalid_tokens.url("/oauth/token"),
        )
    };
    let _env = EnvGuard("OPENPROXY_XAI_TOKEN_URL");
    let (_invalid_db, invalid_app) = c17a_app(
        invalid_generation.url("/v1"),
        oauth_connection("xai-c17a-invalid", "invalid-old", "invalid-refresh"),
    )
    .await;

    for expected in 1..=2 {
        let response = post_chat(invalid_app.clone()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(invalid_tokens.request_count().await, expected);
        assert_eq!(invalid_generation.request_count().await, expected);
    }

    invalid_generation.shutdown().await;
    invalid_tokens.shutdown().await;

    let forbidden_generation = MockUpstream::start([ScriptedResponse::json(
        StatusCode::FORBIDDEN,
        r#"{"error":{"code":"permission_denied","message":"role cannot use model"}}"#,
    )])
    .await;
    let forbidden_tokens = MockUpstream::start([]).await;
    // SAFETY: ENV_LOCK serializes process-environment access in this binary.
    unsafe {
        std::env::set_var(
            "OPENPROXY_XAI_TOKEN_URL",
            forbidden_tokens.url("/oauth/token"),
        )
    };
    let (_forbidden_db, forbidden_app) = c17a_app(
        forbidden_generation.url("/v1"),
        oauth_connection("xai-c17a-forbidden", "forbidden-old", "forbidden-refresh"),
    )
    .await;
    let response = post_chat(forbidden_app).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(forbidden_generation.request_count().await, 1);
    assert_eq!(forbidden_tokens.request_count().await, 0);

    forbidden_generation.shutdown().await;
    forbidden_tokens.shutdown().await;
}

#[tokio::test]
async fn cancelling_foreground_waiter_does_not_lose_rotation_and_next_request_uses_it() {
    let _env_lock = ENV_LOCK.lock().await;
    let release_refresh = Arc::new(Notify::new());
    let generation = MockUpstream::start([
        ScriptedResponse::json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"code":"invalid_token"}}"#,
        ),
        ScriptedResponse::json(StatusCode::OK, success_body(1)),
    ])
    .await;
    let tokens = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        r#"{"access_token":"cancel-access-new","refresh_token":"cancel-refresh-new","expires_in":3600}"#,
    )
    .waiting_for(release_refresh.clone())])
    .await;
    // SAFETY: ENV_LOCK serializes process-environment access in this binary.
    unsafe { std::env::set_var("OPENPROXY_XAI_TOKEN_URL", tokens.url("/oauth/token")) };
    let _env = EnvGuard("OPENPROXY_XAI_TOKEN_URL");
    let (test_db, app) = c17a_app(
        generation.url("/v1"),
        oauth_connection("xai-c17a-cancel", "cancel-access-old", "cancel-refresh-old"),
    )
    .await;

    let cancelled = tokio::spawn(post_chat(app.clone()));
    generation.wait_for_requests(1).await;
    tokens.wait_for_requests(1).await;
    cancelled.abort();
    assert!(cancelled
        .await
        .expect_err("foreground waiter cancelled")
        .is_cancelled());
    release_refresh.notify_waiters();

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if canonical_connection(&test_db, "xai-c17a-cancel")
                .access_token
                .as_deref()
                == Some("cancel-access-new")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("detached foreground refresh persisted");

    let response = post_chat(app).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(tokens.request_count().await, 1);
    assert_eq!(generation.request_count().await, 2);
    let requests = generation.requests().await;
    assert_eq!(
        requests[1].headers.get("authorization").unwrap(),
        "Bearer cancel-access-new"
    );

    generation.shutdown().await;
    tokens.shutdown().await;
}

#[test]
fn foreground_refresh_census_has_one_connection_scoped_owner() {
    let chat = include_str!("../src/server/api/chat.rs");
    let catalog = include_str!("../src/server/codex_catalog.rs");
    let provider = include_str!("../src/core/executor/provider.rs");

    assert!(!chat.contains("dispatch_oauth_refresh("));
    assert!(chat.contains("CONNECTION_REFRESH_COORDINATOR"));
    assert!(chat.contains("connection_credential_generation"));
    assert!(chat.contains("is_refreshable_auth_failure"));
    assert!(catalog.contains("CONNECTION_REFRESH_COORDINATOR"));
    assert!(!catalog.contains("dispatch_oauth_refresh("));
    assert!(!provider.contains("dispatch_oauth_refresh("));
}
