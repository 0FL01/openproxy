mod common;

use std::collections::BTreeMap;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use openproxy::core::utils::session_manager::{resolve_continuation_id, resolve_session_identity};
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use uuid::Uuid;

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
    frame.extend_from_slice(&crc32fast::hash(&frame).to_be_bytes());
    Bytes::from(frame)
}

fn response() -> ScriptedResponse {
    ScriptedResponse::sse([eventstream_frame(
        "assistantResponseEvent",
        &json!({"content": "ok"}),
    )])
    .with_header("content-type", "application/vnd.amazon.eventstream")
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

fn kiro_connection(id: &str, active: bool) -> ProviderConnection {
    let mut connection = common::test_connection("kiro");
    connection.id = id.to_string();
    connection.auth_type = "oauth".into();
    connection.api_key = None;
    connection.access_token = Some(format!("fixture-{id}"));
    connection.is_active = Some(active);
    connection
        .provider_specific_data
        .insert("authMethod".into(), json!("oauth"));
    connection
}

async fn post_chat(app: axum::Router, session: &str, messages: Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .header("content-type", "application/json")
                .header("x-session-id", session)
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

#[tokio::test]
async fn kiro_continuation_is_stable_per_client_and_selected_connection() {
    let upstream = MockUpstream::start((0..5).map(|_| response())).await;
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.api_keys = vec![common::test_api_key()];
            db.provider_nodes = vec![kiro_node(upstream.url("/generateAssistantResponse"))];
            db.provider_connections = vec![
                kiro_connection("kiro-account-a", true),
                kiro_connection("kiro-account-b", false),
            ];
            db.settings.require_login = false;
        })
        .await
        .expect("seed Kiro C06 fixture");
    let state = openproxy::server::state::AppState::new(test_db.db.clone());
    let app = openproxy::build_app(state);

    post_chat(
        app.clone(),
        "client-session",
        json!([{"role": "user", "content": "first"}]),
    )
    .await;
    post_chat(
        app.clone(),
        "client-session",
        json!([
            {"role": "user", "content": "use tool"},
            {"role": "assistant", "content": null, "tool_calls": [{
                "id": "call_1", "type": "function",
                "function": {"name": "lookup", "arguments": "{\"q\":\"rust\"}"}
            }]},
            {"role": "tool", "tool_call_id": "call_1", "content": "tool-result"},
            {"role": "user", "content": "continue"}
        ]),
    )
    .await;

    test_db
        .db
        .update(|db| {
            db.provider_connections[0].is_active = Some(false);
            db.provider_connections[1].is_active = Some(true);
        })
        .await
        .expect("switch Kiro account");
    post_chat(
        app.clone(),
        "client-session",
        json!([{"role": "user", "content": "compacted branch"}]),
    )
    .await;

    test_db
        .db
        .update(|db| {
            db.provider_connections = vec![kiro_connection("kiro-account-a-recreated", true)];
        })
        .await
        .expect("recreate Kiro connection");
    post_chat(
        app.clone(),
        "client-session",
        json!([{"role": "user", "content": "after recreation"}]),
    )
    .await;
    post_chat(
        app,
        "new-client-session",
        json!([{"role": "user", "content": "new user"}]),
    )
    .await;

    let requests = upstream.requests().await;
    assert_eq!(requests.len(), 5);
    let bodies: Vec<Value> = requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("upstream JSON"))
        .collect();
    for body in &bodies {
        assert!(body.get("_kiroSessionEphemeral").is_none());
        assert!(Uuid::parse_str(
            body["conversationState"]["agentContinuationId"]
                .as_str()
                .expect("continuation id")
        )
        .is_ok());
    }
    assert_eq!(
        bodies[0]["conversationState"]["conversationId"],
        bodies[1]["conversationState"]["conversationId"]
    );
    assert_eq!(
        bodies[0]["conversationState"]["agentContinuationId"],
        bodies[1]["conversationState"]["agentContinuationId"]
    );
    assert!(bodies[1].to_string().contains("tool-result"));
    assert_ne!(
        bodies[1]["conversationState"]["agentContinuationId"],
        bodies[2]["conversationState"]["agentContinuationId"]
    );
    assert_ne!(
        bodies[2]["conversationState"]["agentContinuationId"],
        bodies[3]["conversationState"]["agentContinuationId"]
    );
    assert_ne!(
        bodies[3]["conversationState"]["conversationId"],
        bodies[4]["conversationState"]["conversationId"]
    );

    upstream.shutdown().await;
}

#[tokio::test]
async fn stateless_resolution_survives_large_churn_and_task_cancellation() {
    let stable = resolve_continuation_id("stable", Some("account"), "kiro", false);
    let mut tasks = Vec::new();
    for worker in 0..32 {
        tasks.push(tokio::spawn(async move {
            for index in 0..3_125 {
                let session = format!("worker-{worker}-session-{index}");
                let id = resolve_continuation_id(&session, Some("account"), "kiro", false);
                assert!(Uuid::parse_str(&id).is_ok());
                if index % 256 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }));
    }
    for task in tasks.iter().take(16) {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }

    assert_eq!(
        stable,
        resolve_continuation_id("stable", Some("account"), "kiro", false)
    );
    let kiro_a = resolve_session_identity(None, None, Some("account"), "kiro");
    let kiro_b = resolve_session_identity(None, None, Some("account"), "kiro");
    assert!(kiro_a.ephemeral && kiro_b.ephemeral);
    assert_ne!(kiro_a.session_id, kiro_b.session_id);
}
