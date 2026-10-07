use std::collections::BTreeMap;

pub const DEFAULT_CONTEXT_LIMIT: u32 = 500_000;
pub const MAX_CONTEXT_LIMIT: u32 = 1_000_000;
pub const CODEX_OUTPUT_LIMIT: u32 = 128_000;
// This is compatibility metadata for OpenCode, not a verified Codex provider
// limit. OpenCode reserves another 20k when `limit.input` is present, so 50k
// here makes its client-owned compaction start at 430k for a 500k context.
const CODEX_ADVERTISED_INPUT_RESERVE: u32 = 50_000;
// Claude joined the standard advertised-context cap on 2026-10-07: 1M-native
// cc models (4.6+) advertise 500k like the other limited providers, while
// models with a smaller native window (4.5 family) keep their native value.
pub const LIMITED_PROVIDERS: [&str; 5] = ["opencode-zen", "opencode-go", "glm", "codex", "claude"];

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
        "claude" => Some("claude"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_advertising_retains_compatibility_metadata() {
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
    fn claude_uses_the_standard_advertised_context_cap() {
        // 2026-10-07: claude joined providerContextLimits. 1M-native models
        // are advertised at the standard 500k; smaller native windows win.
        assert_eq!(canonical_provider("claude"), Some("claude"));
        assert_eq!(canonical_provider("anthropic"), None);
        assert_eq!(
            configured_limit(&BTreeMap::new(), "claude"),
            Some(DEFAULT_CONTEXT_LIMIT)
        );
        assert_eq!(
            advertised_model_limits(
                "claude",
                DEFAULT_CONTEXT_LIMIT,
                Some(1_000_000),
                None,
                Some(128_000)
            ),
            AdvertisedModelLimits {
                context: 500_000,
                input: None,
                output: Some(128_000),
            }
        );
        assert_eq!(
            advertised_model_limits(
                "claude",
                DEFAULT_CONTEXT_LIMIT,
                Some(200_000),
                None,
                Some(64_000)
            ),
            AdvertisedModelLimits {
                context: 200_000,
                input: None,
                output: Some(64_000),
            }
        );
    }
}
