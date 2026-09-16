use std::collections::BTreeMap;

use serde::Deserialize;

use openproxy_dashboard::model_inventory::{
    build_available_models, BuildInput, CatalogModel, CustomModel, Inventory, LiveModel,
};

use crate::api;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnection {
    pub id: String,
    pub provider: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub auth_type: Option<String>,
    #[serde(default = "default_true")]
    pub is_active: bool,
    #[serde(default)]
    pub test_status: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogModelsEntry {
    alias: String,
    #[serde(default)]
    models: Vec<CatalogModel>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogResponse {
    #[serde(default)]
    provider_id_to_alias: BTreeMap<String, String>,
    #[serde(default)]
    provider_models: Vec<CatalogModelsEntry>,
}

#[derive(Default, Deserialize)]
struct ConnectionsResponse {
    #[serde(default)]
    connections: Vec<ProviderConnection>,
}

#[derive(Default, Deserialize)]
struct CustomModelsResponse {
    #[serde(default)]
    models: Vec<CustomModel>,
}

#[derive(Default, Deserialize)]
struct AliasesResponse {
    #[serde(default)]
    aliases: BTreeMap<String, String>,
}

#[derive(Default, Deserialize)]
struct DisabledResponse {
    #[serde(default)]
    disabled: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderFilter {
    #[serde(default)]
    free_only: bool,
}

#[derive(Default, Deserialize)]
struct FiltersResponse {
    #[serde(default)]
    filters: BTreeMap<String, ProviderFilter>,
}

#[derive(Default, Deserialize)]
struct ProviderModelsResponse {
    #[serde(default)]
    models: Vec<LiveModel>,
}

#[derive(Clone, Debug, Default)]
pub struct ModelState {
    pub provider_aliases: BTreeMap<String, String>,
    catalog_by_alias: BTreeMap<String, Vec<CatalogModel>>,
    live_by_alias: BTreeMap<String, Vec<LiveModel>>,
    pub custom_models: Vec<CustomModel>,
    pub aliases: BTreeMap<String, String>,
    pub disabled: BTreeMap<String, Vec<String>>,
    filters: BTreeMap<String, ProviderFilter>,
    pub connections: Vec<ProviderConnection>,
}

impl ModelState {
    pub fn provider_ids(&self) -> Vec<String> {
        let mut ids = self
            .connections
            .iter()
            .filter(|connection| connection.is_active)
            .map(|connection| connection.provider.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    pub fn alias(&self, provider_id: &str) -> String {
        self.provider_aliases
            .get(provider_id)
            .cloned()
            .unwrap_or_else(|| provider_id.to_string())
    }

    pub fn inventory(&self, provider_id: &str) -> Inventory {
        let alias = self.alias(provider_id);
        build_available_models(BuildInput {
            catalog_models: self
                .catalog_by_alias
                .get(&alias)
                .cloned()
                .unwrap_or_default(),
            live_models: self.live_by_alias.get(&alias).cloned().unwrap_or_default(),
            custom_models: self.custom_models.clone(),
            model_aliases: self
                .aliases
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            disabled_ids: self.disabled.get(&alias).cloned().unwrap_or_default(),
            provider_alias: alias.clone(),
            model_type: "llm".to_string(),
            free_only: self
                .filters
                .get(&alias)
                .is_some_and(|filter| filter.free_only),
        })
    }

    pub fn free_only(&self, provider_id: &str) -> bool {
        let alias = self.alias(provider_id);
        self.filters
            .get(&alias)
            .is_some_and(|filter| filter.free_only)
    }
}

pub async fn load_model_state() -> Result<ModelState, String> {
    let catalog = api::get_json::<CatalogResponse>("/api/catalog").await?;
    let connections = api::get_json::<ConnectionsResponse>("/api/providers").await?;
    let custom = api::get_json::<CustomModelsResponse>("/api/models/custom")
        .await
        .unwrap_or_default();
    let aliases = api::get_json::<AliasesResponse>("/api/models/alias")
        .await
        .unwrap_or_default();
    let disabled = api::get_json::<DisabledResponse>("/api/models/disabled")
        .await
        .unwrap_or_default();
    let filters = api::get_json::<FiltersResponse>("/api/providers/filters")
        .await
        .unwrap_or_default();
    let mut live_by_alias = BTreeMap::<String, Vec<LiveModel>>::new();
    for connection in connections
        .connections
        .iter()
        .filter(|connection| connection.is_active)
    {
        let path = format!(
            "/api/providers/{}/models",
            urlencoding::encode(&connection.id)
        );
        let Ok(response) = api::get_json::<ProviderModelsResponse>(&path).await else {
            continue;
        };
        let alias = catalog
            .provider_id_to_alias
            .get(&connection.provider)
            .cloned()
            .unwrap_or_else(|| connection.provider.clone());
        live_by_alias
            .entry(alias)
            .or_default()
            .extend(response.models);
    }

    Ok(ModelState {
        provider_aliases: catalog.provider_id_to_alias,
        catalog_by_alias: catalog
            .provider_models
            .into_iter()
            .map(|entry| (entry.alias, entry.models))
            .collect(),
        live_by_alias,
        custom_models: custom.models,
        aliases: aliases.aliases,
        disabled: disabled.disabled,
        filters: filters.filters,
        connections: connections.connections,
    })
}

fn default_true() -> bool {
    true
}
