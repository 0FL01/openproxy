mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use futures_util::StreamExt;
use openproxy::core::translator::registry::{global_registry, Format, ResponseTransformState};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio::sync::{Barrier, Notify};

#[tokio::test]
async fn scripted_upstream_controls_headers_errors_rotation_and_request_count() {
    let mock = MockUpstream::start([
        ScriptedResponse::json(StatusCode::OK, r#"{"ok":true}"#)
            .with_header("x-lean-fixture", "native-json"),
        ScriptedResponse::json(StatusCode::UNAUTHORIZED, r#"{"error":"expired"}"#),
        ScriptedResponse::json(
            StatusCode::OK,
            r#"{"access_token":"rotated-test-token","expires_in":3600}"#,
        ),
        ScriptedResponse::sse([Bytes::from_static(b"data: partial\n\n")]).failing_after_chunks(),
    ])
    .await;
    let client = reqwest::Client::new();

    let native = client
        .post(mock.url("/native"))
        .json(&json!({"fixture": "native"}))
        .send()
        .await
        .expect("native response");
    assert_eq!(native.headers()["x-lean-fixture"], "native-json");
    assert_eq!(native.json::<Value>().await.unwrap(), json!({"ok": true}));

    let expired = client
        .post(mock.url("/oauth"))
        .bearer_auth("expired-test-token")
        .send()
        .await
        .expect("expired response");
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);

    let rotated = client
        .post(mock.url("/oauth"))
        .header("x-refresh-generation", "2")
        .send()
        .await
        .expect("rotated response")
        .json::<Value>()
        .await
        .expect("rotated token JSON");
    assert_eq!(rotated["access_token"], "rotated-test-token");

    let failed_body = client
        .get(mock.url("/stream-error"))
        .send()
        .await
        .expect("stream response")
        .bytes()
        .await;
    assert!(
        failed_body.is_err(),
        "mid-stream failure must be observable"
    );

    mock.wait_for_requests(4).await;
    let requests = mock.requests().await;
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[0].body,
        Bytes::from_static(br#"{"fixture":"native"}"#)
    );
    assert_eq!(
        requests[1].headers["authorization"],
        "Bearer expired-test-token"
    );
    assert_eq!(requests[2].headers["x-refresh-generation"], "2");
    mock.shutdown().await;
}

#[tokio::test]
async fn first_event_arrives_before_eof_and_buffering_is_detected() {
    let eof = Arc::new(Notify::new());
    let streaming =
        MockUpstream::start([
            ScriptedResponse::sse([Bytes::from_static(b"data: first\n\n")])
                .holding_eof(eof.clone()),
        ])
        .await;
    let response = reqwest::get(streaming.url("/events"))
        .await
        .expect("streaming headers");
    let mut body = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(1), body.next())
        .await
        .expect("first event must not wait for EOF")
        .expect("first stream item")
        .expect("first stream bytes");
    assert_eq!(first, Bytes::from_static(b"data: first\n\n"));
    eof.notify_waiters();
    assert!(body.next().await.is_none());
    streaming.shutdown().await;

    let release_first = Arc::new(Notify::new());
    let buffered =
        MockUpstream::start([
            ScriptedResponse::sse([Bytes::from_static(b"data: late\n\n")])
                .waiting_for(release_first.clone()),
        ])
        .await;
    let response = reqwest::get(buffered.url("/buffered"))
        .await
        .expect("buffering headers");
    let mut body = response.bytes_stream();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), body.next())
            .await
            .is_err(),
        "the characterization must detect a first event withheld until buffering completes"
    );
    release_first.notify_waiters();
    let late = body.next().await.unwrap().unwrap();
    assert_eq!(late, Bytes::from_static(b"data: late\n\n"));
    buffered.shutdown().await;
}

#[test]
fn native_and_translated_fixtures_characterize_protocol_semantics() {
    let native_request: Value =
        serde_json::from_str(include_str!("fixtures/lean-proxy/openai-chat-native.json")).unwrap();
    let native_response: Value = serde_json::from_str(include_str!(
        "fixtures/lean-proxy/openai-chat-native-response.json"
    ))
    .unwrap();
    let native_sse = include_str!("fixtures/lean-proxy/openai-native.sse");
    assert_eq!(
        native_request["messages"][0]["content"],
        "hello from the lean harness"
    );
    assert_eq!(
        native_response["choices"][0]["message"]["content"],
        "fixture response"
    );
    assert_eq!(native_sse.matches("data:").count(), 3);

    let mut translated_request: Value = serde_json::from_str(include_str!(
        "fixtures/lean-proxy/openai-to-claude-request.json"
    ))
    .unwrap();
    assert!(global_registry().translate_request(
        Format::OpenAi,
        Format::Claude,
        "claude-sonnet-test",
        &mut translated_request,
        true,
        None,
    ));
    assert_eq!(
        translated_request["system"][0]["text"],
        "keep protocol semantics"
    );
    assert_eq!(translated_request["messages"][0]["role"], "user");

    let mut state = ResponseTransformState::default();
    let output = global_registry().translate_response(
        Format::Claude,
        Format::OpenAi,
        include_bytes!("fixtures/lean-proxy/claude-translated.sse"),
        &mut state,
    );
    let output = output.join("");
    assert!(output.contains("translated fixture"));
    assert!(output.contains("chatcmpl-msg_lean_fixture"));
    assert!(output.contains("\"finish_reason\":\"stop\""));
}

#[tokio::test(start_paused = true)]
async fn virtual_clock_and_barrier_make_time_and_concurrency_deterministic() {
    let sleeper = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
        "elapsed"
    });
    tokio::task::yield_now().await;
    assert!(!sleeper.is_finished());
    tokio::time::advance(Duration::from_secs(60)).await;
    assert_eq!(sleeper.await.unwrap(), "elapsed");

    let workers = 16;
    let barrier = Arc::new(Barrier::new(workers));
    let completed = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::with_capacity(workers);
    for _ in 0..workers {
        let barrier = barrier.clone();
        let completed = completed.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            completed.fetch_add(1, Ordering::SeqCst);
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(completed.load(Ordering::SeqCst), workers);
}

#[tokio::test]
async fn temporary_database_lives_for_the_harness_scope() {
    let test_db = TempTestDb::new().await;
    test_db
        .db
        .update(|db| {
            db.extra.insert("leanFixture".into(), json!(true));
        })
        .await
        .expect("persist fixture state");
    assert_eq!(test_db.db.snapshot().extra["leanFixture"], true);
}
