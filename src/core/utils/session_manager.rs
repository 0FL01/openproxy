//! Stateless provider session and continuation identifier derivation.
//!
//! Client-supplied conversation identifiers always win. Provider fallbacks are
//! deterministic and namespaced by adapter plus configured connection, so they
//! remain stable without retaining process-global client or account state.

use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Client headers that may carry an upstream session id (priority order).
const SESSION_HEADER_KEYS: &[&str] = &[
    "x-session-id",
    "session-id",
    "session_id",
    "x-amp-thread-id",
];

/// Result of resolving a conversation-stable session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub session_id: String,
    /// When true the id is one-shot (do not cache continuation across turns).
    pub ephemeral: bool,
}

/// Return a stable Antigravity session id for `connection_id` without keeping
/// process-global state. The UUID + 13 decimal digit shape remains compatible
/// with the upstream binary's `randomUUID() + Date.now()` value. An empty
/// connection id remains one-shot.
pub fn derive_session_id(connection_id: &str) -> String {
    if connection_id.is_empty() {
        return generate_binary_style_id();
    }
    let uuid = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("openproxy:antigravity:session:{connection_id}").as_bytes(),
    );
    let decimal_suffix = uuid.as_u128() % 10_000_000_000_000;
    format!("{uuid}{decimal_suffix:013}")
}

fn derive_scoped_session_id(scope: &str, connection_id: &str) -> String {
    if connection_id.is_empty() {
        return generate_binary_style_id();
    }
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("openproxy:{scope}:session:{connection_id}").as_bytes(),
    )
    .to_string()
}

/// Generate a fresh session id matching the upstream binary's format.
pub fn generate_binary_style_id() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}{now_ms}", Uuid::new_v4())
}

fn normalize_session_id(value: Option<&str>) -> Option<String> {
    let v = value?.trim();
    if v.is_empty() || v.len() > 256 {
        return None;
    }
    Some(v.to_string())
}

fn extract_claude_code_session(user_id: &str) -> Option<String> {
    if user_id.is_empty() {
        return None;
    }
    // `_session_{uuid}` suffix
    if let Some(idx) = user_id.rfind("_session_") {
        let rest = &user_id[idx + "_session_".len()..];
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
    }
    // JSON `{ "session_id": "..." }`
    if user_id.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(user_id) {
            return normalize_session_id(v.get("session_id").and_then(|s| s.as_str()));
        }
    }
    None
}

fn header_value(headers: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    let headers = headers?;
    let want = key.to_lowercase();
    headers
        .iter()
        .find(|(k, _)| k.to_lowercase() == want)
        .and_then(|(_, v)| normalize_session_id(Some(v.as_str())))
}

/// Read client-provided session id from headers/body (no generation).
fn extract_client_session_id(
    headers: Option<&HashMap<String, String>>,
    body: Option<&Value>,
    _scope: &str,
) -> Option<String> {
    if let Some(body) = body {
        if let Some(user_id) = body
            .get("metadata")
            .and_then(|m| m.get("user_id"))
            .and_then(|v| v.as_str())
        {
            if let Some(claude) = extract_claude_code_session(user_id) {
                return Some(format!("claude:{claude}"));
            }
        }
    }

    for key in SESSION_HEADER_KEYS {
        if let Some(v) = header_value(headers, key) {
            return Some(v);
        }
    }

    if let Some(v) = header_value(headers, "x-client-request-id") {
        return Some(v);
    }

    let body = body?;
    normalize_session_id(body.get("prompt_cache_key").and_then(|v| v.as_str()))
        .or_else(|| normalize_session_id(body.get("session_id").and_then(|v| v.as_str())))
        .or_else(|| normalize_session_id(body.get("conversation_id").and_then(|v| v.as_str())))
        .or_else(|| {
            normalize_session_id(
                body.get("metadata")
                    .and_then(|m| m.get("user_id"))
                    .and_then(|v| v.as_str()),
            )
        })
}

/// Resolve a conversation-stable session id (9router `resolveSessionIdentity`).
///
/// Priority: client session header/body → deterministic connection id.
pub fn resolve_session_identity(
    headers: Option<&HashMap<String, String>>,
    body: Option<&Value>,
    connection_id: Option<&str>,
    scope: &str,
) -> SessionIdentity {
    if let Some(client) = extract_client_session_id(headers, body, scope) {
        return SessionIdentity {
            session_id: client,
            ephemeral: false,
        };
    }
    SessionIdentity {
        session_id: derive_scoped_session_id(scope, connection_id.unwrap_or("")),
        ephemeral: false,
    }
}

/// Resolve a stable, adapter/account/session-namespaced continuation UUID.
///
/// The scope accepts an opaque UUID and requires it to remain stable across turns.
/// UUIDv5 supplies that protocol property without retaining client history or a
/// process-global continuation map. Ephemeral sessions always get a fresh UUID.
pub fn resolve_continuation_id(
    session_id: &str,
    connection_id: Option<&str>,
    scope: &str,
    ephemeral: bool,
) -> String {
    if ephemeral {
        return Uuid::new_v4().to_string();
    }
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!(
            "openproxy:{scope}:continuation:{}:{session_id}",
            connection_id.unwrap_or("")
        )
        .as_bytes(),
    )
    .to_string()
}

/// Convenience: extract connectionId / rawHeaders from the translator credentials Value.
pub fn credentials_connection_id(credentials: Option<&Value>) -> Option<String> {
    credentials.and_then(|c| {
        c.get("connectionId")
            .or_else(|| c.get("connection_id"))
            .or_else(|| c.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

/// Parse `rawHeaders` from the translator credentials Value into a lowercase map.
pub fn credentials_raw_headers(credentials: Option<&Value>) -> Option<HashMap<String, String>> {
    let obj = credentials?.get("rawHeaders")?.as_object()?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            map.insert(k.to_lowercase(), s.to_string());
        }
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_connection_id_returns_one_shot_value() {
        let a = derive_session_id("");
        let b = derive_session_id("");
        assert_ne!(a, b);
    }

    #[test]
    fn same_connection_returns_same_stateless_id() {
        let a = derive_session_id("test-conn-1");
        let b = derive_session_id("test-conn-1");
        assert_eq!(a, b);
        assert_eq!(a.len(), 49);
        assert_eq!(Uuid::parse_str(&a[..36]).unwrap().get_version_num(), 5);
        assert!(a[36..].chars().all(|character| character.is_ascii_digit()));
    }

    #[test]
    fn different_connections_get_different_ids() {
        let a = derive_session_id("test-conn-a");
        let b = derive_session_id("test-conn-b");
        assert_ne!(a, b);
    }

    #[test]
    fn binary_style_id_is_uuid_then_timestamp() {
        let id = generate_binary_style_id();
        // 36 hex+hyphen UUID followed by a ms timestamp (>= 13 chars in 2025+)
        assert!(id.len() >= 36 + 13);
    }

    #[test]
    fn identities_are_namespaced_without_retained_state() {
        let opencode = resolve_session_identity(None, None, Some("account"), "opencode");
        let antigravity = resolve_session_identity(None, None, Some("account"), "antigravity");
        assert_eq!(
            opencode,
            resolve_session_identity(None, None, Some("account"), "opencode")
        );
        assert_ne!(opencode.session_id, antigravity.session_id);

        let first = resolve_continuation_id("client", Some("account-a"), "test-scope", false);
        assert_eq!(
            first,
            resolve_continuation_id("client", Some("account-a"), "test-scope", false)
        );
        assert_ne!(
            first,
            resolve_continuation_id("client", Some("account-b"), "test-scope", false)
        );
        assert_ne!(
            first,
            resolve_continuation_id("client", Some("account-a"), "other", false)
        );
    }

    #[test]
    fn one_hundred_thousand_client_sessions_retain_nothing() {
        let first = resolve_continuation_id("session-0", Some("account"), "test-scope", false);
        for index in 0..100_000 {
            let session = format!("session-{index}");
            let id = resolve_continuation_id(&session, Some("account"), "test-scope", false);
            assert_eq!(Uuid::parse_str(&id).unwrap().get_version_num(), 5);
        }
        assert_eq!(
            first,
            resolve_continuation_id("session-0", Some("account"), "test-scope", false)
        );
    }

    #[test]
    fn without_client_identity_is_stable() {
        let first = resolve_session_identity(None, None, Some("account"), "test-scope");
        let second = resolve_session_identity(None, None, Some("account"), "test-scope");
        assert!(!first.ephemeral);
        assert!(!second.ephemeral);
        assert_eq!(first.session_id, second.session_id);
    }
}
