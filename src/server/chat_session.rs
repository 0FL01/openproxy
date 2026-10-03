//! Passive explicit-chat linkage. Never retains raw IDs or session state.

use axum::http::HeaderMap;
use hmac::{digest::KeyInit, Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

const ALIASES: [&str; 3] = [
    "x-opencode-session-id",
    "x-opencode-session",
    "x-session-id",
];
const DOMAIN: &[u8] = b"openproxy/explicit-chat/v1";

pub(super) fn extract(headers: &HeaderMap, api_key_id: &str, secret: &[u8]) -> Value {
    let mut selected: Option<(&str, &[u8])> = None;
    for alias in ALIASES {
        let mut values = headers.get_all(alias).iter();
        let Some(value) = values.next() else {
            continue;
        };
        // Even identical repeated values are ambiguous. Comma folding must
        // not silently turn a duplicate into an apparent single identifier.
        let raw = value.as_bytes();
        if values.next().is_some()
            || !(1..=256).contains(&raw.len())
            || !raw
                .iter()
                .all(|byte| (0x21..=0x7e).contains(byte) && *byte != b',')
        {
            return Value::Null;
        }
        if let Some((_, previous)) = selected {
            if previous != raw {
                return Value::Null;
            }
        } else {
            selected = Some((alias, raw));
        }
    }
    let Some((source, raw)) = selected else {
        return Value::Null;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    // Length-prefix every component; source, provider, model and connection
    // deliberately do not contribute to the digest.
    for component in [DOMAIN, api_key_id.as_bytes(), raw] {
        mac.update(&(component.len() as u64).to_be_bytes());
        mac.update(component);
    }
    json!({"version": 1, "hmac": hex::encode(mac.finalize().into_bytes()), "source": source})
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn aliases_are_case_insensitive_and_source_does_not_change_digest() {
        let mut expected = None;
        for alias in [
            "X-OpenCode-Session-ID",
            "X-OpenCode-Session",
            "X-Session-ID",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::HeaderName::from_bytes(alias.as_bytes()).unwrap(),
                HeaderValue::from_static("chat-one"),
            );
            let value = extract(&headers, "key-one", b"installation-secret");
            assert_eq!(value["version"], 1);
            assert_eq!(value["source"], alias.to_ascii_lowercase());
            assert_eq!(value["hmac"].as_str().unwrap().len(), 64);
            if let Some(expected) = &expected {
                assert_eq!(&value["hmac"], expected);
            } else {
                expected = Some(value["hmac"].clone());
            }
            assert!(!value.to_string().contains("chat-one"));
            assert_ne!(
                value["hmac"],
                extract(&headers, "key-two", b"installation-secret")["hmac"]
            );
            assert_ne!(
                value["hmac"],
                extract(&headers, "key-one", b"rotated-secret")["hmac"]
            );
        }
    }

    #[test]
    fn invalid_conflicting_or_duplicate_headers_are_unknown_without_truncation() {
        assert!(extract(&HeaderMap::new(), "key", b"secret").is_null());
        for raw in [
            vec![],
            vec![b'a'; 257],
            b" a".to_vec(),
            b"a ".to_vec(),
            b"a,b".to_vec(),
            vec![0xff],
            b"a\tb".to_vec(),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(ALIASES[0], HeaderValue::from_bytes(&raw).unwrap());
            assert!(extract(&headers, "key", b"secret").is_null());
        }
        for size in [1, 256] {
            let mut headers = HeaderMap::new();
            headers.insert(
                ALIASES[0],
                HeaderValue::from_bytes(&vec![b'a'; size]).unwrap(),
            );
            assert!(extract(&headers, "key", b"secret").is_object());
        }
        let mut headers = HeaderMap::new();
        headers.insert(ALIASES[0], HeaderValue::from_static("one"));
        headers.insert(ALIASES[1], HeaderValue::from_static("one"));
        assert!(extract(&headers, "key", b"secret").is_object());
        headers.insert(ALIASES[1], HeaderValue::from_static("two"));
        assert!(extract(&headers, "key", b"secret").is_null());
        headers.remove(ALIASES[1]);
        headers.append(ALIASES[0], HeaderValue::from_static("one"));
        assert!(extract(&headers, "key", b"secret").is_null());
    }

    #[test]
    fn component_boundaries_are_unambiguous() {
        let mut a = HeaderMap::new();
        a.insert(ALIASES[0], HeaderValue::from_static("bc"));
        let mut b = HeaderMap::new();
        b.insert(ALIASES[0], HeaderValue::from_static("c"));
        assert_ne!(
            extract(&a, "a", b"secret")["hmac"],
            extract(&b, "ab", b"secret")["hmac"]
        );
    }
}
