mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use common::test_api_key;
use futures_util::StreamExt;
use openproxy::core::stream_framing::{
    FrameError, LineFramer, SseFramer, TextStreamFramer, TextStreamMode,
};
use openproxy::core::translator::registry::{global_registry, Format, ResponseTransformState};
use openproxy::server::state::AppState;
use openproxy::types::{ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::util::ServiceExt;

#[derive(Debug, PartialEq, Eq)]
struct OwnedEvent {
    raw: String,
    data: Option<String>,
    event: Option<String>,
    id: Option<String>,
    retry: Option<u64>,
    comments: usize,
}

fn collect_with_chunks(input: &[u8], chunks: &[usize]) -> Result<Vec<OwnedEvent>, FrameError> {
    let mut framer = SseFramer::with_max_frame_bytes(2 * 1024 * 1024);
    let mut events = Vec::new();
    let mut start = 0;
    for &end in chunks {
        framer.feed(&input[start..end], |event| {
            events.push(OwnedEvent {
                raw: event.raw().to_string(),
                data: event.data().map(str::to_string),
                event: event.event().map(str::to_string),
                id: event.id().map(str::to_string),
                retry: event.retry(),
                comments: event.comment_count(),
            });
        })?;
        start = end;
    }
    framer.feed(&input[start..], |event| {
        events.push(OwnedEvent {
            raw: event.raw().to_string(),
            data: event.data().map(str::to_string),
            event: event.event().map(str::to_string),
            id: event.id().map(str::to_string),
            retry: event.retry(),
            comments: event.comment_count(),
        });
    })?;
    Ok(events)
}

#[test]
fn every_split_boundary_preserves_lf_crlf_fields_utf8_and_multiple_frames() {
    for fixture in [
        ": heartbeat\nevent: delta\nid: evt-1\nretry: 1500\ndata: hé\ndata: llo\n\ndata: {\"n\":2}\n\n",
        ": heartbeat\r\nevent: delta\r\nid: evt-1\r\nretry: 1500\r\ndata: hé\r\ndata: llo\r\n\r\ndata: {\"n\":2}\r\n\r\n",
    ] {
        let bytes = fixture.as_bytes();
        let expected = collect_with_chunks(bytes, &[]).unwrap();
        assert_eq!(expected.len(), 2);
        assert_eq!(expected[0].data.as_deref(), Some("hé\nllo"));
        assert_eq!(expected[0].event.as_deref(), Some("delta"));
        assert_eq!(expected[0].id.as_deref(), Some("evt-1"));
        assert_eq!(expected[0].retry, Some(1500));
        assert_eq!(expected[0].comments, 1);

        for split in 0..=bytes.len() {
            assert_eq!(collect_with_chunks(bytes, &[split]).unwrap(), expected, "split {split}");
        }
        let one_byte_boundaries: Vec<_> = (1..bytes.len()).collect();
        assert_eq!(collect_with_chunks(bytes, &one_byte_boundaries).unwrap(), expected);
    }
}

#[test]
fn scan_cursor_is_linear_for_one_byte_chunks() {
    let payload = format!("data: {}\n\n", "x".repeat(256 * 1024));
    let mut framer = SseFramer::with_max_frame_bytes(512 * 1024);
    let mut frames = 0;
    for byte in payload.as_bytes() {
        framer
            .feed(std::slice::from_ref(byte), |_| frames += 1)
            .unwrap();
    }
    assert_eq!(frames, 1);
    assert!(framer.scanned_bytes() <= payload.len());
}

#[test]
fn large_input_with_many_small_frames_never_becomes_retained_state() {
    let frame = b"data: x\n\n";
    let input = frame.repeat(200_000);
    let mut framer = SseFramer::with_max_frame_bytes(32);
    let mut frames = 0usize;
    framer.feed(&input, |_| frames += 1).unwrap();
    assert_eq!(frames, 200_000);
    assert_eq!(framer.pending_len(), 0);
    assert!(framer.buffer_capacity() <= 64 * 1024);
}

#[test]
fn frame_limit_checked_growth_and_utf8_validation_are_exact() {
    let mut exact = SseFramer::with_max_frame_bytes(8);
    exact.feed(b"12345678", |_| {}).unwrap();
    exact
        .finish(|event| assert_eq!(event.raw(), "12345678"))
        .unwrap();

    let mut over = SseFramer::with_max_frame_bytes(8);
    assert!(matches!(
        over.feed(b"123456789", |_| {}),
        Err(FrameError::FrameTooLarge { limit: 8 })
    ));

    let mut complete_over = SseFramer::with_max_frame_bytes(8);
    assert!(matches!(
        complete_over.feed(b"123456789\n\n", |_| {}),
        Err(FrameError::FrameTooLarge { limit: 8 })
    ));

    let utf8 = "data: café\n\n".as_bytes();
    let split = utf8.iter().position(|byte| *byte == 0xc3).unwrap() + 1;
    let mut framer = SseFramer::with_max_frame_bytes(128);
    let mut data = None;
    framer
        .feed(&utf8[..split], |_| panic!("incomplete UTF-8 event emitted"))
        .unwrap();
    framer
        .feed(&utf8[split..], |event| {
            data = event.data().map(str::to_string)
        })
        .unwrap();
    assert_eq!(data.as_deref(), Some("café"));

    let mut invalid = SseFramer::with_max_frame_bytes(128);
    assert_eq!(
        invalid.feed(b"data: \xff\n\n", |_| {}),
        Err(FrameError::InvalidUtf8)
    );
}

#[test]
fn eof_tail_line_mode_and_capacity_compaction_are_bounded() {
    let mut sse = SseFramer::with_max_frame_bytes(256 * 1024);
    let mut eof = None;
    sse.feed(b"data: final", |_| {}).unwrap();
    sse.finish(|event| eof = event.data().map(str::to_string))
        .unwrap();
    assert_eq!(eof.as_deref(), Some("final"));

    let mut lines = LineFramer::with_max_line_bytes(32);
    let mut output = Vec::new();
    lines
        .feed(b"one\r", |line| output.push(line.to_string()))
        .unwrap();
    lines
        .feed(b"\ntwo", |line| output.push(line.to_string()))
        .unwrap();
    lines.finish(|line| output.push(line.to_string())).unwrap();
    assert_eq!(output, ["one", "two"]);

    let mut compact = SseFramer::with_max_frame_bytes(256 * 1024);
    let input = format!("data: {}\n\ntail", "x".repeat(192 * 1024));
    compact.feed(input.as_bytes(), |_| {}).unwrap();
    assert_eq!(compact.pending_len(), 4);
    assert!(
        compact.buffer_capacity() < 64 * 1024,
        "tiny tail retained large backing allocation"
    );
}

#[test]
fn registry_translation_receives_complete_events_at_every_boundary() {
    let fixture = concat!(
        "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_c32\",\"model\":\"claude-c32\",\"usage\":{\"input_tokens\":2}}}\r\n\r\n",
        "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\r\n\r\n"
    );
    let translate = |parts: &[&[u8]]| {
        let mut state = ResponseTransformState::default();
        let mut output = Vec::new();
        for part in parts {
            output.extend(
                global_registry()
                    .translate_response(Format::Claude, Format::OpenAi, part, &mut state)
                    .unwrap(),
            );
        }
        output.extend(global_registry().finish_stream(Format::Claude, Format::OpenAi, &mut state));
        output
    };
    let expected = translate(&[fixture.as_bytes()]);
    assert!(expected.join("").contains("hello"));
    for split in 0..=fixture.len() {
        assert_eq!(
            normalize_generated_stream_metadata(translate(&[
                &fixture.as_bytes()[..split],
                &fixture.as_bytes()[split..],
            ])),
            normalize_generated_stream_metadata(expected.clone())
        );
    }
    let one_byte: Vec<&[u8]> = fixture.as_bytes().chunks(1).collect();
    assert_eq!(
        normalize_generated_stream_metadata(translate(&one_byte)),
        normalize_generated_stream_metadata(expected)
    );
}

fn translate_stream(source: Format, target: Format, parts: &[&[u8]]) -> Vec<String> {
    let mut state = ResponseTransformState::default();
    let mut output = Vec::new();
    for part in parts {
        output.extend(
            global_registry()
                .translate_response(source, target, part, &mut state)
                .unwrap(),
        );
    }
    output.extend(global_registry().finish_stream(source, target, &mut state));
    output
}

fn normalize_generated_stream_metadata(output: Vec<String>) -> Vec<String> {
    output
        .into_iter()
        .map(|line| {
            let data_start = if line.starts_with("data: ") {
                Some(0)
            } else {
                line.find("\ndata: ").map(|index| index + 1)
            };
            let Some(data_start) = data_start else {
                return line;
            };
            let prefix = &line[..data_start];
            let payload = line[data_start + "data: ".len()..].trim();
            let Ok(mut value) = serde_json::from_str::<Value>(payload) else {
                return line;
            };
            if let Some(object) = value.as_object_mut() {
                if object.contains_key("id") {
                    object.insert("id".to_string(), Value::String("generated".to_string()));
                }
                if object.contains_key("created") {
                    object.insert("created".to_string(), Value::from(0));
                }
            }
            format!(
                "{prefix}data: {}\n\n",
                serde_json::to_string(&value).unwrap()
            )
        })
        .collect()
}

#[test]
fn gemini_sse_and_ollama_records_survive_arbitrary_chunks() {
    let gemini = b"data: {\"responseId\":\"g32\",\"modelVersion\":\"gemini-c32\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"gemini-one-byte\"}]}}]}\r\n\r\n";
    let expected = translate_stream(Format::Gemini, Format::OpenAi, &[gemini]);
    assert!(expected.join("").contains("gemini-one-byte"));
    let one_byte = gemini.chunks(1).collect::<Vec<_>>();
    assert_eq!(
        translate_stream(Format::Gemini, Format::OpenAi, &one_byte),
        expected
    );

    let ollama = concat!(
        "{\"model\":\"llama\",\"message\":{\"role\":\"assistant\",\"content\":\"one\"},\"done\":false}\n",
        "{\"model\":\"llama\",\"message\":{\"role\":\"assistant\",\"content\":\"two\"},\"done\":true}\n"
    );
    let expected = translate_stream(Format::Ollama, Format::OpenAi, &[ollama.as_bytes()]);
    assert!(expected.join("").contains("one"));
    let one_byte = ollama.as_bytes().chunks(1).collect::<Vec<_>>();
    assert_eq!(
        normalize_generated_stream_metadata(translate_stream(
            Format::Ollama,
            Format::OpenAi,
            &one_byte,
        )),
        normalize_generated_stream_metadata(expected)
    );
}

#[test]
fn pivot_routes_feed_payloads_not_intermediate_sse_envelopes() {
    let claude = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-pivot\",\"model\":\"claude-pivot\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"pivot-claude\"}}\n\n"
    );
    for target in [Format::OpenAiResponses, Format::Codex] {
        let output = translate_stream(Format::Claude, target, &[claude.as_bytes()]).join("");
        assert!(
            output.contains("pivot-claude"),
            "target {target:?}: {output}"
        );
        assert!(output.contains("response.output_text.delta"));
    }

    let responses = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"model\":\"gpt-pivot\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"pivot-responses\"}\n\n"
    );
    let output = translate_stream(
        Format::OpenAiResponses,
        Format::Gemini,
        &[responses.as_bytes()],
    )
    .join("");
    assert!(output.contains("pivot-responses"), "{output}");
    assert!(output.contains("candidates"), "{output}");
}

#[test]
fn registry_preserves_completed_output_before_same_chunk_overflow() {
    let valid = b"data: {\"responseId\":\"g32\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"before-overflow\"}]}}]}\n\n";
    let oversized = vec![b'x'; 257];
    let mut combined = valid.to_vec();
    combined.extend_from_slice(&oversized);

    let run = |parts: &[&[u8]]| {
        let mut state = ResponseTransformState {
            text_framer: Some(TextStreamFramer::with_max_frame_bytes(
                TextStreamMode::Sse,
                256,
            )),
            ..Default::default()
        };
        let mut output = Vec::new();
        let mut error = None;
        for part in parts {
            let batch = global_registry().translate_response_batch(
                Format::Gemini,
                Format::OpenAi,
                part,
                &mut state,
            );
            output.extend(batch.chunks);
            if batch.error.is_some() {
                error = batch.error;
                break;
            }
        }
        (output, error.map(|value| value.code))
    };

    let same_chunk = run(&[&combined]);
    let split = run(&[valid, &oversized]);
    assert_eq!(same_chunk, split);
    assert!(same_chunk.0.join("").contains("before-overflow"));
    assert_eq!(same_chunk.1, Some("upstream_sse_frame_too_large"));
}

async fn app_for(upstream: &MockUpstream) -> (axum::Router, TempTestDb) {
    let db = TempTestDb::new().await;
    db.db
        .update(|state| {
            state.api_keys = vec![test_api_key()];
            state.provider_nodes = vec![ProviderNode {
                id: "c32-openai".into(),
                r#type: "openai-compatible".into(),
                name: "C32 OpenAI".into(),
                prefix: Some("c32".into()),
                api_type: Some("chat".into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            state.provider_connections = vec![ProviderConnection {
                id: "c32-account".into(),
                provider: "c32-openai".into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                priority: Some(1),
                api_key: Some("c32-placeholder".into()),
                default_model: Some("gpt-c32".into()),
                ..Default::default()
            }];
        })
        .await
        .unwrap();
    (openproxy::build_app(AppState::new(db.db.clone())), db)
}

async fn app_for_native_provider(
    upstream: &MockUpstream,
    provider: &str,
    node_type: &str,
    prefix: &str,
    api_type: &str,
    model: &str,
) -> (axum::Router, TempTestDb) {
    let db = TempTestDb::new().await;
    db.db
        .update(|state| {
            state.api_keys = vec![test_api_key()];
            state.provider_nodes = vec![ProviderNode {
                id: provider.into(),
                r#type: node_type.into(),
                name: provider.into(),
                prefix: Some(prefix.into()),
                api_type: Some(api_type.into()),
                base_url: Some(upstream.url("/v1")),
                ..Default::default()
            }];
            state.provider_connections = vec![ProviderConnection {
                id: format!("{prefix}-account"),
                provider: provider.into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                priority: Some(1),
                api_key: Some("c32-placeholder".into()),
                default_model: Some(model.into()),
                ..Default::default()
            }];
        })
        .await
        .unwrap();
    (openproxy::build_app(AppState::new(db.db.clone())), db)
}

fn chat_request(stream: bool) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": "c32/gpt-c32",
                "messages": [{"role": "user", "content": "frame"}],
                "stream": stream,
                "stream_options": {"include_usage": true}
            })
            .to_string(),
        ))
        .unwrap()
}

fn responses_request() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": "c32/gpt-c32",
                "input": "frame",
                "stream": false
            })
            .to_string(),
        ))
        .unwrap()
}

fn native_responses_request(prefix: &str, stream: bool) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": format!("{prefix}/gpt-c32"),
                "input": "native",
                "stream": stream
            })
            .to_string(),
        ))
        .unwrap()
}

fn native_messages_request(prefix: &str, stream: bool) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": format!("{prefix}/claude-c32"),
                "messages": [{"role": "user", "content": "native"}],
                "max_tokens": 32,
                "stream": stream
            })
            .to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn native_stream_is_verbatim_immediate_and_cancellation_is_preserved() {
    let event = b"event: delta\r\nid: 9\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"native\"}}]}\r\n\r\n";
    let release = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let upstream =
        MockUpstream::start([
            ScriptedResponse::sse(event.iter().copied().map(|byte| vec![byte]))
                .holding_eof(release.clone())
                .notifying_on_body_drop(dropped.clone()),
        ])
        .await;
    let (app, _db) = app_for(&upstream).await;
    let response = app.oneshot(chat_request(true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let observed = tokio::time::timeout(Duration::from_secs(2), async {
        let mut observed = Vec::new();
        while observed.len() < event.len() {
            observed.extend_from_slice(&body.next().await.unwrap().unwrap());
        }
        observed
    })
    .await
    .expect("native event arrived before held EOF");
    assert_eq!(observed, event);
    drop(body);
    tokio::time::timeout(Duration::from_secs(1), dropped.notified())
        .await
        .expect("cancellation dropped upstream body");
    release.notify_waiters();
    upstream.shutdown().await;
}

#[tokio::test]
async fn overflow_is_502_before_commit_and_terminal_event_after_commit() {
    let oversized = format!("data: {}", "x".repeat(1024 * 1024 + 1));
    let precommit = MockUpstream::start([ScriptedResponse::sse([oversized.clone()])]).await;
    let (app, _db) = app_for(&precommit).await;
    let response = app.oneshot(responses_request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap_or_else(|error| {
        panic!(
            "invalid translated body {error}: {}",
            String::from_utf8_lossy(&body)
        )
    });
    assert_eq!(value["error"]["code"], "upstream_sse_frame_too_large");
    assert_eq!(precommit.request_count().await, 1);
    precommit.shutdown().await;

    let postcommit = MockUpstream::start([ScriptedResponse::sse([
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first\"}}]}\n\n".to_string(),
        oversized,
    ])])
    .await;
    let (app, _db) = app_for(&postcommit).await;
    let response = app.oneshot(chat_request(true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("first"));
    assert!(text.contains("upstream_sse_frame_too_large"));
    assert_eq!(text.matches("upstream_sse_frame_too_large").count(), 1);
    assert_eq!(postcommit.request_count().await, 1);
    postcommit.shutdown().await;
}

#[tokio::test]
async fn native_responses_and_messages_streams_preserve_exact_wire_bytes() {
    let responses_wire = b": keepalive\r\nevent: response.created\r\nid: resp-32\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-32\"}}\r\n\r\n";
    let responses_upstream =
        MockUpstream::start([ScriptedResponse::sse([responses_wire.as_slice()])]).await;
    let (responses_app, _db) = app_for_native_provider(
        &responses_upstream,
        "openai-compatible-responses-c32",
        "openai-compatible",
        "r32",
        "responses",
        "gpt-c32",
    )
    .await;
    let response = responses_app
        .oneshot(native_responses_request("r32", true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 4096).await.unwrap().as_ref(),
        responses_wire
    );
    responses_upstream.shutdown().await;

    let messages_wire = b": ping\r\nevent: message_start\r\nid: msg-32\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-32\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-c32\"}}\r\n\r\n";
    let messages_upstream =
        MockUpstream::start([ScriptedResponse::sse([messages_wire.as_slice()])]).await;
    let (messages_app, _db) = app_for_native_provider(
        &messages_upstream,
        "anthropic-compatible-c32",
        "anthropic-compatible",
        "a32",
        "messages",
        "claude-c32",
    )
    .await;
    let response = messages_app
        .oneshot(native_messages_request("a32", true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 4096).await.unwrap().as_ref(),
        messages_wire
    );
    messages_upstream.shutdown().await;
}

#[tokio::test]
async fn collected_translation_flushes_delimiterless_eof_tail() {
    let tail = b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-eof\",\"model\":\"claude-eof\",\"usage\":{\"input_tokens\":2}}}";
    let upstream = MockUpstream::start([ScriptedResponse::sse([tail.as_slice()])]).await;
    let (app, _db) = app_for_native_provider(
        &upstream,
        "anthropic-compatible-eof-c32",
        "anthropic-compatible",
        "e32",
        "messages",
        "claude-c32",
    )
    .await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": "e32/claude-c32",
                "messages": [{"role": "user", "content": "tail"}],
                "stream": false,
                "stream_options": {"include_usage": true}
            })
            .to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap_or_else(|error| {
        panic!(
            "invalid translated body {error}: {}",
            String::from_utf8_lossy(&body)
        )
    });
    assert_eq!(value["object"], "chat.completion.chunk");
    assert_eq!(value["id"], "chatcmpl-msg-eof");
    upstream.shutdown().await;
}

#[test]
fn active_paths_use_shared_framer_and_keep_binary_protocols_separate() {
    let chat = include_str!("../src/server/api/chat.rs");
    assert!(!chat.contains("fn next_sse_frame"));
    assert!(!chat.contains("responses_stream_completed(buffer"));
    assert_eq!(chat.matches("struct StreamDispatch").count(), 1);
    assert_eq!(chat.matches("framer: Option<TextStreamFramer>").count(), 1);
    assert!(chat.contains("&batch.output[output_start..]"));
    assert!(!chat.contains("StreamingUsageCapture"));
    assert!(!chat.contains("dashboard_framer"));

    let compat = include_str!("../src/server/api/compat.rs");
    assert!(!compat.contains("fn take_sse_frame"));
    assert!(!compat.contains("buf.find(\"\\n\\n\")"));

    let claude = include_str!("../src/core/translator/response/claude_to_openai.rs");
    let responses = include_str!("../src/core/translator/response/openai_responses.rs");
    assert!(!claude.contains("line_buffer.find"));
    assert!(!responses.contains("responses.buffer.find"));

    let registry = include_str!("../src/core/translator/registry.rs");
    assert!(registry.contains("pub text_framer: Option<"));
    for format in [
        "Self::Gemini",
        "Self::Vertex",
        "Self::Antigravity",
        "Self::Ollama",
    ] {
        assert!(registry.contains(format), "missing framing for {format}");
    }
    assert!(!registry.contains("pub frame_buffer: Vec<u8>"));
    assert!(!registry.contains("pub event_buffer: Vec<u8>"));
}
