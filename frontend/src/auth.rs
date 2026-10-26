//! Pure decisions shared by the browser implementation and host-side tests.

use serde_json::{json, Value};

#[derive(Debug, PartialEq)]
pub enum StatusDecision {
    Redirect,
    ShowLogin { has_password: bool },
}

pub fn status_decision(ok: bool, body: Option<&Value>) -> StatusDecision {
    let Some(body) = body.filter(|body| ok && !body.is_null()) else {
        return StatusDecision::ShowLogin { has_password: true };
    };
    if body["requireLogin"] == false || body["authenticated"] == true {
        StatusDecision::Redirect
    } else {
        let has_password = body
            .get("hasPassword")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        StatusDecision::ShowLogin { has_password }
    }
}

#[derive(Debug, PartialEq)]
pub enum LoginDecision {
    Redirect,
    ChangePassword,
    ConnectionFailure,
    Failure {
        message: String,
        reset_hint: String,
        retry_after: u64,
    },
}

pub fn login_decision(ok: bool, body: &Value, retry_header: Option<&str>) -> LoginDecision {
    // Reading a property of a JSON null throws in the reference handler.
    if body.is_null() {
        return LoginDecision::ConnectionFailure;
    }
    if ok && (body["success"] == true || body["authenticated"] == true) {
        return if body["mustChangePassword"] == true {
            LoginDecision::ChangePassword
        } else {
            LoginDecision::Redirect
        };
    }
    LoginDecision::Failure {
        message: nonempty_string(body, "error")
            .unwrap_or("Invalid password. Please try again.")
            .to_owned(),
        reset_hint: nonempty_string(body, "resetHint")
            .unwrap_or_default()
            .to_owned(),
        retry_after: retry_after(body, retry_header),
    }
}

fn nonempty_string<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key)?.as_str().filter(|value| !value.is_empty())
}

fn retry_after(body: &Value, header: Option<&str>) -> u64 {
    // A numeric retryAfter takes precedence even when it is non-positive.
    let seconds = body
        .get("retryAfter")
        .and_then(Value::as_f64)
        .or_else(|| body.get("retry_after_secs").and_then(Value::as_f64))
        .filter(|seconds| *seconds > 0.0)
        .or_else(|| {
            header
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        });
    seconds.map_or(0, |seconds| seconds.ceil() as u64)
}

pub fn validate_new_password(new_password: &str, confirmation: &str) -> Option<&'static str> {
    // The reference uses JavaScript String.length, which counts UTF-16 code units.
    if new_password.encode_utf16().count() < 8 {
        Some("New password must be at least 8 characters long")
    } else if new_password != confirmation {
        Some("Passwords do not match")
    } else {
        None
    }
}

pub fn password_payload(current_password: &str, new_password: &str) -> Value {
    let mut payload = json!({"newPassword": new_password});
    if !current_password.is_empty() {
        payload["currentPassword"] = json!(current_password);
    }
    payload
}

pub fn use_settings_fallback(status: u16) -> bool {
    status == 404 || status == 405
}

pub fn password_destination(body: &Value) -> &'static str {
    if body["sessionsInvalidated"] == true {
        "/login"
    } else {
        "/dashboard"
    }
}

pub fn password_error(body: &Value) -> &str {
    nonempty_string(body, "error").unwrap_or("Failed to set password")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_redirect_and_error_fallback() {
        assert_eq!(
            status_decision(true, Some(&json!({"requireLogin": false}))),
            StatusDecision::Redirect
        );
        assert_eq!(
            status_decision(true, Some(&json!({"authenticated": true}))),
            StatusDecision::Redirect
        );
        assert_eq!(
            status_decision(true, Some(&json!({"hasPassword": false}))),
            StatusDecision::ShowLogin {
                has_password: false
            }
        );
        assert_eq!(
            status_decision(true, Some(&json!({"hasPassword": true}))),
            StatusDecision::ShowLogin { has_password: true }
        );
        assert_eq!(
            status_decision(false, Some(&json!({}))),
            StatusDecision::ShowLogin { has_password: true }
        );
        assert_eq!(
            status_decision(true, None),
            StatusDecision::ShowLogin { has_password: true }
        );
    }

    #[test]
    fn login_and_rate_limit_decisions() {
        assert_eq!(
            login_decision(true, &Value::Null, None),
            LoginDecision::ConnectionFailure
        );
        assert_eq!(
            login_decision(true, &json!({"success": true}), None),
            LoginDecision::Redirect
        );
        assert_eq!(
            login_decision(
                true,
                &json!({"authenticated": true, "mustChangePassword": true}),
                None
            ),
            LoginDecision::ChangePassword
        );
        assert_eq!(
            login_decision(
                false,
                &json!({"error": "Locked", "resetHint": "yes", "retryAfter": 2.1}),
                Some("99")
            ),
            LoginDecision::Failure {
                message: "Locked".into(),
                reset_hint: "yes".into(),
                retry_after: 3
            }
        );
        assert_eq!(
            retry_after(
                &json!({"retryAfter": 0, "retry_after_secs": 20}),
                Some("1.1")
            ),
            2
        );
        assert_eq!(retry_after(&json!({"retry_after_secs": 3.2}), Some("8")), 4);
        assert_eq!(retry_after(&json!({}), Some("not a number")), 0);
        assert_eq!(retry_after(&json!({}), Some(" 42 ")), 42);
        assert_eq!(retry_after(&json!({}), Some("Infinity")), 0);
        assert_eq!(
            login_decision(true, &json!({}), None),
            LoginDecision::Failure {
                message: "Invalid password. Please try again.".into(),
                reset_hint: String::new(),
                retry_after: 0
            }
        );
    }

    #[test]
    fn password_validation_and_session_redirect() {
        assert_eq!(
            validate_new_password("short", "short"),
            Some("New password must be at least 8 characters long")
        );
        assert_eq!(
            validate_new_password("12345678", "different"),
            Some("Passwords do not match")
        );
        assert_eq!(validate_new_password("😀😀😀😀", "😀😀😀😀"), None);
        assert_eq!(
            password_payload("", "12345678"),
            json!({"newPassword": "12345678"})
        );
        assert_eq!(
            password_payload("old", "12345678"),
            json!({"currentPassword": "old", "newPassword": "12345678"})
        );
        assert!(use_settings_fallback(404));
        assert!(use_settings_fallback(405));
        assert!(!use_settings_fallback(500));
        assert_eq!(
            password_destination(&json!({"sessionsInvalidated": true})),
            "/login"
        );
        assert_eq!(password_destination(&json!({})), "/dashboard");
        assert_eq!(password_error(&json!({"error": "failed"})), "failed");
        assert_eq!(
            password_error(&json!({"error": ""})),
            "Failed to set password"
        );
    }
}
