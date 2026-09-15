use std::collections::BTreeMap;

use serde_json::Value;

pub const DEFAULT_CONTEXT_LIMIT: u32 = 500_000;
pub const MAX_CONTEXT_LIMIT: u32 = 1_000_000;
pub const LIMITED_PROVIDERS: [&str; 4] = ["opencode-zen", "opencode-go", "glm", "codex"];

pub fn canonical_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "opencode" | "opencode-zen" => Some("opencode-zen"),
        "opencode-go" => Some("opencode-go"),
        "glm" => Some("glm"),
        "codex" => Some("codex"),
        _ => None,
    }
}

pub fn default_provider_context_limits() -> BTreeMap<String, u32> {
    LIMITED_PROVIDERS
        .into_iter()
        .map(|provider| (provider.to_string(), DEFAULT_CONTEXT_LIMIT))
        .collect()
}

pub fn configured_limit(limits: &BTreeMap<String, u32>, provider: &str) -> Option<u32> {
    let provider = canonical_provider(provider)?;
    Some(
        limits
            .get(provider)
            .copied()
            .unwrap_or(DEFAULT_CONTEXT_LIMIT),
    )
}

pub fn effective_limit(configured: u32, native: Option<u32>) -> u32 {
    native.map_or(configured, |native| native.min(configured))
}

/// Conservative cross-provider estimate for prompt-bearing JSON fields.
/// One token per four serialized UTF-8 bytes is intentionally approximate;
/// request bodies are rejected, never truncated or rewritten.
pub fn estimate_input_tokens(body: &Value) -> u64 {
    const PROMPT_FIELDS: [&str; 7] = [
        "messages",
        "input",
        "instructions",
        "system",
        "contents",
        "tools",
        "tool_choice",
    ];

    let bytes: usize = PROMPT_FIELDS
        .into_iter()
        .filter_map(|field| body.get(field))
        .map(|value| serde_json::to_vec(value).map_or(0, |encoded| encoded.len()))
        .sum();
    (bytes as u64).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn native_limit_wins_when_lower() {
        assert_eq!(effective_limit(500_000, Some(204_800)), 204_800);
        assert_eq!(effective_limit(500_000, Some(1_050_000)), 500_000);
        assert_eq!(effective_limit(500_000, None), 500_000);
    }

    #[test]
    fn estimate_counts_messages_and_tools_but_not_transport_fields() {
        let base = estimate_input_tokens(&json!({
            "model": "glm/glm-5.1",
            "messages": [{"role": "user", "content": "hello"}]
        }));
        let with_tools = estimate_input_tokens(&json!({
            "model": "ignored-model-name",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}],
            "tools": [{"type": "function", "function": {"name": "lookup", "description": "long schema"}}]
        }));
        assert!(with_tools > base);
    }
}
