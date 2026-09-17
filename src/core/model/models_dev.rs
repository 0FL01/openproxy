use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use reqwest::header::USER_AGENT;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::core::translator::registry::Format;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const FAILED_REFRESH_RETRY_DELAY: Duration = Duration::from_secs(30);
const BUNDLED_MODELS_DEV_JSON: &str = include_str!("models_dev_bundled.json");

#[derive(Debug, Clone)]
pub struct OpenCodeModelMetadata {
    pub id: String,
    pub name: String,
    pub format: Format,
    pub family: Option<String>,
    pub context_window: Option<u32>,
    pub max_input: Option<u32>,
    pub max_output: Option<u32>,
    pub attachment: Option<bool>,
    pub input_modalities: Option<Vec<String>>,
    pub output_modalities: Option<Vec<String>>,
    pub reasoning: Option<bool>,
    pub tool_call: Option<bool>,
    pub capabilities: Vec<String>,
    pub reasoning_efforts: Option<Vec<String>>,
}

impl OpenCodeModelMetadata {
    pub fn catalog_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "kind": "llm",
            "targetFormat": self.format.as_str(),
            "family": self.family,
            "contextWindow": self.context_window,
            "maxInput": self.max_input,
            "maxOutput": self.max_output,
            "capabilities": self.capabilities,
            "reasoningEfforts": self.reasoning_efforts.as_deref().unwrap_or_default(),
        })
    }
}

#[derive(Debug, Default)]
pub struct ModelsDevSnapshot {
    providers: HashMap<String, Vec<OpenCodeModelMetadata>>,
}

impl ModelsDevSnapshot {
    pub fn models(&self, provider: &str) -> Option<&[OpenCodeModelMetadata]> {
        let provider = canonical_provider(provider)?;
        self.providers.get(provider).map(Vec::as_slice)
    }

    pub fn find(&self, provider: &str, model: &str) -> Option<&OpenCodeModelMetadata> {
        self.models(provider)?
            .iter()
            .find(|entry| entry.id == model)
    }
}

#[derive(Debug, Default)]
struct RefreshState {
    last_success: Option<Instant>,
    last_attempt: Option<Instant>,
    last_error: Option<String>,
}

pub struct ModelsDevCatalog {
    client: reqwest::Client,
    endpoint: String,
    published: ArcSwap<ModelsDevSnapshot>,
    // This mutex coordinates refresh writers only. Readers use `published`
    // and never wait for this lock or for remote I/O.
    refresh: Mutex<RefreshState>,
}

impl Default for ModelsDevCatalog {
    fn default() -> Self {
        let bundled: Value = serde_json::from_str(BUNDLED_MODELS_DEV_JSON)
            .expect("embedded models.dev snapshot should be valid JSON");
        let bundled = parse_snapshot(bundled)
            .expect("embedded models.dev snapshot should match the expected schema");
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("models.dev HTTP client should build"),
            endpoint: MODELS_DEV_URL.to_string(),
            published: ArcSwap::from_pointee(bundled),
            refresh: Mutex::new(RefreshState::default()),
        }
    }
}

impl ModelsDevCatalog {
    /// Return the currently published immutable catalog immediately.
    ///
    /// This method performs no network I/O and does not acquire the refresh
    /// mutex, so it is safe on the generation path while a refresh is hung.
    pub fn load(&self) -> Arc<ModelsDevSnapshot> {
        self.published.load_full()
    }

    /// Refresh if the last successful publication is stale. Concurrent
    /// control-plane callers share the writer lock and re-check freshness
    /// after acquiring it. A failed refresh never replaces the prior snapshot.
    pub async fn refresh_if_stale(&self) -> Result<Arc<ModelsDevSnapshot>, String> {
        self.refresh_inner(false).await
    }

    /// Explicit control-plane refresh. Readers continue using the prior
    /// immutable snapshot until the new response has fully validated.
    pub async fn refresh(&self) -> Result<Arc<ModelsDevSnapshot>, String> {
        self.refresh_inner(true).await
    }

    async fn refresh_inner(&self, force: bool) -> Result<Arc<ModelsDevSnapshot>, String> {
        let mut state = self.refresh.lock().await;
        if !force {
            if state
                .last_success
                .is_some_and(|loaded| loaded.elapsed() < CACHE_TTL)
            {
                return Ok(self.load());
            }
            if state
                .last_attempt
                .is_some_and(|attempt| attempt.elapsed() < FAILED_REFRESH_RETRY_DELAY)
            {
                return Err(state.last_error.clone().unwrap_or_else(|| {
                    "models.dev refresh is temporarily throttled after a failure".to_string()
                }));
            }
        }
        state.last_attempt = Some(Instant::now());

        let fetched = self.fetch_snapshot().await;
        match fetched {
            Ok(snapshot) => {
                let snapshot = Arc::new(snapshot);
                self.published.store(snapshot.clone());
                state.last_success = Some(Instant::now());
                state.last_error = None;
                Ok(snapshot)
            }
            Err(error) => {
                state.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    async fn fetch_snapshot(&self) -> Result<ModelsDevSnapshot, String> {
        let response = self
            .client
            .get(&self.endpoint)
            .header(USER_AGENT, "opencode")
            .send()
            .await
            .map_err(|error| format!("models.dev request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("models.dev returned HTTP {}", response.status()));
        }
        let value = response
            .json::<Value>()
            .await
            .map_err(|error| format!("models.dev response is invalid: {error}"))?;
        parse_snapshot(value)
    }

    #[cfg(test)]
    pub fn from_json(value: Value) -> Result<Self, String> {
        Self::from_json_with_endpoint(value, MODELS_DEV_URL)
    }

    /// Test/control-plane constructor with an explicit loopback endpoint.
    /// Production uses [`Default`] and the fixed public models.dev endpoint.
    #[doc(hidden)]
    pub fn from_json_with_endpoint(
        value: Value,
        endpoint: impl Into<String>,
    ) -> Result<Self, String> {
        let snapshot = parse_snapshot(value)?;
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .map_err(|error| format!("models.dev HTTP client failed: {error}"))?,
            endpoint: endpoint.into(),
            published: ArcSwap::from_pointee(snapshot),
            refresh: Mutex::new(RefreshState::default()),
        })
    }
}

pub fn is_opencode_provider(provider: &str) -> bool {
    canonical_provider(provider).is_some()
}

fn canonical_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "opencode" | "opencode-zen" => Some("opencode-zen"),
        "opencode-go" => Some("opencode-go"),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct ApiProvider {
    npm: String,
    #[serde(default)]
    models: BTreeMap<String, ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
    name: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    provider: Option<ApiModelProvider>,
    #[serde(default)]
    cost: Option<ApiCost>,
    #[serde(default)]
    limit: Option<ApiLimit>,
    #[serde(default)]
    modalities: Option<ApiModalities>,
    #[serde(default)]
    attachment: Option<bool>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    tool_call: Option<bool>,
    #[serde(default)]
    reasoning_options: Option<Vec<ApiReasoningOption>>,
}

#[derive(Debug, Deserialize)]
struct ApiModelProvider {
    npm: String,
}

#[derive(Debug, Deserialize)]
struct ApiCost {
    input: f64,
    output: f64,
}

#[derive(Debug, Deserialize)]
struct ApiLimit {
    context: Option<u64>,
    input: Option<u64>,
    output: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ApiModalities {
    #[serde(default)]
    input: Vec<String>,
    #[serde(default)]
    output: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ApiReasoningOption {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    values: Vec<String>,
}

fn parse_snapshot(value: Value) -> Result<ModelsDevSnapshot, String> {
    let mut source = value
        .as_object()
        .cloned()
        .ok_or_else(|| "models.dev response must be an object".to_string())?;
    let zen = source
        .remove("opencode")
        .ok_or_else(|| "models.dev has no opencode provider".to_string())?;
    let go = source
        .remove("opencode-go")
        .ok_or_else(|| "models.dev has no opencode-go provider".to_string())?;
    let zen: ApiProvider = serde_json::from_value(zen)
        .map_err(|error| format!("models.dev opencode schema mismatch: {error}"))?;
    let go: ApiProvider = serde_json::from_value(go)
        .map_err(|error| format!("models.dev opencode-go schema mismatch: {error}"))?;

    let mut providers = HashMap::new();
    providers.insert("opencode-zen".to_string(), map_models(zen, true));
    providers.insert("opencode-go".to_string(), map_models(go, false));
    Ok(ModelsDevSnapshot { providers })
}

fn map_models(provider: ApiProvider, free_only: bool) -> Vec<OpenCodeModelMetadata> {
    let mut models: Vec<_> = provider
        .models
        .into_values()
        .filter(|model| !matches!(model.status.as_deref(), Some("alpha" | "deprecated")))
        .filter(|model| {
            !free_only
                || model
                    .cost
                    .as_ref()
                    .is_some_and(|cost| cost.input == 0.0 && cost.output == 0.0)
        })
        .filter_map(|model| map_model(model, &provider.npm))
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

fn map_model(model: ApiModel, provider_npm: &str) -> Option<OpenCodeModelMetadata> {
    let npm = model
        .provider
        .as_ref()
        .map(|provider| provider.npm.as_str())
        .unwrap_or(provider_npm);
    let format = match npm {
        "@ai-sdk/openai-compatible" => Format::OpenAi,
        "@ai-sdk/openai" => Format::OpenAiResponses,
        "@ai-sdk/anthropic" => Format::Claude,
        "@ai-sdk/google" => Format::Gemini,
        _ => return None,
    };

    let modalities = model.modalities.as_ref();
    let mut capabilities = Vec::new();
    for (modality, capability) in [
        ("image", "vision"),
        ("pdf", "pdf"),
        ("audio", "audioInput"),
        ("video", "videoInput"),
    ] {
        if modalities.is_some_and(|values| values.input.iter().any(|value| value == modality)) {
            capabilities.push(capability.to_string());
        }
    }
    if modalities.is_some_and(|values| values.output.iter().any(|value| value == "image")) {
        capabilities.push("imageOutput".to_string());
    }
    if modalities.is_some_and(|values| values.output.iter().any(|value| value == "audio")) {
        capabilities.push("audioOutput".to_string());
    }
    if model.tool_call == Some(true) {
        capabilities.push("tools".to_string());
    }
    if model.reasoning == Some(true) {
        capabilities.push("reasoning".to_string());
    }

    let reasoning_efforts = model.reasoning_options.map(|options| {
        options
            .into_iter()
            .find(|option| option.kind == "effort")
            .map(|option| option.values)
            .unwrap_or_default()
    });
    let (context_window, max_input, max_output) = model
        .limit
        .map(|limit| {
            (
                limit.context.and_then(|value| u32::try_from(value).ok()),
                limit.input.and_then(|value| u32::try_from(value).ok()),
                limit.output.and_then(|value| u32::try_from(value).ok()),
            )
        })
        .unwrap_or((None, None, None));
    let input_modalities = model
        .modalities
        .as_ref()
        .map(|modalities| modalities.input.clone());
    let output_modalities = model
        .modalities
        .as_ref()
        .map(|modalities| modalities.output.clone());

    Some(OpenCodeModelMetadata {
        id: model.id,
        name: model.name,
        format,
        family: model.family,
        context_window,
        max_input,
        max_output,
        attachment: model.attachment,
        input_modalities,
        output_modalities,
        reasoning: model.reasoning,
        tool_call: model.tool_call,
        capabilities,
        reasoning_efforts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_opencode_models_without_model_id_rules() {
        let snapshot = parse_snapshot(json!({
            "opencode": {
                "npm": "@ai-sdk/openai-compatible",
                "models": {
                    "muse-spark-1.3-contributor-free": {
                        "id": "muse-spark-1.3-contributor-free",
                        "name": "Muse Spark 1.3 Free",
                        "provider": {"npm": "@ai-sdk/openai"},
                        "family": "muse-free",
                        "cost": {"input": 0, "output": 0},
                        "attachment": true,
                        "reasoning": true,
                        "tool_call": true,
                        "limit": {"context": 1048576, "input": 917504, "output": 131072},
                        "modalities": {"input": ["text", "image", "pdf"], "output": ["text"]},
                        "reasoning_options": [{"type": "effort", "values": ["medium", "high"]}]
                    },
                    "paid": {
                        "id": "paid", "name": "Paid", "cost": {"input": 1, "output": 1}
                    }
                }
            },
            "opencode-go": {
                "npm": "@ai-sdk/openai-compatible",
                "models": {
                    "muse-spark-1.3-contributor": {
                        "id": "muse-spark-1.3-contributor",
                        "name": "Muse Spark 1.3 Contributor",
                        "provider": {"npm": "@ai-sdk/openai"},
                        "family": "muse",
                        "cost": {"input": 0.1, "output": 0.2}
                    },
                    "deepseek-v4.1-flash": {
                        "id": "deepseek-v4.1-flash",
                        "name": "DeepSeek V4.1 Flash",
                        "cost": {"input": 0.15, "output": 0.6}
                    },
                    "no-effort-config": {
                        "id": "no-effort-config",
                        "name": "No Effort Config",
                        "reasoning_options": [],
                        "cost": {"input": 0.15, "output": 0.6}
                    },
                    "old": {
                        "id": "old", "name": "Old", "status": "deprecated",
                        "cost": {"input": 0, "output": 0}
                    }
                }
            }
        }))
        .unwrap();

        let zen = snapshot.models("opencode-zen").unwrap();
        assert_eq!(zen.len(), 1);
        assert_eq!(zen[0].format, Format::OpenAiResponses);
        assert_eq!(zen[0].context_window, Some(1_048_576));
        assert_eq!(zen[0].max_input, Some(917_504));
        assert_eq!(zen[0].max_output, Some(131_072));
        assert_eq!(zen[0].attachment, Some(true));
        assert_eq!(
            zen[0].input_modalities.as_deref().unwrap(),
            ["text", "image", "pdf"]
        );
        assert_eq!(
            zen[0].reasoning_efforts.as_deref().unwrap(),
            ["medium", "high"]
        );

        let go = snapshot.models("opencode-go").unwrap();
        assert_eq!(go.len(), 3);
        assert_eq!(
            snapshot
                .find("opencode-go", "no-effort-config")
                .unwrap()
                .reasoning_efforts
                .as_deref(),
            Some([].as_slice())
        );
        assert_eq!(
            snapshot
                .find("opencode-go", "deepseek-v4.1-flash")
                .unwrap()
                .reasoning_efforts,
            None
        );
        assert_eq!(
            snapshot
                .find("opencode-go", "muse-spark-1.3-contributor")
                .unwrap()
                .format,
            Format::OpenAiResponses
        );
        assert_eq!(
            snapshot
                .find("opencode-go", "deepseek-v4.1-flash")
                .unwrap()
                .format,
            Format::OpenAi
        );
    }

    #[test]
    fn rejects_unknown_provider_package() {
        let provider = ApiProvider {
            npm: "unknown".to_string(),
            models: BTreeMap::from([(
                "model".to_string(),
                ApiModel {
                    id: "model".to_string(),
                    name: "Model".to_string(),
                    status: None,
                    family: None,
                    provider: None,
                    cost: Some(ApiCost {
                        input: 1.0,
                        output: 1.0,
                    }),
                    limit: None,
                    modalities: None,
                    attachment: None,
                    reasoning: Some(false),
                    tool_call: Some(false),
                    reasoning_options: None,
                },
            )]),
        };
        assert!(map_models(provider, false).is_empty());
    }
}
