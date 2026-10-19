//! Demand-driven xAI discovery. User configuration and generation routing are
//! deliberately independent of this process-local, credential-scoped cache.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::watch;
use tokio::time::{timeout_at, Instant};

use crate::core::executor::read_reqwest_body;
use crate::core::proxy::resolve_proxy_target;
use crate::oauth::token_refresh::{
    connection_credential_generation, CONNECTION_REFRESH_COORDINATOR,
};
use crate::server::state::AppState;
use crate::types::{AppDb, ProviderConnection};

const TTL: Duration = Duration::from_secs(3600);
const RETRY_DELAY: Duration = Duration::from_secs(60);
const DEADLINE: Duration = Duration::from_secs(10);
const ENRICHMENT_TIMEOUT: Duration = Duration::from_secs(2);
const BODY_LIMIT: usize = 1024 * 1024;
const MAX_IDS: usize = 2048;

#[derive(Debug, Clone, PartialEq)]
pub struct XaiModelMetadata {
    pub id: String,
    pub name: String,
    pub context_window: Option<u32>,
    pub capabilities: Vec<String>,
    pub input_modalities: Option<Vec<String>>,
    pub output_modalities: Option<Vec<String>>,
    pub reasoning_efforts: Option<Vec<String>>,
}

impl XaiModelMetadata {
    pub fn catalog_json(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "kind": "llm",
            "contextWindow": self.context_window, "capabilities": self.capabilities,
            "inputModalities": self.input_modalities, "outputModalities": self.output_modalities,
            "reasoningEfforts": self.reasoning_efforts,
        })
    }
}

#[derive(Debug, Clone)]
pub struct XaiInventory {
    pub models: Arc<Vec<XaiModelMetadata>>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct XaiCatalogError {
    pub status: StatusCode,
    pub message: String,
    pub retry_after: u64,
}

impl XaiCatalogError {
    fn new(status: StatusCode, message: &str) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: RETRY_DELAY.as_secs(),
        }
    }
    fn changed() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "xAI connection changed during model discovery",
        )
    }
    fn invalid() -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "Invalid xAI model catalog response",
        )
    }
    fn timed_out() -> Self {
        Self::new(StatusCode::GATEWAY_TIMEOUT, "xAI model discovery timed out")
    }
}

type Outcome = Result<XaiInventory, XaiCatalogError>;

struct Flight {
    result: watch::Sender<Option<Outcome>>,
}

#[derive(Clone)]
struct Publication {
    loaded_at: Instant,
    inventory: XaiInventory,
}

struct Entry {
    identity: String,
    published: Option<Publication>,
    failure: Option<(Instant, XaiCatalogError)>,
    flight: Option<Arc<Flight>>,
}

pub struct XaiModelCatalog {
    entries: Mutex<BTreeMap<String, Entry>>,
    language_url: String,
    models_url: String,
}

impl Default for XaiModelCatalog {
    fn default() -> Self {
        Self::with_urls(
            "https://api.x.ai/v1/language-models",
            "https://api.x.ai/v1/models",
        )
    }
}

pub fn selected_connection(snapshot: &AppDb) -> Option<&ProviderConnection> {
    snapshot
        .provider_connections
        .iter()
        .find(|c| c.provider == "xai" && c.is_active())
}

fn credential(connection: &ProviderConnection) -> Option<&str> {
    connection
        .access_token
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            connection
                .api_key
                .as_deref()
                .filter(|s| !s.trim().is_empty())
        })
}

fn identity(connection: &ProviderConnection) -> String {
    // Never retain historical tokens, and do not invalidate metadata on model
    // preferences, timestamps, health probes, names or other unrelated edits.
    sha256::digest(
        serde_json::to_vec(&(
            &connection.id,
            &connection.created_at,
            &connection.provider,
            &connection.auth_type,
            credential(connection),
            &connection.refresh_token,
        ))
        .expect("connection identity is serializable"),
    )
}

impl XaiModelCatalog {
    /// Endpoint injection for deterministic local fixtures, not user-controlled routing.
    #[doc(hidden)]
    pub fn with_urls(language_url: impl Into<String>, models_url: impl Into<String>) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            language_url: language_url.into(),
            models_url: models_url.into(),
        }
    }

    pub fn published_for_connection(
        &self,
        connection: &ProviderConnection,
    ) -> Option<XaiInventory> {
        if connection.provider != "xai" || !connection.is_active() {
            return None;
        }
        self.entries
            .lock()
            .get(&connection.id)
            .filter(|entry| entry.identity == identity(connection))
            .and_then(|entry| entry.published.as_ref().map(|p| p.inventory.clone()))
    }

    pub fn published_selected(&self, snapshot: &AppDb) -> Option<XaiInventory> {
        self.published_for_connection(selected_connection(snapshot)?)
    }

    pub async fn models_for_connection(
        &self,
        state: &AppState,
        connection: &ProviderConnection,
    ) -> Outcome {
        self.refresh_connection(state, connection, false).await
    }

    pub async fn refresh_connection(
        &self,
        state: &AppState,
        connection: &ProviderConnection,
        force: bool,
    ) -> Outcome {
        let deadline = Instant::now() + DEADLINE;
        let before = state.db.snapshot();
        let mut canonical = before
            .provider_connections
            .iter()
            .find(|c| c.id == connection.id && c.provider == "xai" && c.is_active())
            .cloned()
            .ok_or_else(XaiCatalogError::changed)?;
        let observed = identity(&canonical);
        let (flight, leader) = {
            let mut entries = self.entries.lock();
            // Snapshot under this short lock: an older caller must not evict
            // a connection another operation has just published.
            let current = state.db.snapshot();
            entries.retain(|id, _| {
                current
                    .provider_connections
                    .iter()
                    .any(|c| c.id == *id && c.provider == "xai" && c.is_active())
            });
            let entry = entries
                .entry(canonical.id.clone())
                .or_insert_with(|| Entry {
                    identity: observed.clone(),
                    published: None,
                    failure: None,
                    flight: None,
                });
            if entry.identity != observed {
                *entry = Entry {
                    identity: observed.clone(),
                    published: None,
                    failure: None,
                    flight: None,
                };
            }
            if let Some(flight) = &entry.flight {
                (flight.clone(), false)
            } else {
                if !force {
                    if let Some((next, error)) = &entry.failure {
                        if *next > Instant::now() {
                            let mut error = error.clone();
                            error.retry_after = next
                                .saturating_duration_since(Instant::now())
                                .as_secs()
                                .max(1);
                            return entry
                                .published
                                .as_ref()
                                .map(|p| p.inventory.clone())
                                .ok_or(error);
                        }
                    }
                    if let Some(publication) = &entry.published {
                        if publication.loaded_at.elapsed() < TTL {
                            return Ok(publication.inventory.clone());
                        }
                    }
                }
                let (result, _) = watch::channel(None);
                let flight = Arc::new(Flight { result });
                entry.flight = Some(flight.clone());
                (flight, true)
            }
        };
        drop(before);
        if !leader {
            let mut receiver = flight.result.subscribe();
            let result = timeout_at(deadline, async {
                loop {
                    if let Some(result) = receiver.borrow().clone() {
                        break result;
                    }
                    if receiver.changed().await.is_err() {
                        break Err(XaiCatalogError::timed_out());
                    }
                }
            })
            .await
            .unwrap_or_else(|_| Err(XaiCatalogError::timed_out()));
            return self.fence_outcome(state, &canonical.id, result);
        }

        let mut guard = FlightGuard {
            catalog: self,
            id: canonical.id.clone(),
            flight: flight.clone(),
            finished: false,
        };
        let fetched = timeout_at(deadline, self.fetch(state, &mut canonical, deadline))
            .await
            .unwrap_or_else(|_| Err(XaiCatalogError::timed_out()));
        let after = state.db.snapshot();
        let current = after
            .provider_connections
            .iter()
            .find(|c| c.id == canonical.id && c.provider == "xai" && c.is_active());
        let valid = current.is_some_and(|c| identity(c) == identity(&canonical));
        let result = {
            let mut entries = self.entries.lock();
            match entries
                .get_mut(&canonical.id)
                .filter(|e| e.flight.as_ref().is_some_and(|f| Arc::ptr_eq(f, &flight)))
            {
                Some(entry) if valid => {
                    // A coordinated rotation may legitimately change the identity.
                    entry.identity = identity(&canonical);
                    let outcome = match fetched {
                        Ok(inventory) => {
                            entry.published = Some(Publication {
                                loaded_at: Instant::now(),
                                inventory: inventory.clone(),
                            });
                            entry.failure = None;
                            Ok(inventory)
                        }
                        Err(error) => {
                            entry.failure = Some((Instant::now() + RETRY_DELAY, error.clone()));
                            if let Some(previous) = &mut entry.published {
                                // Never carry facts across an OAuth credential rotation.
                                if observed == entry.identity {
                                    previous.inventory.warning = Some(format!(
                                        "{}; using the previous xAI catalog",
                                        error.message
                                    ));
                                    Ok(previous.inventory.clone())
                                } else {
                                    entry.published = None;
                                    Err(error)
                                }
                            } else {
                                Err(error)
                            }
                        }
                    };
                    entry.flight = None;
                    outcome
                }
                Some(entry) => {
                    entry.flight = None;
                    Err(XaiCatalogError::changed())
                }
                None => Err(XaiCatalogError::changed()),
            }
        };
        flight.result.send_replace(Some(result.clone()));
        guard.finished = true;
        result
    }

    fn fence_outcome(&self, state: &AppState, id: &str, outcome: Outcome) -> Outcome {
        let snapshot = state.db.snapshot();
        let connection = snapshot
            .provider_connections
            .iter()
            .find(|c| c.id == id && c.provider == "xai" && c.is_active())
            .ok_or_else(XaiCatalogError::changed)?;
        if outcome.is_ok() {
            self.published_for_connection(connection)
                .ok_or_else(XaiCatalogError::changed)
        } else {
            outcome
        }
    }

    async fn fetch(
        &self,
        state: &AppState,
        connection: &mut ProviderConnection,
        deadline: Instant,
    ) -> Outcome {
        let primary = self.get_json(state, connection, &self.language_url).await;
        let payload = match primary {
            Err(error)
                if error.status == StatusCode::UNAUTHORIZED
                    && connection.auth_type == "oauth"
                    && connection
                        .refresh_token
                        .as_deref()
                        .is_some_and(|t| !t.is_empty()) =>
            {
                let refreshed = CONNECTION_REFRESH_COORDINATOR
                    .refresh_connection(
                        state.db.clone(),
                        "xai",
                        &connection.id,
                        connection_credential_generation(connection),
                    )
                    .await
                    .map_err(|_| {
                        XaiCatalogError::new(StatusCode::BAD_GATEWAY, "xAI OAuth refresh failed")
                    })?;
                *connection = refreshed.connection;
                self.get_json(state, connection, &self.language_url).await?
            }
            result => result?,
        };
        let families = parse_language(payload)?;
        // Leave the outer deadline room to publish the valid primary catalog.
        let enrichment_deadline =
            (Instant::now() + ENRICHMENT_TIMEOUT).min(deadline - Duration::from_millis(25));
        let enrichment = timeout_at(
            enrichment_deadline,
            self.get_json(state, connection, &self.models_url),
        )
        .await;
        let (contexts, warning) = match enrichment {
            Ok(Ok(value)) => match parse_contexts(value) {
                Ok(contexts) => (contexts, None),
                Err(_) => (
                    BTreeMap::new(),
                    Some("xAI context metadata is unavailable".into()),
                ),
            },
            _ => (
                BTreeMap::new(),
                Some("xAI context metadata is unavailable".into()),
            ),
        };
        Ok(XaiInventory {
            models: Arc::new(expand(families, &contexts)?),
            warning,
        })
    }

    async fn get_json(
        &self,
        state: &AppState,
        connection: &ProviderConnection,
        url: &str,
    ) -> Result<Value, XaiCatalogError> {
        let token = credential(connection).ok_or_else(|| {
            XaiCatalogError::new(
                StatusCode::UNAUTHORIZED,
                "No xAI discovery credential configured",
            )
        })?;
        let snapshot = state.db.snapshot();
        let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);
        let client = state
            .client_pool
            .get_or_insert_with("xai-catalog-no-redirect", proxy.as_ref(), || {
                let mut builder = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_secs(5))
                    .pool_idle_timeout(Duration::from_secs(90));
                if let Some(proxy) = &proxy {
                    if !proxy.url.is_empty() {
                        builder = builder.proxy(
                            reqwest::Proxy::all(&proxy.url)?
                                .no_proxy(reqwest::NoProxy::from_string(&proxy.no_proxy)),
                        );
                    }
                }
                builder.build().map(Arc::new)
            })
            .map_err(|_| {
                XaiCatalogError::new(
                    StatusCode::BAD_GATEWAY,
                    "Cannot configure xAI discovery transport",
                )
            })?;
        let response = client
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| {
                XaiCatalogError::new(
                    StatusCode::BAD_GATEWAY,
                    "xAI model discovery transport failed",
                )
            })?;
        if !response.status().is_success() {
            return Err(XaiCatalogError {
                status: response.status(),
                message: format!(
                    "xAI model endpoint returned HTTP {}",
                    response.status().as_u16()
                ),
                retry_after: RETRY_DELAY.as_secs(),
            });
        }
        let body = read_reqwest_body(response, BODY_LIMIT).await.map_err(|_| {
            XaiCatalogError::new(
                StatusCode::BAD_GATEWAY,
                "xAI model response exceeds its byte limit or is incomplete",
            )
        })?;
        serde_json::from_slice(&body).map_err(|_| XaiCatalogError::invalid())
    }
}

struct FlightGuard<'a> {
    catalog: &'a XaiModelCatalog,
    id: String,
    flight: Arc<Flight>,
    finished: bool,
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let error = XaiCatalogError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "xAI model discovery was cancelled",
        );
        let mut entries = self.catalog.entries.lock();
        if let Some(entry) = entries.get_mut(&self.id).filter(|e| {
            e.flight
                .as_ref()
                .is_some_and(|f| Arc::ptr_eq(f, &self.flight))
        }) {
            entry.flight = None;
            entry.failure = Some((Instant::now() + RETRY_DELAY, error.clone()));
        }
        self.flight.result.send_replace(Some(Err(error)));
    }
}

#[derive(Deserialize)]
struct LanguageResponse {
    models: Vec<LanguageModel>,
}

#[derive(Deserialize)]
struct LanguageModel {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    input_modalities: Option<Vec<String>>,
    output_modalities: Option<Vec<String>>,
    capabilities: Option<Capabilities>,
}

#[derive(Deserialize)]
struct Capabilities {
    reasoning_effort: Option<Vec<String>>,
}

fn valid_string(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn parse_language(value: Value) -> Result<Vec<LanguageModel>, XaiCatalogError> {
    let response: LanguageResponse =
        serde_json::from_value(value).map_err(|_| XaiCatalogError::invalid())?;
    let mut expanded_count = 0usize;
    for model in &response.models {
        expanded_count = expanded_count
            .checked_add(1 + model.aliases.len())
            .ok_or_else(XaiCatalogError::invalid)?;
        if expanded_count > MAX_IDS
            || !valid_string(&model.id)
            || model.aliases.iter().any(|id| !valid_string(id))
        {
            return Err(XaiCatalogError::invalid());
        }
        for values in [
            &model.input_modalities,
            &model.output_modalities,
            &model
                .capabilities
                .as_ref()
                .and_then(|c| c.reasoning_effort.clone()),
        ]
        .into_iter()
        .flatten()
        {
            if values.len() > 16 || values.iter().any(|v| !valid_string(v)) {
                return Err(XaiCatalogError::invalid());
            }
        }
    }
    Ok(response.models)
}

fn parse_contexts(value: Value) -> Result<BTreeMap<String, u32>, XaiCatalogError> {
    #[derive(Deserialize)]
    struct Response {
        data: Vec<Model>,
    }
    #[derive(Deserialize)]
    struct Model {
        id: String,
        #[serde(default)]
        aliases: Vec<String>,
        context_length: Option<u64>,
    }
    let response: Response =
        serde_json::from_value(value).map_err(|_| XaiCatalogError::invalid())?;
    let mut contexts = BTreeMap::new();
    for model in response.data {
        let Some(context) = model
            .context_length
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
        else {
            continue;
        };
        for id in std::iter::once(model.id).chain(model.aliases) {
            if contexts
                .insert(id, context)
                .is_some_and(|old| old != context)
            {
                return Err(XaiCatalogError::invalid());
            }
        }
    }
    Ok(contexts)
}

fn expand(
    families: Vec<LanguageModel>,
    contexts: &BTreeMap<String, u32>,
) -> Result<Vec<XaiModelMetadata>, XaiCatalogError> {
    let canonical_ids: BTreeSet<_> = families.iter().map(|m| m.id.clone()).collect();
    let mut alias_owners = BTreeMap::new();
    for family in &families {
        for alias in &family.aliases {
            if !canonical_ids.contains(alias)
                && alias_owners
                    .insert(alias.clone(), family.id.clone())
                    .is_some_and(|owner| owner != family.id)
            {
                return Err(XaiCatalogError::invalid());
            }
        }
    }
    let excluded: BTreeSet<_> = families
        .iter()
        .filter(|m| {
            std::iter::once(&m.id)
                .chain(&m.aliases)
                .any(|id| id.to_ascii_lowercase().contains("multi-agent"))
        })
        .flat_map(|m| std::iter::once(m.id.clone()).chain(m.aliases.clone()))
        .collect();
    let mut models = BTreeMap::new();
    for family in families {
        if excluded.contains(&family.id) {
            continue;
        }
        let efforts = family.capabilities.and_then(|c| c.reasoning_effort);
        let mut capabilities = Vec::new();
        if family
            .input_modalities
            .as_ref()
            .is_some_and(|m| m.iter().any(|v| v == "image"))
        {
            capabilities.push("vision".into());
        }
        if efforts
            .as_ref()
            .is_some_and(|e| e.iter().any(|v| v != "none"))
        {
            capabilities.push("reasoning".into());
        }
        let context = contexts.get(&family.id).copied().or_else(|| {
            let matching: BTreeSet<_> = family
                .aliases
                .iter()
                .filter_map(|id| contexts.get(id).copied())
                .collect();
            (matching.len() == 1).then(|| *matching.first().expect("one context"))
        });
        for id in std::iter::once(&family.id).chain(&family.aliases) {
            if excluded.contains(id) || (id != &family.id && canonical_ids.contains(id)) {
                continue;
            }
            let model = XaiModelMetadata {
                id: id.clone(),
                name: id.clone(),
                context_window: contexts.get(id).copied().or(context),
                capabilities: capabilities.clone(),
                input_modalities: family.input_modalities.clone(),
                output_modalities: family.output_modalities.clone(),
                reasoning_efforts: efforts.clone(),
            };
            if models
                .insert(id.clone(), model.clone())
                .is_some_and(|old| old != model)
            {
                return Err(XaiCatalogError::invalid());
            }
        }
    }
    Ok(models.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn fixture() -> Value {
        json!({"models":[
            {"id":"future-model", "aliases":["future-latest"], "input_modalities":["text","image"], "output_modalities":["text"], "capabilities":{"reasoning_effort":["none","high","xhigh"]}},
            {"id":"plain", "aliases":[], "input_modalities":["text"], "output_modalities":["text"], "capabilities":null},
            {"id":"future-multi-agent-01", "aliases":["innocent-alias"]}
        ]})
    }

    fn connection() -> ProviderConnection {
        ProviderConnection {
            id: "xai-test".into(),
            provider: "xai".into(),
            auth_type: "apikey".into(),
            api_key: Some("test-credential".into()),
            ..Default::default()
        }
    }

    async fn setup(server: &MockServer) -> (tempfile::TempDir, AppState, ProviderConnection) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::load_from(dir.path()).await.unwrap());
        let connection = connection();
        db.update(|db| db.provider_connections = vec![connection.clone()])
            .await
            .unwrap();
        let mut state = AppState::new(db);
        state.xai_models = Arc::new(XaiModelCatalog::with_urls(
            format!("{}/language", server.uri()),
            format!("{}/models", server.uri()),
        ));
        (dir, state, connection)
    }

    async fn mount(server: &MockServer, delay: Duration) {
        Mock::given(method("GET"))
            .and(path("/language"))
            .and(header("authorization", "Bearer test-credential"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(fixture())
                    .set_delay(delay),
            )
            .mount(server)
            .await;
        Mock::given(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[
                {"id":"future-model", "aliases":["future-latest"], "context_length":500000},
                {"id":"image-generator", "aliases":[], "context_length":2048}
            ]})))
            .mount(server)
            .await;
    }

    #[test]
    fn parser_expands_exact_aliases_and_excludes_whole_multiagent_family() {
        let models = expand(parse_language(fixture()).unwrap(), &BTreeMap::new()).unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["future-latest", "future-model", "plain"]
        );
        assert_eq!(models[0].capabilities, ["vision", "reasoning"]);
        assert_eq!(
            models[0].reasoning_efforts.as_ref().unwrap(),
            &["none", "high", "xhigh"]
        );
        assert!(models[2].capabilities.is_empty());
        assert_eq!(models[2].reasoning_efforts, None);
        assert_eq!(models[2].context_window, None);
        let marked_alias =
            json!({"models":[{"id":"unmarked", "aliases":["grok-multi-agent-latest"]}]});
        assert!(
            expand(parse_language(marked_alias).unwrap(), &BTreeMap::new())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_rows_are_not_a_successful_empty_catalog() {
        for invalid in [
            json!({}),
            json!({"models":null}),
            json!({"models":[{}]}),
            json!({"models":[{"id":""}]}),
            json!({"models":[{"id":"ok","aliases":[123]}]}),
        ] {
            assert!(parse_language(invalid).is_err());
        }
        assert!(expand(
            parse_language(json!({"models":[]})).unwrap(),
            &BTreeMap::new()
        )
        .unwrap()
        .is_empty());
        let aliases = (0..MAX_IDS)
            .map(|n| format!("alias-{n}"))
            .collect::<Vec<_>>();
        assert!(parse_language(json!({"models":[{"id":"too-many","aliases":aliases}]})).is_err());
    }

    #[test]
    fn canonical_precedence_and_ambiguous_aliases() {
        let value = json!({"models":[{"id":"a","aliases":["b"]},{"id":"b","aliases":[],"input_modalities":["image"]}]});
        let forward = expand(parse_language(value.clone()).unwrap(), &BTreeMap::new()).unwrap();
        let mut reversed = value;
        reversed["models"].as_array_mut().unwrap().reverse();
        assert_eq!(
            forward,
            expand(parse_language(reversed).unwrap(), &BTreeMap::new()).unwrap()
        );
        assert_eq!(forward[1].capabilities, ["vision"]);
        let ambiguous = json!({"models":[{"id":"a","aliases":["shared"]},{"id":"b","aliases":["shared"],"input_modalities":["image"]}]});
        assert!(expand(parse_language(ambiguous).unwrap(), &BTreeMap::new()).is_err());
    }

    #[test]
    fn enrichment_cannot_add_members_or_invent_limits() {
        let contexts = parse_contexts(json!({"data":[{"id":"future-latest","context_length":500000},{"id":"image-generator","context_length":2048},{"id":"plain","context_length":0}]})).unwrap();
        let models = expand(parse_language(fixture()).unwrap(), &contexts).unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].context_window, Some(500000));
        assert_eq!(models[1].context_window, Some(500000));
        assert_eq!(models[2].context_window, None);
    }

    #[tokio::test]
    async fn cache_uses_ttl_and_shares_forced_success_and_error() {
        let server = MockServer::start().await;
        mount(&server, Duration::from_millis(100)).await;
        let (_dir, state, connection) = setup(&server).await;
        let (a, b, c) = tokio::join!(
            state
                .xai_models
                .refresh_connection(&state, &connection, true),
            state
                .xai_models
                .refresh_connection(&state, &connection, true),
            state.xai_models.models_for_connection(&state, &connection)
        );
        assert!(a.is_ok() && b.is_ok() && c.is_ok());
        let models = a.unwrap();
        assert_eq!(models.models[0].context_window, Some(500000));
        state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.reset().await;
        Mock::given(path("/language"))
            .respond_with(ResponseTemplate::new(403).set_delay(Duration::from_millis(50)))
            .mount(&server)
            .await;
        let (a, b) = tokio::join!(
            state
                .xai_models
                .refresh_connection(&state, &connection, true),
            state
                .xai_models
                .refresh_connection(&state, &connection, true)
        );
        assert!(a.unwrap().warning.is_some());
        assert!(b.unwrap().warning.is_some());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cold_failures_are_shared_and_negatively_cached() {
        let server = MockServer::start().await;
        Mock::given(path("/language"))
            .respond_with(ResponseTemplate::new(401).set_delay(Duration::from_millis(50)))
            .mount(&server)
            .await;
        let (_dir, state, connection) = setup(&server).await;
        let (a, b) = tokio::join!(
            state.xai_models.models_for_connection(&state, &connection),
            state.xai_models.models_for_connection(&state, &connection)
        );
        assert_eq!(a.unwrap_err().status, StatusCode::UNAUTHORIZED);
        assert_eq!(b.unwrap_err().status, StatusCode::UNAUTHORIZED);
        assert!(state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn harmless_edits_keep_publication_but_credentials_invalidate() {
        let server = MockServer::start().await;
        mount(&server, Duration::ZERO).await;
        let (_dir, state, connection) = setup(&server).await;
        state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .unwrap();
        state
            .db
            .update(|db| {
                let c = &mut db.provider_connections[0];
                c.name = Some("renamed".into());
                c.updated_at = Some("new".into());
                c.default_model = Some("custom".into());
                c.provider_specific_data
                    .insert("enabledModels".into(), json!(["custom"]));
            })
            .await
            .unwrap();
        let current = state.db.snapshot().provider_connections[0].clone();
        assert!(state
            .xai_models
            .published_for_connection(&current)
            .is_some());
        state
            .xai_models
            .models_for_connection(&state, &current)
            .await
            .unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        let mut changed = current;
        changed.api_key = Some("changed".into());
        assert!(state
            .xai_models
            .published_for_connection(&changed)
            .is_none());
    }

    #[tokio::test]
    async fn delayed_credential_change_fences_success_and_failure() {
        for status in [200, 403] {
            let server = MockServer::start().await;
            Mock::given(path("/language"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .set_body_json(fixture())
                        .set_delay(Duration::from_millis(100)),
                )
                .mount(&server)
                .await;
            let (_dir, state, connection) = setup(&server).await;
            let call = state.xai_models.models_for_connection(&state, &connection);
            let edit = async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                state
                    .db
                    .update(|db| db.provider_connections[0].api_key = Some("changed".into()))
                    .await
                    .unwrap();
            };
            let (result, ()) = tokio::join!(call, edit);
            assert_eq!(result.unwrap_err().status, StatusCode::CONFLICT);
            assert!(state
                .xai_models
                .published_selected(&state.db.snapshot())
                .is_none());
        }
    }

    #[tokio::test]
    async fn optional_timeout_and_bad_enrichment_do_not_discard_primary() {
        let server = MockServer::start().await;
        Mock::given(path("/language"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture()))
            .mount(&server)
            .await;
        Mock::given(path("/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data":[]}))
                    .set_delay(Duration::from_secs(3)),
            )
            .mount(&server)
            .await;
        let (_dir, state, connection) = setup(&server).await;
        let inventory = state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .unwrap();
        assert_eq!(inventory.models.len(), 3);
        assert!(inventory.warning.is_some());
        assert!(inventory.models.iter().all(|m| m.context_window.is_none()));
    }

    #[tokio::test]
    async fn leader_cancellation_releases_waiters_and_force_can_retry() {
        let server = MockServer::start().await;
        mount(&server, Duration::from_secs(3)).await;
        let (_dir, state, connection) = setup(&server).await;
        let leader_state = state.clone();
        let leader_connection = connection.clone();
        let leader = tokio::spawn(async move {
            leader_state
                .xai_models
                .models_for_connection(&leader_state, &leader_connection)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let waiter = state.xai_models.models_for_connection(&state, &connection);
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            leader.abort();
        };
        let (result, ()) = tokio::join!(waiter, cancel);
        assert!(result.unwrap_err().message.contains("cancelled"));
        server.reset().await;
        mount(&server, Duration::ZERO).await;
        assert!(state
            .xai_models
            .refresh_connection(&state, &connection, true)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn redirects_are_not_followed_and_oversized_responses_fail() {
        let server = MockServer::start().await;
        let (_dir, state, connection) = setup(&server).await;
        Mock::given(path("/language"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/secret-target", server.uri())),
            )
            .mount(&server)
            .await;
        assert_eq!(
            state
                .xai_models
                .models_for_connection(&state, &connection)
                .await
                .unwrap_err()
                .status,
            StatusCode::FOUND
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        server.reset().await;
        Mock::given(path("/language"))
            .respond_with(ResponseTemplate::new(200).set_body_string(" ".repeat(BODY_LIMIT + 1)))
            .mount(&server)
            .await;
        assert_eq!(
            state
                .xai_models
                .refresh_connection(&state, &connection, true)
                .await
                .unwrap_err()
                .status,
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn slow_optional_enrichment_cannot_consume_the_primary_publication_deadline() {
        let server = MockServer::start().await;
        Mock::given(path("/language"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(fixture())
                    .set_delay(Duration::from_millis(8500)),
            )
            .mount(&server)
            .await;
        Mock::given(path("/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data": []}))
                    .set_delay(Duration::from_secs(3)),
            )
            .mount(&server)
            .await;
        let (_dir, state, connection) = setup(&server).await;
        let inventory = tokio::time::timeout(
            DEADLINE + Duration::from_millis(500),
            state.xai_models.models_for_connection(&state, &connection),
        )
        .await
        .expect("catalog caller remains bounded")
        .expect("validated primary is published before the deadline");
        assert_eq!(inventory.models.len(), 3);
        assert!(inventory.warning.is_some());
    }

    #[tokio::test]
    async fn configured_proxy_and_permission_failure_preserve_transport_policy() {
        let server = MockServer::start().await;
        mount(&server, Duration::ZERO).await;
        let (_dir, mut state, connection) = setup(&server).await;
        state.xai_models = Arc::new(XaiModelCatalog::with_urls(
            "http://xai-fixture.invalid/language",
            "http://xai-fixture.invalid/models",
        ));
        state
            .db
            .update(|db| db.provider_connections[0].proxy_url = Some(server.uri()))
            .await
            .unwrap();
        assert!(state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .is_ok());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.reset().await;
        Mock::given(path("/language"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        state
            .db
            .update(|db| {
                db.provider_connections[0].auth_type = "oauth".into();
                db.provider_connections[0].refresh_token = Some("must-not-be-used-on-403".into());
            })
            .await
            .unwrap();
        assert_eq!(
            state
                .xai_models
                .refresh_connection(&state, &connection, true)
                .await
                .unwrap_err()
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn failed_retry_deadline_expires_and_waiter_cancellation_does_not_cancel_leader() {
        let server = MockServer::start().await;
        Mock::given(path("/language"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let (_dir, state, connection) = setup(&server).await;
        assert!(state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .is_err());
        state
            .xai_models
            .entries
            .lock()
            .get_mut(&connection.id)
            .unwrap()
            .failure
            .as_mut()
            .unwrap()
            .0 = Instant::now();
        assert!(state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.reset().await;
        mount(&server, Duration::from_millis(200)).await;
        let leader_state = state.clone();
        let leader_connection = connection.clone();
        let leader = tokio::spawn(async move {
            leader_state
                .xai_models
                .refresh_connection(&leader_state, &leader_connection, true)
                .await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(tokio::time::timeout(
            Duration::from_millis(30),
            state.xai_models.models_for_connection(&state, &connection)
        )
        .await
        .is_err());
        assert!(leader.await.unwrap().is_ok());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn expiry_triggers_refresh_and_valid_empty_replaces_old_models() {
        let server = MockServer::start().await;
        mount(&server, Duration::ZERO).await;
        let (_dir, state, connection) = setup(&server).await;
        state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .unwrap();
        state
            .xai_models
            .entries
            .lock()
            .get_mut(&connection.id)
            .unwrap()
            .published
            .as_mut()
            .unwrap()
            .loaded_at = Instant::now() - TTL;
        server.reset().await;
        Mock::given(path("/language"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":[]})))
            .mount(&server)
            .await;
        Mock::given(path("/models"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        assert!(state
            .xai_models
            .models_for_connection(&state, &connection)
            .await
            .unwrap()
            .models
            .is_empty());
    }
}
