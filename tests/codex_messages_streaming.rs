mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use futures_util::StreamExt;
use openproxy::server::state::AppState;
use openproxy::types::{CustomModel, ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::util::ServiceExt;

const UPSTREAM_MODEL: &str = "gpt-6-luna-2026-10-01";
const TIMEOUT: Duration = Duration::from_secs(3);

async fn codex_app(upstream: &MockUpstream) -> (TempTestDb, axum::Router) {
    let test_db = TempTestDb::new().await;
    let endpoint = upstream.url("/backend-api/codex/responses");
    test_db
        .db
        .update(move |db| {
            db.api_keys = vec![test_api_key()];
            db.provider_nodes = vec![ProviderNode {
                id: "codex".into(),
                r#type: "codex".into(),
                name: "Codex Messages loopback".into(),
                prefix: Some("cx".into()),
                api_type: Some("responses".into()),
                base_url: Some(endpoint),
                ..Default::default()
            }];
            db.provider_connections = vec![ProviderConnection {
                id: "codex-messages-fixture".into(),
                provider: "codex".into(),
                auth_type: "oauth".into(),
                priority: Some(1),
                is_active: Some(true),
                access_token: Some("fixture-codex-messages-access".into()),
                ..Default::default()
            }];
            db.custom_models = vec![CustomModel {
                provider_alias: "cx".into(),
                id: "gpt-6-luna".into(),
                r#type: "llm".into(),
                name: Some("Codex Messages fixture".into()),
                extra: BTreeMap::new(),
            }];
        })
        .await
        .expect("seed Codex Messages database");
    let app = openproxy::build_app(AppState::new(test_db.db.clone()));
    (test_db, app)
}

fn messages_request(stream: bool, messages: Value) -> Value {
    json!({
        "model": "cx/gpt-6-luna",
        "reasoning_effort": "xhigh",
        "max_tokens": 256,
        "stream": stream,
        "messages": messages,
        "tools": [
            {"name":"echo_probe", "description":"Echo a nonce", "input_schema":{
                "type":"object", "properties":{"nonce":{"type":"integer", "const":17}},
                "required":["nonce"], "additionalProperties":false
            }},
            {"name":"echo_secondary", "description":"Echo a second nonce", "input_schema":{
                "type":"object", "properties":{"nonce":{"type":"integer"}},
                "required":["nonce"]
            }}
        ]
    })
}

fn initial_request(stream: bool) -> Value {
    messages_request(
        stream,
        json!([{"role":"user", "content":"Run the smoke probe"}]),
    )
}

async fn post_messages(app: &axum::Router, body: Value) -> axum::response::Response {
    tokio::time::timeout(
        TIMEOUT,
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "Bearer test-key")
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("Messages request"),
        ),
    )
    .await
    .expect("Messages response headers must arrive without upstream EOF")
    .expect("Messages response")
}

fn event(data: Value) -> String {
    format!(
        "event: {}\ndata: {data}\n\n",
        data["type"].as_str().unwrap()
    )
}

fn created() -> String {
    event(json!({"type":"response.created", "response":{
        "id":"resp_messages_fixture", "created_at":1791072000,
        "model":UPSTREAM_MODEL, "status":"in_progress", "output":[]
    }}))
}

fn text_prefix(text: &str) -> Vec<String> {
    vec![
        created(),
        event(json!({"type":"response.in_progress", "response":{"status":"in_progress"}})),
        event(
            json!({"type":"response.output_item.added", "output_index":0, "item":{
                "id":"msg_fixture", "type":"message", "role":"assistant", "content":[]
            }}),
        ),
        event(
            json!({"type":"response.content_part.added", "item_id":"msg_fixture",
                "output_index":0, "content_index":0, "part":{"type":"output_text", "text":""}
            }),
        ),
        event(
            json!({"type":"response.output_text.delta", "item_id":"msg_fixture",
                "output_index":0, "content_index":0, "delta":text
            }),
        ),
    ]
}

fn completed(output: Vec<Value>) -> String {
    event(json!({"type":"response.completed", "response":{
        "id":"resp_messages_fixture", "created_at":1791072000,
        "model":UPSTREAM_MODEL, "status":"completed", "output":output,
        "usage":{"input_tokens":23, "output_tokens":7, "total_tokens":30,
            "input_tokens_details":{"cached_tokens":3}}
    }}))
}

fn text_fixture(text: &str) -> Vec<String> {
    let item = json!({"id":"msg_fixture", "type":"message", "role":"assistant",
        "status":"completed", "content":[{"type":"output_text", "text":text, "annotations":[]}]
    });
    let mut events = text_prefix(text);
    events.push(event(
        json!({"type":"response.output_text.done", "item_id":"msg_fixture",
            "output_index":0, "content_index":0, "text":text
        }),
    ));
    events.push(event(
        json!({"type":"response.output_item.done", "output_index":0, "item":item}),
    ));
    events.push(completed(vec![item]));
    events
}

fn tool_item(item_id: &str, call_id: &str, name: &str, arguments: &str) -> Value {
    json!({"id":item_id, "type":"function_call", "call_id":call_id,
        "name":name, "arguments":arguments, "status":"completed"})
}

fn tool_added(item_id: &str, call_id: &str, name: &str, index: usize) -> String {
    let mut item = tool_item(item_id, call_id, name, "");
    item["status"] = json!("in_progress");
    event(json!({"type":"response.output_item.added", "output_index":index, "item":item}))
}

fn tool_delta(item_id: &str, index: usize, delta: &str) -> String {
    event(
        json!({"type":"response.function_call_arguments.delta", "item_id":item_id,
            "output_index":index, "delta":delta
        }),
    )
}

fn tool_done(item: &Value, index: usize) -> Vec<String> {
    vec![
        event(
            json!({"type":"response.function_call_arguments.done", "item_id":item["id"],
                "output_index":index, "arguments":item["arguments"]
            }),
        ),
        event(json!({"type":"response.output_item.done", "output_index":index, "item":item})),
    ]
}

fn tool_fixture(name: &str, deltas: &[&str]) -> Vec<String> {
    let item = tool_item(
        "fc_echo_fixture",
        "call_echo_real_17",
        name,
        &deltas.concat(),
    );
    let mut events = vec![
        created(),
        tool_added("fc_echo_fixture", "call_echo_real_17", name, 0),
    ];
    events.extend(
        deltas
            .iter()
            .map(|delta| tool_delta("fc_echo_fixture", 0, delta)),
    );
    events.extend(tool_done(&item, 0));
    events.push(completed(vec![item]));
    events
}

fn parse_events(wire: &str) -> Vec<Value> {
    wire.split("\n\n")
        .filter(|frame| !frame.trim().is_empty())
        .map(|frame| {
            let name = frame
                .lines()
                .find_map(|line| line.strip_prefix("event: "))
                .unwrap_or_else(|| panic!("expected Anthropic SSE event, got {frame}"));
            let data = frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap_or_else(|| panic!("expected SSE data in {frame}"));
            let value: Value = serde_json::from_str(data).expect("Anthropic SSE JSON");
            assert_eq!(value["type"], name, "SSE event/data type mismatch: {frame}");
            assert!(
                matches!(
                    name,
                    "message_start"
                        | "content_block_start"
                        | "content_block_delta"
                        | "content_block_stop"
                        | "message_delta"
                        | "message_stop"
                        | "error"
                ),
                "upstream/pivot event leaked: {frame}"
            );
            value
        })
        .collect()
}

async fn read_stream(response: axum::response::Response) -> Vec<Value> {
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let bytes = tokio::time::timeout(TIMEOUT, to_bytes(response.into_body(), 4 * 1024 * 1024))
        .await
        .expect("Messages body must finish without waiting indefinitely for upstream EOF")
        .expect("Messages SSE body");
    parse_events(std::str::from_utf8(&bytes).expect("UTF-8 SSE"))
}

fn assert_lifecycle(events: &[Value], stop_reason: &str) {
    assert_eq!(
        events.first().unwrap()["type"],
        "message_start",
        "{events:?}"
    );
    assert_eq!(events.last().unwrap()["type"], "message_stop", "{events:?}");
    let mut open = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut starts = 0;
    let mut deltas = 0;
    let mut stops = 0;
    for e in events {
        match e["type"].as_str().unwrap() {
            "message_start" => {
                starts += 1;
                assert_eq!(e["message"]["type"], "message");
                assert_eq!(e["message"]["role"], "assistant");
                assert_eq!(e["message"]["model"], UPSTREAM_MODEL);
                assert_eq!(e["message"]["id"], "resp_messages_fixture");
            }
            "content_block_start" => {
                assert_eq!(deltas, 0, "content after message_delta");
                let index = e["index"].as_u64().expect("block index");
                assert!(seen.insert(index), "duplicate block start: {events:?}");
                assert!(open.insert(index));
            }
            "content_block_delta" => {
                assert!(
                    open.contains(&e["index"].as_u64().unwrap()),
                    "delta outside block: {events:?}"
                );
            }
            "content_block_stop" => {
                assert!(
                    open.remove(&e["index"].as_u64().unwrap()),
                    "unmatched block stop: {events:?}"
                );
            }
            "message_delta" => {
                assert!(
                    open.is_empty(),
                    "message finished with open blocks: {events:?}"
                );
                deltas += 1;
                assert_eq!(e["delta"]["stop_reason"], stop_reason);
                // Anthropic input_tokens excludes the separately reported cache read.
                assert_eq!(e["usage"]["input_tokens"], 20);
                assert_eq!(e["usage"]["cache_read_input_tokens"], 3);
                assert_eq!(e["usage"]["output_tokens"], 7);
            }
            "message_stop" => stops += 1,
            other => panic!("unexpected successful event {other}: {events:?}"),
        }
    }
    assert_eq!((starts, deltas, stops), (1, 1, 1), "{events:?}");
    assert!(!seen.is_empty(), "expected content blocks");
    assert!(open.is_empty());
}

fn content(events: &[Value]) -> Vec<Value> {
    let mut blocks = BTreeMap::new();
    let mut arguments = BTreeMap::<u64, String>::new();
    for e in events {
        let Some(index) = e["index"].as_u64() else {
            continue;
        };
        match e["type"].as_str().unwrap() {
            "content_block_start" => {
                blocks.insert(index, e["content_block"].clone());
            }
            "content_block_delta" => match e["delta"]["type"].as_str().unwrap() {
                "text_delta" => {
                    let block = blocks.get_mut(&index).expect("started text block");
                    let text = format!(
                        "{}{}",
                        block["text"].as_str().unwrap(),
                        e["delta"]["text"].as_str().unwrap()
                    );
                    block["text"] = json!(text);
                }
                "input_json_delta" => arguments
                    .entry(index)
                    .or_default()
                    .push_str(e["delta"]["partial_json"].as_str().unwrap()),
                other => panic!("unexpected delta type {other}"),
            },
            _ => {}
        }
    }
    for (index, args) in arguments {
        blocks.get_mut(&index).unwrap()["input"] =
            serde_json::from_str(&args).expect("complete JSON tool arguments");
    }
    blocks.into_values().collect()
}

fn assert_failure(events: &[Value]) {
    assert_eq!(
        events.iter().filter(|e| e["type"] == "error").count(),
        1,
        "{events:?}"
    );
    assert_eq!(events.last().unwrap()["type"], "error", "{events:?}");
    assert!(
        events.iter().all(|e| e["type"] != "message_stop"),
        "false successful stop: {events:?}"
    );
    assert!(
        events.iter().all(|e| e["type"] != "message_delta"),
        "false successful finish: {events:?}"
    );
    let error = &events.last().unwrap()["error"];
    assert!(
        error["type"].is_string() || error["code"].is_string(),
        "{error}"
    );
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "{error}"
    );
}

#[tokio::test]
async fn text_preserves_upstream_model_usage_and_anthropic_lifecycle() {
    let upstream = MockUpstream::start([ScriptedResponse::sse(text_fixture("SMOKE_OK"))]).await;
    let (_db, app) = codex_app(&upstream).await;
    let mut request = messages_request(
        true,
        json!([{"role":"user", "content":"Reply exactly SMOKE_OK."}]),
    );
    request.as_object_mut().unwrap().remove("tools");
    let events = read_stream(post_messages(&app, request).await).await;
    assert_lifecycle(&events, "end_turn");
    assert_eq!(
        content(&events),
        vec![json!({"type":"text", "text":"SMOKE_OK"})]
    );
    let requests = upstream.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/backend-api/codex/responses");
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["model"], "gpt-6-luna");
    assert_eq!(sent.pointer("/reasoning/effort"), Some(&json!("xhigh")));
}

async fn tool_roundtrip(stream: bool) {
    let upstream = MockUpstream::start([
        ScriptedResponse::sse(tool_fixture(
            "echo_probe",
            &["{", "\"nonce\":", "1", "7", "}"],
        )),
        ScriptedResponse::sse(text_fixture("TOOL_ROUNDTRIP_OK")),
    ])
    .await;
    let (_db, app) = codex_app(&upstream).await;
    let first_prompt = "Call echo_probe with nonce 17";
    let mut first_request =
        messages_request(stream, json!([{"role":"user", "content":first_prompt}]));
    first_request["tool_choice"] = json!({"type":"tool", "name":"echo_probe"});
    let first = post_messages(&app, first_request).await;
    let first_content = if stream {
        let events = read_stream(first).await;
        assert_lifecycle(&events, "tool_use");
        content(&events)
    } else {
        let message = read_json(first).await;
        assert_eq!(message["stop_reason"], "tool_use");
        message["content"].as_array().unwrap().clone()
    };
    assert_eq!(
        first_content,
        vec![json!({"type":"tool_use", "id":"call_echo_real_17",
        "name":"echo_probe", "input":{"nonce":17}})]
    );
    let second = post_messages(
        &app,
        messages_request(
            stream,
            json!([
                {"role":"user", "content":first_prompt},
                {"role":"assistant", "content":first_content},
                {"role":"user", "content":[{"type":"tool_result", "tool_use_id":"call_echo_real_17",
                    "content":"echo_probe nonce=17"}]}
            ]),
        ),
    )
    .await;
    let second_content = if stream {
        let events = read_stream(second).await;
        assert_lifecycle(&events, "end_turn");
        content(&events)
    } else {
        let message = read_json(second).await;
        assert_eq!(message["stop_reason"], "end_turn");
        message["content"].as_array().unwrap().clone()
    };
    assert_eq!(
        second_content,
        vec![json!({"type":"text", "text":"TOOL_ROUNDTRIP_OK"})]
    );
    let requests = upstream.requests().await;
    assert_eq!(
        requests.len(),
        2,
        "client, not proxy, owns the tool roundtrip"
    );
    for request in &requests {
        let sent: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(sent["model"], "gpt-6-luna");
        assert_eq!(sent.pointer("/reasoning/effort"), Some(&json!("xhigh")));
    }
    let sent: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let input = sent["input"].as_array().expect("Responses input");
    let outputs: Vec<_> = input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect();
    assert_eq!(outputs.len(), 1, "{sent}");
    assert_eq!(outputs[0]["call_id"], "call_echo_real_17");
    assert_eq!(outputs[0]["output"], "echo_probe nonce=17");
    let call = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("prior assistant call preserved");
    assert_eq!(call["call_id"], "call_echo_real_17");
    assert_eq!(call["name"], "echo_probe");
    assert_eq!(
        serde_json::from_str::<Value>(call["arguments"].as_str().unwrap()).unwrap(),
        json!({"nonce":17})
    );
}

#[tokio::test]
async fn streaming_tool_arguments_and_client_tool_result_roundtrip() {
    tool_roundtrip(true).await;
}

#[tokio::test]
async fn nonstreaming_tool_and_client_tool_result_roundtrip() {
    tool_roundtrip(false).await;
}

async fn read_json(response: axum::response::Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    let bytes = tokio::time::timeout(TIMEOUT, to_bytes(response.into_body(), 4 * 1024 * 1024))
        .await
        .expect("nonstreaming body timeout")
        .expect("nonstreaming body");
    serde_json::from_slice(&bytes).expect("nonstreaming Messages JSON")
}

#[tokio::test]
async fn nonstreaming_text_remains_messages_json() {
    let upstream = MockUpstream::start([ScriptedResponse::sse(text_fixture("SMOKE_OK"))]).await;
    let (_db, app) = codex_app(&upstream).await;
    let message = read_json(post_messages(&app, initial_request(false)).await).await;
    assert_eq!(message["type"], "message");
    assert_eq!(message["role"], "assistant");
    assert_eq!(message["model"], "gpt-6-luna");
    assert_eq!(message["stop_reason"], "end_turn");
    assert_eq!(
        message["content"],
        json!([{"type":"text", "text":"SMOKE_OK"}])
    );
    assert_eq!(message["usage"]["input_tokens"], 23);
    assert_eq!(message["usage"]["output_tokens"], 7);
}

#[tokio::test]
async fn interleaved_tool_calls_keep_real_ids_names_and_arguments_separate() {
    let a = tool_item("fc_a", "call_a_real", "echo_probe", "{\"nonce\":17}");
    let b = tool_item("fc_b", "call_b_real", "echo_secondary", "{\"nonce\":29}");
    let mut fixture = vec![
        created(),
        tool_added("fc_a", "call_a_real", "echo_probe", 0),
        tool_added("fc_b", "call_b_real", "echo_secondary", 1),
        tool_delta("fc_b", 1, "{\"nonce\":"),
        tool_delta("fc_a", 0, "{\"nonce\":1"),
        tool_delta("fc_b", 1, "29"),
        tool_delta("fc_a", 0, "7}"),
        tool_delta("fc_b", 1, "}"),
    ];
    fixture.extend(tool_done(&b, 1));
    fixture.extend(tool_done(&a, 0));
    fixture.push(completed(vec![a, b]));
    let upstream = MockUpstream::start([ScriptedResponse::sse(fixture)]).await;
    let (_db, app) = codex_app(&upstream).await;
    let events = read_stream(post_messages(&app, initial_request(true)).await).await;
    assert_lifecycle(&events, "tool_use");
    assert_eq!(
        content(&events),
        vec![
            json!({"type":"tool_use", "id":"call_a_real", "name":"echo_probe", "input":{"nonce":17}}),
            json!({"type":"tool_use", "id":"call_b_real", "name":"echo_secondary", "input":{"nonce":29}}),
        ]
    );
}

#[tokio::test]
async fn split_and_coalesced_chunks_have_identical_semantic_events() {
    let fixture = tool_fixture("echo_probe", &["{\"nonce\":", "17", "}"]).concat();
    // Include splits inside both SSE framing and JSON, not only between events.
    let split: Vec<_> = fixture
        .as_bytes()
        .chunks(11)
        .map(bytes::Bytes::copy_from_slice)
        .collect();
    let upstream = MockUpstream::start([
        ScriptedResponse::sse(split),
        ScriptedResponse::sse([fixture]),
    ])
    .await;
    let (_db, app) = codex_app(&upstream).await;
    let fragmented = read_stream(post_messages(&app, initial_request(true)).await).await;
    let coalesced = read_stream(post_messages(&app, initial_request(true)).await).await;
    assert_lifecycle(&fragmented, "tool_use");
    assert_lifecycle(&coalesced, "tool_use");
    assert_eq!(fragmented, coalesced);
}

async fn upstream_failure(terminal: Option<Value>, transport_failure: bool) {
    let mut fixture = text_prefix("PARTIAL_TEXT");
    if let Some(terminal) = terminal {
        fixture.push(event(terminal));
    }
    let mut script = ScriptedResponse::sse(fixture);
    if transport_failure {
        script = script.failing_after_chunks();
    }
    let upstream = MockUpstream::start([script]).await;
    let (_db, app) = codex_app(&upstream).await;
    let events = read_stream(post_messages(&app, initial_request(true)).await).await;
    assert_failure(&events);
    assert_eq!(
        content(&events),
        vec![json!({"type":"text", "text":"PARTIAL_TEXT"})]
    );
    assert_eq!(upstream.request_count().await, 1);
}

#[tokio::test]
async fn response_failed_is_an_error_not_successful_text() {
    upstream_failure(
        Some(json!({"type":"response.failed", "response":{
            "status":"failed", "error":{"code":"fixture_failed", "message":"generation failed"}
        }})),
        false,
    )
    .await;
}

#[tokio::test]
async fn upstream_error_is_an_error_not_successful_text() {
    upstream_failure(
        Some(json!({"type":"error", "code":"fixture_error", "message":"generation error"})),
        false,
    )
    .await;
}

#[tokio::test]
async fn response_incomplete_is_an_error_not_successful_stop() {
    upstream_failure(
        Some(json!({"type":"response.incomplete", "response":{
            "status":"incomplete", "incomplete_details":{"reason":"max_output_tokens"}
        }})),
        false,
    )
    .await;
}

#[tokio::test]
async fn eof_without_terminal_is_an_error_not_successful_stop() {
    upstream_failure(None, false).await;
}

#[tokio::test]
async fn truncated_upstream_body_is_an_error_not_successful_stop() {
    upstream_failure(None, true).await;
}

#[tokio::test]
async fn truncated_terminal_sse_frame_is_an_error_not_successful_stop() {
    let mut fixture = text_prefix("PARTIAL_TEXT");
    fixture.push(
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":".into(),
    );
    let upstream = MockUpstream::start([ScriptedResponse::sse(fixture)]).await;
    let (_db, app) = codex_app(&upstream).await;
    let events = read_stream(post_messages(&app, initial_request(true)).await).await;
    assert_failure(&events);
    assert_eq!(
        content(&events),
        vec![json!({"type":"text", "text":"PARTIAL_TEXT"})]
    );
}

#[tokio::test]
async fn nonterminal_text_delta_arrives_before_eof_and_client_drop_releases_upstream() {
    let body_dropped = Arc::new(Notify::new());
    let upstream = MockUpstream::start([ScriptedResponse::sse(text_prefix("LIVE_TEXT_DELTA"))
        .holding_eof(Arc::new(Notify::new()))
        .notifying_on_body_drop(body_dropped.clone())])
    .await;
    let (_db, app) = codex_app(&upstream).await;
    let response = post_messages(&app, initial_request(true)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let events = tokio::time::timeout(TIMEOUT, async {
        let mut wire = Vec::new();
        loop {
            let chunk = body
                .next()
                .await
                .expect("nonterminal stream remains open")
                .expect("SSE chunk");
            wire.extend_from_slice(&chunk);
            let text = std::str::from_utf8(&wire).expect("UTF-8 SSE");
            // Parse only complete frames; headers or message_start alone do not satisfy this test.
            if let Some(end) = text.rfind("\n\n") {
                let events = parse_events(&text[..end + 2]);
                if events.iter().any(|e| {
                    e["delta"]["type"] == "text_delta" && e["delta"]["text"] == "LIVE_TEXT_DELTA"
                }) {
                    break events;
                }
            }
        }
    })
    .await
    .expect("actual Anthropic text_delta must arrive before upstream EOF");
    assert!(events.iter().all(|e| e["type"] != "message_stop"));
    drop(body);
    tokio::time::timeout(TIMEOUT, body_dropped.notified())
        .await
        .expect("dropping client body must cancel and release upstream body");
    assert_eq!(upstream.request_count().await, 1);
}

#[tokio::test]
async fn completed_stream_finishes_once_and_releases_upstream_without_eof() {
    let body_dropped = Arc::new(Notify::new());
    let mut fixture = text_fixture("SMOKE_OK");
    fixture.push(fixture.last().unwrap().clone());
    fixture.push(event(json!({"type":"response.failed", "response":{
        "id":"resp_messages_fixture", "status":"failed",
        "error":{"code":"fixture_late_failure", "message":"ignore failure after completion"}
    }})));
    // Deliver the duplicate and late failure in the same upstream chunk as completion.
    let upstream = MockUpstream::start([ScriptedResponse::sse([fixture.concat()])
        .holding_eof(Arc::new(Notify::new()))
        .notifying_on_body_drop(body_dropped.clone())])
    .await;
    let (_db, app) = codex_app(&upstream).await;
    let events = read_stream(post_messages(&app, initial_request(true)).await).await;
    assert_lifecycle(&events, "end_turn");
    assert_eq!(
        content(&events),
        vec![json!({"type":"text", "text":"SMOKE_OK"})]
    );
    tokio::time::timeout(TIMEOUT, body_dropped.notified())
        .await
        .expect("response.completed must release upstream without upstream EOF");
}

#[tokio::test]
async fn missing_real_tool_call_id_or_name_is_rejected() {
    for (call_id, name) in [("", "echo_probe"), ("call_real", "")] {
        let mut fixture = text_prefix("PARTIAL_TEXT");
        fixture.push(tool_added("fc_invalid", call_id, name, 1));
        fixture.push(completed(vec![]));
        let upstream = MockUpstream::start([ScriptedResponse::sse(fixture)]).await;
        let (_db, app) = codex_app(&upstream).await;
        let events = read_stream(post_messages(&app, initial_request(true)).await).await;
        assert_failure(&events);
        assert!(
            content(&events)
                .iter()
                .all(|block| block["type"] != "tool_use"),
            "must not fabricate tool identity: {events:?}"
        );
    }
}
