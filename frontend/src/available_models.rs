//! Projection of the reference `buildAvailableModels` (no persistence here).
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRow {
    pub id: String,
    pub name: String,
    pub source: Source,
    pub alias: Option<String>,
    pub free: bool,
    pub disabled: bool,
    pub context: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Catalog,
    Live,
    Custom,
    LegacyAlias,
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

fn kind(v: &Value) -> &str {
    ["kind", "type"]
        .into_iter()
        .map(|key| text(v, key))
        .find(|s| !s.is_empty())
        .unwrap_or("llm")
}

fn js_trim_empty(value: &str) -> bool {
    value.chars().all(|c| matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'))
}

// JavaScript enumerates canonical array-index keys before other strings,
// numerically, regardless of the JSON object's source order.
fn array_index(key: &str) -> Option<u32> {
    let n = key.parse::<u32>().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}

/// Custom rows come first, followed by catalog then live rows. Catalog/live
/// IDs win duplicates; an explicitly saved custom name survives that overlap.
pub fn build(
    alias: &str,
    catalog: &[Value],
    live: &[Value],
    custom: &[Value],
    aliases: &Value,
    disabled: &[String],
) -> Vec<ModelRow> {
    let mut seen = HashSet::new();
    let mut core = Vec::new();
    for (items, source) in [(catalog, Source::Catalog), (live, Source::Live)] {
        for m in items {
            let id = text(m, "id");
            if id.is_empty()
                || (source == Source::Catalog && kind(m) != "llm")
                || !seen.insert(id.to_owned())
            {
                continue;
            }
            let custom_name = custom
                .iter()
                .find(|v| {
                    text(v, "providerAlias") == alias && text(v, "id") == id && kind(v) == "llm"
                })
                .map(|v| text(v, "name"))
                .filter(|s| !js_trim_empty(s));
            let name = custom_name
                .or_else(|| m["name"].as_str().filter(|s| !s.is_empty()))
                .unwrap_or(id);
            let lower = id.to_ascii_lowercase();
            core.push(ModelRow {
                id: id.into(),
                name: name.into(),
                source: source.clone(),
                alias: None,
                free: m["isFree"] == true || lower.ends_with(":free") || lower.ends_with("-free"),
                disabled: disabled.iter().any(|v| v == id),
                context: if source == Source::Catalog {
                    m["contextWindow"]
                        .as_f64()
                        .filter(|v| v.is_finite() && *v > 0.0)
                } else {
                    None
                },
            });
        }
    }
    let mut rows = Vec::new();
    if alias != "a6api" {
        for m in custom {
            let id = text(m, "id");
            if id.is_empty()
                || text(m, "providerAlias") != alias
                || kind(m) != "llm"
                || !seen.insert(id.into())
            {
                continue;
            }
            rows.push(ModelRow {
                id: id.into(),
                name: m["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(id)
                    .into(),
                source: Source::Custom,
                alias: None,
                free: false,
                disabled: disabled.iter().any(|v| v == id),
                context: None,
            });
        }
        if let Some(aliases) = aliases.as_object() {
            let prefix = format!("{alias}/");
            let mut entries: Vec<_> = aliases.iter().collect();
            entries.sort_by_key(|(key, _)| match array_index(key) {
                Some(index) => (0, index),
                None => (1, 0),
            });
            for (key, value) in entries {
                let Some(id) = value.as_str().and_then(|v| v.strip_prefix(&prefix)) else {
                    continue;
                };
                if id.is_empty() || !seen.insert(id.into()) {
                    continue;
                }
                rows.push(ModelRow {
                    id: id.into(),
                    name: id.into(),
                    source: Source::LegacyAlias,
                    alias: Some(key.clone()),
                    free: false,
                    disabled: disabled.iter().any(|v| v == id),
                    context: None,
                });
            }
        }
    }
    rows.extend(core);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_preserves_custom_names_order_and_disabled_customs() {
        let catalog = vec![
            json!({"id":"base","name":"Official","kind":"llm","contextWindow":128000}),
            json!({"id":"image","kind":"image"}),
            json!({"id":"duplicate","name":"Catalog"}),
        ];
        let live = vec![
            json!({"id":"duplicate","name":"Live","isFree":true}),
            json!({"id":"new-free", "contextWindow":128000}),
        ];
        let custom = vec![
            json!({"providerAlias":"kc","id":"base","name":"My Name"}),
            json!({"providerAlias":"kc","id":"mine","name":"Mine"}),
            json!({"providerAlias":"other","id":"other"}),
            json!({"providerAlias":"kc","id":"image-only","kind":"image"}),
        ];
        let aliases = json!({"legacy":"kc/older", "same":"kc/mine", "other":"cx/wrong"});
        let rows = build(
            "kc",
            &catalog,
            &live,
            &custom,
            &aliases,
            &["mine".into(), "base".into()],
        );
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["mine", "older", "base", "duplicate", "new-free"]
        );
        assert_eq!(rows[0].source, Source::Custom);
        assert!(rows[0].disabled && rows[2].disabled);
        assert_eq!(rows[2].name, "My Name");
        assert_eq!(rows[2].context, Some(128000.0));
        assert!(!rows[3].free && rows[4].free);
        assert_eq!(
            rows[4].context, None,
            "live context is not projected by the reference fetcher"
        );
        assert_eq!(rows[1].alias.as_deref(), Some("legacy"));
    }

    #[test]
    fn a6api_suppresses_custom_rows() {
        let rows = build(
            "a6api",
            &[],
            &[],
            &[json!({"providerAlias":"a6api","id":"mine"})],
            &json!({"legacy":"a6api/old"}),
            &[],
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn legacy_alias_order_follows_response_and_bom_only_name_does_not_override_catalog() {
        let rows = build(
            "kc",
            &[json!({"id":"catalog", "name":"Catalog"})],
            &[],
            &[json!({"providerAlias":"kc", "id":"catalog", "name":"\u{feff}"})],
            &json!({"z-first":"kc/first", "a-second":"kc/second"}),
            &[],
        );
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["first", "second", "catalog"]
        );
        assert_eq!(rows[2].name, "Catalog");
    }

    #[test]
    fn numeric_alias_keys_follow_javascript_object_enumeration() {
        let aliases: Value = serde_json::from_str(r#"{"10":"kc/duplicate","2":"kc/duplicate","01":"kc/leading","4294967295":"kc/maximum","0":"kc/zero","plain":"kc/plain"}"#).unwrap();
        let rows = build("kc", &[], &[], &[], &aliases, &[]);
        assert_eq!(
            rows.iter()
                .map(|row| row.alias.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["0", "2", "01", "4294967295", "plain"]
        );
    }
}
