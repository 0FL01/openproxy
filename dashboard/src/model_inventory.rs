use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default, rename = "type")]
    pub model_type: Option<String>,
    #[serde(default)]
    pub is_free: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_free: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomModel {
    #[serde(default)]
    pub provider_alias: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default, rename = "type")]
    pub model_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSource {
    Catalog,
    Live,
    Custom,
    LegacyAlias,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableModel {
    pub id: String,
    pub name: String,
    pub full_model: String,
    pub source: ModelSource,
    pub model_type: String,
    pub is_free: bool,
    pub disabled: bool,
    pub alias: Option<String>,
}

#[derive(Default)]
pub struct BuildInput {
    pub catalog_models: Vec<CatalogModel>,
    pub live_models: Vec<LiveModel>,
    pub custom_models: Vec<CustomModel>,
    pub model_aliases: Vec<(String, String)>,
    pub disabled_ids: Vec<String>,
    pub provider_alias: String,
    pub model_type: String,
    pub free_only: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Inventory {
    pub custom_rows: Vec<AvailableModel>,
    pub enabled_core_rows: Vec<AvailableModel>,
    pub disabled_core_rows: Vec<AvailableModel>,
    pub enabled_rows: Vec<AvailableModel>,
    pub all_rows: Vec<AvailableModel>,
    pub all_core_ids: Vec<String>,
}

pub fn build_available_models(input: BuildInput) -> Inventory {
    let model_type = if input.model_type.is_empty() {
        "llm".to_string()
    } else {
        input.model_type
    };
    let disabled: HashSet<&str> = input.disabled_ids.iter().map(String::as_str).collect();
    let mut seen_core = HashSet::new();
    let mut core_rows = Vec::new();

    for model in &input.catalog_models {
        let kind = model.kind.as_deref().or(model.model_type.as_deref());
        if kind.is_some_and(|kind| kind != model_type) {
            continue;
        }
        push_core(
            &mut core_rows,
            &mut seen_core,
            &input.provider_alias,
            &model_type,
            &disabled,
            &model.id,
            model.name.as_deref(),
            ModelSource::Catalog,
            model.is_free,
        );
    }
    for model in &input.live_models {
        push_core(
            &mut core_rows,
            &mut seen_core,
            &input.provider_alias,
            &model_type,
            &disabled,
            &model.id,
            model.name.as_deref(),
            ModelSource::Live,
            model.is_free,
        );
    }

    let built_in: HashSet<&str> = input
        .catalog_models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    let mut seen_full = HashSet::new();
    let mut custom_rows = Vec::new();
    for model in &input.custom_models {
        let Some(id) = model.id.as_deref().filter(|id| !id.is_empty()) else {
            continue;
        };
        if model.provider_alias.as_deref() != Some(input.provider_alias.as_str())
            || built_in.contains(id)
        {
            continue;
        }
        let row_type = model
            .kind
            .as_deref()
            .or(model.model_type.as_deref())
            .unwrap_or("llm");
        if row_type != model_type {
            continue;
        }
        let full_model = format!("{}/{id}", input.provider_alias);
        if !seen_full.insert(full_model.clone()) {
            continue;
        }
        custom_rows.push(AvailableModel {
            id: id.to_string(),
            name: model.name.clone().unwrap_or_else(|| id.to_string()),
            full_model,
            source: ModelSource::Custom,
            model_type: row_type.to_string(),
            is_free: false,
            disabled: disabled.contains(id),
            alias: None,
        });
    }

    let prefix = format!("{}/", input.provider_alias);
    for (alias, full_model) in &input.model_aliases {
        let Some(id) = full_model.strip_prefix(&prefix).filter(|id| !id.is_empty()) else {
            continue;
        };
        if built_in.contains(id) || !seen_full.insert(full_model.clone()) {
            continue;
        }
        custom_rows.push(AvailableModel {
            id: id.to_string(),
            name: id.to_string(),
            full_model: full_model.clone(),
            source: ModelSource::LegacyAlias,
            model_type: model_type.clone(),
            is_free: false,
            disabled: disabled.contains(id),
            alias: Some(alias.clone()),
        });
    }

    let enabled_core_rows = core_rows
        .iter()
        .filter(|row| !row.disabled && (!input.free_only || row.is_free))
        .cloned()
        .collect::<Vec<_>>();
    let disabled_core_rows = core_rows
        .iter()
        .filter(|row| row.disabled)
        .cloned()
        .collect();
    let enabled_rows = custom_rows
        .iter()
        .chain(enabled_core_rows.iter())
        .cloned()
        .collect();
    let all_rows = custom_rows
        .iter()
        .chain(core_rows.iter())
        .cloned()
        .collect();
    let all_core_ids = core_rows.iter().map(|row| row.id.clone()).collect();

    Inventory {
        custom_rows,
        enabled_core_rows,
        disabled_core_rows,
        enabled_rows,
        all_rows,
        all_core_ids,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_core(
    rows: &mut Vec<AvailableModel>,
    seen: &mut HashSet<String>,
    provider_alias: &str,
    model_type: &str,
    disabled: &HashSet<&str>,
    id: &str,
    name: Option<&str>,
    source: ModelSource,
    explicit_free: Option<bool>,
) {
    if id.is_empty() || !seen.insert(id.to_string()) {
        return;
    }
    rows.push(AvailableModel {
        id: id.to_string(),
        name: name.unwrap_or(id).to_string(),
        full_model: format!("{provider_alias}/{id}"),
        source,
        model_type: model_type.to_string(),
        is_free: explicit_free.unwrap_or_else(|| is_free_model_id(id)),
        disabled: disabled.contains(id),
        alias: None,
    });
}

fn is_free_model_id(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id.ends_with(":free") || id.ends_with("-free")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(id: &str) -> CatalogModel {
        CatalogModel {
            id: id.into(),
            name: Some(id.into()),
            kind: Some("llm".into()),
            ..Default::default()
        }
    }

    #[test]
    fn catalog_precedes_duplicate_live_model() {
        let inventory = build_available_models(BuildInput {
            provider_alias: "or".into(),
            model_type: "llm".into(),
            catalog_models: vec![catalog("alpha")],
            live_models: vec![LiveModel {
                id: "alpha".into(),
                name: Some("Live Alpha".into()),
                is_free: Some(true),
            }],
            ..Default::default()
        });
        assert_eq!(inventory.all_rows.len(), 1);
        assert_eq!(inventory.all_rows[0].source, ModelSource::Catalog);
        assert_eq!(inventory.all_rows[0].name, "alpha");
    }

    #[test]
    fn filters_non_llm_catalog_models() {
        let inventory = build_available_models(BuildInput {
            provider_alias: "p".into(),
            model_type: "llm".into(),
            catalog_models: vec![
                catalog("chat"),
                CatalogModel {
                    id: "image".into(),
                    kind: Some("image".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        assert_eq!(inventory.all_core_ids, ["chat"]);
    }

    #[test]
    fn splits_disabled_and_free_only_core_rows() {
        let inventory = build_available_models(BuildInput {
            provider_alias: "or".into(),
            model_type: "llm".into(),
            catalog_models: vec![catalog("paid"), catalog("free:free"), catalog("disabled")],
            disabled_ids: vec!["disabled".into()],
            free_only: true,
            ..Default::default()
        });
        assert_eq!(
            inventory
                .enabled_core_rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["free:free"]
        );
        assert_eq!(
            inventory
                .disabled_core_rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["disabled"]
        );
    }

    #[test]
    fn custom_rows_precede_core_and_ignore_free_only() {
        let inventory = build_available_models(BuildInput {
            provider_alias: "p".into(),
            model_type: "llm".into(),
            catalog_models: vec![catalog("paid")],
            custom_models: vec![CustomModel {
                provider_alias: Some("p".into()),
                id: Some("custom".into()),
                ..Default::default()
            }],
            free_only: true,
            ..Default::default()
        });
        assert_eq!(
            inventory
                .enabled_rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["custom"]
        );
    }

    #[test]
    fn custom_model_wins_over_legacy_alias_for_same_full_id() {
        let inventory = build_available_models(BuildInput {
            provider_alias: "p".into(),
            model_type: "llm".into(),
            custom_models: vec![CustomModel {
                provider_alias: Some("p".into()),
                id: Some("custom".into()),
                name: Some("Custom".into()),
                ..Default::default()
            }],
            model_aliases: vec![("shortcut".into(), "p/custom".into())],
            ..Default::default()
        });
        assert_eq!(inventory.custom_rows.len(), 1);
        assert_eq!(inventory.custom_rows[0].source, ModelSource::Custom);
    }
}
