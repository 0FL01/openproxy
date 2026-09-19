use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const KEY: &str = "mcp-test-key";

async fn app() -> axum::Router {
    let temp = tempdir().unwrap();
    let db = Arc::new(Db::load_from(temp.path()).await.unwrap());
    db.update(|state| {
        state.settings.require_api_key = false;
        state.api_keys = vec![ApiKey {
            id: "mcp-key".into(),
            name: "MCP test".into(),
            key: KEY.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: BTreeMap::new(),
        }];
    })
    .await
    .unwrap();
    openproxy::build_app(AppState::new(db))
}

fn request(method: Method, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri("/v1/mcp")
        .header("authorization", format!("Bearer {KEY}"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn mcp_protocol_initializes_notifies_and_lists_one_search_tool() {
    let app = app().await;
    let initialize = app
        .clone()
        .oneshot(request(
            Method::POST,
            json!({
                "jsonrpc":"2.0",
                "id":0,
                "method":"initialize",
                "params":{
                    "protocolVersion":"2025-11-25",
                    "capabilities":{},
                    "clientInfo":{"name":"opencode","version":"1.18.31"}
                }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(initialize.status(), StatusCode::OK);
    assert!(initialize.headers().get("mcp-session-id").is_none());
    let initialized = json_body(initialize).await;
    assert_eq!(initialized["id"], 0);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(initialized["result"]["capabilities"], json!({"tools": {}}));

    let mut notification = request(
        Method::POST,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    notification
        .headers_mut()
        .insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
    let notification = app.clone().oneshot(notification).await.unwrap();
    assert_eq!(notification.status(), StatusCode::ACCEPTED);
    assert!(notification
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .is_empty());

    let mut list = request(
        Method::POST,
        json!({"jsonrpc":"2.0","id":"tools-1","method":"tools/list","params":{}}),
    );
    list.headers_mut()
        .insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
    let listed = json_body(app.clone().oneshot(list).await.unwrap()).await;
    assert_eq!(listed["id"], "tools-1");
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "search");
    assert_eq!(
        tools[0]["inputSchema"]["properties"]["response_length"]["enum"],
        json!(["short", "medium", "long"])
    );

    let get = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/mcp")
                .header("authorization", format!("Bearer {KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn mcp_auth_and_origin_are_enforced_before_dispatch() {
    let app = app().await;
    let unauthenticated = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/mcp")
                .header("content-type", "application/json")
                .body(Body::from("not-json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let mut browser = request(
        Method::POST,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    );
    browser
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let browser = app.oneshot(browser).await.unwrap();
    assert_eq!(browser.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn public_codex_web_search_requires_mcp_even_when_tool_choice_is_none() {
    let app = app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model":"codex/gpt-5.6-luna",
                        "input":"search",
                        "tools":[{"type":"web_search"}],
                        "tool_choice":"none",
                        "stream":false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "codex_web_search_requires_mcp");
}

#[tokio::test]
#[ignore = "requires installed OpenCode 1.18.31"]
async fn opencode_discovers_direct_remote_codex_web_tool() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app().await).await.unwrap();
    });

    let home = tempdir().unwrap();
    let config_dir = home.path().join(".config/opencode");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("opencode.json"),
        serde_json::to_vec_pretty(&json!({
            "mcp": {
                "codex_web": {
                    "type": "remote",
                    "url": format!("http://{address}/v1/mcp"),
                    "enabled": true,
                    "oauth": false,
                    "headers": {"Authorization": format!("Bearer {KEY}")},
                    "timeout": 300000
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let output = tokio::process::Command::new("opencode")
        .args(["mcp", "list"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .output()
        .await
        .unwrap();
    server.abort();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("codex_web"), "{stdout}");
    assert!(
        stdout.to_ascii_lowercase().contains("connected"),
        "{stdout}"
    );
}
