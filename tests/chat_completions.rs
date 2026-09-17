use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::sqlite::repo::request_repo::{self, RequestDetailFilter};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProviderNode, Settings};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn active_key(key: &str) -> ApiKey {
    ApiKey {
        id: "test-key-id".into(),
        name: "Local".into(),
        key: key.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
    }
}

fn provider_node(id: &str, prefix: &str, base_url: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: "Compatible".into(),
        prefix: Some(prefix.into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url.into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn connection(id: &str, provider: &str, priority: u32, api_key: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        priority: Some(priority),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: Some("gpt-4o-mini".into()),
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: Some(api_key.into()),
        test_status: None,
        last_tested: None,
        last_error: None,
        last_error_at: None,
        rate_limited_until: None,
        expires_in: None,
        error_code: None,
        consecutive_use_count: None,
        backoff_level: None,
        consecutive_errors: None,
        proxy_url: None,
        proxy_label: None,
        use_connection_proxy: None,
        runtime_transport: None,
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}

async fn seeded_state(nodes: Vec<ProviderNode>, connections: Vec<ProviderConnection>) -> AppState {
    seeded_state_with_settings(nodes, connections, Settings::default()).await
}

async fn seeded_state_with_settings(
    nodes: Vec<ProviderNode>,
    connections: Vec<ProviderConnection>,
    settings: Settings,
) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.provider_nodes = nodes;
        state.provider_connections = connections;
        // Auth is not under test here (see api_auth_and_models) — disable the
        // login guard so requests without keys reach the chat pipeline.
        let mut settings = settings;
        settings.require_login = false;
        state.settings = settings;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn chat_completions_streams_openai_compatible_response() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "stream": true,
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":16,\"completion_tokens\":25,\"total_tokens\":41,\"prompt_tokens_details\":{\"cached_tokens\":3},\"completion_tokens_details\":{\"reasoning_tokens\":21}}}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            ),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
    )
    .await;

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": true,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    if response.status() != StatusCode::OK {
        let b = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        panic!("DBG500: {}", String::from_utf8_lossy(&b));
    }
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("data: {\"choices\""));
    assert!(text.contains("data: [DONE]"));

    let logs = state
        .db
        .sqlite
        .with_conn(|conn| request_repo::list(conn, &RequestDetailFilter::default(), 10, 0))
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].status.as_deref(), Some("success"));
    assert_eq!(logs[0].api_key_id.as_deref(), Some("test-key-id"));
    assert_eq!(logs[0].api_key_name.as_deref(), Some("Local"));
    assert_eq!(logs[0].data["inputTokens"], 16);
    assert_eq!(logs[0].data["outputTokens"], 25);
    assert!(logs[0].data.get("tokens").is_none());
}

#[tokio::test]
async fn chat_completions_reports_oversized_json_as_payload_too_large() {
    let app = openproxy::build_app(seeded_state(Vec::new(), Vec::new()).await);
    let oversized_prompt = "x".repeat(32 * 1024 * 1024);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "compat/gpt-4o-mini",
                        "messages": [{"role": "user", "content": oversized_prompt}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("Request body exceeds 32 MiB limit"));
}

#[tokio::test]
async fn chat_completions_forwards_short_requests_unmutated() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-short",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state_with_settings(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
        Settings::default(),
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [
                            { "role": "user", "content": "hi" }
                        ],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = upstream
        .received_requests()
        .await
        .expect("received requests");
    assert_eq!(requests.len(), 1);

    for (idx, r) in requests.iter().enumerate() {
        let b: serde_json::Value = r.body_json().unwrap_or_default();
        eprintln!(
            "DEBUGUP {idx}: {}",
            serde_json::to_string(&b["messages"]).unwrap_or_default()
        );
    }
    let forwarded: serde_json::Value = requests[0].body_json().expect("forwarded body");
    let messages = forwarded["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "hi");
}

#[tokio::test]
async fn chat_completions_falls_back_to_next_account_on_retryable_error() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer bad-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": { "message": "rate limit exceeded" }
        })))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer good-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-success",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![
            connection("conn-bad", "node-openai", 1, "bad-key"),
            connection("conn-good", "node-openai", 2, "good-key"),
        ],
    )
    .await;

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "chatcmpl-success");

    let snapshot = state.db.snapshot();
    let first = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == "conn-bad")
        .unwrap();
    assert!(!first.extra.contains_key("modelLock_gpt-4o-mini"));
    assert_eq!(first.error_code, None);

    let logs = state
        .db
        .sqlite
        .with_conn(|conn| request_repo::list(conn, &RequestDetailFilter::default(), 10, 0))
        .unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].status.as_deref(), Some("success"));
    assert_eq!(logs[1].status.as_deref(), Some("error"));
    assert_eq!(logs[0].correlation_id, logs[1].correlation_id);
    assert_eq!(logs[0].api_key_id.as_deref(), Some("test-key-id"));
    assert!(!logs
        .iter()
        .any(|log| log.data.to_string().contains("valid-bearer")));
}

#[tokio::test]
async fn chat_completions_keeps_concurrent_requests_on_preferred_account() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_json(json!({
                    "id": "chatcmpl-affinity",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": "ok" },
                        "finish_reason": "stop"
                    }]
                })),
        )
        .expect(11)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![
            connection("conn-b", "node-openai", 1, "second-key"),
            connection("conn-a", "node-openai", 1, "preferred-key"),
        ],
    )
    .await;
    let app = openproxy::build_app(state);

    let mut tasks = Vec::new();
    for _ in 0..11 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer valid-bearer")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "custom/gpt-4o-mini",
                            "messages": [{"role": "user", "content": "hi"}],
                            "stream": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }));
    }

    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::OK);
    }

    let requests = upstream
        .received_requests()
        .await
        .expect("received requests");
    assert_eq!(requests.len(), 11);
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some("Bearer preferred-key")
    }));
}

#[tokio::test]
async fn chat_completions_skips_accounts_that_do_not_advertise_requested_model() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer good-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-model-aware",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let mut unsupported = connection("conn-unsupported", "node-openai", 1, "bad-key");
    unsupported.default_model = None;
    unsupported
        .provider_specific_data
        .insert("enabledModels".into(), json!(["gpt-other"]));

    let mut supported = connection("conn-supported", "node-openai", 2, "good-key");
    supported.default_model = None;
    supported
        .provider_specific_data
        .insert("enabledModels".into(), json!(["gpt-4o-mini"]));

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![unsupported, supported],
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "chatcmpl-model-aware");
}

#[tokio::test]
async fn chat_completions_retries_upstream_on_the_next_request() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "120")
                .set_body_json(json!({
                    "error": { "message": "rate limit exceeded" }
                })),
        )
        .expect(2)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection("conn-1", "node-openai", 1, "upstream-key")],
    )
    .await;

    let request_body = json!({
        "model": "custom/gpt-4o-mini",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": false,
    })
    .to_string();

    let app = openproxy::build_app(state.clone());
    let first = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(request_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (parts, body) = first.into_parts();
    if parts.status != StatusCode::TOO_MANY_REQUESTS {
        let b = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        panic!(
            "retry-after test: status={}; body={}",
            parts.status,
            String::from_utf8_lossy(&b)
        );
    }
    let retry_hdr = parts.headers.get("retry-after").cloned();
    if retry_hdr.is_none() {
        panic!("429 without retry-after header");
    }
    let first_retry_after: i64 = retry_hdr.unwrap().to_str().unwrap().parse().unwrap();
    assert!(first_retry_after >= 100);

    let app = openproxy::build_app(state.clone());
    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(request_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let second_retry_after: i64 = second
        .headers()
        .get("retry-after")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(second_retry_after >= 100);
    let stored = &state.db.snapshot().provider_connections[0];
    assert!(!stored.extra.contains_key("modelLock_gpt-4o-mini"));
    assert_eq!(stored.rate_limited_until, None);
}

#[tokio::test]
async fn chat_completions_retries_model_after_upstream_404() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(json!({ "model": "gpt-missing" })))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": { "message": "model not found" }
        })))
        .expect(2)
        .mount(&upstream)
        .await;

    let mut connection = connection("conn-1", "node-openai", 1, "upstream-key");
    connection.default_model = None;
    connection
        .provider_specific_data
        .insert("enabledModels".into(), json!(["gpt-missing"]));

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection],
    )
    .await;

    let app = openproxy::build_app(state.clone());
    let missing = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-missing",
                        "messages": [{ "role": "user", "content": "hi" }],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let snapshot = state.db.snapshot();
    let stored = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.id == "conn-1")
        .expect("stored connection");
    assert!(!stored.extra.contains_key("modelLock_gpt-missing"));
    assert!(stored.rate_limited_until.is_none());

    let app = openproxy::build_app(state);
    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-missing",
                        "messages": [{ "role": "user", "content": "hi again" }],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(second.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn chat_completions_supports_enabled_models_with_nested_slashes() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer upstream-key"))
        .and(body_partial_json(
            json!({ "model": "meta-llama/llama-3.3-70b" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-nested-model",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "nested ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let mut connection = connection("conn-1", "node-openai", 1, "upstream-key");
    connection.default_model = None;
    connection
        .provider_specific_data
        .insert("enabledModels".into(), json!(["meta-llama/llama-3.3-70b"]));

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![connection],
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/meta-llama/llama-3.3-70b",
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "chatcmpl-nested-model");
}

#[tokio::test]
async fn chat_completions_returns_the_last_upstream_error_when_all_accounts_fail() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer first-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "120")
                .set_body_json(json!({
                    "error": { "message": "rate limit exceeded" }
                })),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer second-key"))
        .and(body_partial_json(json!({ "model": "gpt-4o-mini" })))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": { "message": "temporary upstream issue" }
        })))
        .expect(3)
        .mount(&upstream)
        .await;

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![
            connection("conn-1", "node-openai", 1, "first-key"),
            connection("conn-2", "node-openai", 2, "second-key"),
        ],
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().get("retry-after").is_none());
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json,
        json!({"error": {"message": "temporary upstream issue"}})
    );
}

#[tokio::test]
async fn chat_completions_rejects_connections_without_credentials() {
    let upstream = MockServer::start().await;

    let mut missing_credentials = connection("conn-1", "node-openai", 1, "unused-key");
    missing_credentials.api_key = None;
    missing_credentials.default_model = Some("gpt-4o-mini".into());

    let state = seeded_state(
        vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )],
        vec![missing_credentials],
    )
    .await;

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "custom/gpt-4o-mini",
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("No credentials"));
    assert!(upstream.received_requests().await.unwrap().is_empty());
}
