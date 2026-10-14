use std::collections::BTreeMap;
use std::sync::Arc;

use openproxy::core::executor::ClientPool;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use openproxy::core::executor::{
    convert_openai_sse_to_standard, CodexExecutionRequest, CodexExecutor, CodexExecutorError,
    CodexExecutorResponse,
};

fn connection(provider: &str) -> ProviderConnection {
    ProviderConnection {
        id: format!("{provider}-conn"),
        provider: provider.to_string(),
        auth_type: "apikey".into(),
        name: Some(provider.into()),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: None,
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: Some("sk-test-codex-key".into()),
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

fn connection_with_access_token(provider: &str, token: &str) -> ProviderConnection {
    let mut conn = connection(provider);
    conn.api_key = None;
    conn.access_token = Some(token.into());
    conn
}

fn provider_node() -> ProviderNode {
    ProviderNode {
        id: "codex-node".into(),
        r#type: "openai-compatible".into(),
        name: "Codex Node".into(),
        prefix: Some("codex".into()),
        api_type: Some("responses".into()),
        base_url: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

async fn execute_and_capture_body(model: &str, body: Value) -> Value {
    let (_, request) = execute_and_capture(CodexExecutionRequest {
        model: model.into(),
        body,
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    })
    .await;
    serde_json::from_slice(&request.body).expect("Codex request JSON")
}

async fn execute_and_capture(
    request: CodexExecutionRequest,
) -> (CodexExecutorResponse, wiremock::Request) {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    "data: {\"type\":\"response.done\",\"response\":{}}\n\n",
                    "text/event-stream",
                ),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let mut node = provider_node();
    node.base_url = Some(format!("{}/responses", upstream.uri()));
    let executor = CodexExecutor::new(Arc::new(ClientPool::new()), Some(node)).unwrap();
    let response = executor.execute(request).await.expect("execute request");

    let requests = upstream
        .received_requests()
        .await
        .expect("captured Codex request");
    (response, requests.into_iter().next().unwrap())
}

#[test]
fn codex_executor_new_succeeds_with_valid_pool() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool.clone(), None);
    assert!(executor.is_ok());
    assert!(Arc::ptr_eq(executor.as_ref().unwrap().pool(), &pool));
}

#[test]
fn codex_executor_new_succeeds_with_provider_node() {
    let pool = Arc::new(ClientPool::new());
    let node = provider_node();
    let executor = CodexExecutor::new(pool, Some(node));
    assert!(executor.is_ok());
}

#[test]
fn codex_executor_parse_codex_model_with_codex_prefix() {
    assert_eq!(CodexExecutor::parse_codex_model("codex/o4-mini"), "o4-mini");
    assert_eq!(
        CodexExecutor::parse_codex_model("codex/o4-mini-high"),
        "o4-mini-high"
    );
    assert_eq!(CodexExecutor::parse_codex_model("codex/o3"), "o3");
    assert_eq!(CodexExecutor::parse_codex_model("codex/o3-mini"), "o3-mini");
    assert_eq!(
        CodexExecutor::parse_codex_model("codex/gpt-4-turbo"),
        "gpt-4-turbo"
    );
}

#[test]
fn codex_executor_parse_codex_model_without_prefix() {
    assert_eq!(CodexExecutor::parse_codex_model("o4-mini"), "o4-mini");
    assert_eq!(CodexExecutor::parse_codex_model("o3"), "o3");
    assert_eq!(CodexExecutor::parse_codex_model("gpt-4"), "gpt-4");
    assert_eq!(
        CodexExecutor::parse_codex_model("gpt-4-turbo"),
        "gpt-4-turbo"
    );
}

#[test]
fn codex_sse_conversion_standard_format() {
    let openai_sse = b"event: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\nevent: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}\n\nevent: response.done\ndata: {\"type\":\"response.done\"}\n";

    let result = convert_openai_sse_to_standard(openai_sse);
    let result_str = String::from_utf8(result).unwrap();

    assert!(result_str.contains(
        "data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}"
    ));
    assert!(result_str.contains("data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}"));
    assert!(result_str.contains("data: {\"type\":\"response.done\"}"));
    assert!(result_str.contains("\n\n"));
}

#[test]
fn codex_sse_conversion_empty_input() {
    let result = convert_openai_sse_to_standard(b"");
    assert!(result.is_empty());
}

#[test]
fn codex_sse_conversion_standard_format_unchanged() {
    let standard_sse = b"data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n";
    let result = convert_openai_sse_to_standard(standard_sse);
    let result_str = String::from_utf8(result).unwrap();

    assert!(result_str.contains("data: {\"type\":\"content.delta\""));
    assert!(result_str.contains("\n\n"));
}

#[test]
fn codex_sse_conversion_skips_event_lines() {
    let input = b"event: message\ndata: {\"type\":\"content.delta\"}\n\nevent: done\ndata: {\"type\":\"response.done\"}\n";

    let result = convert_openai_sse_to_standard(input);
    let result_str = String::from_utf8(result).unwrap();

    assert!(!result_str.contains("event:"));
    assert!(result_str.contains("data: {\"type\":\"content.delta\"}"));
    assert!(result_str.contains("data: {\"type\":\"response.done\"}"));
}

#[tokio::test]
async fn codex_executor_execute_missing_credentials_fails() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: ProviderConnection {
            id: "test".into(),
            provider: "codex".into(),
            auth_type: "apikey".into(),
            name: None,
            priority: None,
            is_active: None,
            created_at: None,
            updated_at: None,
            display_name: None,
            email: None,
            global_priority: None,
            default_model: None,
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: None,
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
        },
        proxy: None,
    };

    let result = executor.execute(req).await;
    assert!(result.is_err());

    if let Err(e) = result {
        assert!(matches!(e, CodexExecutorError::MissingCredentials(_)));
    }
}

#[tokio::test]
async fn codex_executor_execute_returns_correct_url() {
    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let (response, request) = execute_and_capture(req).await;
    assert_eq!(request.url.path(), "/responses");
    assert_eq!(
        response.url,
        format!(
            "http://{}/responses",
            request.headers["host"].to_str().unwrap()
        )
    );
}

#[tokio::test]
async fn codex_executor_execute_returns_correct_headers() {
    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: true,
        credentials: connection("codex"),
        proxy: None,
    };

    let (response, _) = execute_and_capture(req).await;
    assert_eq!(
        response
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer sk-test-codex-key"
    );
    assert_eq!(
        response
            .headers
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        response.headers.get("accept").unwrap().to_str().unwrap(),
        "text/event-stream"
    );
}

#[tokio::test]
async fn codex_executor_execute_access_token_preferred() {
    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: connection_with_access_token("codex", "oauth-token-preferred"),
        proxy: None,
    };

    let (response, _) = execute_and_capture(req).await;
    assert_eq!(
        response
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer oauth-token-preferred"
    );
}

#[tokio::test]
async fn codex_executor_execute_non_streaming_no_accept_header() {
    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let (response, _) = execute_and_capture(req).await;
    // Parity: codex force-streams upstream regardless of client preference,
    // so the SSE Accept header is always set.
    assert_eq!(
        response.headers.get("accept").map(|v| v.to_str().unwrap()),
        Some("text/event-stream")
    );
}

#[tokio::test]
async fn codex_executor_uses_effective_cache_key_for_session_affinity() {
    let input = json!([
        {"role":"user", "content":"initial task", "session_id":"legacy-item-session"},
        {"role":"assistant", "content":[{"type":"output_text", "text":"answer"}]},
        {"type":"configuration_update", "reasoning":{"effort":"low"}},
        {"role":"user", "content":"follow-up"}
    ]);
    let mut credentials = connection_with_access_token("codex", "fixture-oauth-access");
    credentials.email = Some("fixture-account@example.invalid".into());
    credentials
        .provider_specific_data
        .insert("chatgptAccountId".into(), json!("fixture-account-id"));
    let (_, request) = execute_and_capture(CodexExecutionRequest {
        model: "codex/gpt-6.1-sol".into(),
        body: json!({
            "input":input,
            "reasoning":{"effort":"high"},
            "prompt_cache_key":"test-session-affinity"
        }),
        stream: true,
        credentials,
        proxy: None,
    })
    .await;
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "gpt-6.1-sol");
    assert_eq!(body["input"], input);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["prompt_cache_key"], "test-session-affinity");
    assert_eq!(
        request.headers["authorization"],
        "Bearer fixture-oauth-access"
    );
    assert_eq!(request.headers["chatgpt-account-id"], "fixture-account-id");
    assert_eq!(request.headers["accept"], "text/event-stream");
    for name in ["content-type", "originator", "version", "user-agent"] {
        assert!(request.headers.contains_key(name), "missing {name}");
    }
    assert_eq!(
        request
            .headers
            .get("session-id")
            .map(|value| value.as_bytes()),
        Some(b"test-session-affinity".as_slice())
    );
    assert!(!request.headers.contains_key("session_id"));
}

#[tokio::test]
async fn codex_executor_affinity_respects_final_key_and_compact_boundary() {
    for (key, legacy, compact, affinity) in [
        (
            Some("another-session"),
            None,
            false,
            Some("another-session"),
        ),
        (
            None,
            Some("legacy-input-session"),
            false,
            Some("legacy-input-session"),
        ),
        (None, None, false, None),
        (
            Some("invalid\r\nkey"),
            Some("do-not-substitute"),
            false,
            None,
        ),
        (Some("compact-key"), None, true, None),
    ] {
        let mut body = json!({"input":[{"role":"user", "content":"fixture"}]});
        if let Some(key) = key {
            body["prompt_cache_key"] = json!(key);
        }
        if let Some(legacy) = legacy {
            body["input"][0]["session_id"] = json!(legacy);
        }
        body["_compact"] = json!(compact);
        let mut credentials = connection("codex");
        credentials.email = Some("never-an-affinity@example.invalid".into());
        let (_, request) = execute_and_capture(CodexExecutionRequest {
            model: "codex/gpt-6.1-sol".into(),
            body,
            stream: false,
            credentials,
            proxy: None,
        })
        .await;
        assert_eq!(
            request.url.path(),
            if compact {
                "/responses/compact"
            } else {
                "/responses"
            }
        );
        assert_eq!(
            request
                .headers
                .get("session-id")
                .map(|value| value.as_bytes()),
            affinity.map(str::as_bytes)
        );
        assert!(!request.headers.contains_key("session_id"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body.get("prompt_cache_key"),
            key.or(legacy).map(|value| json!(value)).as_ref()
        );
        assert!(body.get("_compact").is_none());
    }
}

#[tokio::test]
async fn codex_executor_execute_multiple_messages_input() {
    let transformed = execute_and_capture_body(
        "codex/o4-mini",
        json!({
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi"},
                {"role": "user", "content": "There"}
            ]
        }),
    )
    .await;
    // Parity: multi-message input is forwarded as typed input_text items.
    let input_items = transformed["input"].as_array().expect("input array");
    let texts: Vec<&str> = input_items
        .iter()
        .filter_map(|it| it.pointer("/content/0/text").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(texts, vec!["Hello", "Hi", "There"]);
}

#[tokio::test]
async fn codex_executor_execute_transforms_model_name() {
    let transformed = execute_and_capture_body(
        "codex/o3-mini",
        json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
    )
    .await;
    assert_eq!(transformed["model"], "o3-mini");
}

#[tokio::test]
async fn codex_executor_execute_without_codex_prefix() {
    let transformed = execute_and_capture_body(
        "o4-mini",
        json!({
            "messages": [{"role": "user", "content": "Hello"}]
        }),
    )
    .await;
    assert_eq!(transformed["model"], "o4-mini");
}

#[tokio::test]
async fn codex_executor_execute_empty_messages_fails() {
    let pool = Arc::new(ClientPool::new());
    let executor = CodexExecutor::new(pool, None).unwrap();

    let req = CodexExecutionRequest {
        model: "codex/o4-mini".into(),
        body: json!({
            "messages": []
        }),
        stream: false,
        credentials: connection("codex"),
        proxy: None,
    };

    let result = executor.execute(req).await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        CodexExecutorError::UnsupportedFormat(_)
    ));
}

#[tokio::test]
async fn codex_executor_execute_all_params_copied() {
    let transformed = execute_and_capture_body(
        "codex/o4-mini",
        json!({
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false,
            "temperature": 0.7,
            "max_tokens": 1000,
            "top_p": 0.9,
            "stop": ["END"]
        }),
    )
    .await;
    // Parity (codex.js:462-479): unsupported sampling params are deleted —
    // temperature, top_p, max_tokens. `stop` is preserved.
    for key in ["temperature", "top_p", "max_tokens"] {
        assert!(transformed.get(key).is_none(), "{key} must be stripped");
    }
    assert_eq!(transformed["stop"], json!(["END"]));
}

#[tokio::test]
async fn codex_executor_execute_reasoning_param_stripped() {
    let transformed = execute_and_capture_body(
        "codex/o4-mini",
        json!({
            "messages": [{"role": "user", "content": "Hello"}],
            "reasoning": {"effort": "high"}
        }),
    )
    .await;
    // Parity (bead .38): the reasoning parameter is preserved — Codex
    // upstream consumes reasoning.effort for thinking effort control.
    assert_eq!(
        transformed
            .pointer("/reasoning/effort")
            .and_then(|v| v.as_str()),
        Some("high")
    );
}

#[test]
fn codex_executor_error_missing_credentials_is_debug() {
    let err = CodexExecutorError::MissingCredentials("API key required".to_string());
    let msg = format!("{:?}", err);
    assert!(msg.contains("MissingCredentials"));
}

#[test]
fn codex_executor_error_invalid_credentials_is_debug() {
    let err = CodexExecutorError::InvalidCredentials("Invalid token format".to_string());
    let msg = format!("{:?}", err);
    assert!(msg.contains("InvalidCredentials"));
}

#[test]
fn codex_executor_error_unsupported_format_is_debug() {
    let err = CodexExecutorError::UnsupportedFormat("Missing messages array".to_string());
    let msg = format!("{:?}", err);
    assert!(msg.contains("UnsupportedFormat"));
}
