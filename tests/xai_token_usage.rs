//! xAI original accounting versus its Responses projection. All upstreams are local.
mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use common::lean_harness::{MockUpstream, ScriptedResponse, TempTestDb};
use futures_util::StreamExt;
use openproxy::db::sqlite::repo::request_repo::{self, RequestDetailFilter};
use openproxy::server::application_logs::request_log_flush_with_budget;
use openproxy::server::state::AppState;
use openproxy::types::RuntimeTransport;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::ServiceExt;

fn usage() -> Value {
    json!({"prompt_tokens":32,"completion_tokens":9,"total_tokens":135,
        "prompt_tokens_details":{"cached_tokens":6},
        "completion_tokens_details":{"reasoning_tokens":94}})
}

fn chunks(final_usage: Option<Value>, done: bool) -> Vec<String> {
    let mut chunks = vec![
        format!(
            "data: {}\n\n",
            json!({"id":"chatcmpl-fixture","model":"fixture","choices":[{"index":0,"delta":{"role":"assistant","content":"hello"}}],"usage":{"completion_tokens":999}})
        ),
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".into(),
        "data: {\"choices\":[],\"usage\":null}\n\n".into(),
    ];
    if let Some(usage) = final_usage {
        chunks.push(format!("data: {}\n\n", json!({"choices":[],"usage":usage})));
    }
    if done {
        chunks.push("data: [DONE]\n\n".into());
    }
    chunks
}

async fn fixture(upstream: &MockUpstream, reqwest: bool) -> (axum::Router, TempTestDb) {
    let db = TempTestDb::new().await;
    db.db
        .update(|data| {
            data.api_keys = vec![common::test_api_key()];
            data.settings.require_api_key = true;
            data.settings.require_login = false;
            let mut connection = common::test_connection("xai");
            connection.default_model = Some("fixture".into());
            connection.runtime_transport = Some(RuntimeTransport {
                base_url: Some(upstream.url("/v1/chat/completions")),
            });
            if reqwest {
                // Presence chooses Reqwest; the explicit no-proxy target keeps
                // every request on the same local mock, never on port 9.
                connection.use_connection_proxy = Some(true);
                connection.provider_specific_data.extend([
                    ("connectionProxyEnabled".into(), json!(true)),
                    ("connectionProxyUrl".into(), json!("http://127.0.0.1:9")),
                    ("connectionNoProxy".into(), json!("127.0.0.1,localhost")),
                ]);
            }
            data.provider_connections = vec![connection];
        })
        .await
        .unwrap();
    (openproxy::build_app(AppState::new(db.db.clone())), db)
}

fn request(route: &str, stream: bool, options: Option<Value>) -> Request<Body> {
    let mut body = if route == "/v1/responses" {
        json!({"model":"xai/fixture","input":"hello","stream":stream})
    } else {
        json!({"model":"xai/fixture","messages":[{"role":"user","content":"hello"}],"stream":stream})
    };
    if let Some(options) = options {
        body["stream_options"] = options;
    }
    Request::builder()
        .method("POST")
        .uri(route)
        .header("authorization", "Bearer test-key")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn saved(db: &TempTestDb) -> Value {
    assert!(request_log_flush_with_budget(Duration::from_secs(5)).await);
    let rows = db
        .db
        .sqlite
        .with_conn(|conn| request_repo::list(conn, &RequestDetailFilter::default(), 10, 0))
        .unwrap();
    assert_eq!(rows.len(), 1);
    json!({"status": rows[0].status,"data":rows[0].data})
}

fn assert_original(row: &Value) {
    assert_eq!(row["status"], "success", "{row}");
    for (field, expected) in [
        ("inputTokens", 32),
        ("outputTokens", 9),
        ("cachedTokens", 6),
        ("reasoningTokens", 94),
        ("totalTokens", 135),
    ] {
        assert_eq!(row["data"][field], expected, "{field}: {row}");
    }
    assert_eq!(
        row["data"]["upstreamTps"]["generatedOutputTokens"], 9,
        "{row}"
    );
}

fn completed(text: &str) -> Value {
    let events: Vec<Value> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|payload| serde_json::from_str(payload).ok())
        .filter(|value: &Value| value["type"] == "response.completed")
        .collect();
    assert_eq!(events.len(), 1, "{text}");
    events[0]["response"].clone()
}

fn assert_responses_usage(value: &Value) {
    assert_eq!(
        value["usage"],
        json!({"input_tokens":32,"output_tokens":103,"total_tokens":135,
        "input_tokens_details":{"cached_tokens":6},"output_tokens_details":{"reasoning_tokens":94}})
    );
}

#[tokio::test]
async fn xai_usage_json_and_stream_preserve_native_counts_and_normalize_responses() {
    for (route, stream) in [
        ("/v1/chat/completions", false),
        ("/v1/chat/completions", true),
        ("/v1/responses", false),
        ("/v1/responses", true),
    ] {
        let native = json!({"id":"chatcmpl-fixture","model":"fixture","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":usage()});
        let raw_sse = chunks(Some(usage()), true).concat();
        let script = if stream {
            ScriptedResponse::sse([raw_sse.clone()])
        } else {
            ScriptedResponse::json(StatusCode::OK, native.to_string())
        };
        let upstream = MockUpstream::start([
            script.with_chunk_timing(Duration::from_millis(5), Duration::ZERO)
        ])
        .await;
        let (app, db) = fixture(&upstream, false).await;
        let response = app
            .clone()
            .oneshot(request(route, stream, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        if route == "/v1/responses" {
            let value = if stream {
                completed(&text)
            } else {
                serde_json::from_str(&text).unwrap()
            };
            assert_responses_usage(&value);
        } else if stream {
            assert_eq!(text, raw_sse, "native bytes must remain unchanged");
        } else {
            assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), native);
        }
        assert_original(&saved(&db).await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs")
                    .header("authorization", "Bearer test-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let public: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 256 * 1024).await.unwrap())
                .unwrap();
        let row = &public["requests"][0];
        assert_eq!(row["reasoningTokens"], 94);
        assert_eq!(row["totalTokens"], 135);
        assert_eq!(row["generatedOutputTokens"], 9);
        assert!(row["tokensPerSecond"].as_f64().unwrap() > 0.0);
        let sent: Value = serde_json::from_slice(&upstream.requests().await[0].body).unwrap();
        if stream {
            assert_eq!(sent["stream_options"]["include_usage"], true);
        } else {
            assert!(sent.get("stream_options").is_none());
        }
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn xai_translated_terminal_waits_for_eof_and_logs_before_client_stops_on_both_transports() {
    for reqwest in [false, true] {
        let eof = Arc::new(Notify::new());
        let upstream = MockUpstream::start([ScriptedResponse::sse(chunks(Some(usage()), true))
            .with_chunk_timing(Duration::from_millis(5), Duration::from_millis(3))
            .holding_eof(eof.clone())])
        .await;
        let (app, db) = fixture(&upstream, reqwest).await;
        let response = app
            .oneshot(request("/v1/responses", true, None))
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        let mut text = String::new();
        while !text.contains("response.output_text.delta") {
            let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            text.push_str(std::str::from_utf8(&chunk).unwrap());
        }
        // Consume every available event while HTTP EOF remains held. Neither
        // usage nor upstream DONE may leak an early successful terminal.
        assert!(tokio::time::timeout(Duration::from_millis(50), async {
            while let Some(chunk) = body.next().await {
                text.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
                assert!(!text.contains("response.completed"));
                assert!(!text.contains("[DONE]"));
            }
        })
        .await
        .is_err());
        assert_eq!(saved(&db).await["status"], "pending");
        eof.notify_one();
        while !text.contains("event: response.completed") {
            let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            text.push_str(std::str::from_utf8(&chunk).unwrap());
        }
        // Emulate OpenCode's takeUntil(response.completed), without polling
        // the proxy body to its own EOF.
        drop(body);
        assert_responses_usage(&completed(&text));
        assert_original(&saved(&db).await);
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn xai_explicit_stream_options_are_preserved_and_missing_member_is_defaulted() {
    for options in [
        json!({}),
        json!({"include_usage":false,"custom":7}),
        json!({"include_usage":true}),
        Value::Null,
        json!("invalid"),
    ] {
        let upstream =
            MockUpstream::start([ScriptedResponse::sse(chunks(Some(usage()), true))]).await;
        let (app, _db) = fixture(&upstream, false).await;
        let response = app
            .oneshot(request("/v1/chat/completions", true, Some(options.clone())))
            .await
            .unwrap();
        to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let sent: Value = serde_json::from_slice(&upstream.requests().await[0].body).unwrap();
        let expected = if options == json!({}) {
            json!({"include_usage":true})
        } else {
            options
        };
        assert_eq!(sent["stream_options"], expected);
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn xai_final_missing_usage_stays_unknown_and_clean_eof_without_done_is_valid() {
    for (final_usage, done) in [(None, true), (Some(usage()), false)] {
        let upstream =
            MockUpstream::start([ScriptedResponse::sse(chunks(final_usage.clone(), done))]).await;
        let (app, db) = fixture(&upstream, false).await;
        let response = app
            .oneshot(request("/v1/responses", true, None))
            .await
            .unwrap();
        let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let value = completed(std::str::from_utf8(&bytes).unwrap());
        let row = saved(&db).await;
        assert_eq!(row["status"], "success");
        if final_usage.is_some() {
            assert_responses_usage(&value);
            assert_original(&row);
            assert_eq!(row["data"]["upstreamTps"]["endKind"], "clean_eof");
        } else {
            assert!(value.get("usage").is_none());
            for field in [
                "inputTokens",
                "outputTokens",
                "reasoningTokens",
                "totalTokens",
                "upstreamTps",
            ] {
                assert!(row["data"][field].is_null(), "{field}: {row}");
            }
        }
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn xai_late_errors_and_unfinished_streams_cannot_release_success() {
    for fixture_chunks in [
        vec!["data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n".into()],
        [
            chunks(Some(usage()), true),
            vec!["data: {\"error\":{\"message\":\"late failure\"}}\n\n".into()],
        ]
        .concat(),
        [
            chunks(Some(usage()), true),
            vec!["data: {not-json}\n\n".into()],
        ]
        .concat(),
        [
            chunks(Some(usage()), true),
            vec!["event: error\ndata: {\"message\":\"late failure\"}\n\n".into()],
        ]
        .concat(),
    ] {
        let upstream = MockUpstream::start([ScriptedResponse::sse(fixture_chunks)]).await;
        let (app, db) = fixture(&upstream, false).await;
        let response = app
            .oneshot(request("/v1/responses", true, None))
            .await
            .unwrap();
        let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("response.output_text.delta"), "{text}");
        assert!(!text.contains("response.completed"), "{text}");
        assert!(text.contains("event: error"), "{text}");
        let row = saved(&db).await;
        assert_eq!(row["status"], "error", "{row}");
        assert!(row["data"]["upstreamTps"].is_null());
        upstream.shutdown().await;
    }
}

#[tokio::test]
async fn xai_cancellation_before_eof_remains_interrupted() {
    let eof = Arc::new(Notify::new());
    let upstream = MockUpstream::start([
        ScriptedResponse::sse(chunks(Some(usage()), true)).holding_eof(eof.clone())
    ])
    .await;
    let (app, db) = fixture(&upstream, false).await;
    let response = app
        .oneshot(request("/v1/responses", true, None))
        .await
        .unwrap();
    let mut body = response.into_body().into_data_stream();
    assert!(body.next().await.is_some());
    drop(body);
    let row = saved(&db).await;
    assert_eq!(row["status"], "interrupted");
    assert!(row["data"]["upstreamTps"].is_null());
    eof.notify_one();
    upstream.shutdown().await;
}
