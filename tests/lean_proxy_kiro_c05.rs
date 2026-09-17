mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use openproxy::core::translator::request::claude_to_kiro::claude_to_kiro_request;
use openproxy::core::translator::request::openai_to_kiro::openai_to_kiro_request;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tokio::sync::Barrier;
use tower::util::ServiceExt;

fn eventstream_frame(event_type: &str, payload: &Value) -> Bytes {
    let name = b":event-type";
    let value = event_type.as_bytes();
    let mut headers = Vec::new();
    headers.push(name.len() as u8);
    headers.extend_from_slice(name);
    headers.push(7u8);
    headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
    headers.extend_from_slice(value);

    let payload = serde_json::to_vec(payload).expect("serialize EventStream payload");
    let total = 12 + headers.len() + payload.len() + 4;
    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&(total as u32).to_be_bytes());
    frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    frame.extend_from_slice(&crc32fast::hash(&frame[..8]).to_be_bytes());
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(&payload);
    let message_crc = crc32fast::hash(&frame);
    frame.extend_from_slice(&message_crc.to_be_bytes());
    Bytes::from(frame)
}

fn kiro_node(endpoint: String) -> ProviderNode {
    ProviderNode {
        id: "kiro".into(),
        r#type: "kiro".into(),
        name: "Kiro".into(),
        prefix: Some("kr".into()),
        api_type: Some("chat".into()),
        base_url: Some(endpoint),
        extra: BTreeMap::new(),
        ..Default::default()
    }
}

fn kiro_connection() -> ProviderConnection {
    let mut connection = common::test_connection("kiro");
    connection.id = "kiro-c05".into();
    connection.auth_type = "oauth".into();
    connection.api_key = None;
    connection.access_token = Some("fixture-kiro-token".into());
    connection
        .provider_specific_data
        .insert("authMethod".into(), json!("oauth"));
    connection
}

async fn kiro_app(endpoint: String) -> (axum::Router, TempTestDb) {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![common::test_api_key()];
            db.provider_nodes = vec![kiro_node(endpoint)];
            db.provider_connections = vec![kiro_connection()];
            db.settings.require_login = false;
        })
        .await
        .expect("seed Kiro C05 fixture");
    let state = openproxy::server::state::AppState::new(test_db.db.clone());
    (openproxy::build_app(state), test_db)
}

async fn post_chat(app: axum::Router, messages: Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .header("x-session-id", "shared-c05-session")
                .body(Body::from(
                    json!({
                        "model": "kr/claude-sonnet-4.5",
                        "messages": messages,
                        "stream": true
                    })
                    .to_string(),
                ))
                .expect("Kiro request"),
        )
        .await
        .expect("Kiro app response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("collect Kiro response");
    assert!(String::from_utf8_lossy(&body).contains("data: [DONE]"));
}

fn translator_credentials(connection_id: &str) -> Value {
    json!({
        "provider": "kiro",
        "connectionId": connection_id,
        "rawHeaders": {"x-session-id": "shared-c05-session"}
    })
}

#[tokio::test]
async fn reused_session_sends_changed_first_message_to_upstream() {
    let response = || {
        ScriptedResponse::sse([eventstream_frame(
            "assistantResponseEvent",
            &json!({"content": "ok"}),
        )])
        .with_header("content-type", "application/vnd.amazon.eventstream")
    };
    let upstream = MockUpstream::start([response(), response()]).await;
    let (app, _test_db) = kiro_app(upstream.url("/generateAssistantResponse")).await;

    post_chat(
        app.clone(),
        json!([{"role": "user", "content": "ORIGINAL-MSG0"}]),
    )
    .await;
    post_chat(
        app,
        json!([
            {"role": "user", "content": "CHANGED-MSG0"},
            {"role": "assistant", "content": "prior answer"},
            {"role": "user", "content": "current turn"}
        ]),
    )
    .await;

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_slice(&requests[0].body).expect("first upstream JSON");
    let second: Value = serde_json::from_slice(&requests[1].body).expect("second upstream JSON");
    assert!(first.to_string().contains("ORIGINAL-MSG0"));
    assert!(!second.to_string().contains("ORIGINAL-MSG0"));
    assert!(second.to_string().contains("CHANGED-MSG0"));
    assert!(second.to_string().contains("current turn"));
    assert_eq!(
        first["conversationState"]["conversationId"],
        second["conversationState"]["conversationId"]
    );
    assert_eq!(
        first["conversationState"]["agentContinuationId"],
        second["conversationState"]["agentContinuationId"]
    );

    upstream.shutdown().await;
}

#[test]
fn compaction_system_model_and_account_changes_never_restore_old_content() {
    let credentials = translator_credentials("account-a");
    let mut original = json!({
        "model": "claude-sonnet-4.5",
        "messages": [{"role": "user", "content": "OLD-OPENAI-CONTENT"}]
    });
    openai_to_kiro_request(
        "claude-sonnet-4.5",
        &mut original,
        false,
        Some(&credentials),
    );

    for (model, account, marker) in [
        ("claude-sonnet-4.5", "account-a", "COMPACTED-CURRENT"),
        ("claude-sonnet-4.6", "account-a", "NEW-MODEL-CURRENT"),
        ("claude-sonnet-4.5", "account-b", "NEW-ACCOUNT-CURRENT"),
    ] {
        let credentials = translator_credentials(account);
        let mut body = json!({
            "model": model,
            "messages": [{"role": "user", "content": marker}]
        });
        openai_to_kiro_request(model, &mut body, false, Some(&credentials));
        let serialized = body.to_string();
        assert!(serialized.contains(marker), "{serialized}");
        assert!(!serialized.contains("OLD-OPENAI-CONTENT"), "{serialized}");
        assert_eq!(body["conversationState"]["history"], json!([]));
        assert_eq!(body["_kiroUpstreamModel"], model);
    }

    let mut old_claude = json!({
        "model": "claude-sonnet-4.5",
        "system": "OLD-SYSTEM",
        "messages": [{"role": "user", "content": "OLD-CLAUDE-CONTENT"}]
    });
    claude_to_kiro_request(
        "claude-sonnet-4.5",
        &mut old_claude,
        false,
        Some(&credentials),
    );
    let mut changed_claude = json!({
        "model": "claude-sonnet-4.5",
        "system": "NEW-SYSTEM",
        "messages": [{"role": "user", "content": "NEW-CLAUDE-CONTENT"}]
    });
    claude_to_kiro_request(
        "claude-sonnet-4.5",
        &mut changed_claude,
        false,
        Some(&credentials),
    );
    let serialized = changed_claude.to_string();
    assert!(serialized.contains("NEW-SYSTEM"));
    assert!(serialized.contains("NEW-CLAUDE-CONTENT"));
    assert!(!serialized.contains("OLD-SYSTEM"));
    assert!(!serialized.contains("OLD-CLAUDE-CONTENT"));

    let mut multi_turn = json!({
        "model": "claude-sonnet-4.5",
        "system": "CURRENT-SYSTEM",
        "messages": [
            {"role": "user", "content": "CURRENT-FIRST"},
            {"role": "assistant", "content": "prior answer"},
            {"role": "user", "content": "CURRENT-TURN"}
        ]
    });
    claude_to_kiro_request(
        "claude-sonnet-4.5",
        &mut multi_turn,
        false,
        Some(&credentials),
    );
    let first_history = multi_turn["conversationState"]["history"][0]["userInputMessage"]
        ["content"]
        .as_str()
        .expect("first history content");
    let current = multi_turn["conversationState"]["currentMessage"]["userInputMessage"]["content"]
        .as_str()
        .expect("current content");
    assert!(first_history.contains("CURRENT-SYSTEM"));
    assert!(first_history.contains("CURRENT-FIRST"));
    assert!(current.contains("CURRENT-TURN"));
    assert!(!current.contains("OLD-"));
}

#[test]
fn empty_history_tools_and_required_kiro_fields_remain_valid() {
    let credentials = translator_credentials("account-fields");
    let mut empty_claude = json!({
        "model": "claude-sonnet-4.5",
        "system": "fresh system",
        "messages": [],
        "max_tokens": 4096
    });
    claude_to_kiro_request(
        "claude-sonnet-4.5",
        &mut empty_claude,
        false,
        Some(&credentials),
    );
    let current = empty_claude["conversationState"]["currentMessage"]["userInputMessage"]
        ["content"]
        .as_str()
        .expect("current content");
    assert!(current.contains("fresh system"));
    assert!(current.contains("continue"));
    assert_eq!(empty_claude["conversationState"]["history"], json!([]));
    assert_eq!(empty_claude["inferenceConfig"]["maxTokens"], 4096);
    assert!(empty_claude.get("systemPrompt").is_none());

    let mut tools = json!({
        "model": "claude-sonnet-4.5",
        "messages": [
            {"role": "user", "content": "look it up"},
            {"role": "assistant", "content": null, "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "lookup", "arguments": "{\"q\":\"rust\"}"}
            }]},
            {"role": "tool", "tool_call_id": "call_1", "content": "tool-result"},
            {"role": "user", "content": "summarize"}
        ],
        "tools": [{"type": "function", "function": {
            "name": "lookup",
            "description": "Lookup a value",
            "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
        }}]
    });
    openai_to_kiro_request("claude-sonnet-4.5", &mut tools, false, Some(&credentials));
    assert_eq!(tools["conversationState"]["chatTriggerType"], "MANUAL");
    assert_eq!(tools["conversationState"]["agentTaskType"], "vibe");
    assert_eq!(tools["agentMode"], "vibe");
    assert_eq!(
        tools["conversationState"]["currentMessage"]["userInputMessage"]["origin"],
        "AI_EDITOR"
    );
    assert_eq!(tools["inferenceConfig"]["maxTokens"], 32000);
    assert!(tools.to_string().contains("lookup"));
    assert!(tools.to_string().contains("tool-result"));
    assert!(tools.get("systemPrompt").is_none());
}

#[tokio::test]
async fn concurrent_same_session_translations_do_not_share_message_content() {
    const REQUESTS: usize = 24;
    let barrier = Arc::new(Barrier::new(REQUESTS));
    let mut tasks = Vec::with_capacity(REQUESTS);

    for index in 0..REQUESTS {
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            let marker = format!("request-marker-{index}");
            let credentials = translator_credentials("shared-account");
            let mut body = json!({
                "model": "claude-sonnet-4.5",
                "messages": [{"role": "user", "content": marker}]
            });
            barrier.wait().await;
            openai_to_kiro_request("claude-sonnet-4.5", &mut body, false, Some(&credentials));
            let content = body["conversationState"]["currentMessage"]["userInputMessage"]
                ["content"]
                .as_str()
                .expect("current request content")
                .to_string();
            (index, content)
        }));
    }

    for task in tasks {
        let (index, content) = task.await.expect("join translation");
        assert_eq!(content, format!("request-marker-{index}"));
    }
}
