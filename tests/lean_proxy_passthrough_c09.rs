mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

const USER_AGENTS: [Option<&str>; 4] = [
    Some("claude-cli/2.1.92"),
    Some("opencode/1.18.31"),
    Some("custom-harness/0.0.0"),
    None,
];

fn node(id: &str, kind: &str, prefix: &str, base_url: String, api_type: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: kind.into(),
        name: format!("C09 {kind}"),
        prefix: Some(prefix.into()),
        api_type: Some(api_type.into()),
        base_url: Some(base_url),
        ..Default::default()
    }
}

fn connection(id: &str, provider: &str, model: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        priority: Some(1),
        api_key: Some(format!("{id}-key")),
        default_model: Some(model.into()),
        ..Default::default()
    }
}

fn ok_openai() -> ScriptedResponse {
    ScriptedResponse::json(
        StatusCode::OK,
        r#"{"id":"chatcmpl-c09","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
    )
}

fn ok_claude() -> ScriptedResponse {
    ScriptedResponse::json(
        StatusCode::OK,
        r#"{"id":"msg_c09","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":"claude-sonnet-4","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#,
    )
}

async fn app_with_upstreams(
    openai: &MockUpstream,
    claude: &MockUpstream,
) -> (axum::Router, TempTestDb) {
    let db = TempTestDb::new().await;
    db.db
        .update(|state| {
            state.api_keys = vec![test_api_key()];
            state.provider_nodes = vec![
                node(
                    "openai-compatible-c09",
                    "openai-compatible",
                    "oa",
                    openai.url("/v1"),
                    "chat",
                ),
                node(
                    "anthropic-compatible-c09",
                    "anthropic-compatible",
                    "ac",
                    claude.url("/v1"),
                    "messages",
                ),
            ];
            state.provider_connections = vec![
                connection("c09-openai", "openai-compatible-c09", "model-a"),
                connection("c09-claude", "anthropic-compatible-c09", "claude-sonnet-4"),
            ];
        })
        .await
        .expect("seed C09 database");
    (openproxy::build_app(AppState::new(db.db.clone())), db)
}

async fn post(
    app: &axum::Router,
    uri: &str,
    user_agent: Option<&str>,
    body: &Value,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json");
    if let Some(user_agent) = user_agent {
        builder = builder.header("user-agent", user_agent);
    }
    app.clone()
        .oneshot(
            builder
                .body(Body::from(body.to_string()))
                .expect("C09 request"),
        )
        .await
        .expect("C09 response")
}

async fn assert_ok(response: axum::response::Response) {
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read C09 response");
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
}

#[tokio::test]
async fn same_format_semantics_do_not_depend_on_user_agent() {
    let openai = MockUpstream::start(USER_AGENTS.map(|_| ok_openai())).await;
    let claude = MockUpstream::start(USER_AGENTS.map(|_| ok_claude())).await;
    let (app, _db) = app_with_upstreams(&openai, &claude).await;

    let openai_body = json!({
        "model": "oa/model-a",
        "messages": [{
            "role": "user",
            "content": [{
                "type": "text",
                "text": "keep me",
                "cache_control": {"type": "ephemeral", "ttl": "client-owned"},
                "native_block_extension": {"enabled": true}
            }]
        }],
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup",
                "description": "preserve tool",
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
            },
            "native_tool_extension": "keep"
        }],
        "tool_choice": "auto",
        "reasoning_effort": "high",
        "prompt_cache_key": "client-cache-key",
        "provider_extension": {"future": [1, 2, 3]},
        "stream": false
    });
    for user_agent in USER_AGENTS {
        assert_ok(post(&app, "/v1/chat/completions", user_agent, &openai_body).await).await;
    }

    let openai_requests = openai.requests().await;
    assert_eq!(openai_requests.len(), USER_AGENTS.len());
    let openai_forwarded: Vec<Value> = openai_requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("OpenAI upstream JSON"))
        .collect();
    for forwarded in &openai_forwarded {
        assert_eq!(forwarded, &openai_forwarded[0]);
        assert_eq!(forwarded["model"], "model-a");
        assert_eq!(forwarded["prompt_cache_key"], "client-cache-key");
        assert_eq!(
            forwarded["messages"][0]["content"][0]["cache_control"]["ttl"],
            "client-owned"
        );
        assert_eq!(forwarded["provider_extension"]["future"], json!([1, 2, 3]));
        assert_eq!(forwarded["reasoning_effort"], "high");
        assert_eq!(forwarded["tools"][0]["native_tool_extension"], "keep");
    }

    let claude_body = json!({
        "model": "ac/claude-sonnet-4",
        "system": [{
            "type": "text",
            "text": "system",
            "cache_control": {"type": "ephemeral", "ttl": "client-system"}
        }],
        "messages": [{
            "role": "user",
            "content": [{
                "type": "text",
                "text": "hello",
                "cache_control": {"type": "ephemeral", "ttl": "client-message"},
                "native_block_extension": 7
            }]
        }],
        "tools": [{
            "name": "lookup",
            "description": "preserve native tool",
            "input_schema": {"type": "object"},
            "cache_control": {"type": "ephemeral", "ttl": "client-tool"},
            "native_tool_extension": true
        }],
        "thinking": {"type": "enabled", "budget_tokens": 1024},
        "prompt_cache_key": "claude-client-cache-key",
        "provider_extension": {"beta": "future"},
        "max_tokens": 2048,
        "stream": false
    });
    for user_agent in USER_AGENTS {
        assert_ok(post(&app, "/v1/messages", user_agent, &claude_body).await).await;
    }

    let claude_requests = claude.requests().await;
    assert_eq!(claude_requests.len(), USER_AGENTS.len());
    let claude_forwarded: Vec<Value> = claude_requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("Claude upstream JSON"))
        .collect();
    for forwarded in &claude_forwarded {
        assert_eq!(forwarded, &claude_forwarded[0]);
        assert_eq!(forwarded["model"], "claude-sonnet-4");
        assert_eq!(forwarded["prompt_cache_key"], "claude-client-cache-key");
        assert_eq!(
            forwarded["system"][0]["cache_control"]["ttl"],
            "client-system"
        );
        assert_eq!(
            forwarded["messages"][0]["content"][0]["cache_control"]["ttl"],
            "client-message"
        );
        assert_eq!(forwarded["tools"][0]["cache_control"]["ttl"], "client-tool");
        assert_eq!(forwarded["provider_extension"]["beta"], "future");
        assert_eq!(forwarded["thinking"]["budget_tokens"], 1024);
    }

    openai.shutdown().await;
    claude.shutdown().await;
}

#[tokio::test]
async fn incompatible_formats_translate_even_for_recognized_native_client() {
    let openai = MockUpstream::start([]).await;
    let claude = MockUpstream::start([ok_claude()]).await;
    let (app, _db) = app_with_upstreams(&openai, &claude).await;

    let response = post(
        &app,
        "/v1/chat/completions",
        Some("claude-cli/2.1.92"),
        &json!({
            "model": "ac/claude-sonnet-4",
            "messages": [{"role": "user", "content": "translate me"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "translated tool",
                    "parameters": {"type": "object", "properties": {}}
                }
            }],
            "tool_choice": "auto",
            "reasoning_effort": "high",
            "stream": false
        }),
    )
    .await;
    assert_ok(response).await;

    assert_eq!(openai.request_count().await, 0);
    let requests = claude.requests().await;
    assert_eq!(requests.len(), 1);
    let forwarded: Value = serde_json::from_slice(&requests[0].body).expect("translated JSON");
    assert_eq!(forwarded["model"], "claude-sonnet-4");
    assert_eq!(forwarded["messages"][0]["role"], "user");
    assert!(forwarded["messages"][0]["content"].is_array());
    assert_eq!(forwarded["tools"][0]["name"], "lookup");
    assert!(forwarded["tools"][0].get("function").is_none());
    assert!(forwarded.get("thinking").is_some());

    openai.shutdown().await;
    claude.shutdown().await;
}

#[test]
fn passthrough_has_no_client_identity_gate_or_cache_reanchoring() {
    let detector = include_str!("../src/core/utils/client_detector.rs");
    let chat = include_str!("../src/server/api/chat.rs");
    let claude = include_str!("../src/core/translator/request/claude_format.rs");

    assert!(!detector.contains("is_native_passthrough"));
    assert!(!chat.contains("is_native_passthrough"));
    assert!(!chat.contains("anchor_claude_cache"));
    assert!(!claude.contains("anchor_claude_cache"));
    assert!(chat.contains("plan.target_format == Format::Claude"));
}
