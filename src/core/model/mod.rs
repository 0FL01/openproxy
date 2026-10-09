pub mod capabilities;
pub mod catalog;
pub mod commandcode_catalog;
pub mod models_dev;

use std::collections::{BTreeMap, HashMap};

use once_cell::sync::Lazy;

use crate::types::{AppDb, ModelAliasTarget, ProviderConnection, ProviderModelRef};

static ALIAS_TO_PROVIDER_ID: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    HashMap::from([
        ("cc", "claude"),
        ("cx", "codex"),
        ("ag", "antigravity"),
        ("gh", "github"),
        ("kr", "kiro"),
        ("kc", "kilocode"),
        ("kmc", "kimi-coding"),
        ("cl", "cline"),
        ("oc", "opencode"),
        ("ocg", "opencode-go"),
        ("openai", "openai"),
        ("anthropic", "anthropic"),
        ("gemini", "gemini"),
        ("openrouter", "openrouter"),
        ("glm", "glm"),
        ("kimi", "kimi"),
        ("minimax", "minimax"),
        ("ds", "deepseek"),
        ("deepseek", "deepseek"),
        ("xai", "xai"),
        ("mistral", "mistral"),
        ("pplx", "perplexity"),
        ("perplexity", "perplexity"),
        ("together", "together"),
        ("fireworks", "fireworks"),
        ("cerebras", "cerebras"),
        ("cohere", "cohere"),
        ("commandcode", "commandcode"),
        ("a6api", "a6api"),
        ("nvidia", "nvidia"),
        ("hyp", "hyperbolic"),
        ("hyperbolic", "hyperbolic"),
        ("vx", "vertex"),
        ("vertex", "vertex"),
        ("vxp", "vertex-partner"),
        ("vertex-partner", "vertex-partner"),
        // ── Enterprise & Cloud ──
        ("databricks", "databricks"),
        ("snowflake", "snowflake"),
        ("heroku", "heroku"),
        ("lambda-ai", "lambda-ai"),
        ("ovhcloud", "ovhcloud"),
        ("wandb", "wandb"),
        // ── Gateway / Bridge ──
        ("kilo-gateway", "kilo-gateway"),
        ("v0-vercel", "v0-vercel"),
        // ── Regional CN ──
        ("alibaba", "alibaba"),
        ("ali", "alibaba"),
        ("alibaba-cn", "alibaba-cn"),
        ("ali-cn", "alibaba-cn"),
        ("moonshot", "moonshot"),
        ("volcengine", "volcengine"),
        ("zai", "zai"),
        // ── Regional international ──
        ("gigachat", "gigachat"),
        ("upstage", "upstage"),
        ("maritalk", "maritalk"),
        // ── Inference APIs ──
        ("venice", "venice"),
        ("featherless-ai", "featherless-ai"),
        ("friendliai", "friendliai"),
        ("galadriel", "galadriel"),
        ("llamagate", "llamagate"),
        ("nanogpt", "nanogpt"),
        ("synthetic", "synthetic"),
        ("pollinations", "pollinations"),
        ("meta-llama", "meta-llama"),
        // ── Coding / CLI ──
        ("opencode-zen", "opencode-zen"),
        ("kimi-coding-apikey", "kimi-coding-apikey"),
        ("kmca", "kimi-coding-apikey"),
        ("devin-cli", "devin-cli"),
        ("dv", "devin-cli"),
        ("crof", "crof"),
        // ── Media ──
        ("haiper", "haiper"),
        ("hp", "haiper"),
        ("leonardo", "leonardo"),
        ("leo", "leonardo"),
        ("ideogram", "ideogram"),
        ("ideo", "ideogram"),
        ("suno", "suno"),
        ("udio", "udio"),
        // ── Web / Chat ──
        ("chatgpt-web", "chatgpt-web"),
        ("gemini-web", "gemini-web"),
        ("gweb", "gemini-web"),
        ("muse-spark-web", "muse-spark-web"),
        ("ms-web", "muse-spark-web"),
    ])
});

pub fn a6api_enabled_model_ids(connection: &ProviderConnection) -> Vec<String> {
    connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn a6api_connection_supports_model(connection: &ProviderConnection, model: &str) -> bool {
    connection.provider == "a6api"
        && a6api_enabled_model_ids(connection)
            .iter()
            .any(|id| id == model.trim())
}

/// User-chosen publication families and effort presets, not upstream capability
/// discovery. Raw per-key inventory and exact routing entitlement stay intact.
pub(crate) fn a6api_default_reasoning_efforts(model: &str) -> Option<&'static [&'static str]> {
    const GPT_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
    const GLM_DEEPSEEK_EFFORTS: &[&str] = &["low", "high", "max"];

    let model_id = model.trim().rsplit('/').next()?;
    if model_id.starts_with("gpt-6") {
        Some(GPT_EFFORTS)
    } else if model_id.starts_with("glm-5") || model_id.starts_with("deepseek-v4") {
        Some(GLM_DEEPSEEK_EFFORTS)
    } else {
        None
    }
}

pub fn a6api_active_model_ids(db: &AppDb) -> Vec<String> {
    db.provider_connections
        .iter()
        .filter(|connection| connection.provider == "a6api" && connection.is_active())
        .flat_map(a6api_enabled_model_ids)
        .filter(|model| a6api_default_reasoning_efforts(model).is_some())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModel {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub is_alias: bool,
    pub provider_alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRouteKind {
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub provider: Option<String>,
    pub model: String,
    pub route_kind: ModelRouteKind,
}

pub fn resolve_provider_alias(alias_or_id: &str) -> String {
    ALIAS_TO_PROVIDER_ID
        .get(alias_or_id)
        .copied()
        .unwrap_or(alias_or_id)
        .to_string()
}

pub fn parse_model(model_str: &str) -> ParsedModel {
    if model_str.is_empty() {
        return ParsedModel {
            provider: None,
            model: None,
            is_alias: false,
            provider_alias: None,
        };
    }

    if let Some(first_slash) = model_str.find('/') {
        let provider_or_alias = &model_str[..first_slash];
        let model = &model_str[first_slash + 1..];
        return ParsedModel {
            provider: Some(resolve_provider_alias(provider_or_alias)),
            model: Some(model.to_string()),
            is_alias: false,
            provider_alias: Some(provider_or_alias.to_string()),
        };
    }

    ParsedModel {
        provider: None,
        model: Some(model_str.to_string()),
        is_alias: true,
        provider_alias: None,
    }
}

pub fn resolve_model_alias_from_map(
    alias: &str,
    aliases: &BTreeMap<String, ModelAliasTarget>,
) -> Option<ProviderModelRef> {
    let resolved = aliases.get(alias)?;
    match resolved {
        ModelAliasTarget::Path(path) => {
            path.split_once('/')
                .map(|(provider_or_alias, model)| ProviderModelRef {
                    provider: resolve_provider_alias(provider_or_alias),
                    model: model.to_string(),
                    extra: BTreeMap::new(),
                })
        }
        ModelAliasTarget::Mapping(mapping) => Some(ProviderModelRef {
            provider: resolve_provider_alias(&mapping.provider),
            model: mapping.model.clone(),
            extra: mapping.extra.clone(),
        }),
    }
}

pub fn get_model_info(model_str: &str, db: &AppDb) -> ResolvedModel {
    let parsed = parse_model(model_str);

    if !parsed.is_alias {
        if let (Some(provider), Some(provider_alias), Some(model)) = (
            parsed.provider.clone(),
            parsed.provider_alias.clone(),
            parsed.model.clone(),
        ) {
            if provider == provider_alias {
                for node_type in ["openai-compatible", "anthropic-compatible"] {
                    if let Some(node) = db.provider_nodes.iter().find(|node| {
                        node.r#type == node_type
                            && node.prefix.as_deref() == Some(provider_alias.as_str())
                    }) {
                        return ResolvedModel {
                            // JS parity: credentials are keyed by the node's
                            // id/prefix (the connection `provider` value), not
                            // the display name.
                            provider: node.id.clone().into(),
                            model,
                            route_kind: ModelRouteKind::Direct,
                        };
                    }
                }
            }

            return ResolvedModel {
                provider: Some(provider),
                model,
                route_kind: ModelRouteKind::Direct,
            };
        }
    }

    let alias_name = parsed.model.unwrap_or_default();
    if let Some(resolved) = resolve_model_alias_from_map(&alias_name, &db.model_aliases) {
        return ResolvedModel {
            provider: Some(resolved.provider),
            model: resolved.model,
            route_kind: ModelRouteKind::Direct,
        };
    }

    let fallback = infer_provider_from_model_name(&alias_name).to_string();
    ResolvedModel {
        provider: Some(fallback),
        model: alias_name,
        route_kind: ModelRouteKind::Direct,
    }
}

/// Infer the target provider from a bare model name string, based on known
/// model-family prefixes.  This is the last-resort fallback used when no
/// explicit alias maps the model — it avoids forcing every unknown
/// model to "openai".
///
/// Known model-family prefix → provider mappings:
///
/// | Prefix(es)                        | Provider      |
/// |-----------------------------------|---------------|
/// | `claude-`                         | `anthropic`   |
/// | `gemini-`                         | `gemini`      |
/// | `gpt-`, `o1`, `o3`, `o4`         | `openai`      |
/// | `deepseek-`                       | `openrouter`  |
/// | `mistral-`, `open-mistral-`, …   | `mistral`     |
/// | `command-`, `command-r`           | `cohere`      |
/// | `grok-`                           | `xai`         |
/// | `jamba-`                          | `ai21`        |
/// | Everything else (llama, phi, …)   | `openai`      |
fn infer_provider_from_model_name(model_name: &str) -> &'static str {
    let model_name = model_name.to_lowercase();

    if model_name.starts_with("claude-") {
        "anthropic"
    } else if model_name.starts_with("gemini-") {
        "gemini"
    } else if model_name.starts_with("gpt-")
        || model_name.starts_with("o1")
        || model_name.starts_with("o3")
        || model_name.starts_with("o4")
    {
        "openai"
    } else if model_name.starts_with("deepseek-") {
        "openrouter"
    } else if model_name.starts_with("mistral-")
        || model_name.starts_with("open-mistral-")
        || model_name.starts_with("mistralai-")
        || model_name.starts_with("codestral-")
        || model_name.starts_with("ministral-")
        || model_name.starts_with("mixtral-")
    {
        "mistral"
    } else if model_name.starts_with("command-") || model_name.starts_with("command-r") {
        "cohere"
    } else if model_name.starts_with("grok-") {
        "xai"
    } else if model_name.starts_with("jamba-") {
        "ai21"
    } else {
        // Unknown model prefixes route to "openai" as the generic fallback.
        // Common model families that land here: llama-*, codellama-*, phi-*,
        // nemotron-*, dbrx-*, qwen-*, yi-*, gemma-*.
        "openai"
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        a6api_active_model_ids, a6api_connection_supports_model, a6api_default_reasoning_efforts,
        a6api_enabled_model_ids,
    };
    use crate::types::{AppDb, ProviderConnection};

    fn connection_with_inventory(inventory: serde_json::Value) -> ProviderConnection {
        let mut connection = ProviderConnection {
            provider: "a6api".to_string(),
            ..ProviderConnection::default()
        };
        connection
            .provider_specific_data
            .insert("enabledModels".to_string(), inventory);
        connection
    }

    #[test]
    fn a6api_reasoning_defaults_match_final_segment_families() {
        let gpt_efforts: &[&str] = &["low", "medium", "high", "xhigh", "max"];
        let glm_deepseek_efforts: &[&str] = &["low", "high", "max"];

        for model in [
            "gpt-6",
            "gpt-6-mini",
            "openai/gpt-6",
            " \torg/openai/gpt-6-pro\n",
        ] {
            assert_eq!(a6api_default_reasoning_efforts(model), Some(gpt_efforts));
        }
        for model in [
            "glm-5",
            "zai/glm-5",
            " org/zai/glm-5-turbo ",
            "deepseek-v4",
            "deepseek/deepseek-v4",
            " org/deepseek/deepseek-v4-flash ",
        ] {
            assert_eq!(
                a6api_default_reasoning_efforts(model),
                Some(glm_deepseek_efforts)
            );
        }
    }

    #[test]
    fn a6api_reasoning_defaults_reject_case_and_prefix_mismatches() {
        for model in [
            "",
            " \t\n",
            "GPT-6",
            "openai/GPT-6",
            "GLM-5",
            "deepseek/DeepSeek-v4",
            "gpt-5.5",
            "glm-4.7",
            "deepseek-v3.2",
            "gpt6",
            "glm5",
            "deepseekv4",
            "not-gpt-6",
            "gpt-6/other-model",
            "glm-5/gemini-3-pro",
            "deepseek-v4/",
        ] {
            assert_eq!(a6api_default_reasoning_efforts(model), None, "{model:?}");
        }
    }

    #[test]
    fn a6api_reasoning_defaults_use_literal_prefix_wildcards() {
        for (model, family) in [
            ("gpt-60", "gpt-6"),
            ("vendor/glm-50", "glm-5"),
            ("vendor/deepseek-v40", "deepseek-v4"),
        ] {
            assert_eq!(
                a6api_default_reasoning_efforts(model),
                a6api_default_reasoning_efforts(family)
            );
        }
    }

    #[test]
    fn a6api_active_inventory_is_sorted_eligible_union() {
        let first_active = connection_with_inventory(json!([
            " openai/gpt-6 ",
            "gemini-3-pro",
            "glm-5",
            "gpt-6-mini",
            "openai/gpt-6"
        ]));
        let mut second_active = connection_with_inventory(json!([
            "deepseek/deepseek-v4",
            "glm-5",
            "gpt-60",
            "claude-opus-4.6"
        ]));
        second_active.is_active = Some(true);
        let mut inactive =
            connection_with_inventory(json!(["glm-5-inactive-only", "deepseek-v4-inactive-only"]));
        inactive.is_active = Some(false);
        let mut other_provider = connection_with_inventory(json!(["gpt-6-other-provider-only"]));
        other_provider.provider = "openai".to_string();
        let db = AppDb {
            provider_connections: vec![first_active, second_active, inactive, other_provider],
            ..AppDb::default()
        };

        assert_eq!(
            a6api_active_model_ids(&db),
            [
                "deepseek/deepseek-v4",
                "glm-5",
                "gpt-6-mini",
                "gpt-60",
                "openai/gpt-6"
            ]
        );
    }

    #[test]
    fn a6api_active_inventory_handles_malformed_and_ineligible_inventory() {
        for inventory in [
            json!(null),
            json!("gpt-6"),
            json!({"id": "gpt-6"}),
            json!([null, false, 42, {}, []]),
            json!(["", "  ", "gemini-3-pro", "gpt-5.5", "GPT-6"]),
        ] {
            let db = AppDb {
                provider_connections: vec![connection_with_inventory(inventory)],
                ..AppDb::default()
            };
            assert!(a6api_active_model_ids(&db).is_empty());
        }

        let db = AppDb {
            provider_connections: vec![ProviderConnection {
                provider: "a6api".to_string(),
                ..ProviderConnection::default()
            }],
            ..AppDb::default()
        };
        assert!(a6api_active_model_ids(&db).is_empty());
    }

    #[test]
    fn a6api_raw_support_retains_filtered_out_exact_inventory_ids() {
        let connection =
            connection_with_inventory(json!([null, 42, " google/gemini-3-pro ", "", " gpt-6 "]));
        let other_connection = connection_with_inventory(json!(["gpt-6"]));
        assert_eq!(
            a6api_enabled_model_ids(&connection),
            ["google/gemini-3-pro", "gpt-6"]
        );
        assert!(a6api_connection_supports_model(
            &connection,
            " google/gemini-3-pro "
        ));
        assert!(!a6api_connection_supports_model(
            &connection,
            "gemini-3-pro"
        ));
        assert!(!a6api_connection_supports_model(
            &connection,
            "google/Gemini-3-pro"
        ));
        assert!(!a6api_connection_supports_model(
            &other_connection,
            "google/gemini-3-pro"
        ));
        let db = AppDb {
            provider_connections: vec![connection, other_connection],
            ..AppDb::default()
        };
        assert_eq!(a6api_active_model_ids(&db), ["gpt-6"]);
    }
}
