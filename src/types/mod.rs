use std::collections::{BTreeMap, HashMap};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppDb {
    /// Schema version for forward-compatibility checks.
    /// 0 = pre-encryption (legacy), 1 = AES-256-CBC on connection secrets.
    #[serde(default)]
    pub schema_version: u32,
    /// Hex-encoded SHA-256 checksum of the canonical JSON body (computed
    /// after serialisation but before writing; verified after reading).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checksum: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub provider_connections: Vec<ProviderConnection>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub provider_nodes: Vec<ProviderNode>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub proxy_pools: Vec<ProxyPool>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub model_aliases: BTreeMap<String, ModelAliasTarget>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub custom_models: Vec<CustomModel>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub api_keys: Vec<ApiKey>,
    /// Index of `api_keys` keyed by the raw `key` field for O(1) lookup.
    /// Rebuilt automatically in `normalize()` — not serialized.
    #[serde(default, skip)]
    pub api_key_map: HashMap<String, ApiKey>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub settings: Settings,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl AppDb {
    pub fn normalize(&mut self) {
        self.settings.normalize();
        self.extra.remove("pricing");
        self.extra.remove("mitmAlias");
        self.extra.remove("combos");

        for api_key in &mut self.api_keys {
            if api_key.is_active.is_none() {
                api_key.is_active = Some(true);
            }
            api_key.extra.remove("monthlyBudgetUsd");
            api_key.extra.remove("monthly_budget_usd");
        }

        // Strip empty providerFilters/favoriteModels from extra so that
        // export → from_json_value round-trips stay equal (empty scopes are
        // stored as 0 KV rows and should not appear as `"providerFilters": {}`
        // in the in-memory model). Also normalize legacy empty arrays.
        for key in ["providerFilters", "favoriteModels"] {
            if matches!(self.extra.get(key), Some(Value::Object(m)) if m.is_empty()) {
                self.extra.remove(key);
            }
        }
        if matches!(self.extra.get("disabledModels"), Some(Value::Array(a)) if a.is_empty()) {
            self.extra.remove("disabledModels");
        }
        if let Some(Value::Object(obj)) = self.extra.get("disabledModels") {
            if obj.is_empty() {
                self.extra.remove("disabledModels");
            }
        }

        // Rebuild the HashMap index for O(1) API key lookup.
        self.api_key_map = self
            .api_keys
            .iter()
            .map(|ak| (ak.key.clone(), ak.clone()))
            .collect();
    }

    pub fn from_json_value(value: Value) -> Self {
        let Value::Object(mut fields) = value else {
            return Self::default();
        };
        fields.remove("combos");

        let mut db = Self {
            schema_version: extract_named_field(&mut fields, "schemaVersion"),
            checksum: extract_named_field(&mut fields, "checksum"),
            provider_connections: extract_named_field(&mut fields, "providerConnections"),
            provider_nodes: extract_named_field(&mut fields, "providerNodes"),
            proxy_pools: extract_named_field(&mut fields, "proxyPools"),
            model_aliases: extract_named_field(&mut fields, "modelAliases"),
            custom_models: extract_named_field(&mut fields, "customModels"),
            api_keys: extract_named_field(&mut fields, "apiKeys"),
            api_key_map: HashMap::new(),
            settings: extract_named_field(&mut fields, "settings"),
            extra: fields.into_iter().collect(),
        };
        db.normalize();
        db
    }
}

/// Runtime transport configuration that can override the static provider config's base URL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTransport {
    /// Override base URL for this connection's requests.
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnection {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default = "default_auth_type")]
    pub auth_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub priority: Option<u32>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub global_priority: Option<u32>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub test_status: Option<String>,
    #[serde(default)]
    pub last_tested: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_at: Option<String>,
    #[serde(default)]
    pub rate_limited_until: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub consecutive_use_count: Option<u32>,
    #[serde(default)]
    pub backoff_level: Option<u32>,
    #[serde(default)]
    pub consecutive_errors: Option<u32>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub proxy_label: Option<String>,
    #[serde(default)]
    pub use_connection_proxy: Option<bool>,
    #[serde(default)]
    pub runtime_transport: Option<RuntimeTransport>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub provider_specific_data: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ProviderConnection {
    pub fn is_active(&self) -> bool {
        self.is_active.unwrap_or(true)
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNode {
    pub id: String,
    pub r#type: String,
    pub name: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub api_type: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProxyPool {
    pub id: String,
    pub name: String,
    pub proxy_url: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub no_proxy: String,
    #[serde(
        default = "default_proxy_type",
        deserialize_with = "deserialize_null_default"
    )]
    pub r#type: String,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub strict_proxy: Option<bool>,
    #[serde(default)]
    pub test_status: Option<String>,
    #[serde(default)]
    pub last_tested_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub success_rate: Option<f64>,
    #[serde(default)]
    pub rtt_ms: Option<u64>,
    #[serde(default)]
    pub total_requests: Option<u64>,
    #[serde(default)]
    pub failed_requests: Option<u64>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CustomModel {
    pub provider_alias: String,
    pub id: String,
    #[serde(
        default = "default_model_type",
        deserialize_with = "deserialize_null_default"
    )]
    pub r#type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ApiKey {
    pub fn is_active(&self) -> bool {
        self.is_active.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub cloud_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub cloud_url: String,
    /// Compatibility metadata advertised to clients for providers exposed in
    /// the provider UI. During the C07→C08 transition the legacy heuristic
    /// rejection path still reads the same values; proxy memory byte limits are
    /// separate. Missing and empty maps retain the historical defaults.
    #[serde(
        default = "crate::core::context_limit::default_provider_context_limits",
        deserialize_with = "deserialize_null_default"
    )]
    pub provider_context_limits: BTreeMap<String, u32>,
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_null_default"
    )]
    pub require_api_key: bool,
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_null_default"
    )]
    pub require_login: bool,
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_enabled: bool,
    #[serde(
        default = "default_observability_max_records",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_max_records: u32,
    #[serde(
        default = "default_observability_batch_size",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_batch_size: u32,
    #[serde(
        default = "default_observability_flush_interval_ms",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_flush_interval_ms: u32,
    #[serde(
        default = "default_observability_max_json_size",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_max_json_size: u32,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub outbound_proxy_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub outbound_proxy_url: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub outbound_no_proxy: String,
    #[serde(default, skip_serializing)]
    pub password: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub client_ping_url: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub client_ping_any: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cloud_enabled: false,
            cloud_url: String::new(),
            provider_context_limits: crate::core::context_limit::default_provider_context_limits(),
            // A fresh install must never expose inference routes without a key.
            require_api_key: true,
            // Dashboard login is independent from inference API-key auth.
            require_login: true,
            observability_enabled: true,
            observability_max_records: default_observability_max_records(),
            observability_batch_size: default_observability_batch_size(),
            observability_flush_interval_ms: default_observability_flush_interval_ms(),
            observability_max_json_size: default_observability_max_json_size(),
            outbound_proxy_enabled: false,
            outbound_proxy_url: String::new(),
            outbound_no_proxy: String::new(),
            password: None,
            client_ping_url: String::new(),
            client_ping_any: false,
            extra: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn normalize(&mut self) {
        if let Some(limit) = self.provider_context_limits.remove("opencode") {
            self.provider_context_limits
                .entry("opencode-zen".into())
                .or_insert(limit);
        }
        for (provider, limit) in crate::core::context_limit::default_provider_context_limits() {
            self.provider_context_limits
                .entry(provider)
                .or_insert(limit);
        }
        // Drop stale tunnel/tailscale keys from pre-removal databases so they
        // don't round-trip forever via the flattened `extra` map.
        for key in [
            "tunnelEnabled",
            "tunnelUrl",
            "tunnelProvider",
            "tailscaleEnabled",
            "tailscaleUrl",
            "tunnelDashboardAccess",
            "payloadRules",
            "systemPrompt",
            "ccFilterNaming",
            "providerThinking",
            "capacityAdapter",
            "comboStrategy",
            "comboStrategies",
            "comboStickyRoundRobinLimit",
            "fallbackStrategy",
            "fallback_strategy",
            "stickyRoundRobinLimit",
            "sticky_round_robin_limit",
            "providerStrategies",
            "provider_strategies",
            "mitmRouterBaseUrl",
            "mitmPort",
            "codexWebSearchContextSize",
        ] {
            self.extra.remove(key);
        }
        if !self.outbound_proxy_enabled && !self.outbound_proxy_url.trim().is_empty() {
            self.outbound_proxy_enabled = true;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ModelAliasTarget {
    Path(String),
    Mapping(ProviderModelRef),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelRef {
    pub provider: String,
    pub model: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct TokenUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    #[serde(default)]
    pub cached_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: &'static str,
    pub component: &'static str,
}

impl HealthResponse {
    pub fn new(component: &'static str) -> Self {
        Self {
            status: "ok",
            component,
        }
    }
}

fn default_auth_type() -> String {
    "oauth".into()
}

fn default_proxy_type() -> String {
    "http".into()
}

fn default_model_type() -> String {
    "llm".into()
}

fn default_observability_max_records() -> u32 {
    1000
}

fn default_observability_batch_size() -> u32 {
    20
}

fn default_observability_flush_interval_ms() -> u32 {
    5000
}

fn default_observability_max_json_size() -> u32 {
    1024
}

fn default_true() -> bool {
    true
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(|value| value.unwrap_or_default())
}

fn extract_named_field<T>(fields: &mut serde_json::Map<String, Value>, key: &str) -> T
where
    T: Default + DeserializeOwned,
{
    fields
        .remove(key)
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::Settings;
    use serde_json::json;

    #[test]
    fn settings_normalize_removes_only_retired_codex_search_depth() {
        let mut settings = Settings::default();
        settings
            .extra
            .insert("codexWebSearchContextSize".into(), json!("high"));
        settings.extra.insert("unrelatedSetting".into(), json!(42));

        settings.normalize();

        assert!(!settings.extra.contains_key("codexWebSearchContextSize"));
        assert_eq!(settings.extra.get("unrelatedSetting"), Some(&json!(42)));
    }
}
