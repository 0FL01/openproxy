use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::USER_AGENT;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::core::translator::registry::Format;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone)]
pub struct OpenCodeModelMetadata {
    pub id: String,
    pub name: String,
    pub format: Format,
    pub family: Option<String>,
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    pub capabilities: Vec<String>,
    pub reasoning_efforts: Vec<String>,
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
            "maxOutput": self.max_output,
            "capabilities": self.capabilities,
            "reasoningEfforts": self.reasoning_efforts,
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
struct CacheState {
    loaded_at: Option<Instant>,
    snapshot: Option<Arc<ModelsDevSnapshot>>,
}

#[derive(Debug)]
pub struct ModelsDevCatalog {
    client: reqwest::Client,
    cache: Mutex<CacheState>,
}

impl Default for ModelsDevCatalog {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("models.dev HTTP client should build"),
            cache: Mutex::new(CacheState::default()),
        }
    }
}

impl ModelsDevCatalog {
    pub async fn snapshot(&self) -> Result<Arc<ModelsDevSnapshot>, String> {
        let mut cache = self.cache.lock().await;
        if cache
            .loaded_at
            .is_some_and(|loaded| loaded.elapsed() < CACHE_TTL)
        {
            if let Some(snapshot) = &cache.snapshot {
                return Ok(snapshot.clone());
            }
        }

        let fetched = async {
            let response = self
                .client
                .get(MODELS_DEV_URL)
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
        .await;

        match fetched {
            Ok(snapshot) => {
                let snapshot = Arc::new(snapshot);
                cache.loaded_at = Some(Instant::now());
                cache.snapshot = Some(snapshot.clone());
                Ok(snapshot)
            }
            Err(error) => {
                let stale = cache.snapshot.clone().ok_or(error)?;
                cache.loaded_at = Some(Instant::now());
                Ok(stale)
            }
        }
    }

    #[cfg(test)]
    pub fn from_json(value: Value) -> Result<Self, String> {
        let snapshot = Arc::new(parse_snapshot(value)?);
        Ok(Self {
            client: reqwest::Client::new(),
            cache: Mutex::new(CacheState {
                loaded_at: Some(Instant::now()),
                snapshot: Some(snapshot),
            }),
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
    reasoning: bool,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    reasoning_options: Vec<ApiReasoningOption>,
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

    let modalities = model.modalities.unwrap_or(ApiModalities {
        input: Vec::new(),
        output: Vec::new(),
    });
    let mut capabilities = Vec::new();
    for (modality, capability) in [
        ("image", "vision"),
        ("pdf", "pdf"),
        ("audio", "audioInput"),
        ("video", "videoInput"),
    ] {
        if modalities.input.iter().any(|value| value == modality) {
            capabilities.push(capability.to_string());
        }
    }
    if modalities.output.iter().any(|value| value == "image") {
        capabilities.push("imageOutput".to_string());
    }
    if modalities.output.iter().any(|value| value == "audio") {
        capabilities.push("audioOutput".to_string());
    }
    if model.tool_call {
        capabilities.push("tools".to_string());
    }
    if model.reasoning {
        capabilities.push("reasoning".to_string());
    }

    let reasoning_efforts = model
        .reasoning_options
        .into_iter()
        .find(|option| option.kind == "effort")
        .map(|option| option.values)
        .unwrap_or_default();
    let (context_window, max_output) = model
        .limit
        .map(|limit| {
            (
                limit.context.and_then(|value| u32::try_from(value).ok()),
                limit.output.and_then(|value| u32::try_from(value).ok()),
            )
        })
        .unwrap_or((None, None));

    Some(OpenCodeModelMetadata {
        id: model.id,
        name: model.name,
        format,
        family: model.family,
        context_window,
        max_output,
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
                        "reasoning": true,
                        "tool_call": true,
                        "limit": {"context": 1048576, "output": 131072}
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

        let go = snapshot.models("opencode-go").unwrap();
        assert_eq!(go.len(), 2);
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
                    reasoning: false,
                    tool_call: false,
                    reasoning_options: Vec::new(),
                },
            )]),
        };
        assert!(map_models(provider, false).is_empty());
    }
}
