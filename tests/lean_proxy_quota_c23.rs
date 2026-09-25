mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::lean_harness::{MockUpstream, ScriptedResponse};
use openproxy::core::usage::quota_fetcher::fetch_claude_quota_from_url;
use serde_json::Value;
use tokio::sync::{Barrier, Notify};

const QUOTA_BODY: &str = r#"{
  "five_hour":{"utilization":12.5,"resets_at":"2026-09-18T12:00:00Z"},
  "seven_day":{"utilization":40.0,"resets_at":"2026-09-22T12:00:00Z"},
  "seven_day_sonnet":{"utilization":55.5,"resets_at":"2026-09-22T12:00:00Z"}
}"#;

#[tokio::test]
async fn every_explicit_request_fetches_fresh_and_never_returns_stale_on_error() {
    let upstream = MockUpstream::start([
        ScriptedResponse::json(StatusCode::OK, QUOTA_BODY),
        ScriptedResponse::json(StatusCode::OK, QUOTA_BODY),
        ScriptedResponse::json(StatusCode::UNAUTHORIZED, r#"{"error":"expired"}"#),
        ScriptedResponse::json(StatusCode::INTERNAL_SERVER_ERROR, "upstream failed"),
        ScriptedResponse::json(StatusCode::OK, "not-json"),
    ])
    .await;
    let url = upstream.url("/api/oauth/usage");

    let first = fetch_claude_quota_from_url("token-c23", &url).await;
    let second = fetch_claude_quota_from_url("token-c23", &url).await;
    assert_eq!(first, second);
    assert_eq!(first["quotas"]["session (5h)"]["used"], 12.5);
    assert_eq!(first["quotas"]["weekly (7d)"]["used"], 40.0);
    assert_eq!(first["quotas"]["weekly sonnet (7d)"]["used"], 55.5);

    let auth_error = fetch_claude_quota_from_url("token-c23", &url).await;
    assert_eq!(
        auth_error["message"],
        "Invalid or expired Claude token. Please re-authorize the connection.",
        "a prior success must not be returned as fresh after 401"
    );
    let http_error = fetch_claude_quota_from_url("token-c23", &url).await;
    assert_eq!(http_error["message"], "Claude quota API error (500).");
    let json_error = fetch_claude_quota_from_url("token-c23", &url).await;
    assert!(json_error["message"]
        .as_str()
        .is_some_and(|message| message.starts_with("Claude error:")));

    assert_eq!(upstream.request_count().await, 5);
    for request in upstream.requests().await {
        assert_eq!(request.path, "/api/oauth/usage");
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer token-c23")
        );
    }
    upstream.shutdown().await;
}

#[tokio::test]
async fn cancellation_and_concurrent_callers_leave_no_hidden_state() {
    let body_dropped = Arc::new(Notify::new());
    let release_eof = Arc::new(Notify::new());
    let concurrent = 24usize;
    let mut scripts = vec![ScriptedResponse::json(StatusCode::OK, QUOTA_BODY)
        .holding_eof(release_eof)
        .notifying_on_body_drop(body_dropped.clone())];
    scripts.extend((0..=concurrent).map(|_| ScriptedResponse::json(StatusCode::OK, QUOTA_BODY)));
    let upstream = MockUpstream::start(scripts).await;
    let url = upstream.url("/api/oauth/usage");

    let cancelled_url = url.clone();
    let cancelled = tokio::spawn(async move {
        fetch_claude_quota_from_url("cancelled-token", &cancelled_url).await
    });
    upstream.wait_for_requests(1).await;
    cancelled.abort();
    assert!(cancelled
        .await
        .expect_err("held quota request is cancelled")
        .is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), body_dropped.notified())
        .await
        .expect("cancellation drops held upstream quota body");

    let after_cancel = fetch_claude_quota_from_url("after-cancel", &url).await;
    assert!(after_cancel.get("quotas").is_some());

    let barrier = Arc::new(Barrier::new(concurrent + 1));
    let mut callers = Vec::with_capacity(concurrent);
    for index in 0..concurrent {
        let url = url.clone();
        let barrier = barrier.clone();
        callers.push(tokio::spawn(async move {
            barrier.wait().await;
            fetch_claude_quota_from_url(&format!("rotated-token-{index}"), &url).await
        }));
    }
    barrier.wait().await;
    for caller in callers {
        let value: Value = caller.await.expect("join concurrent quota caller");
        assert!(value.get("quotas").is_some());
    }

    assert_eq!(
        upstream.request_count().await,
        concurrent + 2,
        "without retained cache/singleflight every explicit caller performs one request"
    );
    upstream.shutdown().await;
}

#[test]
fn source_has_no_quota_cache_and_polling_frequency_remains_bounded() {
    let quota = include_str!("../src/core/usage/quota_fetcher.rs");
    let usage = include_str!("../src/server/api/usage.rs");
    let auto_ping = include_str!("../src/server/api/quota_auto_ping.rs");
    let dashboard = include_str!("../web/src/components/usage/ProviderLimits/index.tsx");

    for forbidden in [
        "mod claude_cache",
        "CacheEntry",
        "get_cached(",
        "get_or_start_fetch",
        "complete_fetch",
        "store_stale",
        "get_stale",
        "USAGE_CACHE_TTL",
        "OnceCell<Value>",
        "HashMap<String, CacheEntry>",
    ] {
        assert!(
            !quota.contains(forbidden),
            "legacy Claude quota cache symbol remains: {forbidden}"
        );
    }
    assert!(quota.contains("fetch_claude_quota_from_url"));
    assert!(!usage.contains("bypasses the in-memory quota cache"));

    assert!(dashboard.contains("const REFRESH_INTERVAL_MS = 60000"));
    assert!(dashboard.contains("setInterval"));
    assert!(dashboard.contains("document.hidden"));
    assert!(auto_ping.contains("TICK_INTERVAL: Duration = Duration::from_secs(60)"));
    assert!(auto_ping.contains("claudeAutoPing"));
}
