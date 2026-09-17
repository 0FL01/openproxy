use std::collections::BTreeMap;

use serde_json::Value;

pub const DEFAULT_CONTEXT_LIMIT: u32 = 500_000;
pub const MAX_CONTEXT_LIMIT: u32 = 1_000_000;
pub const CODEX_OUTPUT_LIMIT: u32 = 128_000;
// This is compatibility metadata for OpenCode, not a verified Codex provider
// limit. OpenCode reserves another 20k when `limit.input` is present, so 50k
// here makes its client-owned compaction start at 430k for a 500k context.
const CODEX_ADVERTISED_INPUT_RESERVE: u32 = 50_000;
pub const LIMITED_PROVIDERS: [&str; 4] = ["opencode-zen", "opencode-go", "glm", "codex"];

/// Context metadata advertised to clients. These values do not define proxy
/// memory limits and do not authorize request rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvertisedModelLimits {
    pub context: u32,
    pub input: Option<u32>,
    pub output: Option<u32>,
}

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

/// Apply a configured compatibility override to provider/model metadata.
///
/// Codex historically advertised the configured value even when its catalog
/// reported a smaller context window. That behavior is retained for client
/// compatibility, but is not evidence that the upstream accepts that size.
pub fn advertised_model_limits(
    provider: &str,
    configured: u32,
    native_context: Option<u32>,
    native_input: Option<u32>,
    native_output: Option<u32>,
) -> AdvertisedModelLimits {
    if canonical_provider(provider) == Some("codex") {
        return AdvertisedModelLimits {
            context: configured,
            input: Some(
                configured
                    .saturating_sub(CODEX_ADVERTISED_INPUT_RESERVE)
                    .max(1),
            ),
            output: Some(CODEX_OUTPUT_LIMIT),
        };
    }

    let context = native_context.map_or(configured, |native| native.min(configured));
    AdvertisedModelLimits {
        context,
        input: native_input.map(|input| input.min(context)),
        output: native_output,
    }
}

/// Transitional C07 policy reader for the heuristic rejection removed by C08.
/// Keep it separate from [`advertised_model_limits`]: deleting this policy must
/// not alter metadata consumed by OpenCode and other clients.
pub fn legacy_proxy_rejection_limit(
    provider: &str,
    configured: u32,
    native_context: Option<u32>,
) -> u32 {
    if canonical_provider(provider) == Some("codex") {
        configured
    } else {
        native_context.map_or(configured, |native| native.min(configured))
    }
}

/// Transitional input headroom used only by the legacy rejection policy.
pub fn legacy_proxy_input_limit(provider: &str, context: u32) -> Option<u32> {
    (canonical_provider(provider) == Some("codex")).then(|| {
        context
            .saturating_sub(CODEX_ADVERTISED_INPUT_RESERVE)
            .max(1)
    })
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
        .map(|value| {
            let mut prompt = value.clone();
            remove_inline_binary_payloads(&mut prompt);
            serde_json::to_vec(&prompt).map_or(0, |encoded| encoded.len())
        })
        .sum();
    (bytes as u64).div_ceil(4)
}

fn remove_inline_binary_payloads(value: &mut Value) {
    match value {
        Value::String(text) => {
            if text.starts_with("data:") {
                if let Some(index) = text.find(";base64,") {
                    text.truncate(index + ";base64,".len());
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                remove_inline_binary_payloads(value);
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("base64") {
                if let Some(Value::String(data)) = object.get_mut("data") {
                    data.clear();
                }
            }
            for value in object.values_mut() {
                remove_inline_binary_payloads(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn codex_advertising_is_separate_from_legacy_rejection_policy() {
        let advertised =
            advertised_model_limits("codex", 500_000, Some(272_000), None, Some(64_000));
        assert_eq!(
            advertised,
            AdvertisedModelLimits {
                context: 500_000,
                input: Some(450_000),
                output: Some(128_000),
            }
        );
        assert_eq!(
            legacy_proxy_rejection_limit("codex", 500_000, Some(272_000)),
            500_000
        );
        assert_eq!(legacy_proxy_input_limit("codex", 500_000), Some(450_000));
    }

    #[test]
    fn native_metadata_still_caps_other_providers() {
        assert_eq!(
            advertised_model_limits("glm", 500_000, Some(204_800), Some(300_000), Some(32_000),),
            AdvertisedModelLimits {
                context: 204_800,
                input: Some(204_800),
                output: Some(32_000),
            }
        );
        assert_eq!(
            advertised_model_limits("glm", 500_000, Some(1_050_000), None, None).context,
            500_000
        );
        assert_eq!(
            advertised_model_limits("glm", 500_000, None, None, None).context,
            500_000
        );
    }

    #[test]
    fn empty_map_keeps_documented_default_compatibility_value() {
        let empty = BTreeMap::new();
        assert_eq!(configured_limit(&empty, "glm"), Some(DEFAULT_CONTEXT_LIMIT));
        assert_eq!(configured_limit(&empty, "unconfigured"), None);
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

    #[test]
    fn estimate_ignores_inline_image_payloads() {
        let small = estimate_input_tokens(&json!({
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "image_url": "data:image/png;base64,A"
                }]
            }]
        }));
        let large = estimate_input_tokens(&json!({
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "image_url": format!("data:image/png;base64,{}", "A".repeat(1_000_000))
                }]
            }]
        }));
        let claude = estimate_input_tokens(&json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "A".repeat(1_000_000)
                    }
                }]
            }]
        }));

        assert_eq!(large, small);
        assert!(claude < 100);
    }
}
