use std::collections::{BTreeMap, HashMap};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::payload_rules::{PayloadRulesConfig, SystemPromptConfig};

pub const DEFAULT_MITM_ROUTER_BASE: &str = "http://localhost:4623";

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
    pub mitm_alias: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub combos: Vec<Combo>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub api_keys: Vec<ApiKey>,
    /// Index of `api_keys` keyed by the raw `key` field for O(1) lookup.
    /// Rebuilt automatically in `normalize()` — not serialized.
    #[serde(default, skip)]
    pub api_key_map: HashMap<String, ApiKey>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub settings: Settings,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub pricing: PricingTable,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl AppDb {
    pub fn normalize(&mut self) {
        self.settings.normalize();

        for api_key in &mut self.api_keys {
            if api_key.is_active.is_none() {
                api_key.is_active = Some(true);
            }
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

        let mut db = Self {
            schema_version: extract_named_field(&mut fields, "schemaVersion"),
            checksum: extract_named_field(&mut fields, "checksum"),
            provider_connections: extract_named_field(&mut fields, "providerConnections"),
            provider_nodes: extract_named_field(&mut fields, "providerNodes"),
            proxy_pools: extract_named_field(&mut fields, "proxyPools"),
            model_aliases: extract_named_field(&mut fields, "modelAliases"),
            custom_models: extract_named_field(&mut fields, "customModels"),
            mitm_alias: extract_named_field(&mut fields, "mitmAlias"),
            combos: extract_named_field(&mut fields, "combos"),
            api_keys: extract_named_field(&mut fields, "apiKeys"),
            api_key_map: HashMap::new(),
            settings: extract_named_field(&mut fields, "settings"),
            pricing: extract_named_field(&mut fields, "pricing"),
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

/// Per-combo strategy entry — 9router `settings.comboStrategies[name]`.
///
/// Accepts either a bare strategy string (`"fusion"`) for backward compatibility
/// or a nested object with `fallbackStrategy`, `judgeModel`, and `fusionTuning`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ComboStrategyEntry {
    /// Legacy flat form: `"round-robin"` / `"fusion"` / `"fallback"`.
    Name(String),
    /// Nested form matching 9router dashboard + chat handlers.
    Config(ComboStrategyConfig),
}

/// Per-provider account-fallback strategy entry — 9router
/// `settings.providerStrategies[providerId]`.
///
/// Accepts either a bare strategy string (`"round-robin"`) for backward
/// compatibility with older openproxy data or the 9router nested object
/// (`{ fallbackStrategy, stickyRoundRobinLimit, rotateStrategy, proxyPoolId }`)
/// the dashboard writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ProviderStrategyEntry {
    /// Legacy flat form: `"fill-first"` / `"round-robin"` / `"sticky"` / …
    Name(String),
    /// Nested form matching the 9router dashboard + auth service.
    Config(ProviderStrategyConfig),
}

impl ProviderStrategyEntry {
    /// Account fallback strategy name used by chat connection selection.
    pub fn fallback_strategy(&self) -> Option<&str> {
        match self {
            Self::Name(s) => Some(s.as_str()),
            Self::Config(c) => c.fallback_strategy.as_deref().filter(|s| !s.is_empty()),
        }
    }

    /// Sticky round-robin limit override (9router `stickyRoundRobinLimit`).
    pub fn sticky_round_robin_limit(&self) -> Option<u32> {
        match self {
            Self::Name(_) => None,
            Self::Config(c) => c.sticky_round_robin_limit,
        }
    }

    /// Free-provider rotation strategy (9router `rotateStrategy`).
    pub fn rotate_strategy(&self) -> Option<&str> {
        match self {
            Self::Name(_) => None,
            Self::Config(c) => c.rotate_strategy.as_deref().filter(|s| !s.is_empty()),
        }
    }

    /// Bound proxy pool id (9router `proxyPoolId`).
    pub fn proxy_pool_id(&self) -> Option<&str> {
        match self {
            Self::Name(_) => None,
            Self::Config(c) => c.proxy_pool_id.as_deref().filter(|s| !s.is_empty()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStrategyConfig {
    #[serde(default)]
    pub fallback_strategy: Option<String>,
    #[serde(default)]
    pub sticky_round_robin_limit: Option<u32>,
    #[serde(default)]
    pub rotate_strategy: Option<String>,
    #[serde(default)]
    pub proxy_pool_id: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ComboStrategyEntry {
    /// Strategy name used by the dispatcher (`fallback` | `round-robin` | `fusion` | …).
    pub fn strategy_name(&self) -> &str {
        match self {
            Self::Name(s) => s.as_str(),
            Self::Config(c) => c.fallback_strategy.as_deref().unwrap_or("fallback"),
        }
    }

    pub fn judge_model(&self) -> Option<&str> {
        match self {
            Self::Config(c) => c.judge_model.as_deref().filter(|s| !s.is_empty()),
            Self::Name(_) => None,
        }
    }

    pub fn fusion_tuning(&self) -> Option<&Value> {
        match self {
            Self::Config(c) => c.fusion_tuning.as_ref(),
            Self::Name(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComboStrategyConfig {
    #[serde(default)]
    pub fallback_strategy: Option<String>,
    #[serde(default)]
    pub judge_model: Option<String>,
    #[serde(default)]
    pub fusion_tuning: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Combo {
    pub id: String,
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub models: Vec<String>,
    /// Combo members the operator has explicitly muted. The dispatcher
    /// filters these out *before* rotation / capacity / iteration, so a
    /// "known bad" model can stay in the configured list (for visibility
    /// or quick re-enable) without ever being dispatched to. Empty by
    /// default.
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub disabled_models: Vec<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
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
    /// Per-key monthly spend cap in USD. When set, requests are hard-blocked
    /// with HTTP 429 once the current calendar month's tracked spend reaches
    /// it (free-tier Feature 3: budget caps & hard kill-switch).
    #[serde(default)]
    pub monthly_budget_usd: Option<f64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ApiKey {
    pub fn is_active(&self) -> bool {
        self.is_active.unwrap_or(true)
    }

    /// Effective monthly budget in USD, resolving the typed field first and
    /// then the `extra` fallbacks (`monthlyBudgetUsd`, `monthly_budget_usd`).
    /// Returns `None` when no budget is configured.
    pub fn monthly_budget(&self) -> Option<f64> {
        if let Some(value) = self.monthly_budget_usd {
            return Some(value);
        }
        for key in ["monthlyBudgetUsd", "monthly_budget_usd"] {
            if let Some(value) = self.extra.get(key).and_then(|v| v.as_f64()) {
                return Some(value);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub cloud_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub cloud_url: String,
    /// Sticky limit for account round-robin (9router `stickyRoundRobinLimit`).
    #[serde(
        default = "default_sticky_round_robin_limit",
        deserialize_with = "deserialize_null_default"
    )]
    pub sticky_round_robin_limit: u32,
    /// Per-provider account-fallback overrides. Accepts legacy bare string
    /// (`"round-robin"`) or the 9router nested object the dashboard writes
    /// (`{ fallbackStrategy, stickyRoundRobinLimit, rotateStrategy, proxyPoolId }`).
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub provider_strategies: BTreeMap<String, ProviderStrategyEntry>,
    #[serde(
        default = "default_combo_strategy",
        deserialize_with = "deserialize_null_default"
    )]
    pub combo_strategy: String,
    /// Per-combo strategy overrides. Accepts legacy string (`"fusion"`) or
    /// 9router nested object (`{ fallbackStrategy, judgeModel, fusionTuning }`).
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub combo_strategies: BTreeMap<String, ComboStrategyEntry>,
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
    #[serde(
        default = "default_mitm_router_base_url",
        deserialize_with = "deserialize_null_default"
    )]
    pub mitm_router_base_url: String,
    #[serde(
        default = "default_mitm_port",
        deserialize_with = "deserialize_null_default"
    )]
    pub mitm_port: u16,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub payload_rules: PayloadRulesConfig,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub system_prompt: SystemPromptConfig,
    #[serde(default, skip_serializing)]
    pub password: Option<String>,
    /// Account-level round-robin: "fill-first" or "round-robin".
    #[serde(
        default = "default_fallback_strategy",
        deserialize_with = "deserialize_null_default"
    )]
    pub fallback_strategy: String,
    /// Sticky limit for combo round-robin (separate from account sticky).
    #[serde(
        default = "default_combo_sticky_round_robin_limit",
        deserialize_with = "deserialize_null_default"
    )]
    pub combo_sticky_round_robin_limit: u32,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub client_ping_url: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub client_ping_any: bool,
    /// Per-capability model pools for the capacity adapter
    /// (9router settingsRepo.js capacityAdapter defaults). Read at dispatch
    /// time; PATCHed by the dashboard via `update_settings_api`.
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub capacity_adapter: Value,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cloud_enabled: false,
            cloud_url: String::new(),
            sticky_round_robin_limit: default_sticky_round_robin_limit(),
            provider_strategies: BTreeMap::new(),
            combo_strategy: default_combo_strategy(),
            combo_strategies: BTreeMap::new(),
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
            mitm_router_base_url: default_mitm_router_base_url(),
            mitm_port: default_mitm_port(),
            payload_rules: PayloadRulesConfig::default(),
            system_prompt: SystemPromptConfig::default(),
            password: None,
            fallback_strategy: default_fallback_strategy(),
            combo_sticky_round_robin_limit: default_combo_sticky_round_robin_limit(),
            client_ping_url: String::new(),
            client_ping_any: false,
            capacity_adapter: json!({}),
            extra: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn normalize(&mut self) {
        // Drop stale tunnel/tailscale keys from pre-removal databases so they
        // don't round-trip forever via the flattened `extra` map.
        for key in [
            "tunnelEnabled",
            "tunnelUrl",
            "tunnelProvider",
            "tailscaleEnabled",
            "tailscaleUrl",
            "tunnelDashboardAccess",
        ] {
            self.extra.remove(key);
        }
        if !self.outbound_proxy_enabled && !self.outbound_proxy_url.trim().is_empty() {
            self.outbound_proxy_enabled = true;
        }

        self.fallback_strategy = normalize_fallback_strategy(&self.fallback_strategy);
        if self.combo_sticky_round_robin_limit == 0 {
            self.combo_sticky_round_robin_limit = default_combo_sticky_round_robin_limit();
        }
        self.payload_rules.normalize();
        self.system_prompt.normalize();
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

pub type PricingTable = BTreeMap<String, BTreeMap<String, Value>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageDb {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub history: Vec<UsageEntry>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub total_requests_lifetime: u64,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub daily_summary: BTreeMap<String, DailySummary>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl UsageDb {
    pub fn normalize(&mut self) {
        if self.total_requests_lifetime < self.history.len() as u64 {
            self.total_requests_lifetime = self.history.len() as u64;
        }

        self.daily_summary.clear();
        for entry in &self.history {
            aggregate_usage_entry(&mut self.daily_summary, entry);
        }
    }

    pub fn from_json_value(value: Value) -> Self {
        let Value::Object(mut fields) = value else {
            return Self::default();
        };

        let mut usage = Self {
            history: extract_named_field(&mut fields, "history"),
            total_requests_lifetime: extract_named_field(&mut fields, "totalRequestsLifetime"),
            daily_summary: extract_named_field(&mut fields, "dailySummary"),
            extra: fields.into_iter().collect(),
        };
        usage.normalize();
        usage
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub model: String,
    #[serde(default)]
    pub tokens: Option<TokenUsage>,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub bytes_before: u64,
    #[serde(default)]
    pub bytes_after: u64,
    #[serde(default)]
    pub bytes_saved: u64,
    #[serde(default)]
    pub image_prompts: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DailySummary {
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub by_provider: BTreeMap<String, SummaryCounter>,
    #[serde(default)]
    pub by_model: BTreeMap<String, SummaryCounter>,
    #[serde(default)]
    pub by_account: BTreeMap<String, SummaryCounter>,
    #[serde(default)]
    pub by_api_key: BTreeMap<String, SummaryCounter>,
    #[serde(default)]
    pub by_endpoint: BTreeMap<String, SummaryCounter>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SummaryCounter {
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub raw_model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
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

fn default_sticky_round_robin_limit() -> u32 {
    3
}

fn default_combo_strategy() -> String {
    "fallback".into()
}

fn default_fallback_strategy() -> String {
    "fill-first".into()
}

fn default_combo_sticky_round_robin_limit() -> u32 {
    1
}

fn normalize_fallback_strategy(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "round-robin" | "roundrobin" | "round_robin" => "round-robin".into(),
        _ => "fill-first".into(),
    }
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

fn default_mitm_router_base_url() -> String {
    DEFAULT_MITM_ROUTER_BASE.into()
}

/// Default MITM proxy port. 0 = OS-assigned ephemeral port.
fn default_mitm_port() -> u16 {
    0
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

fn aggregate_usage_entry(daily_summary: &mut BTreeMap<String, DailySummary>, entry: &UsageEntry) {
    let date_key = entry
        .timestamp
        .as_deref()
        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.date_naive().to_string())
        .or_else(|| {
            entry
                .timestamp
                .as_ref()
                .map(|timestamp| timestamp.chars().take(10).collect())
        })
        .unwrap_or_else(|| "unknown".into());

    let summary = daily_summary
        .entry(date_key)
        .or_insert_with(|| DailySummary {
            requests: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: 0,
            cached_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            cost: 0.0,
            by_provider: BTreeMap::new(),
            by_model: BTreeMap::new(),
            by_account: BTreeMap::new(),
            by_api_key: BTreeMap::new(),
            by_endpoint: BTreeMap::new(),
            extra: BTreeMap::new(),
        });

    let prompt_tokens = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.prompt_tokens.or(tokens.input_tokens))
        .unwrap_or(0);
    let completion_tokens = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.completion_tokens.or(tokens.output_tokens))
        .unwrap_or(0);
    let reasoning_tokens = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.reasoning_tokens)
        .unwrap_or(0);
    let cached_tokens = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.cached_tokens)
        .unwrap_or(0);
    let cache_read_input_tokens = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.cache_read_input_tokens)
        .unwrap_or(0);
    let cache_creation_input_tokens = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.cache_creation_input_tokens)
        .unwrap_or(0);
    let cost = entry.cost.unwrap_or(0.0);

    summary.requests += 1;
    summary.prompt_tokens += prompt_tokens;
    summary.completion_tokens += completion_tokens;
    summary.reasoning_tokens += reasoning_tokens;
    summary.cached_tokens += cached_tokens;
    summary.cache_read_input_tokens += cache_read_input_tokens;
    summary.cache_creation_input_tokens += cache_creation_input_tokens;
    summary.cost += cost;

    if let Some(provider) = entry.provider.as_deref() {
        add_to_counter(
            &mut summary.by_provider,
            provider,
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            cached_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            cost,
            None,
        );
    }

    let model_key = match entry.provider.as_deref() {
        Some(provider) => format!("{}|{}", entry.model, provider),
        None => entry.model.clone(),
    };
    add_to_counter(
        &mut summary.by_model,
        &model_key,
        prompt_tokens,
        completion_tokens,
        reasoning_tokens,
        cached_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
        cost,
        Some((
            Some(entry.model.clone()),
            entry.provider.clone(),
            None,
            None,
        )),
    );

    if let Some(connection_id) = entry.connection_id.as_deref() {
        add_to_counter(
            &mut summary.by_account,
            connection_id,
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            cached_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            cost,
            Some((
                Some(entry.model.clone()),
                entry.provider.clone(),
                None,
                None,
            )),
        );
    }

    let api_key = entry
        .api_key
        .clone()
        .unwrap_or_else(|| "local-no-key".into());
    let api_key_key = format!(
        "{}|{}|{}",
        api_key,
        entry.model,
        entry.provider.clone().unwrap_or_else(|| "unknown".into())
    );
    add_to_counter(
        &mut summary.by_api_key,
        &api_key_key,
        prompt_tokens,
        completion_tokens,
        reasoning_tokens,
        cached_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
        cost,
        Some((
            Some(entry.model.clone()),
            entry.provider.clone(),
            Some(entry.api_key.clone().unwrap_or_default()),
            None,
        )),
    );

    let endpoint = entry.endpoint.clone().unwrap_or_else(|| "Unknown".into());
    let endpoint_key = format!(
        "{}|{}|{}",
        endpoint,
        entry.model,
        entry.provider.clone().unwrap_or_else(|| "unknown".into())
    );
    add_to_counter(
        &mut summary.by_endpoint,
        &endpoint_key,
        prompt_tokens,
        completion_tokens,
        reasoning_tokens,
        cached_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
        cost,
        Some((
            Some(entry.model.clone()),
            entry.provider.clone(),
            None,
            Some(endpoint),
        )),
    );
}

type SummaryMetadata = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn add_to_counter(
    target: &mut BTreeMap<String, SummaryCounter>,
    key: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    reasoning_tokens: u64,
    cached_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    cost: f64,
    metadata: Option<SummaryMetadata>,
) {
    let counter = target
        .entry(key.to_string())
        .or_insert_with(|| SummaryCounter {
            requests: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: 0,
            cached_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            cost: 0.0,
            raw_model: None,
            provider: None,
            api_key: None,
            endpoint: None,
            extra: BTreeMap::new(),
        });

    counter.requests += 1;
    counter.prompt_tokens += prompt_tokens;
    counter.completion_tokens += completion_tokens;
    counter.reasoning_tokens += reasoning_tokens;
    counter.cached_tokens += cached_tokens;
    counter.cache_read_input_tokens += cache_read_input_tokens;
    counter.cache_creation_input_tokens += cache_creation_input_tokens;
    counter.cost += cost;

    if let Some((raw_model, provider, api_key, endpoint)) = metadata {
        if raw_model.is_some() {
            counter.raw_model = raw_model;
        }
        if provider.is_some() {
            counter.provider = provider;
        }
        if api_key.is_some() {
            counter.api_key = api_key;
        }
        if endpoint.is_some() {
            counter.endpoint = endpoint;
        }
    }
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
mod provider_strategy_tests {
    use super::*;

    /// 9router dashboard writes nested objects — must deserialize (was a
    /// hard PATCH rejection when the field was BTreeMap<String, String>).
    #[test]
    fn provider_strategies_accept_nested_objects() {
        let json = json!({
            "providerStrategies": {
                "claude": { "fallbackStrategy": "round-robin", "stickyRoundRobinLimit": 5 },
                "qwen": { "rotateStrategy": "none", "proxyPoolId": "pool-1" }
            }
        });
        let settings: Settings = serde_json::from_value(json).unwrap();
        let claude = settings.provider_strategies.get("claude").unwrap();
        assert_eq!(claude.fallback_strategy(), Some("round-robin"));
        assert_eq!(claude.sticky_round_robin_limit(), Some(5));
        let qwen = settings.provider_strategies.get("qwen").unwrap();
        assert_eq!(qwen.rotate_strategy(), Some("none"));
        assert_eq!(qwen.proxy_pool_id(), Some("pool-1"));
    }

    /// Legacy openproxy data stores bare strings.
    #[test]
    fn provider_strategies_accept_legacy_strings() {
        let json = json!({ "providerStrategies": { "gemini": "round-robin" } });
        let settings: Settings = serde_json::from_value(json).unwrap();
        let entry = settings.provider_strategies.get("gemini").unwrap();
        assert_eq!(entry.fallback_strategy(), Some("round-robin"));
        assert!(matches!(entry, ProviderStrategyEntry::Name(_)));
    }

    /// Round-trips through the API payload shape.
    #[test]
    fn provider_strategy_entry_serializes_back_to_object() {
        let entry = ProviderStrategyEntry::Config(ProviderStrategyConfig {
            fallback_strategy: Some("sticky".into()),
            sticky_round_robin_limit: Some(7),
            ..Default::default()
        });
        let v = serde_json::to_value(&entry).unwrap();
        assert_eq!(v["fallbackStrategy"], "sticky");
        assert_eq!(v["stickyRoundRobinLimit"], 7);
    }
}
