//! Wire-level tripwire for the Claude OAuth refresh request body.
//!
//! Claude Code 2.1.289 always sends `scope` in its refresh body, but RFC
//! 6749 §6 makes the parameter optional and our scope-less refresh is
//! live-verified on a real Claude account. Adding a foreign scope string
//! risks a terminal `invalid_scope` (the CLI treats it as terminal auth),
//! so this test pins the exact production body instead: any field
//! added/removed/renamed on the wire fails here before it can reach the
//! live token endpoint.

#![allow(clippy::await_holding_lock)]
use openproxy::core::tls::ensure_rustls_provider;
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

struct EnvVarGuard {
    key: &'static str,
    old_value: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old_value = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value) };
        Self { key, old_value }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.old_value.take() {
            unsafe { std::env::set_var(self.key, value) };
        } else {
            unsafe { std::env::remove_var(self.key) };
        }
    }
}

#[tokio::test]
async fn claude_refresh_posts_exact_wire_body_without_scope() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_rustls_provider();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CLAUDE_TOKEN_URL",
        &format!("{}/v1/oauth/token", server.uri()),
    );

    // Exact parsed-body equality: the absence of `scope` (and of any other
    // field) is part of the pin. Key order is not asserted (not meaningful).
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(wiremock::matchers::header(
            "content-type",
            "application/json",
        ))
        .and(wiremock::matchers::body_json(json!({
            "grant_type": "refresh_token",
            "refresh_token": "sentinel-refresh-token",
            "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "claude-access-rotated",
            "refresh_token": "claude-refresh-rotated",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let result = openproxy::oauth::token_refresh::dispatch_oauth_refresh(
        "claude",
        "sentinel-refresh-token",
        &Default::default(),
    )
    .await
    .expect("mock token endpoint accepts the pinned body");

    // Rotation must be carried through.
    assert_eq!(result.access_token, "claude-access-rotated");
    assert_eq!(
        result.refresh_token.as_deref(),
        Some("claude-refresh-rotated")
    );
    assert_eq!(result.expires_in, Some(3600));

    server.verify().await;
}

#[tokio::test]
async fn claude_refresh_body_with_scope_fails_the_tripwire() {
    // Guard the guard: if someone adds `scope` (or any extra field) to the
    // production body, the pinned mock above stops matching. This test
    // documents the failure shape: the upstream 4xx error surfaces
    // immediately (429/4xx are non-retryable since the F3 change) and the
    // sentinel tokens never leak into the error string.
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_rustls_provider();
    let server = MockServer::start().await;
    let _token_url = EnvVarGuard::set(
        "OPENPROXY_CLAUDE_TOKEN_URL",
        &format!("{}/v1/oauth/token", server.uri()),
    );

    // Accept any body, reject with invalid_scope.
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_scope",
            "error_description": "requested scope exceeds the grant"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = openproxy::oauth::token_refresh::dispatch_oauth_refresh(
        "claude",
        "sentinel-refresh-token",
        &Default::default(),
    )
    .await
    .expect_err("mock endpoint rejects the scope");

    assert!(error.contains("HTTP 400"), "unexpected error: {error}");
    assert!(!error.contains("sentinel-refresh-token"));
}
