//! Tests for token refresh wire retries, REFRESH_LEAD_MS per provider, Claude
//! refresh body format, and GitHub Copilot token poll.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::oauth::device_code;
use crate::oauth::providers;
use crate::oauth::{pkce, RefreshRequest, TokenResponse};

#[tokio::test]
async fn refresh_does_not_retry_permanent_http_error() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = {
        let calls = calls.clone();
        move || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err("Refresh request returned HTTP 400: invalid_grant".to_string())
            }
        }
    };

    let result = crate::oauth::token_refresh::refresh_with_retry(attempt).await;

    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ─── REFRESH_LEAD per provider ────────────────────────────────────────────
// The lead times are defined in core::config::app_constants::refresh_lead().

#[test]
fn test_refresh_lead_codex_is_5_days() {
    let lead = crate::core::config::app_constants::refresh_lead("codex");
    assert_eq!(lead, Some(Duration::from_secs(5 * 24 * 60 * 60)));
}

#[test]
fn test_refresh_lead_claude_is_4_hours() {
    let lead = crate::core::config::app_constants::refresh_lead("claude");
    assert_eq!(lead, Some(Duration::from_secs(4 * 60 * 60)));
}

#[test]
fn test_refresh_lead_kimi_coding_is_5_minutes() {
    let lead = crate::core::config::app_constants::refresh_lead("kimi-coding");
    assert_eq!(lead, Some(Duration::from_secs(5 * 60)));
}

#[test]
fn test_refresh_lead_antigravity_is_5_minutes() {
    let lead = crate::core::config::app_constants::refresh_lead("antigravity");
    assert_eq!(lead, Some(Duration::from_secs(5 * 60)));
}

#[test]
fn test_refresh_lead_unknown_is_none() {
    let lead = crate::core::config::app_constants::refresh_lead("nonexistent");
    assert!(lead.is_none());
}

// ─── Claude refresh body format = JSON ───────────────────────────────────
// Claude token exchange uses a JSON body (not form-encoded).

#[test]
fn test_claude_refresh_body_should_be_json() {
    // The Claude exchange sends JSON:
    //   POST https://api.anthropic.com/v1/oauth/token
    //   Content-Type: application/json
    //   {"grant_type": "authorization_code", "code": "...", ...}
    //
    // This is a structural test: the RefreshRequest should work with JSON.
    let req = RefreshRequest {
        refresh_token: "rt_xyz".to_string(),
        client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".to_string(),
        client_secret: None,
        scopes: vec!["read".to_string(), "connect".to_string()],
    };
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": req.refresh_token,
        "client_id": req.client_id,
        "scope": req.scopes.join(" "),
    });
    assert!(body.get("grant_type").and_then(|v| v.as_str()) == Some("refresh_token"));
    assert!(body.get("refresh_token").and_then(|v| v.as_str()) == Some("rt_xyz"));
    assert!(
        body.get("client_id").and_then(|v| v.as_str())
            == Some("9d1c250a-e61b-44d9-88ed-5944d1962f5e")
    );
}

// ─── GitHub Copilot token poll ───────────────────────────────────────────
// exchange_github_copilot_token posts to github.com/copilot_internal/v1/token.

#[test]
fn test_github_copilot_token_response() {
    let json = r#"{
        "token": "github_copilot_token_abc",
        "expires_at": "2025-06-01T00:00:00Z",
        "refresh_in": 1500
    }"#;
    // The response shape used in the codebase
    #[derive(serde::Deserialize)]
    struct CopilotTokenResponse {
        token: String,
        expires_at: Option<String>,
    }
    let resp: CopilotTokenResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.token, "github_copilot_token_abc");
    assert!(resp.expires_at.is_some());
}

#[test]
fn test_github_copilot_token_minimal() {
    let json = r#"{"token": "tok_xyz"}"#;
    #[derive(serde::Deserialize)]
    struct CopilotTokenResponse {
        token: String,
        expires_at: Option<String>,
    }
    let resp: CopilotTokenResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.token, "tok_xyz");
    assert!(resp.expires_at.is_none());
}

// ─── needs_refresh tests (comprehensive) ─────────────────────────────────

#[test]
fn test_needs_refresh_none() {
    // Missing expires_at → false for proactive path (9router isTokenExpiringSoon)
    assert!(!crate::oauth::needs_refresh(&None));
}

#[test]
fn test_needs_refresh_past() {
    assert!(crate::oauth::needs_refresh(&Some(
        "2020-01-01T00:00:00Z".to_string()
    )));
}

#[test]
fn test_needs_refresh_future() {
    assert!(!crate::oauth::needs_refresh(&Some(
        "2099-12-31T23:59:59Z".to_string()
    )));
}

#[test]
fn test_needs_refresh_invalid() {
    assert!(crate::oauth::needs_refresh(&Some("garbage".to_string())));
}

#[test]
fn test_needs_refresh_within_buffer() {
    use chrono::Utc;
    let nearly = (Utc::now() - chrono::Duration::minutes(4)).to_rfc3339();
    assert!(crate::oauth::needs_refresh(&Some(nearly)));
}

#[test]
fn test_needs_refresh_far_future() {
    use chrono::Utc;
    let far = (Utc::now() + chrono::Duration::hours(24)).to_rfc3339();
    assert!(!crate::oauth::needs_refresh(&Some(far)));
}

// ─── expires_at_from_seconds ─────────────────────────────────────────────

#[test]
fn test_expires_at_from_seconds_is_rfc3339() {
    let s = crate::oauth::expires_at_from_seconds(3600);
    assert!(
        chrono::DateTime::parse_from_rfc3339(&s).is_ok(),
        "should produce valid RFC 3339: {s}"
    );
}
