mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use once_cell::sync::Lazy;
use openproxy::oauth::token_refresh::active_connection_refresh_count;
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

fn due_xai_connection(id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: "xai".into(),
        auth_type: "oauth".into(),
        is_active: Some(true),
        priority: Some(1),
        access_token: Some(format!("access-{id}-old")),
        refresh_token: Some(format!("refresh-{id}-old")),
        expires_at: Some((chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339()),
        default_model: Some("grok-c17b".into()),
        ..Default::default()
    }
}

fn xai_node(base_url: String) -> ProviderNode {
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

async fn seeded_state(
    generation_url: String,
    connection: ProviderConnection,
) -> (TempTestDb, AppState) {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(move |db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![xai_node(generation_url)];
            db.provider_connections = vec![connection];
        })
        .await
        .expect("seed C17B database");
    let state = AppState::new(test_db.db.clone());
    (test_db, state)
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
                    "model": "xai/grok-c17b",
                    "messages": [{"role": "user", "content": "coordinate all callers"}],
                    "stream": false
                })
                .to_string(),
            ))
            .expect("C17B chat request"),
    )
    .await
    .expect("C17B chat response")
}

async fn get_usage(app: axum::Router, id: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .uri(format!("/api/usage/{id}"))
            .header("authorization", "Bearer test-key")
            .body(Body::empty())
            .expect("C17B usage request"),
    )
    .await
    .expect("C17B usage response")
}

fn canonical(test_db: &TempTestDb, id: &str) -> ProviderConnection {
    test_db
        .db
        .snapshot()
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
        .expect("canonical C17B connection")
}

#[tokio::test]
async fn proactive_quota_and_foreground_401_share_one_generation_refresh() {
    let _env_lock = ENV_LOCK.lock().await;
    let release_refresh = Arc::new(Notify::new());
    let generation = MockUpstream::start([
        ScriptedResponse::json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"code":"invalid_token","message":"expired"}}"#,
        ),
        ScriptedResponse::json(
            StatusCode::OK,
            r#"{"id":"chatcmpl-c17b","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
        ),
    ])
    .await;
    let tokens = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        r#"{"access_token":"access-c17b-new","refresh_token":"refresh-c17b-new","expires_in":3600}"#,
    )
    .waiting_for(release_refresh.clone())])
    .await;
    // SAFETY: ENV_LOCK serializes process-environment access in this binary.
    unsafe { std::env::set_var("OPENPROXY_XAI_TOKEN_URL", tokens.url("/oauth/token")) };
    let _env = EnvGuard("OPENPROXY_XAI_TOKEN_URL");

    let (test_db, state) =
        seeded_state(generation.url("/v1"), due_xai_connection("xai-c17b-shared")).await;
    let app = openproxy::build_app(state.clone());

    let proactive = tokio::spawn({
        let state = state.clone();
        async move { openproxy::oauth::background_refresh::run_tick(&state).await }
    });
    tokio::time::timeout(Duration::from_secs(5), tokens.wait_for_requests(1))
        .await
        .expect("proactive token refresh started");

    let quota = tokio::spawn(get_usage(app.clone(), "xai-c17b-shared"));
    let chat = tokio::spawn(post_chat(app));
    tokio::time::timeout(Duration::from_secs(5), generation.wait_for_requests(1))
        .await
        .expect("foreground generation started");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(tokens.request_count().await, 1);

    release_refresh.notify_waiters();
    tokio::time::timeout(Duration::from_secs(5), proactive)
        .await
        .expect("proactive refresh completed")
        .expect("proactive refresh joined");
    let quota_response = tokio::time::timeout(Duration::from_secs(5), quota)
        .await
        .expect("quota request completed")
        .expect("quota task joined");
    assert_eq!(quota_response.status(), StatusCode::OK);
    let _ = to_bytes(quota_response.into_body(), 1024 * 1024)
        .await
        .expect("read quota response");
    let chat_response = tokio::time::timeout(Duration::from_secs(5), chat)
        .await
        .expect("chat request completed")
        .expect("chat task joined");
    assert_eq!(chat_response.status(), StatusCode::OK);
    let _ = to_bytes(chat_response.into_body(), 1024 * 1024)
        .await
        .expect("read chat response");

    assert_eq!(tokens.request_count().await, 1);
    assert_eq!(generation.request_count().await, 2);
    let stored = canonical(&test_db, "xai-c17b-shared");
    assert_eq!(stored.access_token.as_deref(), Some("access-c17b-new"));
    assert_eq!(stored.refresh_token.as_deref(), Some("refresh-c17b-new"));
    assert_eq!(active_connection_refresh_count(), 0);

    generation.shutdown().await;
    tokens.shutdown().await;
}

#[tokio::test]
async fn stale_background_result_cannot_roll_back_newer_canonical_generation() {
    let _env_lock = ENV_LOCK.lock().await;
    let release_refresh = Arc::new(Notify::new());
    let tokens = MockUpstream::start([ScriptedResponse::json(
        StatusCode::OK,
        r#"{"access_token":"stale-background-access","refresh_token":"stale-background-refresh","expires_in":3600}"#,
    )
    .waiting_for(release_refresh.clone())])
    .await;
    // SAFETY: ENV_LOCK serializes process-environment access in this binary.
    unsafe { std::env::set_var("OPENPROXY_XAI_TOKEN_URL", tokens.url("/oauth/token")) };
    let _env = EnvGuard("OPENPROXY_XAI_TOKEN_URL");
    let generation = MockUpstream::start([]).await;
    let (test_db, state) = seeded_state(
        generation.url("/v1"),
        due_xai_connection("xai-c17b-no-rollback"),
    )
    .await;

    let proactive = tokio::spawn({
        let state = state.clone();
        async move { openproxy::oauth::background_refresh::run_tick(&state).await }
    });
    tokio::time::timeout(Duration::from_secs(5), tokens.wait_for_requests(1))
        .await
        .expect("stale proactive token refresh started");
    test_db
        .db
        .update(|db| {
            let connection = db
                .provider_connections
                .iter_mut()
                .find(|connection| connection.id == "xai-c17b-no-rollback")
                .expect("connection to rotate concurrently");
            connection.access_token = Some("foreground-winner-access".into());
            connection.refresh_token = Some("foreground-winner-refresh".into());
            connection.updated_at = Some(chrono::Utc::now().to_rfc3339());
        })
        .await
        .expect("publish newer canonical generation");
    release_refresh.notify_waiters();
    proactive.await.expect("stale proactive task joined");

    let stored = canonical(&test_db, "xai-c17b-no-rollback");
    assert_eq!(
        stored.access_token.as_deref(),
        Some("foreground-winner-access")
    );
    assert_eq!(
        stored.refresh_token.as_deref(),
        Some("foreground-winner-refresh")
    );
    assert_eq!(tokens.request_count().await, 1);
    assert_eq!(active_connection_refresh_count(), 0);

    generation.shutdown().await;
    tokens.shutdown().await;
}

#[test]
fn control_and_background_refresh_census_has_no_direct_bypass() {
    let sources = [
        include_str!("../src/oauth/background_refresh.rs"),
        include_str!("../src/server/api/quota_auto_ping.rs"),
        include_str!("../src/server/api/usage.rs"),
        include_str!("../src/server/api/oauth.rs"),
        include_str!("../src/server/api/provider_models.rs"),
        include_str!("../src/server/api/provider_connection_test.rs"),
        include_str!("../src/server/codex_catalog.rs"),
    ];
    for source in sources {
        assert!(!source.contains("dispatch_oauth_refresh("));
        assert!(!source.contains("refresh_codex_token("));
    }

    assert!(sources
        .iter()
        .all(|source| source.contains("CONNECTION_REFRESH_COORDINATOR")));
    let connection_test = sources[5];
    assert!(connection_test.contains("refresh(") && connection_test.contains("effective_proxy"));
    let oauth = sources[3];
    assert!(!oauth.contains("REFRESH_LOCKS"));
    assert!(!oauth.contains("get_refresh_lock_key"));
}
