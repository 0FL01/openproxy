use std::collections::HashMap;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderCatalogFile {
    provider_id_to_alias: HashMap<String, String>,
    provider_models: Vec<ProviderModelsEntry>,
    providers: Vec<ProviderCatalogProvider>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub quota_family: Option<String>,
    #[serde(default)]
    pub strip: Option<String>,
    #[serde(default)]
    pub target_format: Option<String>,
    #[serde(default)]
    pub upstream_model_id: Option<String>,
    #[serde(default, alias = "contextLength")]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub reasoning_efforts: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsEntry {
    pub alias: String,
    pub models: Vec<ProviderCatalogModel>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogProvider {
    pub id: String,
    pub alias: String,
    pub service_kinds: Vec<String>,
    #[serde(default)]
    pub vision: Option<bool>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output: Option<u32>,
    #[serde(default)]
    pub tools: Option<bool>,
}

#[derive(Debug)]
pub struct ProviderCatalog {
    provider_id_to_alias: HashMap<String, String>,
    provider_models: Vec<ProviderModelsEntry>,
    provider_models_by_alias: HashMap<String, Vec<ProviderCatalogModel>>,
    providers_by_id: HashMap<String, ProviderCatalogProvider>,
}

impl ProviderCatalog {
    pub fn provider_info(&self, provider_id: &str) -> Option<&ProviderCatalogProvider> {
        self.providers_by_id.get(provider_id)
    }

    pub fn static_alias_for_provider(&self, provider_id: &str) -> Option<&str> {
        self.provider_id_to_alias
            .get(provider_id)
            .map(String::as_str)
    }

    pub fn iter_provider_models(&self) -> impl Iterator<Item = &ProviderModelsEntry> {
        self.provider_models.iter()
    }

    pub fn models_for_alias(&self, alias: &str) -> Option<&[ProviderCatalogModel]> {
        self.provider_models_by_alias.get(alias).map(Vec::as_slice)
    }

    pub fn find_model(&self, provider_id: &str, model_id: &str) -> Option<&ProviderCatalogModel> {
        let alias = self.static_alias_for_provider(provider_id)?;
        self.models_for_alias(alias)?
            .iter()
            .find(|m| m.id == model_id)
    }

    /// Build reverse map: alias → provider_id.
    ///
    /// Self-referencing entries (where provider_id == alias) are inserted
    /// **only** when no non-self-referencing entry already claimed that alias.
    /// This prevents `qianfan → qianfan` from overwriting the correct
    /// `qianfan → baidu` mapping produced by the forward entry `baidu → qianfan`.
    pub fn alias_to_provider_id(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        // Pass 1: non-self-referencing entries (baidu → qianfan).
        for (provider_id, alias) in &self.provider_id_to_alias {
            if provider_id != alias {
                map.insert(alias.clone(), provider_id.clone());
            }
        }
        // Pass 2: self-referencing entries (openai → openai) only if not taken.
        for (provider_id, alias) in &self.provider_id_to_alias {
            if provider_id == alias && !map.contains_key(alias) {
                map.insert(alias.clone(), provider_id.clone());
            }
        }
        map
    }
}

static PROVIDER_CATALOG: Lazy<ProviderCatalog> = Lazy::new(|| {
    let raw = include_str!("provider_catalog.json");
    let parsed: ProviderCatalogFile =
        serde_json::from_str(raw).expect("provider_catalog.json should be valid");

    let provider_models_by_alias = parsed
        .provider_models
        .iter()
        .map(|entry| (entry.alias.clone(), entry.models.clone()))
        .collect();

    let providers_by_id = parsed
        .providers
        .iter()
        .map(|provider| (provider.id.clone(), provider.clone()))
        .collect();

    ProviderCatalog {
        provider_id_to_alias: parsed.provider_id_to_alias,
        provider_models: parsed.provider_models,
        provider_models_by_alias,
        providers_by_id,
    }
});

pub fn provider_catalog() -> &'static ProviderCatalog {
    &PROVIDER_CATALOG
}

#[cfg(test)]
mod tests {
    use super::*;

    // OpenCode models are populated from models.dev at runtime. The static
    // catalog keeps only the provider registration and an empty insertion point.
    #[test]
    fn opencode_zen_registered_in_static_catalog() {
        let catalog = provider_catalog();

        let provider = catalog
            .provider_info("opencode-zen")
            .expect("opencode-zen should have a provider entry in provider_catalog.json");
        assert_eq!(provider.alias, "opencode-zen");

        let models = catalog
            .models_for_alias("opencode-zen")
            .expect("opencode-zen should have a providerModels entry");
        assert!(models.is_empty());
    }

    // Bead .46: all 17 parity providers must be registered in the static
    // catalog (providerIdToAlias + providerModels + providers[]), keyed by the
    // same aliases the 9router v0.5.50 registry files declare.
    #[test]
    fn all_17_parity_providers_registered() {
        let catalog = provider_catalog();

        // (provider id, js alias) — mirror of the `alias` fields in
        // open-sse/providers/registry/*.js for the 17 parity providers.
        let expected: &[(&str, &str)] = &[
            ("api-airforce", "af"),
            ("baidu", "qianfan"),
            ("bluesminds", "bm"),
            ("clinepass", "clinepass"),
            ("codebuddy-intl", "cbai"),
            ("featherless", "featherless"),
            ("kilo-gateway", "kgw"),
            ("perplexity-agent", "perplexity-agent"),
            ("poolside", "poolside"),
            ("selfhosted-embedding", "selfhosted-embedding"),
            ("selfhosted-stt", "selfhosted-stt"),
            ("selfhosted-tts", "selfhosted-tts"),
            ("tencent", "hunyuan"),
            ("tokenrouter", "tokenrouter"),
            ("venice", "venice"),
            ("zed", "zd"),
            ("alims-intl", "alims-intl"),
        ];

        for (provider_id, alias) in expected {
            assert_eq!(
                catalog.static_alias_for_provider(provider_id),
                Some(*alias),
                "providerIdToAlias[{}] should resolve to {}",
                provider_id,
                alias
            );
            assert!(
                catalog.models_for_alias(alias).is_some(),
                "providerModels should contain an entry for alias {}",
                alias
            );
            let provider = catalog
                .provider_info(provider_id)
                .unwrap_or_else(|| panic!("providers[] should contain {}", provider_id));
            assert_eq!(provider.alias, *alias);
        }
    }

    // The catalog models carry the JS `contextLength` through into
    // context_window (bead .46: it was previously dropped by deserialization).
    #[test]
    fn catalog_model_context_length_survives_deserialization() {
        let catalog = provider_catalog();

        let m = catalog
            .find_model("baidu", "deepseek-v4-pro")
            .expect("baidu/deepseek-v4-pro should be in provider_catalog.json");
        assert_eq!(m.context_window, Some(1_048_576));
        assert_eq!(m.kind, "llm");

        // api-airforce and kilo-gateway carry explicit context lengths too.
        let m = catalog
            .find_model("api-airforce", "google/gemini-2.5-flash")
            .expect("api-airforce/google/gemini-2.5-flash should be in the catalog");
        assert_eq!(m.context_window, Some(1_048_576));
        let m = catalog
            .find_model("kilo-gateway", "nvidia/nemotron-3-ultra-550b-a55b:free")
            .expect("kilo-gateway nemotron should be in the catalog");
        assert_eq!(m.context_window, Some(1_000_000));
    }

    // providerIdToAlias must resolve provider ids to the JS aliases, and
    // find_model must reach models through them (bead .46).
    #[test]
    fn parity_provider_aliases_resolve() {
        let catalog = provider_catalog();

        for (provider_id, alias) in [
            ("venice", "venice"),
            ("tencent", "hunyuan"),
            ("baidu", "qianfan"),
            ("zed", "zd"),
            ("codebuddy-intl", "cbai"),
            ("kilo-gateway", "kgw"),
            ("api-airforce", "af"),
            ("bluesminds", "bm"),
            ("tokenrouter", "tokenrouter"),
            ("perplexity-agent", "perplexity-agent"),
            ("alitp-intl", "alitp-intl"),
        ] {
            assert_eq!(
                catalog.static_alias_for_provider(provider_id),
                Some(alias),
                "{} should map to {}",
                provider_id,
                alias
            );
        }

        // Reverse resolution: alias -> provider id.
        let reverse = catalog.alias_to_provider_id();
        for (provider_id, alias) in [
            ("venice", "venice"),
            ("tencent", "hunyuan"),
            ("baidu", "qianfan"),
        ] {
            assert_eq!(reverse.get(alias).map(String::as_str), Some(provider_id));
        }

        let m = catalog
            .find_model("baidu", "deepseek-v4-pro")
            .expect("baidu/deepseek-v4-pro should resolve through qianfan");
        assert_eq!(m.name.as_deref(), Some("DeepSeek V4 Pro"));

        // alitp-intl (Alibaba Token Plan) — new in v0.5.55.
        let m = catalog
            .find_model("alitp-intl", "qwen3.7-max")
            .expect("alitp-intl/qwen3.7-max should resolve");
        assert_eq!(m.name.as_deref(), Some("Qwen3.7 Max"));
        let m = catalog
            .find_model("alitp-intl", "deepseek-v4-pro")
            .expect("alitp-intl/deepseek-v4-pro should resolve");
        assert_eq!(m.name.as_deref(), Some("DeepSeek V4 Pro"));

        // GLM 5.3 — new in v0.5.55.
        let m = catalog
            .find_model("glm", "glm-5.3")
            .expect("glm/glm-5.3 should resolve");
        assert_eq!(m.name.as_deref(), Some("GLM 5.3"));
        let m = catalog
            .find_model("glm-cn", "glm-5.3")
            .expect("glm-cn/glm-5.3 should resolve");
        assert_eq!(m.name.as_deref(), Some("GLM 5.3"));

        // Gemini 3.7 Flash tiered — new in v0.5.55.
        for (id, name) in [
            ("gemini-3.7-flash-high", "Gemini 3.7 Flash (High)"),
            ("gemini-3.7-flash-medium", "Gemini 3.7 Flash (Medium)"),
            ("gemini-3.7-flash-low", "Gemini 3.7 Flash (Low)"),
        ] {
            let m = catalog
                .find_model("antigravity", id)
                .unwrap_or_else(|| panic!("antigravity/{id} should resolve"));
            assert_eq!(m.name.as_deref(), Some(name));
            // Each tier maps to an upstreamModelId.
            let upstream = m.upstream_model_id.as_deref().unwrap_or("");
            assert!(
                upstream.contains("gemini-3.7-flash-tiered"),
                "{id} should have upstreamModelId containing tiered, got: {upstream}"
            );
        }
    }
}
