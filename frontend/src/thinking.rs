//! The reference's model-aware thinking picker and copy suffix rules.
use serde_json::Value;

fn pattern(pattern: &str, model: &str) -> bool {
    let model = model.to_lowercase();
    let mut remaining = model.as_str();
    let mut first = true;
    for part in pattern.split('*').filter(|s| !s.is_empty()) {
        let Some(index) = remaining.find(part) else {
            return false;
        };
        if first && !pattern.starts_with('*') && index != 0 {
            return false;
        }
        remaining = &remaining[index + part.len()..];
        first = false;
    }
    pattern.ends_with('*') || remaining.is_empty()
}

/// `None` means this model does not support a selectable suffix.
pub fn levels(provider: &str, model: &str, catalog: &[Value]) -> Option<Vec<String>> {
    if model.is_empty() {
        return None;
    }
    let declared = catalog
        .iter()
        .find(|v| v["id"] == model && v["kind"] == "llm");
    if provider == "xai" || provider == "codex" && declared.is_some() {
        let mut levels = Vec::new();
        for value in declared
            .and_then(|v| v["reasoningEfforts"].as_array())
            .into_iter()
            .flatten()
        {
            if let Some(s) = value.as_str().filter(|s| !s.is_empty() && *s != "auto")
                && !levels.iter().any(|v| v == s)
            {
                levels.push(s.to_owned());
            }
        }
        return (!levels.is_empty()).then_some(levels);
    }
    let base = model.rsplit('/').next().unwrap_or(model);
    let adaptive = [
        "claude-opus-4.6",
        "claude-opus-4.7",
        "claude-opus-4-7",
        "claude-opus-4.8",
        "claude-opus-4-6",
        "claude-opus-4-8",
        "claude-opus-4.8-thinking",
        "claude-opus-4-8-thinking",
        "claude-sonnet-4.6",
        "claude-sonnet-4-6",
        "claude-sonnet-5",
        "claude-sonnet-5-thinking",
        "claude-sonnet-5-agentic",
        "claude-sonnet-5-thinking-agentic",
    ];
    let override_caps = if provider == "nvidia" {
        match model {
            "minimaxai/minimax-m2.7" | "minimaxai/minimax-m3" => Some(("openai", false)),
            "z-ai/glm-5.2" | "deepseek-ai/deepseek-v4-pro" | "deepseek-ai/deepseek-v4-flash" => {
                Some(("openai", true))
            }
            _ => None,
        }
    } else if provider == "codebuddy-cn" {
        match model {
            "glm-5.0" | "glm-4.7" => Some(("openai", true)),
            "glm-5.2" | "glm-5.1" | "glm-5.0-turbo" | "glm-5v-turbo" | "minimax-m3"
            | "minimax-m2.7" | "kimi-k2.7" | "kimi-k2.6" | "kimi-k2.5" | "hy3-preview"
            | "deepseek-v4-pro" | "deepseek-v4-flash" | "deepseek-v3-2-volc" => {
                Some(("openai", false))
            }
            _ => None,
        }
    } else {
        None
    };
    let exact = if adaptive.contains(&base) || adaptive.contains(&model) {
        Some(("claude-adaptive", true))
    } else if base == "glm-4.6v" || model == "glm-4.6v" {
        Some(("zai", true))
    } else {
        None
    };
    // These are the reference's ordered PATTERN_CAPS, grouped only where
    // adjacent entries have identical values.
    let patterns: &[(&[&str], &str, bool)] = &[
        (
            &[
                "*claude*opus-4.6*",
                "*claude*opus-4.7*",
                "*claude*opus-4.8*",
                "*claude*sonnet-4.6*",
                "*claude*sonnet-4.7*",
            ],
            "claude-adaptive",
            true,
        ),
        (&["*claude*"], "claude-budget", true),
        (&["*gemini-3*"], "gemini-level", false),
        (&["*gemini-2.5*"], "gemini-budget", true),
        (
            &["*gpt-5*", "*gpt-oss*", "*o1*", "*o3*", "*o4*", "*grok*"],
            "openai",
            true,
        ),
        (
            &[
                "*qwen*vl*",
                "*qwen*omni*",
                "*qwen*coder*",
                "*qwen*max*",
                "*qwen3.5*",
                "*qwen3.6*",
                "*qwen3.7*",
                "*qwen*plus*",
                "*qwen*235b*",
            ],
            "qwen",
            true,
        ),
        (&["*qwq*"], "qwen", false),
        (&["*qwen*"], "qwen", true),
        (&["*kimi*k2.7*code*"], "kimi", false),
        (&["*kimi*"], "kimi", true),
        (&["*glm*"], "zai", true),
        (&["*deepseek-v4*"], "deepseek", true),
        (&["*reasoner*", "*deepseek-r*"], "deepseek", false),
        (&["*deepseek-chat*"], "", true),
        (&["*deepseek*"], "deepseek", true),
        (&["*minimax-m3*"], "minimax", true),
        (&["*minimax*"], "minimax", false),
        (&["*hunyuan*", "hy3*"], "hunyuan", true),
        (&["*step-*"], "step", true),
        (&["*nemotron*", "*ling-*"], "base", true),
    ];
    let caps = override_caps.or(exact).or_else(|| {
        patterns
            .iter()
            .find(|(p, _, _)| p.iter().any(|p| pattern(p, base) || pattern(p, model)))
            .map(|(_, format, can_disable)| (*format, *can_disable))
    })?;
    if caps.0.is_empty() {
        return None;
    }
    let values: &[&str] = if pattern("*gpt-5.6-sol*", model) {
        &["none", "minimal", "low", "medium", "high", "xhigh", "max"]
    } else if pattern("*codex*", model) {
        &["low", "medium", "high", "xhigh"]
    } else {
        match caps.0 {
            "openai" => &["none", "minimal", "low", "medium", "high", "xhigh"],
            "claude-adaptive" | "kimi" => &["none", "low", "medium", "high", "max"],
            "claude-budget" => &["none", "low", "medium", "high", "xhigh", "max"],
            "gemini-level" => &["minimal", "low", "medium", "high"],
            "zai" | "minimax" => &["none", "thinking"],
            "deepseek" => &["none", "high", "max"],
            _ => &["none", "low", "medium", "high"],
        }
    };
    Some(
        values
            .iter()
            .filter(|v| caps.1 || **v != "none")
            .map(|v| (*v).to_owned())
            .collect(),
    )
}

pub fn provider_levels(
    provider: &str,
    model_ids: impl IntoIterator<Item = String>,
    catalog: &[Value],
) -> Vec<String> {
    let mut set = Vec::new();
    for id in model_ids {
        for level in levels(provider, &id, catalog).unwrap_or_default() {
            if (level != "none" || provider == "xai") && !set.contains(&level) {
                set.push(level);
            }
        }
    }
    if set.is_empty() {
        return if provider == "xai" {
            vec!["auto".into()]
        } else if matches!(provider, "codex" | "openai") {
            ["auto", "none", "low", "medium", "high"]
                .iter()
                .map(|s| (*s).into())
                .collect()
        } else {
            ["auto", "on", "none"].iter().map(|s| (*s).into()).collect()
        };
    }
    let order = [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "thinking", "on", "off",
    ];
    let mut ordered = vec!["auto".to_owned()];
    ordered.extend(
        order
            .into_iter()
            .filter(|l| set.iter().any(|v| v == l))
            .map(str::to_owned),
    );
    for value in set {
        if !ordered.contains(&value) {
            ordered.push(value);
        }
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn reference_fallback_and_model_levels() {
        assert_eq!(
            provider_levels("kilocode", ["fixture".into()], &[]),
            ["auto", "on", "none"]
        );
        assert_eq!(levels("kilocode", "deepseek-chat", &[]), None);
        assert_eq!(
            levels("kilocode", "gpt-5.6-sol", &[]).unwrap(),
            ["none", "minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            levels("nvidia", "minimaxai/minimax-m2.7", &[]).unwrap(),
            ["minimal", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            levels("kilocode", "glm-4.6v", &[]).unwrap(),
            ["none", "thinking"]
        );
        assert_eq!(
            provider_levels("kilocode", ["gpt-5-codex".into(), "glm-4.6v".into()], &[]),
            ["auto", "low", "medium", "high", "xhigh", "thinking"]
        );
        let catalog = [
            json!({"id":"model", "kind":"llm", "reasoningEfforts":["auto", "none", "high", "high"]}),
        ];
        assert_eq!(levels("xai", "model", &catalog).unwrap(), ["none", "high"]);
        assert_eq!(levels("xai", "unknown-grok", &catalog), None);
        assert_eq!(
            levels("codex", "model", &catalog).unwrap(),
            ["none", "high"]
        );
    }
}
