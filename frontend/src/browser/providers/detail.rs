//! Incremental provider detail: real connections and Available Models.
mod edit;
mod oauth;

use super::key_modal::KeyModal;
use super::{dashboard_request, json, json_body, text, Toast, Value};
use crate::{
    available_models::{self, ModelRow, Source},
    browser::{
        endpoint::copy_text,
        shell::{dashboard_get_no_store, notify},
        ui::{Button, CardSkeleton, Modal, Toggle, CARD_CLASS},
    },
    thinking,
};
use dioxus::prelude::*;
use std::collections::HashMap;

#[derive(Clone, Default, PartialEq)]
struct Models {
    ready: bool,
    error: bool,
    saved_ready: bool,
    catalog_ready: bool,
    catalog: Vec<Value>,
    thinking_catalog: Vec<Value>,
    live: Vec<Value>,
    custom: Vec<Value>,
    aliases: Value,
    disabled: Vec<String>,
    favorites: Vec<String>,
    free_only: bool,
    caps: Vec<Value>,
}

#[derive(Clone, Copy)]
struct ModelLoad {
    models: Signal<Models>,
    catalog_version: Signal<u64>,
    optional_version: Signal<u64>,
    free_version: Signal<u64>,
    favorites_version: Signal<u64>,
}

fn encoded(value: &str) -> String {
    js_sys::encode_uri_component(value)
        .as_string()
        .unwrap_or_default()
}

fn custom_delete_path(alias: &str, id: &str) -> String {
    // The reference uses URLSearchParams here (not encodeURIComponent):
    // spaces become `+`, and its escaping rules differ for a few other bytes.
    if let Ok(params) = web_sys::UrlSearchParams::new() {
        params.append("providerAlias", alias);
        params.append("id", id);
        params.append("type", "llm");
        let query: String = params.to_string().into();
        format!("/api/models/custom?{query}")
    } else {
        format!(
            "/api/models/custom?providerAlias={}&id={}&type=llm",
            encoded(alias),
            encoded(id)
        )
    }
}

fn js_trim(value: &str) -> String {
    js_sys::JsString::from(value)
        .trim()
        .as_string()
        .unwrap_or_default()
}

async fn get(path: &str) -> Option<Value> {
    let no_store = path == "/api/catalog"
        || path == "/api/providers"
        || path == "/api/provider-nodes"
        || path == "/api/models/custom"
        || path.starts_with("/api/models/disabled?")
        || path == "/api/providers/filters"
        || path == "/api/models/favorites"
        || path == "/api/models";
    let response = if no_store {
        dashboard_get_no_store(path).await
    } else {
        dashboard_request("GET", path, None).await
    }
    .ok()?;
    if !response.ok() {
        return None;
    }
    json_body(&response).await
}

fn array(value: &Value, key: &str) -> Vec<Value> {
    value[key].as_array().cloned().unwrap_or_default()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn storage_alias(id: &str) -> String {
    let aliases: Value =
        serde_json::from_str(include_str!("../provider_aliases.json")).unwrap_or_default();
    aliases[id].as_str().unwrap_or(id).to_owned()
}

fn thinking_options(id: &str, alias: &str, data: &Models) -> Vec<String> {
    let llm = |m: &&Value| {
        ["kind", "type"]
            .into_iter()
            .map(|key| text(m, key))
            .find(|kind| !kind.is_empty())
            .is_none_or(|kind| kind == "llm")
    };
    let catalog = data.catalog.iter().filter(llm);
    let custom = data
        .custom
        .iter()
        .filter(|m| text(m, "providerAlias") == alias && llm(m));
    let ids = catalog
        .chain(data.live.iter())
        .chain(custom)
        .map(|m| text(m, "id").to_owned());
    thinking::provider_levels(id, ids, &data.thinking_catalog)
}

fn free_only_local(alias: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| {
            s.get_item(&format!("openproxy:freeOnly:{alias}"))
                .ok()
                .flatten()
        })
        .is_some_and(|s| s == "1")
}

fn refresh_catalog(id: String, mut load: ModelLoad) {
    load.catalog_version += 1;
    let version = *load.catalog_version.peek();
    spawn(async move {
        let catalog = get("/api/catalog").await;
        if *load.catalog_version.peek() != version {
            return;
        }
        let mut models = load.models.write();
        let Some(catalog) = catalog.filter(|c| c["providerModels"].is_array()) else {
            if !models.catalog_ready {
                models.error = true;
            }
            return;
        };
        let alias = catalog["providerIdToAlias"][&id]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or(&id);
        models.catalog = catalog["providerModels"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| text(r, "alias") == alias))
            .map(|r| array(r, "models"))
            .unwrap_or_default();
        models.thinking_catalog = catalog["providerModels"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| text(r, "alias") == storage_alias(&id)))
            .map(|r| array(r, "models"))
            .unwrap_or_default();
        models.catalog_ready = true;
        models.ready = models.saved_ready;
        if models.ready {
            models.error = false;
        }
    });
}

async fn load_models(id: String, alias: String, mut load: ModelLoad) {
    load.optional_version += 1;
    let version = *load.optional_version.peek();
    let free_version = *load.free_version.peek();
    let favorites_version = *load.favorites_version.peek();
    refresh_catalog(id.clone(), load);

    // Live discovery and display metadata are independent of saved/configured
    // rows. Late responses cannot overwrite edits or a newer load.
    let live_path = if id == "kilocode" || alias == "kc" {
        Some("/api/providers/kilo/free-models".to_owned())
    } else if id == "openrouter" {
        Some(format!(
            "/api/providers/suggested-models?url={}&type=openrouter-free",
            encoded("https://openrouter.ai/api/v1/models")
        ))
    } else {
        None
    };
    if let Some(path) = live_path {
        let kilo = id == "kilocode" || alias == "kc";
        spawn(async move {
            let data = get(&path).await;
            if *load.optional_version.peek() != version {
                return;
            }
            let mut rows = data
                .as_ref()
                .map(|v| array(v, if kilo { "models" } else { "data" }))
                .unwrap_or_default();
            rows.retain(Value::is_object);
            for item in &mut rows {
                item["isFree"] = json!(kilo);
            }
            load.models.write().live = rows;
        });
    }
    {
        let alias = alias.clone();
        spawn(async move {
            let filters = get("/api/providers/filters").await;
            if *load.optional_version.peek() == version && *load.free_version.peek() == free_version
            {
                load.models.write().free_only = filters
                    .as_ref()
                    .and_then(|v| v["filters"][&alias]["freeOnly"].as_bool())
                    .unwrap_or_else(|| free_only_local(&alias));
            }
        });
    }
    {
        let alias = alias.clone();
        spawn(async move {
            let favorites = get("/api/models/favorites").await;
            if *load.optional_version.peek() == version
                && *load.favorites_version.peek() == favorites_version
                && let Some(favorites) = favorites
            {
                load.models.write().favorites = strings(&favorites["favorites"][&alias]);
            }
        });
    }
    spawn(async move {
        let caps = get("/api/models").await;
        if *load.optional_version.peek() == version
            && let Some(caps) = caps
        {
            load.models.write().caps = array(&caps, "models");
        }
    });

    let disabled_path = format!("/api/models/disabled?providerAlias={}", encoded(&alias));
    let (custom, aliases, disabled) = futures_util::join!(
        get("/api/models/custom"),
        get("/api/models/alias"),
        get(&disabled_path)
    );
    if *load.optional_version.peek() != version {
        return;
    }
    let mut models = load.models.write();
    let (Some(custom), Some(aliases), Some(disabled)) = (custom, aliases, disabled) else {
        models.error = true;
        return;
    };
    if !custom["models"].is_array()
        || !aliases["aliases"].is_object()
        || !disabled["ids"].is_array()
    {
        models.error = true;
        return;
    }
    models.custom = array(&custom, "models");
    models.aliases = aliases["aliases"].clone();
    models.disabled = strings(&disabled["ids"]);
    models.saved_ready = true;
    models.ready = models.catalog_ready;
    if models.ready {
        models.error = false;
    }
}

async fn refresh_disabled(alias: &str, mut models: Signal<Models>) {
    if let Some(data) = get(&format!(
        "/api/models/disabled?providerAlias={}",
        encoded(alias)
    ))
    .await
        && data["ids"].is_array()
    {
        models.write().disabled = strings(&data["ids"]);
    }
}

async fn refresh_custom(mut models: Signal<Models>) {
    if let Some(data) = get("/api/models/custom").await
        && data["models"].is_array()
    {
        models.write().custom = array(&data, "models");
    }
}

async fn refresh_aliases(mut models: Signal<Models>) {
    if let Some(data) = get("/api/models/alias").await
        && data["aliases"].is_object()
    {
        models.write().aliases = data["aliases"].clone();
    }
}

async fn load_connections(
    id: &str,
    mut connections: Signal<Vec<Value>>,
    mut loading: Signal<bool>,
    mut error: Signal<bool>,
    mut generation: Signal<u64>,
) {
    let current = (*generation.peek()).wrapping_add(1);
    generation.set(current);
    let result = get("/api/providers")
        .await
        .and_then(|v| v["connections"].as_array().cloned());
    if *generation.peek() != current {
        return;
    }
    match result {
        Some(items) => {
            connections.set(
                items
                    .into_iter()
                    .filter(|v| text(v, "provider") == id)
                    .collect(),
            );
            error.set(false);
        }
        None => error.set(true),
    }
    loading.set(false);
}

fn provider(id: &str) -> Option<Value> {
    let registry: Value = serde_json::from_str(include_str!("../provider_data.json")).ok()?;
    ["oauth", "apiKey", "free", "freeTier"]
        .into_iter()
        .find_map(|group| {
            registry[group]
                .as_array()?
                .iter()
                .find(|p| text(p, "id") == id)
                .cloned()
        })
}

fn context_label(value: f64) -> String {
    let (n, suffix) = if value >= 1_000_000_000.0 {
        (value / 1_000_000_000.0, "B")
    } else if value >= 1_000_000.0 {
        (value / 1_000_000.0, "M")
    } else if value >= 1000.0 {
        (value / 1000.0, "K")
    } else {
        (value, "")
    };
    let n = (n * 10.0).round() / 10.0;
    format!("{n}{suffix} ctx")
}

fn caps(id: &str, row: &ModelRow, data: &Models) -> (bool, bool) {
    if id == "xai" {
        let row = data
            .catalog
            .iter()
            .find(|m| text(m, "id") == row.id && text(m, "kind") == "llm");
        let has = |cap: &str| {
            row.and_then(|r| r["capabilities"].as_array())
                .is_some_and(|a| a.iter().any(|v| v == cap))
        };
        return (has("vision"), has("reasoning"));
    }
    let full = format!("{id}/{}", row.id);
    let entry = data
        .caps
        .iter()
        .find(|m| text(m, "fullModel") == full)
        .or_else(|| data.caps.iter().find(|m| text(m, "model") == row.id));
    if let Some(entry) = entry {
        let c = &entry["caps"];
        if c.as_object().is_some_and(|o| o.values().any(|v| v == true)) {
            return (c["vision"] == true, c["reasoning"] == true);
        }
    }
    let name = format!("{id}/{}", row.id).to_lowercase();
    if name.contains("claude")
        || name.contains("gemini")
        || name.contains("gpt-5") && !name.contains("codex")
        || name.contains("grok") && !name.contains("grok-code")
        || name.contains("kimi")
        || name.contains("minimax")
        || name.contains("deepseek-v4")
        || name.contains("qwen3.5")
        || name.contains("qwen3.6")
        || name.contains("qwen3.7")
        || name.contains("qwen")
            && (name.contains("vl") || name.contains("omni") || name.contains("plus"))
        || name.contains("glm") && name.contains('v')
        || name.contains("o1") && !name.contains("o1-mini")
        || name.contains("o3")
        || name.contains("o4")
    {
        return (true, true);
    }
    if name.contains("gemma")
        || name.contains("gpt-4o")
        || name.contains("gpt-4.1")
        || name.contains("gpt-4-turbo")
        || name.contains("pixtral")
        || name.contains("mistral") && name.contains("small")
        || name.contains("vision")
        || name.contains("-vl")
    {
        return (true, false);
    }
    if [
        "gpt-5",
        "gpt-oss",
        "o1-mini",
        "grok-code",
        "qwen",
        "qwq",
        "glm",
        "deepseek",
        "reasoning",
    ]
    .iter()
    .any(|s| name.contains(s))
    {
        return (false, true);
    }
    (false, false)
}

#[derive(Clone)]
enum Action {
    Disable(String),
    Restore(String),
    Remove(ModelRow),
    Favorite(String),
    Test(String),
    Copy(String, String),
}

#[derive(Clone)]
enum ConnectionAction {
    Toggle(String, bool),
    Delete(String),
    BulkDelete(Vec<String>),
    Swap(usize, usize),
    SetProxy(String, Option<String>),
    ApplyProxy(Option<String>, bool),
    TestAll,
    StopTests,
}

#[derive(Clone, Default, PartialEq)]
struct ModelTest {
    running: bool,
    status: Option<bool>,
    error: String,
}

#[derive(Clone, Default, PartialEq)]
struct ConnectionTests {
    running: bool,
    stopping: bool,
    current: Option<String>,
    results: HashMap<String, (String, String)>,
    total: usize,
    completed: usize,
    passed: usize,
    failed: usize,
    stopped: bool,
}

#[component]
pub(in crate::browser) fn ProviderDetailPage(id: String) -> Element {
    let compatible =
        id.starts_with("openai-compatible-") || id.starts_with("anthropic-compatible-");
    let anthropic = id.starts_with("anthropic-compatible-");
    let alias = storage_alias(&id);
    let mut models = use_signal(Models::default);
    let mut thinking_mode = use_signal(|| "auto".to_owned());
    let mut load = ModelLoad {
        models,
        catalog_version: use_signal(|| 0),
        optional_version: use_signal(|| 0),
        free_version: use_signal(|| 0),
        favorites_version: use_signal(|| 0),
    };
    let mut connections = use_signal(Vec::<Value>::new);
    let mut pools = use_signal(Vec::<Value>::new);
    let mut selected = use_signal(Vec::<String>::new);
    let mut bulk_delete = use_signal(|| false);
    let mut bulk_deleting = use_signal(|| false);
    let mut proxy_open = use_signal(|| false);
    let mut proxy_busy = use_signal(|| false);
    let mut connection_tests = use_signal(ConnectionTests::default);
    let mut node = use_signal(|| None::<Value>);
    let mut node_loading = use_signal(|| compatible);
    let mut node_error = use_signal(|| false);
    let loading = use_signal(|| true);
    let connection_error = use_signal(|| false);
    let connection_generation = use_signal(|| 0_u64);
    let mut image_error = use_signal(|| false);
    let mut add = use_signal(|| false);
    let mut model_id = use_signal(String::new);
    let mut saving = use_signal(|| false);
    let mut modal_error = use_signal(String::new);
    let mut modal_test_status = use_signal(String::new);
    let mut modal_test_error = use_signal(String::new);
    let mut modal_test_generation = use_signal(|| 0_u64);
    let mut disable_all = use_signal(|| None::<Vec<String>>);
    let mut delete_connection = use_signal(|| None::<Value>);
    let edit_connection = use_signal(|| None::<Value>);
    let mut add_key = use_signal(|| None::<String>);
    let mut oauth_open = use_signal(|| None::<u64>);
    let mut oauth_generation = use_signal(|| 0_u64);
    let mut deleting = use_signal(|| false);
    let mut results = use_signal(HashMap::<String, ModelTest>::new);
    let mut models_test_error = use_signal(String::new);
    let copied = use_signal(String::new);
    let generation = use_signal(|| 0_u32);
    let toasts = use_context::<Signal<Vec<Toast>>>();
    {
        let id = id.clone();
        let alias = alias.clone();
        use_effect(move || {
            if id == "xai" && !thinking_options(&id, &alias, &models()).contains(&thinking_mode()) {
                thinking_mode.set("auto".into());
            }
        });
    }
    {
        let id = id.clone();
        use_future(move || {
            let id = id.clone();
            async move {
                load_connections(
                    &id,
                    connections,
                    loading,
                    connection_error,
                    connection_generation,
                )
                .await;
            }
        });
    }
    use_future(move || async move {
        if let Some(data) = get("/api/proxy-pools?isActive=true").await
            && data["proxyPools"].is_array()
        {
            pools.set(array(&data, "proxyPools"));
        }
    });
    let refresh_after_connection = {
        let id = id.clone();
        use_callback(move |()| {
            let id = id.clone();
            if matches!(id.as_str(), "codex" | "commandcode" | "a6api" | "xai") {
                refresh_catalog(id.clone(), load);
            }
            spawn(async move {
                load_connections(
                    &id,
                    connections,
                    loading,
                    connection_error,
                    connection_generation,
                )
                .await;
            });
        })
    };
    let open_oauth = use_callback(move |()| {
        let next = (*oauth_generation.peek()).wrapping_add(1);
        oauth_generation.set(next);
        oauth_open.set(Some(next));
    });
    let oauth_request = use_callback(move |work: oauth::OwnedRequest| {
        spawn(async move {
            let response = dashboard_request(work.method, &work.path, work.body.as_ref()).await;
            let result = match response {
                Ok(response) => {
                    let ok = response.ok();
                    let data = json_body(&response).await.unwrap_or_default();
                    if ok || matches!(work.completion, oauth::Completion::DevicePoll) {
                        Ok(data)
                    } else {
                        Err(data["error"]
                            .as_str()
                            .unwrap_or("Authentication failed")
                            .to_owned())
                    }
                }
                Err(_) => Err("Network error".into()),
            };
            let success = result.as_ref().is_ok_and(|data| match work.completion {
                oauth::Completion::Exchange => true,
                oauth::Completion::DevicePoll => data["success"] == true,
                oauth::Completion::ProxyStatus => data["status"] == "done",
            });
            if success {
                refresh_after_connection.call(());
                if *oauth_open.peek() == Some(work.attempt) {
                    oauth_open.set(None);
                }
            }
            let _ = work.reply.send(result);
        });
    });
    // These callbacks own their tasks in the detail page, not a removable
    // row or dismissible modal. A late 401 can still recheck the session.
    let catalog_provider = id.clone();
    let connection_action = use_callback(move |action: ConnectionAction| match action {
        ConnectionAction::Toggle(id, next) => {
            let provider_id = catalog_provider.clone();
            spawn(async move {
                let path = format!("/api/providers/{}", encoded(&id));
                if let Ok(response) =
                    dashboard_request("PUT", &path, Some(&json!({"isActive":next}))).await
                    && response.ok()
                {
                    for conn in connections.write().iter_mut() {
                        if text(conn, "id") == id {
                            conn["isActive"] = json!(next);
                        }
                    }
                    if provider_id == "xai" {
                        load_connections(
                            &provider_id,
                            connections,
                            loading,
                            connection_error,
                            connection_generation,
                        )
                        .await;
                    }
                    if matches!(
                        provider_id.as_str(),
                        "codex" | "commandcode" | "a6api" | "xai"
                    ) {
                        refresh_catalog(provider_id, load);
                    }
                }
            });
        }
        ConnectionAction::Delete(id) => {
            if deleting() {
                return;
            }
            deleting.set(true);
            let provider_id = catalog_provider.clone();
            spawn(async move {
                let ok =
                    dashboard_request("DELETE", &format!("/api/providers/{}", encoded(&id)), None)
                        .await
                        .is_ok_and(|r| r.ok());
                if ok {
                    connections.write().retain(|c| text(c, "id") != id);
                    selected.write().retain(|s| s != &id);
                    if matches!(
                        provider_id.as_str(),
                        "codex" | "commandcode" | "a6api" | "xai"
                    ) {
                        refresh_catalog(provider_id, load);
                    }
                    notify(toasts, "Connection deleted", false);
                } else {
                    notify(toasts, "Failed to delete connection", true);
                }
                deleting.set(false);
                if delete_connection
                    .peek()
                    .as_ref()
                    .is_some_and(|conn| text(conn, "id") == id)
                {
                    delete_connection.set(None);
                }
            });
        }
        ConnectionAction::BulkDelete(ids) => {
            if ids.is_empty() || bulk_deleting() {
                return;
            }
            bulk_deleting.set(true);
            let provider_id = catalog_provider.clone();
            spawn(async move {
                let mut deleted = Vec::new();
                for id in &ids {
                    if dashboard_request("DELETE", &format!("/api/providers/{}", encoded(id)), None)
                        .await
                        .is_ok_and(|response| response.ok())
                    {
                        deleted.push(id.clone());
                    }
                }
                connections
                    .write()
                    .retain(|c| !deleted.iter().any(|id| id == text(c, "id")));
                selected.write().retain(|id| !deleted.contains(id));
                if !deleted.is_empty()
                    && matches!(
                        provider_id.as_str(),
                        "codex" | "commandcode" | "a6api" | "xai"
                    )
                {
                    refresh_catalog(provider_id, load);
                }
                let failed = ids.len() - deleted.len();
                if failed > 0 {
                    notify(
                        toasts,
                        &format!("Deleted {} connection(s), {failed} failed.", deleted.len()),
                        true,
                    );
                } else {
                    notify(
                        toasts,
                        &format!(
                            "Deleted {} connection{}",
                            deleted.len(),
                            if deleted.len() == 1 { "" } else { "s" }
                        ),
                        false,
                    );
                }
                bulk_deleting.set(false);
                bulk_delete.set(false);
            });
        }
        ConnectionAction::Swap(first, second) => {
            let mut list = connections.peek().clone();
            if first >= list.len() || second >= list.len() || first == second {
                return;
            }
            list.swap(first, second);
            let first_id = text(&list[first], "id").to_owned();
            let second_id = text(&list[second], "id").to_owned();
            connections.set(list);
            let provider_id = catalog_provider.clone();
            spawn(async move {
                let first_path = format!("/api/providers/{}", encoded(&first_id));
                let second_path = format!("/api/providers/{}", encoded(&second_id));
                let first_body = json!({"priority":first});
                let second_body = json!({"priority":second});
                let (a, b) = futures_util::join!(
                    dashboard_request("PUT", &first_path, Some(&first_body)),
                    dashboard_request("PUT", &second_path, Some(&second_body))
                );
                if a.is_err() || b.is_err() {
                    load_connections(
                        &provider_id,
                        connections,
                        loading,
                        connection_error,
                        connection_generation,
                    )
                    .await;
                }
            });
        }
        ConnectionAction::SetProxy(id, pool) => {
            let provider_id = catalog_provider.clone();
            spawn(async move {
                if dashboard_request(
                    "PUT",
                    &format!("/api/providers/{}", encoded(&id)),
                    Some(&json!({"proxyPoolId":pool})),
                )
                .await
                .is_ok_and(|r| r.ok())
                {
                    for conn in connections.write().iter_mut() {
                        if text(conn, "id") == id {
                            if !conn["providerSpecificData"].is_object() {
                                conn["providerSpecificData"] = json!({});
                            }
                            conn["providerSpecificData"]["proxyPoolId"] = json!(pool);
                        }
                    }
                    if provider_id == "xai" {
                        refresh_catalog(provider_id, load);
                    }
                }
            });
        }
        ConnectionAction::ApplyProxy(pool, rotate) => {
            if proxy_busy() {
                return;
            }
            let ids = connections
                .peek()
                .iter()
                .map(|c| text(c, "id").to_owned())
                .collect::<Vec<_>>();
            let active = pools
                .peek()
                .iter()
                .filter(|p| p["isActive"] == true)
                .map(|p| text(p, "id").to_owned())
                .collect::<Vec<_>>();
            if rotate && active.is_empty() {
                notify(toasts, "No active proxy pools available.", true);
                return;
            }
            proxy_busy.set(true);
            let provider_id = catalog_provider.clone();
            spawn(async move {
                let mut failed = 0;
                for (i, id) in ids.iter().enumerate() {
                    let target = if rotate {
                        Some(active[i % active.len()].clone())
                    } else {
                        pool.clone()
                    };
                    if !dashboard_request(
                        "PUT",
                        &format!("/api/providers/{}", encoded(id)),
                        Some(&json!({"proxyPoolId":target})),
                    )
                    .await
                    .is_ok_and(|r| r.ok())
                    {
                        failed += 1;
                    }
                }
                if failed > 0 {
                    notify(
                        toasts,
                        &format!("Updated with {failed} failed request(s)."),
                        true,
                    );
                }
                load_connections(
                    &provider_id,
                    connections,
                    loading,
                    connection_error,
                    connection_generation,
                )
                .await;
                if provider_id == "xai" {
                    refresh_catalog(provider_id, load);
                }
                proxy_busy.set(false);
                proxy_open.set(false);
            });
        }
        ConnectionAction::TestAll => {
            if connection_tests().running || connections().is_empty() {
                return;
            }
            let ids = connections
                .peek()
                .iter()
                .map(|c| text(c, "id").to_owned())
                .collect::<Vec<_>>();
            connection_tests.set(ConnectionTests {
                running: true,
                total: ids.len(),
                results: ids
                    .iter()
                    .map(|id| (id.clone(), ("queued".into(), String::new())))
                    .collect(),
                ..Default::default()
            });
            let provider_id = catalog_provider.clone();
            spawn(async move {
                for (index, id) in ids.iter().enumerate() {
                    if connection_tests.peek().stopping {
                        connection_tests.write().stopped = true;
                        break;
                    }
                    {
                        let mut state = connection_tests.write();
                        state.current = Some(id.clone());
                        state
                            .results
                            .insert(id.clone(), ("testing".into(), String::new()));
                    }
                    let result = dashboard_request(
                        "POST",
                        &format!("/api/providers/{}/test", encoded(id)),
                        None,
                    )
                    .await;
                    let data = match result {
                        Ok(response) => json_body(&response).await,
                        Err(_) => None,
                    };
                    let valid = data.as_ref().is_some_and(|v| v["valid"] == true);
                    let error = data
                        .as_ref()
                        .and_then(|v| v["error"].as_str())
                        .unwrap_or(if data.is_none() { "Test failed" } else { "" })
                        .to_owned();
                    {
                        let mut state = connection_tests.write();
                        state.completed = index + 1;
                        if valid {
                            state.passed += 1;
                        } else {
                            state.failed += 1;
                        }
                        state.results.insert(
                            id.clone(),
                            (if valid { "success" } else { "failed" }.into(), error),
                        );
                    }
                    if index + 1 < ids.len() {
                        gloo_timers::future::TimeoutFuture::new(1000).await;
                    }
                }
                {
                    let mut state = connection_tests.write();
                    state.running = false;
                    state.stopping = false;
                    state.current = None;
                }
                load_connections(
                    &provider_id,
                    connections,
                    loading,
                    connection_error,
                    connection_generation,
                )
                .await;
                if matches!(provider_id.as_str(), "commandcode" | "a6api") {
                    refresh_catalog(provider_id, load);
                }
            });
        }
        ConnectionAction::StopTests => {
            if connection_tests.peek().running {
                connection_tests.write().stopping = true;
            }
        }
    });
    let modal_test = {
        let alias = alias.clone();
        use_callback(move |id: String| {
            if id.is_empty() || modal_test_status() == "testing" {
                return;
            }
            let alias = alias.clone();
            let generation = *modal_test_generation.peek();
            modal_test_status.set("testing".into());
            modal_test_error.set(String::new());
            spawn(async move {
                let result = match dashboard_request(
                    "POST",
                    "/api/models/test",
                    Some(&json!({"model":format!("{alias}/{id}")})),
                )
                .await
                {
                    Ok(response) => {
                        let data = json_body(&response).await.unwrap_or_default();
                        (
                            if data["ok"] == true { "ok" } else { "error" }.to_owned(),
                            text(&data, "error").to_owned(),
                        )
                    }
                    Err(_) => ("error".into(), "Network error".into()),
                };
                if *modal_test_generation.peek() == generation {
                    modal_test_status.set(result.0);
                    modal_test_error.set(result.1);
                }
            });
        })
    };
    let save_model = {
        let alias = alias.clone();
        use_callback(move |id: String| {
            if id.is_empty() || saving() {
                return;
            }
            let alias = alias.clone();
            saving.set(true);
            spawn(async move {
                match dashboard_request(
                    "POST",
                    "/api/models/custom",
                    Some(&json!({"providerAlias":alias,"id":id,"type":"llm"})),
                )
                .await
                {
                    Ok(response) if response.ok() => {
                        refresh_custom(models).await;
                        add.set(false);
                    }
                    Ok(response) => {
                        let data = json_body(&response).await.unwrap_or_default();
                        let error = data["error"]
                            .as_str()
                            .unwrap_or("Failed to add custom model");
                        modal_error.set(error.to_owned());
                        notify(toasts, error, true);
                    }
                    Err(_) => modal_error.set("Failed to add custom model".into()),
                }
                saving.set(false);
            });
        })
    };
    {
        let id = id.clone();
        let alias = alias.clone();
        use_future(move || {
            let id = id.clone();
            let alias = alias.clone();
            async move {
                load_models(id, alias, load).await;
            }
        });
    }
    {
        let id = id.clone();
        use_future(move || {
            let id = id.clone();
            async move {
                if compatible {
                    match get("/api/provider-nodes")
                        .await
                        .and_then(|v| v["nodes"].as_array().cloned())
                    {
                        Some(items) => node.set(items.into_iter().find(|v| text(v, "id") == id)),
                        None => node_error.set(true),
                    }
                    node_loading.set(false);
                }
            }
        });
    }
    let action = {
        let alias = alias.clone();
        use_callback(move |action: Action| {
            let alias = alias.clone();
            match action {
                Action::Copy(id, full_model) => {
                    copy_text(full_model, format!("model-{id}"), copied, generation)
                }
                Action::Favorite(id) => {
                    load.favorites_version += 1;
                    let old = models.peek().favorites.clone();
                    let mut next = old.clone();
                    if next.contains(&id) {
                        next.retain(|v| v != &id);
                    } else {
                        next.push(id);
                    }
                    models.write().favorites = next.clone();
                    spawn(async move {
                        if dashboard_request(
                            "PUT",
                            "/api/models/favorites",
                            Some(&json!({"alias":alias,"favorites":next})),
                        )
                        .await
                        .is_err()
                        {
                            models.write().favorites = old;
                        }
                    });
                }
                Action::Disable(id) => {
                    spawn(async move {
                        if dashboard_request(
                            "POST",
                            "/api/models/disabled",
                            Some(&json!({"providerAlias":alias,"ids":[id]})),
                        )
                        .await
                        .is_ok()
                        {
                            refresh_disabled(&alias, models).await;
                        }
                    });
                }
                Action::Restore(id) => {
                    spawn(async move {
                        let path = format!(
                            "/api/models/disabled?providerAlias={}&id={}",
                            encoded(&alias),
                            encoded(&id)
                        );
                        if dashboard_request("DELETE", &path, None).await.is_ok() {
                            refresh_disabled(&alias, models).await;
                        }
                    });
                }
                Action::Remove(row) => {
                    spawn(async move {
                        let path = if row.source == Source::LegacyAlias {
                            format!(
                                "/api/models/alias?alias={}",
                                encoded(row.alias.as_deref().unwrap_or_default())
                            )
                        } else {
                            custom_delete_path(&alias, &row.id)
                        };
                        if let Ok(response) = dashboard_request("DELETE", &path, None).await
                            && response.ok()
                        {
                            if row.source == Source::LegacyAlias {
                                refresh_aliases(models).await;
                            } else {
                                refresh_custom(models).await;
                            }
                        }
                    });
                }
                Action::Test(id) => {
                    if results.peek().get(&id).is_some_and(|result| result.running) {
                        return;
                    }
                    results.write().entry(id.clone()).or_default().running = true;
                    spawn(async move {
                        let (status, error) = if let Ok(response) = dashboard_request(
                            "POST",
                            "/api/models/test",
                            Some(&json!({"model":format!("{alias}/{id}")})),
                        )
                        .await
                        {
                            if let Some(data) = json_body(&response).await {
                                if data["ok"] == true {
                                    (true, String::new())
                                } else {
                                    (
                                        false,
                                        data["error"]
                                            .as_str()
                                            .filter(|s| !s.is_empty())
                                            .unwrap_or("Model not reachable")
                                            .to_owned(),
                                    )
                                }
                            } else {
                                (false, "Network error".to_owned())
                            }
                        } else {
                            (false, "Network error".to_owned())
                        };
                        if !compatible {
                            models_test_error.set(error.clone());
                        }
                        results.write().insert(
                            id,
                            ModelTest {
                                running: false,
                                status: Some(status),
                                error,
                            },
                        );
                    });
                }
            }
        })
    };
    if loading() || node_loading() {
        return rsx! { div { class: "flex flex-col gap-8", CardSkeleton {} CardSkeleton {} } };
    }
    if node_error() {
        return rsx! { div { role: "alert", class: "text-center py-20", "Compatible provider could not be loaded." a { href: "/dashboard/providers", class: "text-primary mt-4 inline-block", "Back to Providers" } } };
    }
    let current_node = node();
    let info = if compatible {
        current_node.as_ref().map(|node| json!({
        "id":id,
        "name":node["name"].as_str().filter(|v| !v.is_empty()).unwrap_or(if anthropic { "Anthropic Compatible" } else { "OpenAI Compatible" }),
        "color":if anthropic { "#D97757" } else { "#10A37F" },
        "textIcon":if anthropic { "AC" } else { "OC" }
    }))
    } else {
        provider(&id)
    };
    let Some(info) = info else {
        return rsx! { div { class: "text-center py-20", p { class: "text-text-muted", "Provider not found" } a { href: "/dashboard/providers", class: "text-primary mt-4 inline-block", "Back to Providers" } } };
    };
    let name = text(&info, "name").to_owned();
    let color = text(&info, "color").to_owned();
    let icon_text = info["textIcon"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| id.chars().take(2).collect::<String>().to_uppercase());
    let sites: Value =
        serde_json::from_str(include_str!("../provider_sites.json")).unwrap_or_default();
    let metadata: Value =
        serde_json::from_str(include_str!("../provider_detail_metadata.json")).unwrap_or_default();
    let details = metadata[&id].clone();
    let site = sites[&id]["url"].as_str().map(str::to_owned);
    let site_label = if sites[&id]["kind"] == "key" {
        "Get API Key"
    } else {
        "Sign up / Learn more"
    };
    let is_no_auth = info["noAuth"] == true;
    let registry: Value =
        serde_json::from_str(include_str!("../provider_data.json")).unwrap_or_default();
    let auth_modes: Value =
        serde_json::from_str(include_str!("../provider_auth_modes.json")).unwrap_or_default();
    let in_group = |group: &str| {
        registry[group]
            .as_array()
            .is_some_and(|items| items.iter().any(|v| text(v, "id") == id))
    };
    let has_mode = |mode: &str| {
        auth_modes[&id]
            .as_array()
            .is_some_and(|modes| modes.iter().any(|v| v == mode))
    };
    let has_oauth = in_group("oauth") || in_group("free") || has_mode("oauth");
    let has_api = compatible
        || in_group("apiKey")
        || in_group("freeTier")
        || has_mode("apikey")
        || has_mode("cookie");
    let display_alias = if compatible {
        current_node
            .as_ref()
            .and_then(|n| n["prefix"].as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&id)
            .to_owned()
    } else {
        alias.clone()
    };
    let icon_path = if anthropic {
        "/providers/anthropic-m.png".to_owned()
    } else if compatible {
        if current_node
            .as_ref()
            .is_some_and(|n| text(n, "apiType") == "responses")
        {
            "/providers/oai-r.png".to_owned()
        } else {
            "/providers/oai-cc.png".to_owned()
        }
    } else {
        format!("/providers/{id}.png")
    };
    let data = models();
    let levels = thinking_options(&id, &alias, &data);
    let rows = available_models::build(
        &alias,
        if compatible { &[] } else { &data.catalog },
        if compatible { &[] } else { &data.live },
        &data.custom,
        &data.aliases,
        if compatible { &[] } else { &data.disabled },
    );
    let active: Vec<ModelRow> = rows
        .iter()
        .filter(|r| {
            !r.disabled
                && (matches!(r.source, Source::Custom | Source::LegacyAlias)
                    || !data.free_only
                    || r.free)
        })
        .cloned()
        .collect();
    let disabled: Vec<ModelRow> = rows.iter().filter(|r| r.disabled).cloned().collect();
    let has_free = rows.iter().any(|r| r.free);
    let connection_count = connections().len();
    let selected_ids: Vec<String> = selected()
        .into_iter()
        .filter(|id| connections().iter().any(|c| text(c, "id") == id))
        .collect();
    let all_selected = connection_count > 0 && selected_ids.len() == connection_count;
    let tests = connection_tests();
    let id_for_retry = id.clone();
    let alias_for_retry = alias.clone();
    rsx! {
        div { class: "flex min-w-0 flex-col gap-6 px-1 sm:gap-8 sm:px-0",
            div { class: "min-w-0",
                a { href: "/dashboard/providers", class: "inline-flex items-center gap-1 text-sm text-text-muted hover:text-primary transition-colors mb-4", span { class: "material-symbols-outlined text-lg", "arrow_back" } "Back to Providers" }
                div { class: "flex min-w-0 items-center gap-3 sm:gap-4",
                    div { class: "flex size-12 shrink-0 items-center justify-center rounded-lg", style: "background-color: {color}15",
                        if image_error() { span { class: "text-sm font-bold", style: "color: {color}", "{icon_text}" } } else { img { src: "{icon_path}", alt: "{name}", width: "48", height: "48", sizes: "48px", class: "max-h-12 max-w-12 rounded-lg object-contain", onerror: move |_| image_error.set(true) } }
                    }
                    div { class: "min-w-0",
                        div { class: "flex items-center gap-3 flex-wrap", h1 { class: "truncate text-2xl font-semibold tracking-tight sm:text-3xl", "{name}" }
                            if let Some(site) = site { a { href: "{site}", target: "_blank", rel: "noopener noreferrer", class: "text-xs text-primary hover:underline inline-flex items-center gap-1", span { class: "material-symbols-outlined text-sm", "open_in_new" } "{site_label}" } }
                        }
                        p { class: "text-text-muted", "{connection_count}" " connection" if connection_count != 1 { "s" } }
                    }
                }
            }
            if let Some(notice) = details["noticeText"].as_str().filter(|s| !s.is_empty()) { div { class: "flex flex-col gap-2 rounded-lg border border-blue-500/30 bg-blue-500/10 px-3 py-2 sm:flex-row sm:items-center", span { class: "material-symbols-outlined text-[16px] text-blue-500 shrink-0", "info" } p { class: "min-w-0 flex-1 text-xs leading-relaxed text-blue-600 dark:text-blue-400", "{notice}" } if let Some(url) = details["noticeUrl"].as_str() { a { href: "{url}", target: "_blank", rel: "noopener noreferrer", class: "inline-flex justify-center rounded bg-blue-500 px-2 py-1 text-xs font-medium text-white transition-colors hover:bg-blue-600 sm:py-0.5", "Get API Key →" } } } }
            if details["freeTierInfo"].is_object() { FreeTier { info: details["freeTierInfo"].clone() } }
            if let Some(node) = current_node { div { class: CARD_CLASS,
                div { class: "mb-4 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between", div { class: "min-w-0", h2 { class: "text-lg font-semibold", if anthropic { "Anthropic Compatible Details" } else { "OpenAI Compatible Details" } }
                    p { class: "break-all text-sm text-text-muted", if anthropic { "Messages API" } else if text(&node, "apiType") == "responses" { "Responses API" } else { "Chat Completions" } " · " "{text(&node, \"baseUrl\").trim_end_matches('/')}" "/" if anthropic { "messages" } else if text(&node, "apiType") == "responses" { "responses" } else { "chat/completions" } }
                }
                    Button { label: "Add API Key".to_owned(), small: true, icon: "add".to_owned(), disabled: connection_count > 0, extra_class: "w-full sm:w-auto", onclick: { let id = id.clone(); move |_| add_key.set(Some(id.clone())) } }
                }
                if connection_count > 0 { p { class: "text-sm text-text-muted", "Only one connection is allowed per compatible node. Add another node if you need more connections." } }
            } }
            if is_no_auth {
                div { class: CARD_CLASS, div { class: "flex items-center gap-3 mb-4", div { class: "inline-flex items-center justify-center w-10 h-10 rounded-full bg-green-500/10 text-green-500", span { class: "material-symbols-outlined text-[20px]", "lock_open" } } div { class: "flex-1", p { class: "text-sm font-medium", "No authentication required" } p { class: "text-xs text-text-muted", "This provider is ready to use without credentials." } } } }
            } else {
                div { class: CARD_CLASS,
                    div { class: "mb-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between", h2 { class: "text-lg font-semibold", "Connections" }
                        div { class: "flex flex-col gap-3 sm:flex-row sm:items-center sm:gap-4",
                            if connection_count > 0 && !pools().is_empty() { Button { label: "Apply Proxy".to_owned(), small: true, variant: "secondary", icon: "lan".to_owned(), onclick: move |_| proxy_open.set(true) } }
                            if !selected_ids.is_empty() { Button { label: format!("Delete Selected ({})", selected_ids.len()), small: true, variant: "danger", icon: "delete".to_owned(), onclick: move |_| bulk_delete.set(true) } }
                            if connection_count > 0 {
                                Button { label: if tests.running { "Testing Connection One-by-One...".to_owned() } else { "Test Connection One-by-One".to_owned() }, small: true, variant: "secondary", icon: "sync".to_owned(), disabled: tests.running, onclick: move |_| connection_action.call(ConnectionAction::TestAll) }
                                if tests.running { Button { label: if tests.stopping { "Stopping...".to_owned() } else { "Stop".to_owned() }, small: true, variant: "ghost", icon: "stop".to_owned(), disabled: tests.stopping, onclick: move |_| connection_action.call(ConnectionAction::StopTests) } }
                            }
                            div { class: "flex items-center gap-2", span { class: "text-xs text-text-muted font-medium", "Thinking" } select { class: "text-xs px-2 py-1 border border-border rounded-md bg-background focus:outline-none focus:border-primary", title: "Appends (level) suffix to copied model names", value: "{thinking_mode}", onchange: move |e| thinking_mode.set(e.value()), for level in levels.clone() { option { value: "{level}", "{level.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default()}{level.chars().skip(1).collect::<String>()}" } } } }
                        }
                    }
                    if connection_error() { p { class: "text-sm text-text-muted", "Connections could not be loaded." } Button { label: "Retry".to_owned(), small: true, variant: "secondary", onclick: { let id = id.clone(); move |_| { let id = id.clone(); spawn(async move { load_connections(&id, connections, loading, connection_error, connection_generation).await; }); } } } }
                    else if connection_count == 0 { div { class: "flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between", div { class: "flex items-center gap-3", div { class: "inline-flex items-center justify-center w-9 h-9 rounded-full bg-primary/10 text-primary shrink-0", span { class: "material-symbols-outlined text-[18px]", if has_oauth { "lock" } else { "key" } } } div { class: "min-w-0", p { class: "text-sm text-text-muted", "No connections yet" } if has_oauth && has_api { p { class: "text-xs text-text-muted", "Choose " if id == "xai" { "Grok Build OAuth" } else { "OAuth" } " or " if id == "xai" { "xAI API Key" } else { "API Key" } "." } } } }
                        AddButtons { provider_id: id.clone(), has_oauth, has_api, compatible, connected: false, add_key, open_oauth }
                    } }
                    else {
                        if tests.total > 0 { div { class: "mb-4 rounded-lg border border-black/10 bg-black/[0.02] px-3 py-2 text-xs text-text-muted dark:border-white/10 dark:bg-white/[0.03]", div { class: "flex flex-wrap items-center gap-3", span { "Total: {tests.total}" } span { "Completed: {tests.completed}" } span { "Passed: {tests.passed}" } span { "Failed: {tests.failed}" } if tests.stopped { span { class: "text-amber-600 dark:text-amber-400", "Stopped" } } if let Some(current) = tests.current.clone() { span { "Running: " "{connections().iter().find(|c| text(c, \"id\") == current).and_then(|c| c[\"name\"].as_str()).unwrap_or(&current)}" } } } } }
                        div { class: "mb-3 flex items-center gap-2 border-b border-black/[0.03] pb-2 dark:border-white/[0.03]", label { class: "flex cursor-pointer items-center gap-1.5 text-xs text-text-muted hover:text-primary", input { r#type: "checkbox", checked: all_selected, class: "h-3.5 w-3.5 rounded border-gray-300 text-primary focus:ring-primary", onchange: move |_| { if all_selected { selected.set(Vec::new()); } else { selected.set(connections().iter().map(|c| text(c, "id").to_owned()).collect()); } } } "Select All" } }
                        div { class: "flex min-w-0 flex-col divide-y divide-black/[0.03] dark:divide-white/[0.03]",
                            for (index, conn) in connections().into_iter().enumerate() { Connection { key: "{text(&conn, \"id\")}", selected: selected_ids.contains(&text(&conn, "id").to_owned()), index, count: connection_count, is_oauth: has_oauth, delete_connection, edit_connection, selection: selected, pools: pools(), test: tests.results.get(text(&conn, "id")).cloned(), action: connection_action, conn } }
                        }
                        if !compatible { AddButtons { provider_id: id.clone(), has_oauth, has_api, compatible, connected: true, add_key, open_oauth } }
                    }
                }
            }
            div { class: CARD_CLASS,
                div { class: "mb-4 flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between",
                    div { class: "flex items-center gap-3", h2 { class: "text-lg font-semibold", "Available Models" }
                        select { class: "rounded-md border border-border bg-background px-2 py-1 text-xs focus:border-primary focus:outline-none", value: "{thinking_mode}", onchange: move |e| thinking_mode.set(e.value()), for level in levels.clone() { option { value: "{level}", if level == "auto" { "Thinking: Auto" } else { "Thinking: " "{level.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default()}{level.chars().skip(1).collect::<String>()}" } } } }
                    }
                    if data.ready && !compatible { div { class: "flex flex-wrap items-center gap-2",
                        if has_free && id != "opencode-zen" {
                            button { r#type: "button", title: "Show only free models", class: if data.free_only { "inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium transition-colors border-primary bg-primary/10 text-primary" } else { "inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium transition-colors border-border bg-surface text-text-muted hover:border-primary/50 hover:text-primary" }, onclick: { let alias = alias.clone(); move |_| {
                                let next = !models.peek().free_only; load.free_version += 1; models.write().free_only = next;
                                if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) { let _ = storage.set_item(&format!("openproxy:freeOnly:{alias}"), if next { "1" } else { "0" }); }
                                let alias = alias.clone(); spawn(async move { let _ = dashboard_request("PUT", "/api/providers/filters", Some(&json!({"alias":alias,"freeOnly":next}))).await; });
                            } }, span { class: "material-symbols-outlined text-[13px]", "sell" } "Free only" }
                        }
                        if !disabled.is_empty() { Button { label: "Active All".to_owned(), small: true, variant: "secondary", icon: "restart_alt".to_owned(), onclick: { let alias = alias.clone(); move |_| { let alias = alias.clone(); spawn(async move { let path = format!("/api/models/disabled?providerAlias={}", encoded(&alias)); if dashboard_request("DELETE", &path, None).await.is_ok() { refresh_disabled(&alias, models).await; } }); } } } }
                        if !active.is_empty() { Button { label: "Disable All".to_owned(), small: true, variant: "secondary", icon: "block".to_owned(), onclick: { let ids = active.iter().map(|r| r.id.clone()).collect::<Vec<_>>(); move |_| disable_all.set(Some(ids.clone())) } } }
                    } }
                }
                if !models_test_error().is_empty() { p { class: "text-xs text-red-500 mb-3 break-words", "{models_test_error}" } }
                if data.error { div { role: "alert", class: "text-sm text-text-muted", "Unable to load available models. Saved configuration was not changed." Button { label: "Retry".to_owned(), small: true, variant: "secondary", onclick: move |_| { models.write().error = false; let id = id_for_retry.clone(); let alias = alias_for_retry.clone(); spawn(async move { load_models(id, alias, load).await; }); } } } }
                else if !data.ready { p { class: "text-sm text-text-muted", "Loading models..." } }
                else if compatible { CompatibleModels { alias: alias.clone(), display_alias: display_alias.clone(), anthropic, rows: active, models, copied: copied(), results: results(), can_test: connection_count > 0, action, toasts } }
                else { div { class: "flex flex-wrap gap-3",
                    for row in active { ModelChip { key: "{row.id}", row, provider_id: id.clone(), alias: alias.clone(), models: data.clone(), thinking_mode: thinking_mode(), results: results(), copied: copied(), can_test: connection_count > 0 || is_no_auth, action } }
                    if id != "a6api" { button { class: "flex w-full items-center justify-center gap-1.5 rounded-lg border border-dashed border-primary/40 px-3 py-2 text-xs text-primary transition-colors hover:border-primary hover:bg-primary/5 sm:w-auto", onclick: move |_| { model_id.set(String::new()); modal_error.set(String::new()); modal_test_status.set(String::new()); modal_test_error.set(String::new()); modal_test_generation += 1; add.set(true); }, span { class: "material-symbols-outlined text-sm", "add" } "Add Model" } }
                    if !disabled.is_empty() { div { class: "w-full mt-2", p { class: "text-xs text-text-muted mb-2", "Disabled models (" "{disabled.len()}" "):" } div { class: "flex flex-wrap gap-2", for row in disabled { button { key: "{row.id}", title: "Restore model", class: "flex items-center gap-1 px-2.5 py-1.5 rounded-lg border border-dashed border-black/10 dark:border-white/10 text-xs text-text-muted hover:text-primary hover:border-primary/40 hover:bg-primary/5 transition-colors", onclick: { let id = row.id.clone(); move |_| action.call(Action::Restore(id.clone())) }, span { class: "material-symbols-outlined text-[13px]", "add" } "{row.id}" } } } } }
                } }
            }
        }
        if add() { AddModel { alias: alias.clone(), model_id, saving, modal_error, add, test_status: modal_test_status, test_error: modal_test_error, test_generation: modal_test_generation, test: modal_test, save: save_model } }
        if let Some(ids) = disable_all() { Modal { title: "Disable all models".to_owned(), small: true, on_close: move |_| disable_all.set(None), footer: rsx! { Button { label: "Cancel".to_owned(), variant: "ghost", onclick: move |_| disable_all.set(None) } Button { label: "Disable all".to_owned(), variant: "danger", onclick: { let alias = alias.clone(); let ids = ids.clone(); move |_| { let alias = alias.clone(); let ids = ids.clone(); disable_all.set(None); spawn(async move { if dashboard_request("POST", "/api/models/disabled", Some(&json!({"providerAlias":alias,"ids":ids}))).await.is_ok() { refresh_disabled(&alias, models).await; } }); } } } }, p { class: "text-text-muted", "Disable all " strong { "{ids.len()}" } " model(s) for this provider?" } } }
        if let Some(conn) = delete_connection() { DeleteConnection { conn, delete_connection, deleting, action: connection_action } }
        if bulk_delete() { Modal { title: format!("Delete {} Connection{}", selected_ids.len(), if selected_ids.len() == 1 { "" } else { "s" }), small: true, on_close: move |_| { if !bulk_deleting() { bulk_delete.set(false); } }, footer: rsx! { Button { label: "Cancel".to_owned(), variant: "ghost", disabled: bulk_deleting(), onclick: move |_| bulk_delete.set(false) } Button { label: "Delete".to_owned(), variant: "danger", loading: bulk_deleting(), onclick: { let ids = selected_ids.clone(); move |_| connection_action.call(ConnectionAction::BulkDelete(ids.clone())) } } }, p { class: "text-text-muted", "Delete " "{selected_ids.len()}" " connection" if selected_ids.len() != 1 { "s" } "? This cannot be undone." } } }
        if proxy_open() { ApplyProxyModal { pools: pools(), count: connection_count, busy: proxy_busy(), open: proxy_open, action: connection_action } }
        KeyModal { provider: add_key, pools, connections, provider_name: Some(name.clone()), on_refresh: refresh_after_connection }
        edit::EditConnection { provider_id: id.clone(), selected: edit_connection, pools, on_saved: refresh_after_connection }
        if let Some(attempt) = oauth_open() { oauth::OAuthFlow { key: "{attempt}", provider: id.clone(), name: name.clone(), attempt, on_close: move |_| { if *oauth_open.peek() == Some(attempt) { oauth_open.set(None); } }, on_request: oauth_request } }
    }
}

#[component]
fn AddButtons(
    provider_id: String,
    has_oauth: bool,
    has_api: bool,
    compatible: bool,
    connected: bool,
    mut add_key: Signal<Option<String>>,
    open_oauth: Callback<()>,
) -> Element {
    let oauth_label = if provider_id == "xai" {
        "Grok Build OAuth"
    } else {
        "OAuth"
    };
    let key_label = if provider_id == "xai" {
        "xAI API Key"
    } else {
        "API Key"
    };
    let wrapper = if connected {
        "mt-4 grid grid-cols-1 gap-2 sm:flex"
    } else {
        "flex gap-2"
    };
    let extra = if connected { "w-full sm:w-auto" } else { "" };
    rsx! { div { class: wrapper,
        if has_oauth && has_api {
            Button { label: oauth_label.to_owned(), small: true, icon: "lock".to_owned(), variant: "secondary", extra_class: extra, onclick: move |_| open_oauth.call(()) }
            Button { label: key_label.to_owned(), small: true, icon: "key".to_owned(), extra_class: extra, onclick: { let id = provider_id.clone(); move |_| add_key.set(Some(id.clone())) } }
        } else if has_oauth || has_api {
            Button { label: if connected { "Add".to_owned() } else if compatible { "Add API Key".to_owned() } else { "Add Connection".to_owned() }, small: true, icon: "add".to_owned(), extra_class: extra, onclick: { let id = provider_id.clone(); move |_| { if has_oauth { open_oauth.call(()); } else { add_key.set(Some(id.clone())); } } } }
        }
    } }
}

#[component]
fn FreeTier(info: Value) -> Element {
    let card = match text(&info, "creditCard") {
        "none" => "No card required",
        "registration" => "Registration required",
        "phone" => "Phone verification",
        "required" => "Card required",
        _ => "",
    };
    let card_variant = if text(&info, "creditCard") == "none" {
        "bg-success-bg text-success-text"
    } else {
        "bg-accent-amber/20 text-[color:var(--color-warning)]"
    };
    let base = "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full";
    rsx! { div { class: "flex flex-col gap-4 rounded-lg border border-green-500/30 bg-green-500/10 p-4",
        div { class: "flex flex-wrap items-center gap-2",
            span { class: "{base} bg-success-bg text-success-text px-2 py-0.5 text-[10px] tracking-wide", span { class: "size-1.5 rounded-full bg-success-text" } "Free tier" }
            span { class: "{base} bg-surface-card text-body px-2 py-0.5 text-[10px] tracking-wide", "{text(&info, \"accessModel\")}" }
            span { class: "{base} {card_variant} px-2 py-0.5 text-[10px] tracking-wide", "{card}" }
            if info["productionAllowed"] == false { span { class: "{base} bg-accent-amber/20 text-[color:var(--color-warning)] px-2 py-0.5 text-[10px] tracking-wide", "Eval/prototyping only" } }
        }
        div { class: "grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3",
            if !text(&info, "rateLimit").is_empty() { FreeTierField { label: "Rate limit", value: text(&info, "rateLimit").to_owned() } }
            if !text(&info, "maxContext").is_empty() { FreeTierField { label: "Max context", value: text(&info, "maxContext").to_owned() } }
            if let Some(count) = info["freeModels"].as_u64() { FreeTierField { label: "Free models", value: format!("{count} models") } }
        }
        if let Some(caveats) = info["caveats"].as_array().filter(|v| !v.is_empty()) { div { class: "flex flex-col gap-1.5", span { class: "text-[11px] font-medium uppercase tracking-wide text-text-muted", "Caveats" } ul { class: "flex flex-col gap-1", for caveat in caveats { li { class: "flex items-start gap-1.5 text-xs leading-relaxed text-body", span { class: "material-symbols-outlined mt-0.5 text-[14px] text-accent-amber shrink-0", "warning" } span { "{caveat.as_str().unwrap_or_default()}" } } } } } }
        if !text(&info, "lastVerified").is_empty() { p { class: "text-[11px] text-text-muted", "Limits verified " "{text(&info, \"lastVerified\")}" ". Reported by providers and may change — treat as a planning guide." if !text(&info, "source").is_empty() { " " a { href: "{text(&info, \"source\")}", target: "_blank", rel: "noopener noreferrer", class: "text-primary hover:underline", "Source" } } } }
    } }
}

#[component]
fn FreeTierField(label: &'static str, value: String) -> Element {
    rsx! { div { class: "flex flex-col gap-1", span { class: "text-[11px] font-medium uppercase tracking-wide text-text-muted", "{label}" } span { class: "text-sm text-body", "{value}" } } }
}

#[component]
fn Connection(
    conn: Value,
    selected: bool,
    index: usize,
    count: usize,
    is_oauth: bool,
    mut delete_connection: Signal<Option<Value>>,
    mut edit_connection: Signal<Option<Value>>,
    mut selection: Signal<Vec<String>>,
    pools: Vec<Value>,
    test: Option<(String, String)>,
    action: Callback<ConnectionAction>,
) -> Element {
    let mut proxy_menu = use_signal(|| false);
    let id = text(&conn, "id").to_owned();
    let auth = conn["authType"]
        .as_str()
        .filter(|v| !v.is_empty())
        .unwrap_or(if is_oauth { "oauth" } else { "apikey" })
        .to_lowercase();
    let icon = if auth == "oauth" || auth == "device-code" {
        "lock"
    } else if auth == "cookie" {
        "cookie"
    } else {
        "key"
    };
    let name = ["name", "email", "displayName", "id"]
        .into_iter()
        .map(|key| js_trim(text(&conn, key)))
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if auth == "oauth" || auth == "device-code" {
                "OAuth Account".into()
            } else if auth == "cookie" {
                "Cookie Account".into()
            } else {
                "API Key".into()
            }
        });
    let primary_name = js_trim(text(&conn, "name"));
    let secondary = ["email", "displayName"]
        .into_iter()
        .map(|key| js_trim(text(&conn, key)))
        .find(|value| !primary_name.is_empty() && !value.is_empty() && *value != primary_name);
    let active = conn["isActive"] != false;
    let status = if !active {
        "disabled"
    } else {
        conn["testStatus"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("Unknown")
    };
    let (badge, dot) = if active && matches!(status, "active" | "success") {
        ("bg-success-bg text-success-text", "bg-success-text")
    } else if active && matches!(status, "error" | "expired" | "unavailable") {
        (
            "bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)]",
            "bg-[color:var(--color-danger)]",
        )
    } else {
        ("bg-surface-card text-body", "bg-stone")
    };
    let label = if active {
        "Disable connection"
    } else {
        "Enable connection"
    };
    let checkbox_name = conn["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&id)
        .to_owned();
    let specific = &conn["providerSpecificData"];
    let bound_pool = text(specific, "proxyPoolId").to_owned();
    let pool = if bound_pool.is_empty() {
        None
    } else {
        pools.iter().find(|p| text(p, "id") == bound_pool)
    };
    let legacy_url = text(specific, "connectionProxyUrl");
    let legacy_proxy = specific["connectionProxyEnabled"] == true && !legacy_url.is_empty();
    let has_proxy = !bound_pool.is_empty() || legacy_proxy;
    let proxy_badge = if pool.is_some_and(|p| p["isActive"] == true) {
        "bg-success-bg text-success-text"
    } else {
        "bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)]"
    };
    let raw_proxy_url = pool
        .map(|p| text(p, "proxyUrl"))
        .filter(|s| !s.is_empty())
        .unwrap_or(legacy_url);
    let parsed_proxy = web_sys::Url::new(raw_proxy_url).ok();
    let masked_proxy_url = parsed_proxy.as_ref().map(|url| {
        let port = url.port();
        format!(
            "{}//{}{}",
            url.protocol(),
            url.hostname(),
            if port.is_empty() {
                String::new()
            } else {
                format!(":{port}")
            }
        )
    });
    let proxy_display = if !bound_pool.is_empty() {
        if let Some(pool) = pool {
            format!("Pool: {}", text(pool, "name"))
        } else {
            format!("Pool: {bound_pool} (inactive/missing)")
        }
    } else if legacy_proxy {
        // Do not expose embedded proxy credentials or query tokens in the DOM.
        let safe_url = parsed_proxy
            .as_ref()
            .filter(|url| {
                url.username().is_empty()
                    && url.password().is_empty()
                    && url.search().is_empty()
                    && url.hash().is_empty()
            })
            .map(|_| legacy_url)
            .or(masked_proxy_url.as_deref())
            .unwrap_or("[invalid proxy URL]");
        format!("Legacy: {safe_url}")
    } else {
        String::new()
    };
    let no_proxy = pool
        .map(|p| text(p, "noProxy"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(specific, "connectionNoProxy"));
    rsx! {
        div { class: "flex min-w-0 items-stretch", label { class: "flex shrink-0 items-center px-2", input { r#type: "checkbox", checked: selected, aria_label: "Select connection {checkbox_name}", class: "h-3.5 w-3.5 rounded border-gray-300 text-primary focus:ring-primary", onchange: { let id = id.clone(); move |_| { if selected { selection.write().retain(|value| value != &id); } else { selection.write().push(id.clone()); } } } } }
        div { class: "flex-1 min-w-0",
        div { class: if active { "group flex min-w-0 flex-col gap-3 rounded-lg p-2 transition-colors hover:bg-black/[0.02] dark:hover:bg-white/[0.02] sm:flex-row sm:items-center sm:justify-between" } else { "group flex min-w-0 flex-col gap-3 rounded-lg p-2 transition-colors hover:bg-black/[0.02] dark:hover:bg-white/[0.02] sm:flex-row sm:items-center sm:justify-between opacity-60" },
            div { class: "flex min-w-0 flex-1 items-start gap-2 sm:items-center sm:gap-3",
                div { class: "flex shrink-0 flex-col",
                    button { disabled: index == 0, class: if index == 0 { "rounded p-0.5 cursor-not-allowed text-text-muted/30" } else { "rounded p-0.5 text-text-muted hover:bg-sidebar hover:text-primary" }, onclick: move |_| action.call(ConnectionAction::Swap(index, index.saturating_sub(1))), span { class: "material-symbols-outlined text-sm", "keyboard_arrow_up" } }
                    button { disabled: index + 1 >= count, class: if index + 1 >= count { "rounded p-0.5 cursor-not-allowed text-text-muted/30" } else { "rounded p-0.5 text-text-muted hover:bg-sidebar hover:text-primary" }, onclick: move |_| action.call(ConnectionAction::Swap(index, index + 1)), span { class: "material-symbols-outlined text-sm", "keyboard_arrow_down" } }
                }
                span { class: "material-symbols-outlined shrink-0 text-base text-text-muted", "{icon}" }
                div { class: "min-w-0 flex-1", p { class: "truncate text-sm font-medium", "{name}" }
                    if let Some(secondary) = secondary { p { class: "truncate text-xs text-text-muted", "{secondary}" } }
                    div { class: "mt-1 flex min-w-0 flex-wrap items-center gap-1.5 sm:gap-2",
                        span { class: "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full {badge} px-2 py-0.5 text-[10px] tracking-wide", span { class: "size-1.5 rounded-full {dot}" } "{status}" }
                        if has_proxy { span { class: "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full {proxy_badge} px-2 py-0.5 text-[10px] tracking-wide", "Proxy" } }
                        if !text(&conn, "lastError").is_empty() && active { span { class: "max-w-full truncate text-xs text-red-500 sm:max-w-[300px]", title: "{text(&conn, \"lastError\")}", "{text(&conn, \"lastError\")}" } }
                        if conn["priority"].is_null() { span { class: "text-xs text-text-muted", "#" } } else { span { class: "text-xs text-text-muted", "#{conn[\"priority\"]}" } }
                        if conn["globalPriority"].as_f64().is_some_and(|v| v != 0.0) { span { class: "text-xs text-text-muted", "Auto: " "{conn[\"globalPriority\"]}" } }
                        if let Some((state, error)) = test.clone() { span { class: if state == "success" { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-success-bg text-success-text px-2 py-0.5 text-[10px] tracking-wide" } else if state == "failed" { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)] px-2 py-0.5 text-[10px] tracking-wide" } else if state == "testing" { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-brand-coral/15 text-brand-coral px-2 py-0.5 text-[10px] tracking-wide" } else { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-surface-card text-body px-2 py-0.5 text-[10px] tracking-wide" }, "{state}" if state == "failed" && !error.is_empty() { ": {error}" } } }
                    }
                    if has_proxy { div { class: "mt-1 flex flex-wrap items-center gap-2", span { class: "max-w-full truncate text-[11px] text-text-muted sm:max-w-[420px]", title: "{proxy_display}", "{proxy_display}" }
                        if let Some(masked) = masked_proxy_url { code { class: "max-w-full truncate rounded bg-black/5 px-1 py-0.5 font-mono text-[10px] text-text-muted dark:bg-white/5 sm:max-w-[260px]", "{masked}" } }
                        if !no_proxy.is_empty() { span { class: "max-w-full truncate text-[11px] text-text-muted sm:max-w-[320px]", title: "{no_proxy}", "no_proxy: " "{no_proxy}" } }
                    } }
                }
            }
            div { class: "flex w-full items-center justify-between gap-2 sm:w-auto sm:justify-end",
                div { class: "grid flex-1 grid-cols-3 gap-1 sm:flex sm:flex-none",
                if !pools.is_empty() { div { class: "relative", button { class: if has_proxy { "flex w-full flex-col items-center rounded px-2 py-1 transition-colors hover:bg-black/5 dark:hover:bg-white/5 text-primary" } else { "flex w-full flex-col items-center rounded px-2 py-1 transition-colors hover:bg-black/5 dark:hover:bg-white/5 text-text-muted hover:text-primary" }, onclick: move |_| proxy_menu.toggle(), span { class: "material-symbols-outlined text-[18px]", "lan" } span { class: "text-[10px] leading-tight", "Proxy" } }
                    if proxy_menu() { div { class: "absolute right-0 top-full z-50 mt-1 max-w-[78vw] min-w-[160px] rounded-lg border border-border bg-bg py-1 shadow-lg", button { class: if bound_pool.is_empty() { "w-full px-3 py-1.5 text-left text-sm hover:bg-black/5 dark:hover:bg-white/5 font-medium text-primary" } else { "w-full px-3 py-1.5 text-left text-sm hover:bg-black/5 dark:hover:bg-white/5 text-text-main" }, onclick: { let id = id.clone(); move |_| { proxy_menu.set(false); action.call(ConnectionAction::SetProxy(id.clone(), None)); } }, "None" } for pool in pools { button { key: "{text(&pool, \"id\")}", class: if text(&pool, "id") == bound_pool { "w-full px-3 py-1.5 text-left text-sm hover:bg-black/5 dark:hover:bg-white/5 font-medium text-primary" } else { "w-full px-3 py-1.5 text-left text-sm hover:bg-black/5 dark:hover:bg-white/5 text-text-main" }, onclick: { let id = id.clone(); let pool = text(&pool, "id").to_owned(); move |_| { proxy_menu.set(false); action.call(ConnectionAction::SetProxy(id.clone(), Some(pool.clone()))); } }, "{text(&pool, \"name\")}" } } } }
                } }
                button { class: "flex flex-col items-center rounded px-2 py-1 text-text-muted hover:bg-black/5 hover:text-primary dark:hover:bg-white/5", onclick: { let conn = conn.clone(); move |_| edit_connection.set(Some(conn.clone())) }, span { class: "material-symbols-outlined text-[18px]", "edit" } span { class: "text-[10px] leading-tight", "Edit" } }
                button { class: "flex flex-col items-center rounded px-2 py-1 text-red-500 hover:bg-red-500/10", onclick: { let conn = conn.clone(); move |_| delete_connection.set(Some(conn.clone())) }, span { class: "material-symbols-outlined text-[18px]", "delete" } span { class: "text-[10px] leading-tight", "Delete" } }
                }
                Toggle { small: true, checked: active, label: label.to_owned(), onchange: { let id = id.clone(); move |next| action.call(ConnectionAction::Toggle(id.clone(), next)) } }
            }
        }
        } }
    }
}

#[component]
fn DeleteConnection(
    conn: Value,
    mut delete_connection: Signal<Option<Value>>,
    deleting: Signal<bool>,
    action: Callback<ConnectionAction>,
) -> Element {
    let id = text(&conn, "id").to_owned();
    let name = ["name", "email", "id"]
        .into_iter()
        .map(|key| text(&conn, key))
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_owned();
    rsx! { Modal { title: "Delete connection".to_owned(), small: true, on_close: move |_| delete_connection.set(None),
        footer: rsx! { Button { label: "Cancel".to_owned(), variant: "ghost", disabled: deleting(), onclick: move |_| delete_connection.set(None) } Button { label: "Delete".to_owned(), variant: "danger", loading: deleting(), onclick: move |_| action.call(ConnectionAction::Delete(id.clone())) } },
        p { class: "text-text-muted", "Are you sure you want to delete connection " code { "{name}" } "? This cannot be undone." }
    } }
}

#[component]
fn ApplyProxyModal(
    pools: Vec<Value>,
    count: usize,
    busy: bool,
    mut open: Signal<bool>,
    action: Callback<ConnectionAction>,
) -> Element {
    let has_active = pools.iter().any(|p| p["isActive"] == true);
    let class = "flex items-center gap-2 rounded-lg px-3 py-2 text-left transition-colors hover:bg-black/[0.04] dark:hover:bg-white/[0.04] disabled:cursor-not-allowed disabled:opacity-50";
    rsx! { Modal { title: format!("Apply Proxy ({count} connections)"), on_close: move |_| { if !busy { open.set(false); } },
        div { class: "flex flex-col gap-3", div { class: "flex flex-col",
            button { disabled: busy || !has_active, class, onclick: move |_| action.call(ConnectionAction::ApplyProxy(None, true)), span { class: "material-symbols-outlined text-text-muted text-[18px]", "sync_alt" } span { class: "text-sm text-text-main", "One-to-one (rotate)" } }
            button { disabled: busy, class, onclick: move |_| action.call(ConnectionAction::ApplyProxy(None, false)), span { class: "material-symbols-outlined text-text-muted text-[18px]", "link_off" } span { class: "text-sm text-text-main", "None (unbind all)" } }
            for pool in pools { button { key: "{text(&pool, \"id\")}", disabled: busy || pool["isActive"] != true, class, onclick: { let id = text(&pool, "id").to_owned(); move |_| action.call(ConnectionAction::ApplyProxy(Some(id.clone()), false)) }, span { class: "material-symbols-outlined text-text-muted text-[18px]", "lan" } span { class: "truncate text-sm text-text-main", "{text(&pool, \"name\")}" } if pool["isActive"] != true { span { class: "text-[10px] text-text-muted", "(inactive)" } } } }
        }
        if busy { p { class: "text-xs text-text-muted", "Applying..." } }
        Button { label: "Cancel".to_owned(), variant: "ghost", full_width: true, disabled: busy, onclick: move |_| open.set(false) }
        }
    } }
}

#[component]
fn ModelChip(
    row: ModelRow,
    provider_id: String,
    alias: String,
    models: Models,
    thinking_mode: String,
    results: HashMap<String, ModelTest>,
    copied: String,
    can_test: bool,
    action: Callback<Action>,
) -> Element {
    let favorite = models.favorites.contains(&row.id);
    let custom = matches!(row.source, Source::Custom | Source::LegacyAlias);
    let test = results.get(&row.id).cloned().unwrap_or_default();
    let border = match test.status {
        Some(true) => "border-green-500/40",
        Some(false) => "border-red-500/40",
        _ => "border-border",
    };
    let icon = match test.status {
        Some(true) => "check_circle",
        Some(false) => "cancel",
        _ => "smart_toy",
    };
    let (vision, reasoning) = caps(&provider_id, &row, &models);
    let mode = if provider_id != "xai" && thinking_mode == "off" {
        "none"
    } else {
        &thinking_mode
    };
    let suffix = if mode == "auto" || mode.is_empty() {
        String::new()
    } else if thinking::levels(&provider_id, &row.id, &models.thinking_catalog)
        .is_some_and(|values| values.iter().any(|value| value == mode))
    {
        format!("({mode})")
    } else {
        String::new()
    };
    let full = format!("{alias}/{}{suffix}", row.id);
    let copied_this = copied == format!("model-{}", row.id);
    let favorite_label = if favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    };
    rsx! {
        div { class: "group min-w-0 max-w-full rounded-lg border px-3 py-2 {border} hover:bg-sidebar/50",
            div { class: "flex min-w-0 items-start gap-2 sm:items-center",
                button { r#type: "button", class: "shrink-0 rounded p-0.5 hover:bg-sidebar", title: favorite_label, aria_label: favorite_label, onclick: { let id = row.id.clone(); move |_| action.call(Action::Favorite(id.clone())) },
                    span { class: if favorite { "material-symbols-outlined text-[16px] text-yellow-400" } else { "material-symbols-outlined text-[16px] text-text-muted" }, if favorite { "star" } else { "star_outline" } }
                }
                span { class: "material-symbols-outlined shrink-0 text-base", style: if test.status == Some(true) { "color: #22c55e" } else if test.status == Some(false) { "color: #ef4444" } else { "" }, "{icon}" }
                div { class: "flex min-w-0 flex-1 flex-col gap-1", code { class: "max-w-[72vw] truncate rounded bg-sidebar px-1.5 py-0.5 font-mono text-xs text-text-muted sm:max-w-[360px]", "{full}" }
                    span { class: "flex min-w-0 items-center text-[9px] gap-1 pl-1",
                        span { class: "truncate text-[9px] italic text-text-muted/70", "{row.name}" }
                        if vision || reasoning { span { class: "inline-flex items-center gap-0.5 ",
                            if vision { Capability { icon: "visibility", description: "Vision — Supports image input" } }
                            if reasoning { Capability { icon: "neurology", description: "Reasoning — Supports reasoning / thinking" } }
                        } }
                        if let Some(context) = row.context { span { class: "shrink-0 text-text-muted/70", title: "Context window: {context} tokens", "{context_label(context)}" } }
                    }
                }
                if can_test { div { class: "relative shrink-0 group/btn",
                    button { disabled: test.running, class: if test.running { "rounded p-0.5 text-text-muted transition-opacity hover:bg-sidebar hover:text-primary opacity-100" } else { "rounded p-0.5 text-text-muted transition-opacity hover:bg-sidebar hover:text-primary opacity-100 sm:opacity-0 sm:group-hover:opacity-100" }, onclick: { let id = row.id.clone(); move |_| action.call(Action::Test(id.clone())) }, span { class: "material-symbols-outlined text-sm", style: if test.running { "animation: spin 1s linear infinite" } else { "" }, if test.running { "progress_activity" } else { "science" } } }
                    span { class: "pointer-events-none absolute mt-1 top-5 left-1/2 -translate-x-1/2 text-[10px] text-text-muted whitespace-nowrap opacity-0 group-hover/btn:opacity-100 transition-opacity", if test.running { "Testing..." } else { "Test" } }
                } }
                div { class: "relative shrink-0 group/btn",
                    button { class: "rounded p-0.5 text-text-muted hover:bg-sidebar hover:text-primary", aria_label: "Copy model", onclick: { let id = row.id.clone(); let full = full.clone(); move |_| action.call(Action::Copy(id.clone(), full.clone())) }, span { class: "material-symbols-outlined text-sm", if copied_this { "check" } else { "content_copy" } } }
                    span { class: "pointer-events-none absolute mt-1 top-5 left-1/2 -translate-x-1/2 text-[10px] text-text-muted whitespace-nowrap opacity-0 group-hover/btn:opacity-100 transition-opacity", if copied_this { "Copied!" } else { "Copy" } }
                }
                if custom { button { title: "Remove custom model", class: "ml-auto rounded p-0.5 text-text-muted opacity-100 transition-opacity hover:bg-red-500/10 hover:text-red-500 sm:opacity-0 sm:group-hover:opacity-100", onclick: { let row = row.clone(); move |_| action.call(Action::Remove(row.clone())) }, span { class: "material-symbols-outlined text-sm", "close" } } }
                else { button { title: "Disable model", class: "ml-auto rounded p-0.5 text-text-muted opacity-100 transition-opacity hover:bg-red-500/10 hover:text-red-500 sm:opacity-0 sm:group-hover:opacity-100", onclick: { let id = row.id.clone(); move |_| action.call(Action::Disable(id.clone())) }, span { class: "material-symbols-outlined text-sm", "block" } } }
            }
        }
    }
}

#[component]
fn Capability(icon: &'static str, description: &'static str) -> Element {
    rsx! { div { class: "relative inline-flex group", span { class: "material-symbols-outlined leading-none cursor-help text-text-muted/70", style: "font-size: 12px", "{icon}" } div { class: "pointer-events-none absolute bottom-full left-1/2 -translate-x-1/2 mb-1.5 z-50 w-max max-w-56 rounded px-2 py-1 text-[11px] leading-snug bg-gray-900 text-white opacity-0 group-hover:opacity-100 transition-opacity duration-150 whitespace-normal", "{description}" } } }
}

#[component]
fn CompatibleModels(
    alias: String,
    display_alias: String,
    anthropic: bool,
    rows: Vec<ModelRow>,
    models: Signal<Models>,
    copied: String,
    results: HashMap<String, ModelTest>,
    can_test: bool,
    action: Callback<Action>,
    toasts: Signal<Vec<Toast>>,
) -> Element {
    let mut input = use_signal(String::new);
    let mut adding = use_signal(|| false);
    let trimmed = js_trim(&input());
    let add = {
        let alias = alias.clone();
        let trimmed = trimmed.clone();
        let rows = rows.clone();
        use_callback(move |()| {
            if trimmed.is_empty() || adding() {
                return;
            }
            if rows.iter().any(|row| row.id == trimmed) {
                notify(toasts, "Model already exists for this provider.", true);
                return;
            }
            let alias = alias.clone();
            let id = trimmed.clone();
            adding.set(true);
            spawn(async move {
                match dashboard_request(
                    "POST",
                    "/api/models/custom",
                    Some(&json!({"providerAlias":alias,"id":id,"type":"llm"})),
                )
                .await
                {
                    Ok(response) if response.ok() => {
                        refresh_custom(models).await;
                        input.set(String::new());
                    }
                    Ok(response) => {
                        let data = json_body(&response).await.unwrap_or_default();
                        notify(
                            toasts,
                            data["error"]
                                .as_str()
                                .unwrap_or("Failed to add custom model"),
                            true,
                        );
                    }
                    Err(_) => notify(toasts, "Failed to add custom model", true),
                }
                adding.set(false);
            });
        })
    };
    rsx! { div { class: "flex flex-col gap-4",
        p { class: "text-sm text-text-muted", "Add " if anthropic { "Anthropic" } else { "OpenAI" } "-compatible models manually." }
        div { class: "flex items-end gap-2 flex-wrap", div { class: "flex-1 min-w-[240px]", label { r#for: "new-compatible-model-input", class: "text-xs text-text-muted mb-1 block", "Model ID" } input { id: "new-compatible-model-input", r#type: "text", value: "{input}", placeholder: if anthropic { "claude-3-opus-20240229" } else { "gpt-4o" }, class: "w-full px-3 py-2 text-sm border border-border rounded-lg bg-background focus:outline-none focus:border-primary", oninput: move |e| input.set(e.value()), onkeydown: move |e| { if e.key() == Key::Enter { add.call(()); } } } }
            Button { small: true, icon: "add".to_owned(), label: if adding() { "Adding...".to_owned() } else { "Add".to_owned() }, disabled: trimmed.is_empty() || adding(), onclick: move |_| add.call(()) }
        }
        if rows.is_empty() { div { class: "flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border bg-surface/30 py-8 text-center", span { class: "material-symbols-outlined text-2xl text-text-muted", "psychology" } p { class: "text-sm text-text-muted", "No models yet" } } }
        else { div { class: "flex flex-col gap-3", for row in rows {
            CompatibleModelRow { key: "{row.id}", test: results.get(&row.id).cloned().unwrap_or_default(), row, display_alias: display_alias.clone(), copied: copied.clone(), can_test, action }
        } } }
    } }
}

#[component]
fn CompatibleModelRow(
    row: ModelRow,
    display_alias: String,
    copied: String,
    can_test: bool,
    test: ModelTest,
    action: Callback<Action>,
) -> Element {
    let border = match test.status {
        Some(true) => "border-green-500/40",
        Some(false) => "border-red-500/40",
        None => "border-border",
    };
    let icon = match test.status {
        Some(true) => "check_circle",
        Some(false) => "cancel",
        None => "smart_toy",
    };
    let icon_style = match test.status {
        Some(true) => "color: #22c55e",
        Some(false) => "color: #ef4444",
        None => "",
    };
    rsx! { div { class: "flex items-center gap-3 p-3 rounded-lg border {border} hover:bg-sidebar/50",
        span { class: "material-symbols-outlined text-base text-text-muted", style: "{icon_style}", title: "{test.error}", "{icon}" }
        div { class: "flex-1 min-w-0", p { class: "text-sm font-medium truncate", "{row.id}" }
            div { class: "flex items-center gap-1 mt-1", code { class: "text-xs text-text-muted font-mono bg-sidebar px-1.5 py-0.5 rounded max-w-[200px] truncate", title: "{display_alias}/{row.id}", "{display_alias}/{row.id}" }
                div { class: "relative group/btn", button { aria_label: "Copy model", class: "p-0.5 hover:bg-sidebar rounded text-text-muted hover:text-primary", onclick: { let id = row.id.clone(); let full = format!("{display_alias}/{}", row.id); move |_| action.call(Action::Copy(id.clone(), full.clone())) }, span { class: "material-symbols-outlined text-sm", if copied == format!("model-{}", row.id) { "check" } else { "content_copy" } } } span { class: "pointer-events-none absolute top-5 left-1/2 -translate-x-1/2 text-[10px] text-text-muted whitespace-nowrap opacity-0 group-hover/btn:opacity-100 transition-opacity", if copied == format!("model-{}", row.id) { "Copied!" } else { "Copy" } } }
                if can_test { div { class: "relative group/btn", button { aria_label: "Test model", disabled: test.running, class: "p-0.5 hover:bg-sidebar rounded text-text-muted hover:text-primary transition-colors", onclick: { let id = row.id.clone(); move |_| action.call(Action::Test(id.clone())) }, span { class: "material-symbols-outlined text-sm", style: if test.running { "animation: spin 1s linear infinite" } else { "" }, if test.running { "progress_activity" } else { "science" } } } span { class: "pointer-events-none absolute top-5 left-1/2 -translate-x-1/2 text-[10px] text-text-muted whitespace-nowrap opacity-0 group-hover/btn:opacity-100 transition-opacity", if test.running { "Testing..." } else { "Test" } } } }
            }
        }
        button { title: "Remove model", class: "p-1 hover:bg-red-50 rounded text-red-500", onclick: move |_| action.call(Action::Remove(row.clone())), span { class: "material-symbols-outlined text-sm", "delete" } }
    } }
}

#[component]
fn AddModel(
    alias: String,
    mut model_id: Signal<String>,
    saving: Signal<bool>,
    modal_error: Signal<String>,
    mut add: Signal<bool>,
    mut test_status: Signal<String>,
    mut test_error: Signal<String>,
    mut test_generation: Signal<u64>,
    test: Callback<String>,
    save: Callback<String>,
) -> Element {
    let clean = js_trim(&model_id());
    let prefix = format!("{alias}/");
    let clean = clean.strip_prefix(&prefix).unwrap_or(&clean).to_owned();
    let preview = if clean.is_empty() { "model-id" } else { &clean };
    rsx! { Modal { title: "Add Custom Model".to_owned(), on_close: move |_| { test_generation += 1; add.set(false); },
        div { class: "flex flex-col gap-4",
            div { label { class: "text-sm font-medium mb-1.5 block", "Model ID" }
                div { class: "flex gap-2", input { r#type: "text", value: "{model_id}", placeholder: "e.g. claude-opus-4-5", class: "flex-1 px-3 py-2 text-sm border border-border rounded-lg bg-background focus:outline-none focus:border-primary", autofocus: true, oninput: move |e| { model_id.set(e.value()); test_status.set(String::new()); test_error.set(String::new()); test_generation += 1; }, onkeydown: { let clean = clean.clone(); move |e| { if e.key() == Key::Enter { test.call(clean.clone()); } } } }
                    Button { label: if test_status() == "testing" { "Testing...".to_owned() } else { "Test".to_owned() }, variant: "secondary", icon: "science".to_owned(), loading: test_status() == "testing", disabled: clean.is_empty(), onclick: { let clean = clean.clone(); move |_| test.call(clean.clone()) } }
                }
                p { class: "text-xs text-text-muted mt-1", "Sent to provider as: " code { class: "font-mono bg-sidebar px-1 rounded", "{preview}" } " · display: " code { class: "font-mono bg-sidebar px-1 rounded", "{alias}/{preview}" } }
            }
            if test_status() == "ok" { div { class: "flex items-center gap-2 text-sm text-green-600", span { class: "material-symbols-outlined text-base", "check_circle" } "Model is reachable" } }
            if test_status() == "error" { div { class: "flex items-start gap-2 text-sm text-red-500", span { class: "material-symbols-outlined text-base shrink-0", "cancel" } span { if test_error().is_empty() { "Model not reachable" } else { "{test_error}" } } } }
            if !modal_error().is_empty() { p { role: "alert", class: "text-sm text-red-500", "{modal_error}" } }
            div { class: "flex gap-2 pt-1",
                Button { label: "Cancel".to_owned(), variant: "ghost", full_width: true, small: true, onclick: move |_| { test_generation += 1; add.set(false); } }
                Button { label: if saving() { "Adding...".to_owned() } else { "Add Model".to_owned() }, full_width: true, small: true, disabled: clean.is_empty() || saving(), onclick: { let clean = clean.clone(); move |_| save.call(clean.clone()) } }
            }
        }
    } }
}
