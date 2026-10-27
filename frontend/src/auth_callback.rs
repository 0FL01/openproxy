//! Validation shared by the callback channels before any OAuth exchange.
use serde_json::Value;

pub fn valid(
    data: &Value,
    provider: &str,
    expected_state: &str,
    nonce: Option<&str>,
    storage: bool,
    now: f64,
) -> bool {
    if !data.is_object() || provider.is_empty() || expected_state.is_empty() {
        return false;
    }
    if provider == "cline" {
        // Cline's authorization URL does not forward OAuth state. Its exact
        // per-attempt nonce travels in the registered redirect URI instead.
        let state_ok = matches!(data.get("state"), None | Some(Value::Null))
            || data["state"]
                .as_str()
                .is_some_and(|v| v.is_empty() || v == expected_state);
        if !nonce
            .filter(|v| !v.is_empty())
            .is_some_and(|v| data["attemptNonce"].as_str() == Some(v))
            || !state_ok
        {
            return false;
        }
    } else if data["state"].as_str() != Some(expected_state) {
        return false;
    }
    match data.get("provider") {
        None | Some(Value::Null) => {}
        Some(Value::String(value)) if value == provider => {}
        _ => return false,
    }
    if storage {
        let Some(timestamp) = data["timestamp"].as_f64() else {
            return false;
        };
        if !timestamp.is_finite()
            || !now.is_finite()
            || timestamp > now
            || now - timestamp >= 30_000.0
        {
            return false;
        }
    }
    ["error", "token", "code"]
        .into_iter()
        .any(|key| data[key].as_str().is_some_and(|s| !s.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binds_callback_to_attempt_and_provider() {
        assert!(valid(
            &json!({"code":"code", "state":"nonce"}),
            "claude",
            "nonce",
            None,
            false,
            100_000.0
        ));
        assert!(valid(
            &json!({"error":"denied", "state":"nonce", "provider":"claude"}),
            "claude",
            "nonce",
            None,
            false,
            100_000.0
        ));
        assert!(!valid(
            &json!({"code":"code", "state":"wrong"}),
            "claude",
            "nonce",
            None,
            false,
            100_000.0
        ));
        assert!(!valid(
            &json!({"code":"code", "state":"nonce", "provider":"codex"}),
            "claude",
            "nonce",
            None,
            false,
            100_000.0
        ));
        assert!(!valid(
            &json!({"code":"code"}),
            "claude",
            "nonce",
            None,
            false,
            100_000.0
        ));
        assert!(!valid(
            &json!({"state":"nonce"}),
            "claude",
            "nonce",
            None,
            false,
            100_000.0
        ));
    }

    #[test]
    fn storage_requires_recent_non_future_timestamp() {
        let data = |timestamp| json!({"code":"code", "state":"nonce", "timestamp":timestamp});
        assert!(valid(
            &data(70_001),
            "claude",
            "nonce",
            None,
            true,
            100_000.0
        ));
        assert!(!valid(
            &data(70_000),
            "claude",
            "nonce",
            None,
            true,
            100_000.0
        ));
        assert!(!valid(
            &data(100_001),
            "claude",
            "nonce",
            None,
            true,
            100_000.0
        ));
        assert!(!valid(
            &json!({"code":"code", "state":"nonce"}),
            "claude",
            "nonce",
            None,
            true,
            100_000.0
        ));
    }

    #[test]
    fn cline_requires_its_own_nonce_and_rejects_conflicting_state() {
        let good = json!({"code":"code", "attemptNonce":"bound"});
        assert!(valid(
            &good,
            "cline",
            "backend-state",
            Some("bound"),
            false,
            100.0
        ));
        assert!(!valid(&good, "cline", "backend-state", None, false, 100.0));
        assert!(!valid(
            &good,
            "cline",
            "backend-state",
            Some("other"),
            false,
            100.0
        ));
        assert!(!valid(
            &json!({"code":"code", "attemptNonce":"bound", "state":"wrong"}),
            "cline",
            "backend-state",
            Some("bound"),
            false,
            100.0
        ));
        assert!(!valid(
            &json!({"code":"code", "attemptNonce":"bound", "state":42}),
            "cline",
            "backend-state",
            Some("bound"),
            false,
            100.0
        ));
        assert!(!valid(
            &json!({"code":"code", "attemptNonce":"bound", "provider":"claude"}),
            "cline",
            "backend-state",
            Some("bound"),
            false,
            100.0
        ));
        assert!(!valid(
            &json!({"code":"code"}),
            "cline",
            "backend-state",
            Some("bound"),
            false,
            100.0
        ));
    }
}
