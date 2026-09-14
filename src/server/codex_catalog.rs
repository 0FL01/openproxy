use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::core::config::app_constants::{
    CODEX_CLIENT_VERSION, CODEX_ORIGINATOR, CODEX_USER_AGENT,
};
use crate::core::proxy::resolve_proxy_target;
use crate::core::usage::quota_fetcher::codex_account_id;
use crate::oauth::token_refresh::{dispatch_oauth_refresh, needs_refresh_with_lead, RefreshResult};
use crate::server::state::AppState;
use crate::types::ProviderConnection;

const MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const REFRESH_LEAD_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq)]
pub struct CodexModelMetadata {
    pub id: String,
    pub name: String,
    pub context_window: Option<u64>,
    pub capabilities: Vec<String>,
    pub reasoning_efforts: Vec<String>,
}

impl CodexModelMetadata {
    pub fn catalog_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "kind": "llm",
            "targetFormat": "openai-responses",
            "contextWindow": self.context_window,
            "capabilities": self.capabilities,
            "reasoningEfforts": self.reasoning_efforts,
        })
    }
}

#[derive(Debug, Clone)]
pub struct CodexInventory {
    pub models: Arc<Vec<CodexModelMetadata>>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexCatalogError {
    pub status: StatusCode,
    pub message: String,
}

impl CodexCatalogError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

#[derive(Debug, Default)]
struct CacheEntry {
    identity: String,
    loaded_at: Option<Instant>,
    models: Option<Arc<Vec<CodexModelMetadata>>>,
}

#[derive(Debug, Default)]
pub struct CodexModelCatalog {
    entries: Mutex<HashMap<String, Arc<Mutex<CacheEntry>>>>,
}

impl CodexModelCatalog {
    pub async fn models_for_connection(
        &self,
        state: &AppState,
        connection: &ProviderConnection,
    ) -> Result<CodexInventory, CodexCatalogError> {
        let identity = cache_identity(connection);
        let entry = {
            let mut entries = self.entries.lock().await;
            entries
                .entry(connection.id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(CacheEntry::default())))
                .clone()
        };
        let mut cache = entry.lock().await;

        if cache.identity == identity
            && cache
                .loaded_at
                .is_some_and(|loaded| loaded.elapsed() < CACHE_TTL)
        {
            if let Some(models) = &cache.models {
                return Ok(CodexInventory {
                    models: models.clone(),
                    warning: None,
                });
            }
        }

        match fetch_models(state, connection).await {
            Ok(models) => {
                let models = Arc::new(models);
                cache.identity = state
                    .db
                    .snapshot()
                    .provider_connections
                    .iter()
                    .find(|candidate| candidate.id == connection.id)
                    .map(cache_identity)
                    .unwrap_or(identity);
                cache.loaded_at = Some(Instant::now());
                cache.models = Some(models.clone());
                Ok(CodexInventory {
                    models,
                    warning: None,
                })
            }
            Err(error) if cache.identity == identity && cache.models.is_some() => {
                cache.loaded_at = Some(Instant::now());
                Ok(CodexInventory {
                    models: cache.models.clone().expect("checked above"),
                    warning: Some(error.message),
                })
            }
            Err(error) => Err(error),
        }
    }

    pub async fn union_active(
        &self,
        state: &AppState,
        connections: &[ProviderConnection],
    ) -> CodexInventory {
        let mut models = BTreeMap::new();
        let mut warnings = Vec::new();
        for connection in connections
            .iter()
            .filter(|connection| connection.provider == "codex" && connection.is_active())
        {
            match self.models_for_connection(state, connection).await {
                Ok(inventory) => {
                    for model in inventory.models.iter() {
                        models
                            .entry(model.id.clone())
                            .or_insert_with(|| model.clone());
                    }
                    if let Some(warning) = inventory.warning {
                        warnings.push(warning);
                    }
                }
                Err(error) => warnings.push(error.message),
            }
        }
        CodexInventory {
            models: Arc::new(models.into_values().collect()),
            warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        }
    }

    /// Return supporting connection IDs only when the model is present in at
    /// least one warm cache. Unknown and cold models remain permissive.
    pub async fn cached_supporters(
        &self,
        model: &str,
        connections: &[ProviderConnection],
    ) -> Option<HashSet<String>> {
        let entries: Vec<(String, Arc<Mutex<CacheEntry>>)> = self
            .entries
            .lock()
            .await
            .iter()
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect();
        let mut supporters = HashSet::new();
        for (id, entry) in entries {
            let Some(connection) = connections.iter().find(|connection| connection.id == id) else {
                continue;
            };
            let cache = entry.lock().await;
            if cache.identity == cache_identity(connection)
                && cache
                    .models
                    .as_ref()
                    .is_some_and(|models| models.iter().any(|candidate| candidate.id == model))
            {
                supporters.insert(id);
            }
        }
        (!supporters.is_empty()).then_some(supporters)
    }

    pub async fn invalidate(&self, connection_id: &str) {
        self.entries.lock().await.remove(connection_id);
    }

    #[cfg(test)]
    pub async fn seed(&self, connection: &ProviderConnection, models: Vec<CodexModelMetadata>) {
        let entry = Arc::new(Mutex::new(CacheEntry {
            identity: cache_identity(connection),
            loaded_at: Some(Instant::now()),
            models: Some(Arc::new(models)),
        }));
        self.entries
            .lock()
            .await
            .insert(connection.id.clone(), entry);
    }
}

fn cache_identity(connection: &ProviderConnection) -> String {
    let account = codex_account_id(&connection.provider_specific_data).unwrap_or_default();
    let token = connection.access_token.as_deref().unwrap_or_default();
    format!("{account}:{}", sha256::digest(token))
}

async fn fetch_models(
    state: &AppState,
    connection: &ProviderConnection,
) -> Result<Vec<CodexModelMetadata>, CodexCatalogError> {
    let mut connection = connection.clone();
    if needs_refresh_with_lead(&connection.expires_at, REFRESH_LEAD_MS) {
        refresh_connection(state, &mut connection).await?;
    }

    let token = connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| CodexCatalogError::new(StatusCode::UNAUTHORIZED, "No valid token found"))?;
    match fetch_with_token(state, &connection, token).await {
        Err(error)
            if matches!(
                error.status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) && connection.refresh_token.is_some() =>
        {
            refresh_connection(state, &mut connection).await?;
            let token = connection.access_token.as_deref().unwrap_or_default();
            fetch_with_token(state, &connection, token).await
        }
        result => result,
    }
}

async fn fetch_with_token(
    state: &AppState,
    connection: &ProviderConnection,
    token: &str,
) -> Result<Vec<CodexModelMetadata>, CodexCatalogError> {
    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);
    let client = state
        .client_pool
        .get(&format!("codex-models:{}", connection.id), proxy.as_ref())
        .map_err(|error| {
            CodexCatalogError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build Codex models client: {error}"),
            )
        })?;
    let mut request = client
        .get(MODELS_URL)
        .query(&[("client_version", CODEX_CLIENT_VERSION)])
        .bearer_auth(token)
        .header("Accept", "application/json")
        .header("originator", CODEX_ORIGINATOR)
        .header("Version", CODEX_CLIENT_VERSION)
        .header("User-Agent", CODEX_USER_AGENT);
    if let Some(account_id) = codex_account_id(&connection.provider_specific_data) {
        request = request.header("ChatGPT-Account-ID", account_id);
    }
    let response = request.send().await.map_err(|error| {
        CodexCatalogError::new(
            StatusCode::BAD_GATEWAY,
            format!("Failed to fetch Codex models: {error}"),
        )
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(CodexCatalogError::new(
            status,
            format!("Codex models endpoint returned HTTP {}", status.as_u16()),
        ));
    }
    let payload = response.json::<Value>().await.map_err(|error| {
        CodexCatalogError::new(
            StatusCode::BAD_GATEWAY,
            format!("Codex models response is invalid: {error}"),
        )
    })?;
    parse_codex_models(payload)
        .map_err(|error| CodexCatalogError::new(StatusCode::BAD_GATEWAY, error))
}

async fn refresh_connection(
    state: &AppState,
    connection: &mut ProviderConnection,
) -> Result<(), CodexCatalogError> {
    let refresh_token = connection
        .refresh_token
        .as_deref()
        .ok_or_else(|| CodexCatalogError::new(StatusCode::UNAUTHORIZED, "Token expired"))?;
    let refreshed =
        dispatch_oauth_refresh("codex", refresh_token, &connection.provider_specific_data)
            .await
            .map_err(|error| CodexCatalogError::new(StatusCode::UNAUTHORIZED, error))?;
    persist_refresh(state, connection, &refreshed).await;
    connection.access_token = Some(refreshed.access_token.clone());
    if let Some(refresh_token) = refreshed.refresh_token {
        connection.refresh_token = Some(refresh_token);
    }
    if let Some(expires_in) = refreshed.expires_in {
        connection.expires_at =
            Some((Utc::now() + chrono::Duration::seconds(expires_in)).to_rfc3339());
    }
    Ok(())
}

async fn persist_refresh(
    state: &AppState,
    connection: &ProviderConnection,
    refresh: &RefreshResult,
) {
    let connection_id = connection.id.clone();
    let refresh = refresh.clone();
    let _ = state
        .db
        .update(move |db| {
            let Some(target) = db
                .provider_connections
                .iter_mut()
                .find(|candidate| candidate.id == connection_id)
            else {
                return;
            };
            target.access_token = Some(refresh.access_token.clone());
            if let Some(token) = &refresh.refresh_token {
                target.refresh_token = Some(token.clone());
            }
            if let Some(expires_in) = refresh.expires_in {
                target.expires_in = Some(expires_in);
                target.expires_at =
                    Some((Utc::now() + chrono::Duration::seconds(expires_in)).to_rfc3339());
            }
            target.updated_at = Some(Utc::now().to_rfc3339());
        })
        .await;
}

#[derive(Debug, Deserialize)]
struct CodexModelsResponse {
    #[serde(default, alias = "data")]
    models: Vec<CodexModelDto>,
}

#[derive(Debug, Deserialize)]
struct CodexModelDto {
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    supported_in_api: Option<bool>,
    #[serde(default)]
    minimal_client_version: Option<String>,
    #[serde(default)]
    max_context_window: Option<u64>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    supported_reasoning_levels: Vec<CodexReasoningLevel>,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    supports_search_tool: bool,
}

#[derive(Debug, Deserialize)]
struct CodexReasoningLevel {
    effort: String,
}

fn parse_codex_models(payload: Value) -> Result<Vec<CodexModelMetadata>, String> {
    let response: CodexModelsResponse = serde_json::from_value(payload)
        .map_err(|error| format!("Codex models response has an invalid schema: {error}"))?;
    let mut models = BTreeMap::new();
    for model in response.models {
        if model.visibility.as_deref() == Some("hide") || model.supported_in_api == Some(false) {
            continue;
        }
        if let Some(version) = model.minimal_client_version.as_deref() {
            let Some(required) = parse_version(version) else {
                continue;
            };
            let current = parse_version(CODEX_CLIENT_VERSION)
                .expect("CODEX_CLIENT_VERSION must be numeric dot-separated");
            if version_is_newer(&required, &current) {
                continue;
            }
        }
        let id = model
            .slug
            .or(model.id)
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());
        let Some(id) = id else { continue };
        let mut capabilities = vec!["tools".to_string()];
        if !model.supported_reasoning_levels.is_empty() {
            capabilities.push("reasoning".to_string());
        }
        if model.input_modalities.iter().any(|value| value == "image") {
            capabilities.push("vision".to_string());
        }
        if model.supports_search_tool {
            capabilities.push("search".to_string());
        }
        let reasoning_efforts = model
            .supported_reasoning_levels
            .into_iter()
            .map(|level| level.effort)
            .filter(|effort| !effort.trim().is_empty())
            .collect();
        let name = model
            .display_name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.clone());
        models.entry(id.clone()).or_insert(CodexModelMetadata {
            id,
            name,
            context_window: model.max_context_window.or(model.context_window),
            capabilities,
            reasoning_efforts,
        });
    }
    Ok(models.into_values().collect())
}

fn parse_version(value: &str) -> Option<Vec<u64>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    value
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()
}

fn version_is_newer(required: &[u64], current: &[u64]) -> bool {
    let width = required.len().max(current.len());
    (0..width)
        .map(|index| {
            (
                required.get(index).copied().unwrap_or(0),
                current.get(index).copied().unwrap_or(0),
            )
        })
        .find(|(required, current)| required != current)
        .is_some_and(|(required, current)| required > current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_filters_and_sanitizes_codex_models() {
        let models = parse_codex_models(json!({"models": [
            {"slug":"gpt-current","display_name":"Current","visibility":"list","supported_in_api":true,"minimal_client_version":CODEX_CLIENT_VERSION,"context_window":272000,"max_context_window":872000,"supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],"input_modalities":["text","image"],"supports_search_tool":true,"base_instructions":"secret"},
            {"slug":"gpt-hidden","visibility":"hide"},
            {"slug":"gpt-disabled","supported_in_api":false},
            {"slug":"gpt-future","minimal_client_version":"999.0.0"},
            {"slug":"gpt-malformed","minimal_client_version":"next"},
            {"slug":"gpt-current","display_name":"duplicate"},
            {"slug":"  "}
        ]})).unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-current");
        assert_eq!(models[0].context_window, Some(872000));
        assert_eq!(models[0].reasoning_efforts, ["low", "high"]);
        assert_eq!(
            models[0].capabilities,
            ["tools", "reasoning", "vision", "search"]
        );
        let output = models[0].catalog_json();
        assert!(output.get("base_instructions").is_none());
        assert_eq!(output["targetFormat"], "openai-responses");
    }

    #[tokio::test]
    async fn cached_supporters_do_not_cross_credential_identity() {
        let catalog = CodexModelCatalog::default();
        let connection = ProviderConnection {
            id: "connection".into(),
            provider: "codex".into(),
            access_token: Some("token-a".into()),
            ..Default::default()
        };
        catalog
            .seed(
                &connection,
                vec![CodexModelMetadata {
                    id: "gpt-test".into(),
                    name: "GPT Test".into(),
                    context_window: None,
                    capabilities: Vec::new(),
                    reasoning_efforts: Vec::new(),
                }],
            )
            .await;

        assert_eq!(
            catalog
                .cached_supporters("gpt-test", std::slice::from_ref(&connection))
                .await,
            Some(HashSet::from([connection.id.clone()]))
        );

        let mut changed = connection;
        changed.access_token = Some("token-b".into());
        assert_eq!(
            catalog.cached_supporters("gpt-test", &[changed]).await,
            None
        );
    }
}
