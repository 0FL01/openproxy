use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::core::executor::read_reqwest_body;
use crate::core::translator::registry::Format;

pub const COMMANDCODE_MODELS_URL: &str = "https://api.commandcode.ai/provider/v1/models";
pub const COMMANDCODE_API_BASE: &str = "https://api.commandcode.ai/provider/v1";

const CATALOG_BODY_LIMIT: usize = 2 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const FAILED_REFRESH_RETRY_DELAY: Duration = Duration::from_secs(30);
const BUNDLED_CATALOG: &str = include_str!("commandcode_models_bundled.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum CommandCodeEndpoint {
    #[serde(rename = "/chat/completions")]
    ChatCompletions,
    #[serde(rename = "/responses")]
    Responses,
    #[serde(rename = "/messages")]
    Messages,
}

impl CommandCodeEndpoint {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "/chat/completions" => Some(Self::ChatCompletions),
            "/responses" => Some(Self::Responses),
            "/messages" => Some(Self::Messages),
            _ => None,
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/chat/completions",
            Self::Responses => "/responses",
            Self::Messages => "/messages",
        }
    }

    pub fn format(self) -> Format {
        match self {
            Self::ChatCompletions => Format::OpenAi,
            Self::Responses => Format::OpenAiResponses,
            Self::Messages => Format::Claude,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCodeModelMetadata {
    pub id: String,
    pub name: String,
    pub context_length: u32,
    pub supported_endpoints: Vec<CommandCodeEndpoint>,
}

impl CommandCodeModelMetadata {
    pub fn catalog_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "kind": "llm",
            "contextWindow": self.context_length,
            "supportedEndpoints": self.supported_endpoints,
        })
    }

    pub fn endpoint_for_source(&self, source: Format) -> CommandCodeEndpoint {
        self.supported_endpoints
            .iter()
            .copied()
            .find(|endpoint| endpoint.format() == source)
            .or_else(|| {
                [
                    CommandCodeEndpoint::ChatCompletions,
                    CommandCodeEndpoint::Responses,
                    CommandCodeEndpoint::Messages,
                ]
                .into_iter()
                .find(|endpoint| self.supported_endpoints.contains(endpoint))
            })
            .expect("validated Command Code models have a supported endpoint")
    }

    pub fn endpoint_url(&self, source: Format) -> String {
        format!(
            "{}{}",
            COMMANDCODE_API_BASE,
            self.endpoint_for_source(source).path()
        )
    }
}

#[derive(Debug, Default)]
pub struct CommandCodeCatalogSnapshot {
    models: Vec<CommandCodeModelMetadata>,
}

impl CommandCodeCatalogSnapshot {
    pub fn models(&self) -> &[CommandCodeModelMetadata] {
        &self.models
    }

    pub fn find(&self, model: &str) -> Option<&CommandCodeModelMetadata> {
        self.models.iter().find(|entry| entry.id == model)
    }
}

#[derive(Debug, Default)]
struct RefreshState {
    last_success: Option<Instant>,
    last_attempt: Option<Instant>,
    last_error: Option<String>,
}

pub struct CommandCodeModelCatalog {
    client: reqwest::Client,
    endpoint: String,
    published: ArcSwap<CommandCodeCatalogSnapshot>,
    refresh: Mutex<RefreshState>,
}

impl Default for CommandCodeModelCatalog {
    fn default() -> Self {
        let bundled = parse_snapshot(
            serde_json::from_str(BUNDLED_CATALOG)
                .expect("embedded Command Code catalog should be valid JSON"),
        )
        .expect("embedded Command Code catalog should match the Provider API schema");
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("Command Code catalog HTTP client should build"),
            endpoint: COMMANDCODE_MODELS_URL.to_string(),
            published: ArcSwap::from_pointee(bundled),
            refresh: Mutex::new(RefreshState::default()),
        }
    }
}

impl CommandCodeModelCatalog {
    pub fn load(&self) -> Arc<CommandCodeCatalogSnapshot> {
        self.published.load_full()
    }

    pub async fn refresh_if_stale(&self) -> Result<Arc<CommandCodeCatalogSnapshot>, String> {
        self.refresh_inner(false).await
    }

    pub async fn refresh(&self) -> Result<Arc<CommandCodeCatalogSnapshot>, String> {
        self.refresh_inner(true).await
    }

    async fn refresh_inner(&self, force: bool) -> Result<Arc<CommandCodeCatalogSnapshot>, String> {
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
                    "Command Code catalog refresh is temporarily throttled".to_string()
                }));
            }
        }
        state.last_attempt = Some(Instant::now());

        match self.fetch_snapshot().await {
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

    async fn fetch_snapshot(&self) -> Result<CommandCodeCatalogSnapshot, String> {
        let response = self
            .client
            .get(&self.endpoint)
            .send()
            .await
            .map_err(|error| format!("Command Code models request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Command Code models returned HTTP {}",
                response.status()
            ));
        }
        let body = read_reqwest_body(response, CATALOG_BODY_LIMIT)
            .await
            .map_err(|error| format!("Command Code models response is invalid: {error}"))?;
        let value = serde_json::from_slice(&body)
            .map_err(|error| format!("Command Code models response is invalid JSON: {error}"))?;
        parse_snapshot(value)
    }

    #[cfg(test)]
    pub fn from_json_with_endpoint(
        value: Value,
        endpoint: impl Into<String>,
    ) -> Result<Self, String> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .map_err(|error| format!("Command Code catalog HTTP client failed: {error}"))?,
            endpoint: endpoint.into(),
            published: ArcSwap::from_pointee(parse_snapshot(value)?),
            refresh: Mutex::new(RefreshState::default()),
        })
    }
}

#[derive(Debug, Deserialize)]
struct ApiCatalog {
    object: String,
    data: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
    name: String,
    context_length: u64,
    supported_endpoints: Vec<String>,
}

fn parse_snapshot(value: Value) -> Result<CommandCodeCatalogSnapshot, String> {
    let payload: ApiCatalog = serde_json::from_value(value)
        .map_err(|error| format!("Command Code models schema mismatch: {error}"))?;
    if payload.object != "list" {
        return Err("Command Code models response must be an object=list envelope".to_string());
    }
    if payload.data.is_empty() {
        return Err("Command Code models response contains no models".to_string());
    }

    let mut ids = HashSet::new();
    let mut models = Vec::with_capacity(payload.data.len());
    for row in payload.data {
        if row.id.is_empty() || row.id.trim() != row.id {
            return Err("Command Code model id must be non-empty and unpadded".to_string());
        }
        if row.name.is_empty() || row.name.trim() != row.name {
            return Err(format!("Command Code model {} has an invalid name", row.id));
        }
        if !ids.insert(row.id.clone()) {
            return Err(format!("Command Code model id {} is duplicated", row.id));
        }
        let context_length = u32::try_from(row.context_length)
            .map_err(|_| format!("Command Code model {} context length is too large", row.id))?;
        if context_length == 0 {
            return Err(format!(
                "Command Code model {} has zero context length",
                row.id
            ));
        }
        let mut supported_endpoints = Vec::new();
        for raw in row.supported_endpoints {
            if let Some(endpoint) = CommandCodeEndpoint::parse(&raw) {
                if !supported_endpoints.contains(&endpoint) {
                    supported_endpoints.push(endpoint);
                }
            }
        }
        if supported_endpoints.is_empty() {
            return Err(format!(
                "Command Code model {} has no supported Provider API endpoint",
                row.id
            ));
        }
        models.push(CommandCodeModelMetadata {
            id: row.id,
            name: row.name,
            context_length,
            supported_endpoints,
        });
    }

    Ok(CommandCodeCatalogSnapshot { models })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture() -> Value {
        json!({
            "object": "list",
            "data": [
                {
                    "id": "moonshotai/Kimi-K3",
                    "name": "Kimi K3",
                    "context_length": 1000000,
                    "supported_endpoints": ["/chat/completions", "/responses"]
                },
                {
                    "id": "claude-sonnet-5",
                    "name": "Claude Sonnet 5",
                    "context_length": 1000000,
                    "supported_endpoints": ["/messages"]
                }
            ]
        })
    }

    #[test]
    fn parses_exact_ids_context_and_supported_endpoints() {
        let snapshot = parse_snapshot(fixture()).unwrap();
        let kimi = snapshot.find("moonshotai/Kimi-K3").unwrap();
        assert_eq!(kimi.name, "Kimi K3");
        assert_eq!(kimi.context_length, 1_000_000);
        assert_eq!(
            kimi.supported_endpoints,
            [
                CommandCodeEndpoint::ChatCompletions,
                CommandCodeEndpoint::Responses
            ]
        );
        assert!(snapshot.find("moonshotai/kimi-k3").is_none());
    }

    #[test]
    fn rejects_rows_without_a_known_endpoint() {
        let value = json!({
            "object": "list",
            "data": [{
                "id": "future-model",
                "name": "Future Model",
                "context_length": 1000,
                "supported_endpoints": ["/future"]
            }]
        });
        assert!(parse_snapshot(value).is_err());
    }

    #[tokio::test]
    async fn failed_refresh_retains_last_good_snapshot() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let catalog = CommandCodeModelCatalog::from_json_with_endpoint(
            fixture(),
            format!("{}/models", server.uri()),
        )
        .unwrap();

        assert!(catalog.refresh().await.is_err());
        assert!(catalog.load().find("moonshotai/Kimi-K3").is_some());
    }
}
