use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use parking_lot::Mutex as SyncMutex;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::core::config::app_constants::{
    CODEX_CLIENT_VERSION, CODEX_ORIGINATOR, CODEX_USER_AGENT,
};
use crate::core::proxy::resolve_proxy_target;
use crate::core::usage::quota_fetcher::codex_account_id;
use crate::oauth::token_refresh::{
    connection_credential_generation, needs_refresh_with_lead, CONNECTION_REFRESH_COORDINATOR,
};
use crate::server::state::AppState;
use crate::types::{AppDb, ProviderConnection};

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

#[derive(Debug, Clone)]
struct PublishedConnectionInventory {
    identity: String,
    loaded_at: Instant,
    models: Arc<Vec<CodexModelMetadata>>,
    warning: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CodexRoutingConfiguration {
    active_identities: BTreeMap<String, String>,
    explicit_models: BTreeMap<String, BTreeSet<String>>,
    custom_models: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
struct CodexCatalogSnapshot {
    configuration: CodexRoutingConfiguration,
    entries: BTreeMap<String, PublishedConnectionInventory>,
    union: Arc<Vec<CodexModelMetadata>>,
    warning: Option<String>,
}

/// Opaque publication handle used by deterministic integration tests to prove
/// that failed or in-flight refreshes do not replace the visible snapshot.
#[doc(hidden)]
#[derive(Clone)]
pub struct CodexCatalogPublication(Arc<CodexCatalogSnapshot>);

impl CodexCatalogPublication {
    #[doc(hidden)]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

pub struct CodexModelCatalog {
    published: ArcSwap<CodexCatalogSnapshot>,
    publication_lock: SyncMutex<()>,
    refresh_lock: Mutex<()>,
    models_url: String,
}

impl Default for CodexModelCatalog {
    fn default() -> Self {
        Self::with_models_url(MODELS_URL)
    }
}

impl CodexModelCatalog {
    /// Construct a catalog with an explicit endpoint. This is public only for
    /// deterministic loopback tests and self-hosted control-plane fixtures.
    #[doc(hidden)]
    pub fn with_models_url(models_url: impl Into<String>) -> Self {
        Self {
            published: ArcSwap::from_pointee(CodexCatalogSnapshot::default()),
            publication_lock: SyncMutex::new(()),
            refresh_lock: Mutex::new(()),
            models_url: models_url.into(),
        }
    }

    /// Return the immutable published inventory for one canonical connection.
    /// This never waits for a refresh mutex and never performs network I/O.
    pub fn published_for_connection(
        &self,
        snapshot: &AppDb,
        connection: &ProviderConnection,
    ) -> Option<CodexInventory> {
        let published = self.reconcile_configuration(snapshot);
        let entry = published.entries.get(&connection.id)?;
        (entry.identity == cache_identity(connection)).then(|| CodexInventory {
            models: entry.models.clone(),
            warning: entry.warning.clone(),
        })
    }

    /// Refresh one connection from the control plane. Readers continue using
    /// the prior immutable publication while the HTTP request is in flight.
    pub async fn models_for_connection(
        &self,
        state: &AppState,
        connection: &ProviderConnection,
    ) -> Result<CodexInventory, CodexCatalogError> {
        self.refresh_connection(state, connection, false).await
    }

    /// Force an explicit dashboard/setup refresh even when the publication is
    /// younger than the normal control-plane freshness interval.
    pub async fn refresh_connection(
        &self,
        state: &AppState,
        connection: &ProviderConnection,
        force: bool,
    ) -> Result<CodexInventory, CodexCatalogError> {
        let _refresh = self.refresh_lock.lock().await;
        let before_db = state.db.snapshot();
        let before = self.reconcile_configuration(&before_db);
        let canonical = before_db
            .provider_connections
            .iter()
            .find(|candidate| {
                candidate.id == connection.id
                    && candidate.provider == "codex"
                    && candidate.is_active()
            })
            .cloned()
            .ok_or_else(|| {
                CodexCatalogError::new(
                    StatusCode::NOT_FOUND,
                    "Codex connection is no longer active",
                )
            })?;
        let identity = cache_identity(&canonical);

        if !force {
            if let Some(entry) = before
                .entries
                .get(&canonical.id)
                .filter(|entry| entry.identity == identity && entry.loaded_at.elapsed() < CACHE_TTL)
            {
                return Ok(CodexInventory {
                    models: entry.models.clone(),
                    warning: entry.warning.clone(),
                });
            }
        }

        let previous = before
            .entries
            .get(&canonical.id)
            .filter(|entry| entry.identity == identity)
            .cloned();
        match fetch_models(state, &canonical, &self.models_url).await {
            Ok(models) => {
                let models = Arc::new(models);
                let after_db = state.db.snapshot();
                let Some(after_connection) =
                    after_db.provider_connections.iter().find(|candidate| {
                        candidate.id == canonical.id
                            && candidate.provider == "codex"
                            && candidate.is_active()
                    })
                else {
                    return Err(CodexCatalogError::new(
                        StatusCode::CONFLICT,
                        "Codex connection changed while model discovery was active",
                    ));
                };
                let published_identity = cache_identity(after_connection);
                self.publish_entry(
                    &after_db,
                    &after_connection.id,
                    PublishedConnectionInventory {
                        identity: published_identity,
                        loaded_at: Instant::now(),
                        models: models.clone(),
                        warning: None,
                    },
                );
                Ok(CodexInventory {
                    models,
                    warning: None,
                })
            }
            Err(error) if previous.is_some() => {
                let previous = previous.expect("checked above");
                Ok(CodexInventory {
                    models: previous.models,
                    warning: Some(error.message),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Refresh all configured Codex connections from one bounded control task.
    pub async fn refresh_active(&self, state: &AppState) {
        let snapshot = state.db.snapshot();
        self.reconcile_configuration(&snapshot);
        let connections: Vec<_> = snapshot
            .provider_connections
            .iter()
            .filter(|connection| connection.provider == "codex" && connection.is_active())
            .cloned()
            .collect();
        drop(snapshot);
        for connection in connections {
            if let Err(error) = self.models_for_connection(state, &connection).await {
                tracing::warn!(
                    connection_id = %connection.id,
                    error = %error.message,
                    "Codex model refresh failed; retaining published inventory"
                );
            }
        }
    }

    /// Return the pre-merged active inventory. A configuration change rebuilds
    /// it once; unchanged requests only clone the published Arc.
    pub fn union_active(&self, snapshot: &AppDb) -> CodexInventory {
        let published = self.reconcile_configuration(snapshot);
        CodexInventory {
            models: published.union.clone(),
            warning: published.warning.clone(),
        }
    }

    /// Return the exact active connection set that may route this model.
    /// Remote publication, explicit enabled/default models, and user custom
    /// Codex models are authoritative. Cold unknown models return an empty set
    /// instead of falling through to an arbitrary account.
    pub fn cached_supporters(&self, model: &str, snapshot: &AppDb) -> HashSet<String> {
        let published = self.reconcile_configuration(snapshot);
        let model = model.trim().to_string();
        let mut supporters = HashSet::new();
        for (id, identity) in &published.configuration.active_identities {
            let remote_support = published.entries.get(id).is_some_and(|entry| {
                entry.identity == *identity
                    && entry.models.iter().any(|candidate| candidate.id == model)
            });
            let explicitly_configured = published
                .configuration
                .explicit_models
                .get(id)
                .is_some_and(|models| models.contains(&model));
            if remote_support
                || explicitly_configured
                || published.configuration.custom_models.contains(&model)
            {
                supporters.insert(id.clone());
            }
        }
        supporters
    }

    pub fn invalidate(&self, snapshot: &AppDb, connection_id: &str) {
        let _guard = self.publication_lock.lock();
        let current = self.published.load_full();
        let mut entries = current.entries.clone();
        entries.remove(connection_id);
        self.published.store(Arc::new(build_snapshot(
            routing_configuration(snapshot),
            entries,
        )));
    }

    #[cfg(test)]
    pub fn seed(
        &self,
        snapshot: &AppDb,
        connection: &ProviderConnection,
        models: Vec<CodexModelMetadata>,
    ) {
        self.publish_entry(
            snapshot,
            &connection.id,
            PublishedConnectionInventory {
                identity: cache_identity(connection),
                loaded_at: Instant::now(),
                models: Arc::new(models),
                warning: None,
            },
        );
    }

    #[doc(hidden)]
    pub fn published_snapshot_identity(&self) -> CodexCatalogPublication {
        CodexCatalogPublication(self.published.load_full())
    }

    fn reconcile_configuration(&self, snapshot: &AppDb) -> Arc<CodexCatalogSnapshot> {
        let configuration = routing_configuration(snapshot);
        let current = self.published.load_full();
        if current.configuration == configuration {
            return current;
        }
        let _guard = self.publication_lock.lock();
        let current = self.published.load_full();
        if current.configuration == configuration {
            return current;
        }
        let entries = current
            .entries
            .iter()
            .filter(|(id, entry)| {
                configuration
                    .active_identities
                    .get(*id)
                    .is_some_and(|identity| identity == &entry.identity)
            })
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect();
        let next = Arc::new(build_snapshot(configuration, entries));
        self.published.store(next.clone());
        next
    }

    fn publish_entry(
        &self,
        snapshot: &AppDb,
        connection_id: &str,
        entry: PublishedConnectionInventory,
    ) {
        let configuration = routing_configuration(snapshot);
        if configuration
            .active_identities
            .get(connection_id)
            .is_none_or(|identity| identity != &entry.identity)
        {
            return;
        }
        let _guard = self.publication_lock.lock();
        let current = self.published.load_full();
        let mut entries: BTreeMap<_, _> = current
            .entries
            .iter()
            .filter(|(id, published)| {
                configuration
                    .active_identities
                    .get(*id)
                    .is_some_and(|identity| identity == &published.identity)
            })
            .map(|(id, published)| (id.clone(), published.clone()))
            .collect();
        entries.insert(connection_id.to_string(), entry);
        self.published
            .store(Arc::new(build_snapshot(configuration, entries)));
    }
}

fn build_snapshot(
    configuration: CodexRoutingConfiguration,
    entries: BTreeMap<String, PublishedConnectionInventory>,
) -> CodexCatalogSnapshot {
    let mut models = BTreeMap::new();
    let mut warnings = Vec::new();
    for (id, entry) in &entries {
        if configuration.active_identities.get(id) != Some(&entry.identity) {
            continue;
        }
        for model in entry.models.iter() {
            models
                .entry(model.id.clone())
                .or_insert_with(|| model.clone());
        }
        if let Some(warning) = &entry.warning {
            warnings.push(format!("{id}: {warning}"));
        }
    }
    CodexCatalogSnapshot {
        configuration,
        entries,
        union: Arc::new(models.into_values().collect()),
        warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
    }
}

fn routing_configuration(snapshot: &AppDb) -> CodexRoutingConfiguration {
    let active: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|connection| connection.provider == "codex" && connection.is_active())
        .collect();
    let mut configuration = CodexRoutingConfiguration::default();
    for connection in &active {
        configuration
            .active_identities
            .insert(connection.id.clone(), cache_identity(connection));
        let prefixes = codex_prefixes(connection);
        let mut explicit = BTreeSet::new();
        if let Some(models) = connection
            .provider_specific_data
            .get("enabledModels")
            .and_then(Value::as_array)
        {
            explicit.extend(
                models
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|model| normalize_model_id(model, &prefixes)),
            );
        }
        if let Some(model) = connection.default_model.as_deref() {
            explicit.insert(normalize_model_id(model, &prefixes));
        }
        configuration
            .explicit_models
            .insert(connection.id.clone(), explicit);
    }

    let aliases: BTreeSet<_> = active
        .iter()
        .flat_map(|connection| codex_prefixes(connection))
        .collect();
    for custom in snapshot.custom_models.iter().filter(|custom| {
        custom.r#type.is_empty() || custom.r#type == "llm" || custom.r#type == "chat"
    }) {
        if aliases.contains(custom.provider_alias.trim()) {
            configuration.custom_models.insert(normalize_model_id(
                &custom.id,
                &aliases.iter().cloned().collect::<Vec<_>>(),
            ));
        }
    }
    configuration
}

fn codex_prefixes(connection: &ProviderConnection) -> Vec<String> {
    let mut prefixes = vec!["codex".to_string(), "cx".to_string()];
    if let Some(prefix) = connection
        .provider_specific_data
        .get("prefix")
        .and_then(Value::as_str)
        .or_else(|| connection.extra.get("prefix").and_then(Value::as_str))
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
    {
        prefixes.push(prefix.to_string());
    }
    prefixes
}

fn normalize_model_id(model: &str, prefixes: &[String]) -> String {
    let trimmed = model.trim();
    for prefix in prefixes {
        if let Some(stripped) = trimmed.strip_prefix(&format!("{prefix}/")) {
            return stripped.to_string();
        }
    }
    trimmed.to_string()
}

fn cache_identity(connection: &ProviderConnection) -> String {
    let account = codex_account_id(&connection.provider_specific_data).unwrap_or_default();
    let token = connection.access_token.as_deref().unwrap_or_default();
    format!("{account}:{}", sha256::digest(token))
}

async fn fetch_models(
    state: &AppState,
    connection: &ProviderConnection,
    models_url: &str,
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
    match fetch_with_token(state, &connection, token, models_url).await {
        Err(error)
            if matches!(
                error.status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) && connection.refresh_token.is_some() =>
        {
            refresh_connection(state, &mut connection).await?;
            let token = connection.access_token.as_deref().unwrap_or_default();
            fetch_with_token(state, &connection, token, models_url).await
        }
        result => result,
    }
}

async fn fetch_with_token(
    state: &AppState,
    connection: &ProviderConnection,
    token: &str,
    models_url: &str,
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
        .get(models_url)
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
    let observed_generation = connection_credential_generation(connection);
    let refreshed = CONNECTION_REFRESH_COORDINATOR
        .refresh_connection(
            state.db.clone(),
            "codex",
            &connection.id,
            observed_generation,
        )
        .await
        .map_err(|error| CodexCatalogError::new(StatusCode::UNAUTHORIZED, error))?;
    *connection = refreshed.connection;
    Ok(())
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

    #[test]
    fn cached_supporters_do_not_cross_credential_identity() {
        let catalog = CodexModelCatalog::default();
        let connection = ProviderConnection {
            id: "connection".into(),
            provider: "codex".into(),
            access_token: Some("token-a".into()),
            ..Default::default()
        };
        let mut snapshot = AppDb {
            provider_connections: vec![connection.clone()],
            ..Default::default()
        };
        catalog.seed(
            &snapshot,
            &connection,
            vec![CodexModelMetadata {
                id: "gpt-test".into(),
                name: "GPT Test".into(),
                context_window: None,
                capabilities: Vec::new(),
                reasoning_efforts: Vec::new(),
            }],
        );

        assert_eq!(
            catalog.cached_supporters("gpt-test", &snapshot),
            HashSet::from([connection.id.clone()])
        );

        let mut changed = connection;
        changed.access_token = Some("token-b".into());
        snapshot.provider_connections = vec![changed];
        assert_eq!(
            catalog.cached_supporters("gpt-test", &snapshot),
            HashSet::new()
        );
    }
}
