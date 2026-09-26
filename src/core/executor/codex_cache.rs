//! Private, request-local cache observations. Never retain input or response text.
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use hmac::{digest::KeyInit, Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

#[derive(Clone, Default)]
pub(crate) struct CodexCacheObservation(Arc<Mutex<Option<Value>>>);

struct HashWriter(Hmac<Sha256>);

impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn hasher(domain: &[u8]) -> HashWriter {
    let mut hash = Hmac::<Sha256>::new_from_slice(crate::core::auth::api_key_secret().as_bytes())
        .expect("HMAC accepts any key length");
    hash.update(b"openproxy.codex-cache.v1\0");
    hash.update(domain);
    hash.update(b"\0");
    HashWriter(hash)
}

fn digest(hash: &HashWriter) -> String {
    hex::encode(hash.0.clone().finalize().into_bytes())
}

fn tag(domain: &[u8], value: &str) -> String {
    let mut hash = hasher(domain);
    hash.0.update(value.as_bytes());
    digest(&hash)
}

impl CodexCacheObservation {
    pub(crate) fn sent(&self, body: &Value, headers: &reqwest::header::HeaderMap, url: &str) {
        if body
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| model.len() > 512)
        {
            return;
        }
        let mut envelope = hasher(b"envelope");
        envelope.0.update(url.as_bytes());
        envelope.0.update(b"\0");
        // Borrow envelope fields: tools/instructions can also be large.
        let fields: std::collections::BTreeMap<&str, &Value> = body
            .as_object()
            .into_iter()
            .flat_map(|object| object.iter())
            .filter(|(key, _)| key.as_str() != "input")
            .map(|(key, value)| (key.as_str(), value))
            .collect();
        serde_json::to_writer(&mut envelope, &fields).expect("JSON Value serialization");
        let input = body.get("input").and_then(Value::as_array);
        let count = input.map_or(0, Vec::len);
        let mut hash = hasher(b"input");
        let mut boundaries = Vec::with_capacity(16);
        if let Some(input) = input {
            for (index, item) in input.iter().enumerate() {
                // JSON cannot contain a literal NUL; this separator is unambiguous.
                serde_json::to_writer(&mut hash, item).expect("JSON Value serialization");
                hash.0.update(b"\0");
                if index + 16 >= count {
                    boundaries.push(json!({"count": index + 1, "hmac": digest(&hash)}));
                }
            }
        }
        let data = json!({
            "version": 1,
            "model": body.get("model"),
            "accountHmac": headers.get("chatgpt-account-id").and_then(|v| v.to_str().ok()).map(|v| tag(b"account", v)),
            "cacheKeyHmac": body.get("prompt_cache_key").and_then(Value::as_str).map(|v| tag(b"cache-key", v)),
            "envelopeHmac": digest(&envelope),
            "inputCount": count,
            "inputPrefixes": boundaries,
            "sentAt": chrono::Utc::now().to_rfc3339(),
            "completedAt": null,
            "inputTokens": null,
            "cachedTokens": null,
            "cacheWriteTokens": null,
        });
        *self.0.lock().expect("cache observation lock") = Some(data);
    }

    pub(crate) fn observe(&self, event: Option<&str>, value: Option<&Value>) {
        let mut guard = self.0.lock().expect("cache observation lock");
        let Some(data) = guard.as_mut() else { return };
        if (event == Some("response.completed")
            || value.and_then(|v| v.get("type")).and_then(Value::as_str)
                == Some("response.completed"))
            && data["completedAt"].is_null()
        {
            data["completedAt"] = json!(chrono::Utc::now().to_rfc3339());
        }
        let Some(usage) =
            value.and_then(|v| v.get("usage").or_else(|| v.get("response")?.get("usage")))
        else {
            return;
        };
        let number = |v: Option<&Value>| {
            v.and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse::<u64>().ok()))
        };
        let input =
            number(usage.get("input_tokens")).or_else(|| number(usage.get("prompt_tokens")));
        let cached = number(
            usage
                .get("input_tokens_details")
                .and_then(|v| v.get("cached_tokens")),
        )
        .or_else(|| {
            number(
                usage
                    .get("prompt_tokens_details")
                    .and_then(|v| v.get("cached_tokens")),
            )
        })
        .or_else(|| number(usage.get("cached_tokens")))
        .or_else(|| number(usage.get("cache_read_input_tokens")));
        let write = number(usage.get("cache_creation_input_tokens"));
        for (key, value) in [
            ("inputTokens", input),
            ("cachedTokens", cached),
            ("cacheWriteTokens", write),
        ] {
            if let Some(value) = value {
                data[key] = json!(value);
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Option<Value> {
        self.0.lock().expect("cache observation lock").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(input: Vec<Value>) -> Value {
        let observation = CodexCacheObservation::default();
        observation.sent(&json!({"model":"gpt-test", "input":input, "instructions":"private-instructions", "prompt_cache_key":"private-key"}), &Default::default(), "https://example.test/responses");
        observation.snapshot().unwrap()
    }

    #[test]
    fn codex_cache_prefixes_prove_append_and_bound_observability() {
        let original = vec![
            json!({"content":"private-prompt"}),
            json!({"content":"answer"}),
        ];
        let a = observation(original.clone());
        let mut appended = original.clone();
        appended.push(json!({"content":"next"}));
        let b = observation(appended.clone());
        assert_eq!(a["inputPrefixes"][1], b["inputPrefixes"][1]);
        assert_eq!(a["envelopeHmac"], b["envelopeHmac"]);
        appended[0]["content"] = json!("changed");
        assert_ne!(
            a["inputPrefixes"][1],
            observation(appended)["inputPrefixes"][1]
        );
        let mut long = original;
        long.extend((0..16).map(|i| json!(i)));
        let long = observation(long);
        assert_eq!(long["inputPrefixes"].as_array().unwrap().len(), 16);
        assert_eq!(long["inputPrefixes"][0]["count"], 3);
        let serialized = long.to_string();
        assert!(serialized.len() < 4096);
        assert!(!serialized.contains("private-"));
    }

    #[test]
    fn codex_cache_source_usage_keeps_zero_and_partial_updates() {
        let observation = CodexCacheObservation::default();
        observation.sent(
            &json!({"model":"test", "input":[]}),
            &Default::default(),
            "https://example.test/responses",
        );
        observation.observe(None, Some(&json!({"usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":0},"secret":"private-response"}})));
        observation.observe(
            None,
            Some(&json!({"usage":{"input_tokens":null,"output_tokens":10}})),
        );
        let data = observation.snapshot().unwrap();
        assert_eq!(data["inputTokens"], 100);
        assert_eq!(data["cachedTokens"], 0);
        assert!(data["completedAt"].is_null());
        assert!(data["cacheWriteTokens"].is_null());
        observation.observe(None, Some(&json!({"type":"response.completed","response":{"usage":{"input_tokens_details":{"cached_tokens":64},"cache_creation_input_tokens":32}}})));
        let data = observation.snapshot().unwrap();
        assert_eq!(data["cachedTokens"], 64);
        assert_eq!(data["cacheWriteTokens"], 32);
        assert!(data["completedAt"].is_string());
        assert!(!data.to_string().contains("private-response"));
    }
}
