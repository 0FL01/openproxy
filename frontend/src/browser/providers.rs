//! Provider list. Metadata is a snapshot of the reference provider registries.
use super::{
    json_body,
    shell::{dashboard_request, notify, notify_warning, Toast},
    ui::{CardSkeleton, Toggle},
};
use dioxus::prelude::*;
use futures_channel::oneshot;
use serde_json::{json, Value};
use std::cmp::Ordering;
use wasm_bindgen::JsValue;

mod compatible;
mod detail;
mod key_modal;
mod new;
use compatible::CompatibleModal;
pub(super) use detail::edit::EditConnection;
pub(super) use detail::ProviderDetailPage;
use key_modal::KeyModal;
pub(super) use new::NewProviderPage;

const GRID: &str = "grid grid-cols-1 gap-3 sm:grid-cols-2 sm:gap-4 lg:grid-cols-3 xl:grid-cols-4";
const CARD: &str = "bg-canvas border border-hairline text-ink rounded-mini-lg shadow-none p-3 h-full hover:bg-black/[0.01] dark:hover:bg-white/[0.01] transition-colors cursor-pointer";
const BADGE: &str = "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full";

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn matches_query(name: &str, query: &str) -> bool {
    query.is_empty()
        || js_sys::JsString::from(name)
            .to_lower_case()
            .as_string()
            .is_some_and(|name| name.contains(query))
}

fn compare_name(a: &Value, b: &Value) -> Ordering {
    js_sys::JsString::from(text(a, "name"))
        .locale_compare(
            text(b, "name"),
            &js_sys::Array::new(),
            &js_sys::Object::new(),
        )
        .cmp(&0)
}

fn date_millis(value: &str) -> f64 {
    if value.is_empty() {
        0.0
    } else {
        js_sys::Date::new(&JsValue::from_str(value)).get_time()
    }
}

fn error_tag(connection: &Value) -> String {
    match text(connection, "lastErrorType") {
        "runtime_error" => return "RUNTIME".into(),
        "upstream_auth_error" | "auth_missing" | "token_refresh_failed" | "token_expired" => {
            return "AUTH".into();
        }
        "upstream_rate_limited" => return "429".into(),
        "upstream_unavailable" => return "5XX".into(),
        "network_error" => return "NET".into(),
        _ => {}
    }
    let code = connection["errorCode"]
        .as_i64()
        .or_else(|| connection["errorCode"].as_str()?.parse::<i64>().ok());
    if let Some(code) = code.filter(|code| *code >= 400) {
        return code.to_string();
    }
    let message = text(connection, "lastError");
    let bytes = message.as_bytes();
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    for (index, code) in bytes.windows(3).enumerate() {
        if matches!(code[0], b'4' | b'5')
            && code[1].is_ascii_digit()
            && code[2].is_ascii_digit()
            && (index == 0 || !is_word(bytes[index - 1]))
            && (index + 3 == bytes.len() || !is_word(bytes[index + 3]))
        {
            let code = std::str::from_utf8(code).unwrap_or_default();
            return if matches!(code, "401" | "403") {
                "AUTH".into()
            } else {
                code.into()
            };
        }
    }
    let message = message.to_lowercase();
    if [
        "quota",
        "balance",
        "insufficient",
        "payment required",
        "billing",
    ]
    .iter()
    .any(|s| message.contains(s))
    {
        "QUOTA"
    } else if ["rate limit", "too many requests"]
        .iter()
        .any(|s| message.contains(s))
    {
        "429"
    } else if ["invalid api key", "credentials", "unauthorized", "auth"]
        .iter()
        .any(|s| message.contains(s))
    {
        "AUTH"
    } else if ["not supported", "model not found", "does not exist"]
        .iter()
        .any(|s| message.contains(s))
    {
        "MODEL"
    } else if [
        "upstream",
        "bad gateway",
        "unavailable",
        "overloaded",
        "timeout",
        "timed out",
    ]
    .iter()
    .any(|s| message.contains(s))
    {
        "5XX"
    } else if ["runtime", "not runnable", "not installed"]
        .iter()
        .any(|s| message.contains(s))
    {
        "RUNTIME"
    } else if [
        "invalid api key",
        "token invalid",
        "revoked",
        "unauthorized",
    ]
    .iter()
    .any(|s| message.contains(s))
    {
        "AUTH"
    } else {
        "ERR"
    }
    .into()
}

fn relative_time(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let mins = ((js_sys::Date::now() - date_millis(raw)) / 60_000.0).floor();
    if mins < 1.0 {
        "just now".into()
    } else if mins < 60.0 {
        format!("{mins}m ago")
    } else if mins < 1440.0 {
        format!("{}h ago", (mins / 60.0).floor())
    } else {
        format!("{}d ago", (mins / 1440.0).floor())
    }
}

#[derive(Default)]
struct Stats {
    total: usize,
    connected: usize,
    errors: usize,
    all_disabled: bool,
    error_code: String,
    error_time: String,
}

fn stats(connections: &[Value], id: &str, auth: &str) -> Stats {
    let matches: Vec<_> = connections
        .iter()
        .filter(|connection| {
            text(connection, "provider") == id && text(connection, "authType") == auth
        })
        .collect();
    let errors: Vec<_> = matches
        .iter()
        .copied()
        .filter(|connection| {
            matches!(
                text(connection, "testStatus"),
                "error" | "expired" | "unavailable"
            )
        })
        .collect();
    let latest = errors.iter().copied().reduce(|first, candidate| {
        if date_millis(text(candidate, "lastErrorAt")) > date_millis(text(first, "lastErrorAt")) {
            candidate
        } else {
            first
        }
    });
    Stats {
        total: matches.len(),
        connected: matches
            .iter()
            .filter(|connection| matches!(text(connection, "testStatus"), "active" | "success"))
            .count(),
        errors: errors.len(),
        all_disabled: !matches.is_empty()
            && matches
                .iter()
                .all(|connection| connection["isActive"] == false),
        error_code: latest.map_or_else(String::new, error_tag),
        error_time: latest.map_or_else(String::new, |value| {
            relative_time(text(value, "lastErrorAt"))
        }),
    }
}

fn start_load(path: &'static str) -> oneshot::Receiver<Result<web_sys::Response, JsValue>> {
    let (sender, receiver) = oneshot::channel();
    // Keep each fetch observed even after another fetch rejects. The reference
    // Promise.all rejects early but the remaining fetches still run, including
    // the shared Dashboard's 401 recheck.
    spawn(async move {
        let _ = sender.send(dashboard_request("GET", path, None).await);
    });
    receiver
}

async fn receive_load(
    receiver: oneshot::Receiver<Result<web_sys::Response, JsValue>>,
) -> Result<web_sys::Response, ()> {
    receiver.await.map_err(|_| ())?.map_err(|_| ())
}

async fn load_data(
    mut connections: Signal<Vec<Value>>,
    mut nodes: Signal<Vec<Value>>,
    mut pools: Signal<Vec<Value>>,
    mut loading: Signal<bool>,
) {
    let connections_request = start_load("/api/providers");
    let nodes_request = start_load("/api/provider-nodes");
    let pools_request = start_load("/api/proxy-pools?isActive=true");
    if let Ok((connections_response, nodes_response, pools_response)) = futures_util::try_join!(
        receive_load(connections_request),
        receive_load(nodes_request),
        receive_load(pools_request)
    ) && let Some(connections_data) = json_body(&connections_response).await
        && let Some(nodes_data) = json_body(&nodes_response).await
    {
        // Astro parses both required bodies before applying any state. Pool JSON
        // alone is optional; a malformed pool response is treated as empty.
        let pools_data = json_body(&pools_response).await.unwrap_or_default();
        if connections_response.ok() {
            if connections_data.is_null() {
                loading.set(false);
                return;
            }
            if let Some(items) = connections_data["connections"].as_array() {
                connections.set(items.clone());
            }
        }
        if nodes_response.ok() {
            if nodes_data.is_null() {
                loading.set(false);
                return;
            }
            if let Some(items) = nodes_data["nodes"].as_array() {
                nodes.set(items.clone());
            }
        }
        if pools_response.ok()
            && let Some(items) = pools_data["proxyPools"].as_array()
        {
            pools.set(items.clone());
        }
    }
    loading.set(false);
}

#[component]
pub fn ProvidersPage(search: Signal<String>) -> Element {
    let mut connections = use_signal(Vec::<Value>::new);
    let nodes = use_signal(Vec::<Value>::new);
    let pools = use_signal(Vec::<Value>::new);
    let loading = use_signal(|| true);
    let mut show_all = use_signal(|| false);
    let mut free_only = use_signal(|| false);
    let mut openai_modal = use_signal(|| false);
    let mut anthropic_modal = use_signal(|| false);
    let mut add_provider = use_signal(|| None::<String>);
    let mut testing = use_signal(String::new);
    let mut results = use_signal(|| None::<Value>);
    let toasts = use_context::<Signal<Vec<Toast>>>();

    use_future(move || async move { load_data(connections, nodes, pools, loading).await });
    use_future(move || async move {
        let Some(window) = web_sys::window() else {
            return;
        };
        let location = window.location();
        let (original_search, original_hash) = super::PROVIDER_INITIAL_LOCATION
            .with(|initial| initial.borrow_mut().take())
            .unwrap_or_else(|| {
                (
                    location.search().unwrap_or_default(),
                    location.hash().unwrap_or_default(),
                )
            });
        let Ok(params) = web_sys::UrlSearchParams::new_with_str(&original_search) else {
            return;
        };
        let mut changed = false;
        if let Some(compatible) = params.get("compatible") {
            match compatible.as_str() {
                "openai" => {
                    openai_modal.set(true);
                    params.delete("compatible");
                    changed = true;
                }
                "anthropic" => {
                    anthropic_modal.set(true);
                    params.delete("compatible");
                    changed = true;
                }
                _ => {}
            }
        }
        if let Some(id) = params.get("add").filter(|id| !id.is_empty()) {
            let registry: Value =
                serde_json::from_str(include_str!("provider_data.json")).unwrap_or_default();
            let contains = |category: &str| {
                registry[category]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|provider| text(provider, "id") == id))
            };
            let oauth = contains("oauth");
            let free = contains("free");
            let api_key = contains("apiKey") || contains("freeTier") || id == "xai";
            if (oauth && !api_key) || free {
                let encoded = js_sys::encode_uri_component(&id)
                    .as_string()
                    .unwrap_or_default();
                let _ = location.set_href(&format!("/dashboard/providers/{encoded}"));
                return;
            }
            if api_key {
                add_provider.set(Some(id));
            }
            params.delete("add");
            changed = true;
        }
        if changed || !original_search.is_empty() || !original_hash.is_empty() {
            let query: String = params.to_string().into();
            let suffix = if changed {
                if query.is_empty() {
                    String::new()
                } else {
                    format!("?{query}")
                }
            } else {
                original_search
            };
            let next = format!(
                "{}{suffix}{original_hash}",
                location.pathname().unwrap_or_default()
            );
            if let Ok(history) = window.history() {
                let _ = history.replace_state_with_url(&js_sys::Object::new(), "", Some(&next));
            }
        }
    });

    let toggle = use_callback(move |(id, auth, active): (String, &'static str, bool)| {
        let ids: Vec<String> = connections
            .peek()
            .iter()
            .filter(|connection| {
                text(connection, "provider") == id && text(connection, "authType") == auth
            })
            .filter_map(|connection| connection["id"].as_str().map(str::to_owned))
            .collect();
        for connection in connections.write().iter_mut() {
            if text(connection, "provider") == id && text(connection, "authType") == auth {
                connection["isActive"] = json!(active);
            }
        }
        for id in ids {
            spawn(async move {
                let encoded = js_sys::encode_uri_component(&id)
                    .as_string()
                    .unwrap_or_default();
                let _ = dashboard_request(
                    "PUT",
                    &format!("/api/providers/{encoded}"),
                    Some(&json!({"isActive": active})),
                )
                .await;
            });
        }
    });

    let test_batch = use_callback(move |mode: &'static str| {
        if !testing.peek().is_empty() {
            return;
        }
        testing.set(mode.into());
        results.set(None);
        spawn(async move {
            let data = match dashboard_request(
                "POST",
                "/api/providers/test-batch",
                Some(&json!({"mode": mode, "providerId": null})),
            )
            .await
            {
                Ok(response) => json_body(&response).await,
                Err(_) => None,
            };
            if let Some(data) = data {
                if let Some(summary) = data["summary"].as_object() {
                    let total = summary.get("total").and_then(Value::as_u64).unwrap_or(0);
                    let passed = summary.get("passed").and_then(Value::as_u64).unwrap_or(0);
                    let failed = summary.get("failed").and_then(Value::as_u64).unwrap_or(0);
                    let message = if total == 0 {
                        format!("No {mode} providers connected")
                    } else if failed == 0 {
                        format!("All {total} tests passed")
                    } else {
                        format!("{passed}/{total} passed, {failed} failed")
                    };
                    if total == 0 {
                        notify_warning(toasts, &message, Some("Nothing to test"));
                    } else if failed > 0 {
                        notify_warning(toasts, &message, None);
                    } else {
                        notify(toasts, &message, false);
                    }
                }
                results.set(Some(data));
            } else {
                notify(toasts, "Provider test failed", true);
                results.set(Some(json!({"error":"Test request failed"})));
            }
            testing.set(String::new());
        });
    });

    if loading() {
        return rsx! { div { class: "flex flex-col gap-8", CardSkeleton {} CardSkeleton {} } };
    }

    let registry: Value =
        serde_json::from_str(include_str!("provider_data.json")).unwrap_or_default();
    let query = js_sys::JsString::from(search())
        .trim()
        .to_lower_case()
        .as_string()
        .unwrap_or_default();
    let matching = |provider: &&Value| matches_query(text(provider, "name"), &query);
    let entries = |key: &str| {
        registry[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter(matching)
            .cloned()
            .collect::<Vec<_>>()
    };
    let all_connections = connections();
    let mut oauth = entries("oauth");
    let mut free = entries("free");
    let mut free_tier = entries("freeTier");
    let mut api_key = entries("apiKey");
    let priority_sort = |a: &Value, b: &Value, auth: &str| {
        a["priority"]
            .as_i64()
            .unwrap_or(999)
            .cmp(&b["priority"].as_i64().unwrap_or(999))
            .then_with(|| {
                (stats(&all_connections, text(b, "id"), auth).connected > 0)
                    .cmp(&(stats(&all_connections, text(a, "id"), auth).connected > 0))
            })
            .then_with(|| compare_name(a, b))
    };
    oauth.sort_by(|a, b| priority_sort(a, b, "oauth"));
    free.sort_by_key(|provider| provider["noAuth"] != true);
    free_tier.sort_by(|a, b| priority_sort(a, b, "apikey"));
    free_tier.sort_by_key(|provider| provider["noAuth"] != true);
    api_key.sort_by(|a, b| {
        (stats(&all_connections, text(b, "id"), "apikey").total > 0)
            .cmp(&(stats(&all_connections, text(a, "id"), "apikey").total > 0))
            .then_with(|| compare_name(a, b))
    });

    let has_any_static = !oauth.is_empty() || !free.is_empty() || !free_tier.is_empty();
    if free_only() {
        oauth.retain(|provider| provider["freeTier"] == true);
        free.clear();
        free_tier.clear();
        api_key.retain(|provider| provider["freeTier"] == true);
    }
    let mut compatible: Vec<Value> = nodes().iter().filter_map(|node| {
        let kind = text(node, "type");
        if !matches!(kind, "openai-compatible" | "anthropic-compatible") || text(node, "id").is_empty() { return None; }
        let name = node["name"].as_str().filter(|name| !name.is_empty()).unwrap_or(if kind == "openai-compatible" { "OpenAI Compatible" } else { "Anthropic Compatible" });
        if !matches_query(name, &query) { return None; }
        Some(json!({"id": text(node,"id"), "name": name, "color": if kind == "openai-compatible" {"#10A37F"} else {"#D97757"}, "textIcon": if kind == "openai-compatible" {"OC"} else {"AC"}, "apiType": node["apiType"], "kind": kind }))
    }).collect();
    compatible.sort_by(|a, b| {
        let kind =
            (text(a, "kind") != "openai-compatible").cmp(&(text(b, "kind") != "openai-compatible"));
        kind.then_with(|| {
            (stats(&all_connections, text(b, "id"), "apikey").total > 0)
                .cmp(&(stats(&all_connections, text(a, "id"), "apikey").total > 0))
        })
        .then_with(|| compare_name(a, b))
    });
    let has_any = has_any_static || !api_key.is_empty() || !compatible.is_empty();
    let visible_keys = if query.is_empty() && !show_all() {
        api_key.len().min(20)
    } else {
        api_key.len()
    };

    rsx! {
        div { class: "flex min-w-0 flex-col gap-6 px-1 sm:px-0",
            if !has_any {
                div { class: "text-center py-8 border border-dashed border-border rounded-xl",
                    span { class: "material-symbols-outlined text-[32px] text-text-muted mb-2", "search_off" }
                    p { class: "text-text-muted text-sm", "No providers match your search" }
                }
            }
            if query.is_empty() || !compatible.is_empty() {
                div { class: "flex flex-col gap-4",
                    div { class: "flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between",
                        h2 { class: "text-lg sm:text-xl font-semibold flex items-center gap-2 leading-tight", "Custom Providers (OpenAI/Anthropic Compatible) " }
                        div { class: "grid grid-cols-1 gap-2 sm:flex sm:w-auto",
                            button { class: "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas bg-brand-coral text-on-primary hover:bg-brand-coral-active disabled:bg-primary-disabled disabled:text-muted-soft h-8 px-3 text-[12px] font-medium w-full sm:w-auto", onclick: move |_| anthropic_modal.set(true), span { class: "material-symbols-outlined text-[18px]", "add" } "Add Anthropic Compatible" }
                            button { class: "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas bg-canvas text-ink border border-hairline hover:border-ink/40 hover:bg-surface-soft disabled:border-hairline disabled:text-muted h-8 px-3 text-[12px] font-medium w-full !bg-white !text-black hover:!bg-gray-100 sm:w-auto", onclick: move |_| openai_modal.set(true), span { class: "material-symbols-outlined text-[18px]", "add" } "Add OpenAI Compatible" }
                        }
                    }
                    if compatible.is_empty() {
                        div { class: "flex items-center justify-center gap-2 py-2 border border-dashed border-border rounded-xl text-text-muted text-sm", span { class: "material-symbols-outlined text-[18px]", "extension" } span { "No custom providers — use buttons above to add OpenAI/Anthropic compatible endpoints" } }
                    } else {
                        div { class: GRID, for provider in compatible { ProviderCard { key: "custom-{provider}", provider, auth: "apikey", card_type: "compatible", connections, on_toggle: toggle } } }
                    }
                }
            }
            if !oauth.is_empty() {
                div { class: "flex flex-col gap-4",
                    div { class: "flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between", h2 { class: "text-lg sm:text-xl font-semibold flex items-center gap-2 leading-tight", "OAuth Providers" } div { class: "flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center", BatchButton { mode: "oauth", title: "Test all OAuth connections", aria: "Test all OAuth connections", testing, on_test: test_batch } } }
                    div { class: GRID, for provider in oauth { ProviderCard { key: "oauth-{provider}", provider, auth: "oauth", card_type: "oauth", connections, on_toggle: toggle } } }
                }
            }
            if !free.is_empty() || !free_tier.is_empty() {
                div { class: "flex flex-col gap-4",
                    div { class: "flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between",
                        h2 { class: "text-lg sm:text-xl font-semibold flex items-center gap-2 leading-tight", "Free Tier Providers" }
                        div { class: "flex items-center gap-4",
                            label { class: "flex items-center gap-2 text-sm cursor-pointer", input { r#type: "checkbox", checked: free_only(), class: "w-4 h-4 text-green-600 bg-gray-100 border-gray-300 rounded focus:ring-green-500", onchange: move |event| free_only.set(event.checked()) } span { "Show only free tier" } }
                            BatchButton { mode: "free", title: "Test all Free connections", aria: "Test all Free provider connections", testing, on_test: test_batch }
                        }
                    }
                    div { class: GRID,
                        for provider in free { ProviderCard { key: "free-{provider}", provider, auth: "oauth", card_type: "free", connections, on_toggle: toggle } }
                        for provider in free_tier { ProviderCard { key: "tier-{provider}", provider, auth: "apikey", card_type: "apikey", connections, on_toggle: toggle } }
                    }
                }
            }
            if !api_key.is_empty() {
                div { class: "flex flex-col gap-4",
                    div { class: "flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between", h2 { class: "text-lg sm:text-xl font-semibold flex items-center gap-2 leading-tight", "API Key Providers " } BatchButton { mode: "apikey", title: "Test all API Key connections", aria: "Test all API Key connections", testing, on_test: test_batch } }
                    div { class: GRID, for provider in api_key.iter().take(visible_keys).cloned() { ProviderCard { key: "apikey-{provider}", provider, auth: "apikey", card_type: "apikey", connections, on_toggle: toggle } } }
                    if query.is_empty() && !show_all() && api_key.len() > 20 {
                        button { class: "flex w-full items-center justify-center gap-1.5 rounded-lg border border-dashed border-primary/40 px-3 py-2.5 text-sm font-medium text-primary transition-colors hover:border-primary hover:bg-primary/5", onclick: move |_| show_all.set(true), span { class: "material-symbols-outlined text-[16px]", "expand_more" } {"Show all "} "{api_key.len()}" {" providers"} }
                    }
                }
            }
        }
        CompatibleModal { open: openai_modal, kind: "openai-compatible", nodes }
        CompatibleModal { open: anthropic_modal, kind: "anthropic-compatible", nodes }
        KeyModal { provider: add_provider, pools, connections }
        if let Some(data) = results() { TestResults { data, on_close: move |_| results.set(None) } }
    }
}

#[component]
fn BatchButton(
    mode: &'static str,
    title: &'static str,
    aria: &'static str,
    testing: Signal<String>,
    on_test: EventHandler<&'static str>,
) -> Element {
    let active = testing() == mode || testing() == "all";
    let colors = if active {
        "bg-primary/20 border-primary/40 text-primary animate-pulse"
    } else {
        "bg-bg border-border text-text-muted hover:text-text-main hover:border-primary/40"
    };
    let icon = if active {
        "material-symbols-outlined text-[14px] animate-spin"
    } else {
        "material-symbols-outlined text-[14px]"
    };
    rsx! { button { onclick: move |_| on_test.call(mode), disabled: !testing().is_empty(), title, aria_label: aria, class: "flex w-full items-center justify-center gap-1.5 rounded-lg border px-3 py-2 text-xs font-medium transition-colors sm:w-auto sm:py-1.5 disabled:cursor-not-allowed disabled:opacity-50 {colors}", span { class: icon, "play_arrow" } if active { "Testing..." } else { "Test All" } } }
}

#[component]
fn ProviderCard(
    provider: Value,
    auth: &'static str,
    card_type: &'static str,
    connections: Signal<Vec<Value>>,
    on_toggle: EventHandler<(String, &'static str, bool)>,
) -> Element {
    let mut failed_image = use_signal(|| false);
    let id = text(&provider, "id").to_owned();
    let name = text(&provider, "name").to_owned();
    let color = text(&provider, "color").to_owned();
    let background = if color.len() > 7 {
        color.clone()
    } else {
        format!("{color}15")
    };
    let card_stats = stats(&connections(), &id, auth);
    let card_class = if card_stats.all_disabled {
        format!("{CARD} opacity-50")
    } else {
        CARD.into()
    };
    let compatible_openai = id.starts_with("openai-compatible-");
    let compatible_anthropic = id.starts_with("anthropic-compatible-");
    let icon_id = if compatible_openai {
        if text(&provider, "apiType") == "responses" {
            "oai-r"
        } else {
            "oai-cc"
        }
    } else if compatible_anthropic {
        "anthropic-m"
    } else {
        &id
    };
    let src = format!("/providers/{icon_id}.png");
    let fallback = provider["textIcon"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| id.chars().take(2).collect::<String>().to_uppercase());
    let icon_class = if matches!(card_type, "oauth" | "free") {
        "object-contain rounded-lg max-w-[32px] max-h-[32px]"
    } else {
        "object-contain rounded-lg max-w-[30px] max-h-[30px]"
    };
    let rate = text(&provider, "rateLimit").to_owned();
    let error_unit = if card_stats.errors == 1 {
        "Error"
    } else {
        "Errors"
    };
    let error_label = if card_stats.error_code.is_empty() || card_stats.error_code == "ERR" {
        format!("{} {error_unit}", card_stats.errors)
    } else {
        format!(
            "{} {error_unit} ({})",
            card_stats.errors, card_stats.error_code
        )
    };
    let href = format!("/dashboard/providers/{id}");
    let switch_label = if card_stats.all_disabled {
        "Enable provider"
    } else {
        "Disable provider"
    };
    let toggle_id = id.clone();
    let next = card_stats.all_disabled;
    rsx! {
        a { href, class: "group min-w-0",
            div { class: card_class,
                div { class: "flex min-w-0 items-center justify-between gap-3",
                    div { class: "flex min-w-0 items-center gap-3",
                        div { class: "size-8 shrink-0 rounded-lg flex items-center justify-center", style: "background-color: {background};",
                            if failed_image() {
                                span { class: "inline-flex items-center justify-center font-bold rounded-lg {icon_class}", style: "width: 30px; height: 30px; color: {color}; font-size: 11px;", "{fallback}" }
                            } else {
                                img { src, alt: "{name}", width: "30", height: "30", class: icon_class, onerror: move |_| failed_image.set(true) }
                            }
                        }
                        div { class: "min-w-0",
                            h3 { class: "truncate font-semibold", "{name}" }
                            if !rate.is_empty() { p { class: "truncate text-[11px] text-green-600 dark:text-green-400", "{rate}" } }
                            div { class: "flex min-w-0 items-center gap-1.5 text-xs flex-wrap",
                                if card_stats.all_disabled {
                                    span { class: "{BADGE} bg-surface-card text-body px-2 py-0.5 text-[10px] tracking-wide", span { class: "flex items-center gap-1", span { class: "material-symbols-outlined text-[12px]", "pause_circle" } "Disabled" } }
                                } else if card_type == "free" && provider["noAuth"] == true {
                                    span { class: "{BADGE} bg-success-bg text-success-text px-2 py-0.5 text-[10px] tracking-wide", span { class: "size-1.5 rounded-full bg-success-text" } "Ready" }
                                } else {
                                    if card_stats.connected > 0 { span { class: "{BADGE} bg-success-bg text-success-text px-2 py-0.5 text-[10px] tracking-wide", span { class: "size-1.5 rounded-full bg-success-text" } "{card_stats.connected}" {" Connected"} } }
                                    if card_stats.errors > 0 {
                                        span { class: "{BADGE} bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)] px-2 py-0.5 text-[10px] tracking-wide", span { class: "size-1.5 rounded-full bg-[color:var(--color-danger)]" } "{error_label}" }
                                    }
                                    if card_stats.connected == 0 && card_stats.errors == 0 {
                                        if card_stats.total > 0 { span { class: "{BADGE} bg-surface-card text-body px-2 py-0.5 text-[10px] tracking-wide", "{card_stats.total}" {" Added"} } }
                                        else { span { class: "text-text-muted", "No connections" } }
                                    }
                                    if compatible_openai { span { class: "{BADGE} bg-surface-card text-body px-2 py-0.5 text-[10px] tracking-wide", if text(&provider,"apiType") == "responses" { "Responses" } else { "Chat" } } }
                                    if compatible_anthropic { span { class: "{BADGE} bg-surface-card text-body px-2 py-0.5 text-[10px] tracking-wide", "Messages" } }
                                    if !card_stats.error_time.is_empty() { span { class: "text-text-muted", "{card_stats.error_time}" } }
                                }
                                if provider["freeTier"] == true { span { class: "{BADGE} bg-success-bg text-success-text px-2 py-0.5 text-[10px] tracking-wide", span { class: "size-1.5 rounded-full bg-success-text" } span { class: "flex items-center gap-1", span { class: "size-2 bg-green-500 rounded-full inline-block" } "Free" } } }
                            }
                        }
                    }
                    div { class: "flex shrink-0 items-center gap-2",
                        if card_stats.total > 0 {
                            div { class: "opacity-100 transition-opacity sm:opacity-0 sm:group-hover:opacity-100", onclick: move |event| { event.prevent_default(); event.stop_propagation(); on_toggle.call((toggle_id.clone(), auth, next)); }, Toggle { checked: !card_stats.all_disabled, small: true, label: switch_label.to_owned(), onchange: move |_| {} } }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TestResults(data: Value, on_close: EventHandler<()>) -> Element {
    let summary = &data["summary"];
    let items = data["results"].as_array().cloned().unwrap_or_default();
    let mode = match text(&data, "mode") {
        "oauth" => "OAuth",
        "free" => "Free",
        "apikey" => "API Key",
        "provider" => "Provider",
        "all" => "All",
        other => other,
    };
    rsx! {
        div { class: "fixed inset-0 z-50 flex items-start justify-center px-3 pt-[6vh] sm:pt-[10vh]", onclick: move |_| on_close.call(()),
            div { class: "absolute inset-0 bg-black/60 backdrop-blur-sm" }
            div { class: "relative bg-surface border border-border rounded-xl w-full max-w-[600px] max-h-[86vh] sm:max-h-[80vh] overflow-y-auto shadow-2xl", onclick: move |event| event.stop_propagation(),
                div { class: "sticky top-0 z-10 flex items-center justify-between px-5 py-3 border-b border-border bg-surface/95 backdrop-blur-sm rounded-t-xl", h3 { class: "font-semibold", "Test Results" } button { onclick: move |_| on_close.call(()), class: "p-1 rounded-lg hover:bg-bg text-text-muted hover:text-text-main transition-colors", aria_label: "Close test results", span { class: "material-symbols-outlined text-lg", "close" } } }
                div { class: "p-5",
                    if data["error"].is_string() && !data["results"].is_array() {
                        div { class: "text-center py-6", span { class: "material-symbols-outlined text-red-500 text-[32px] mb-2 block", "error" } p { class: "text-sm text-red-400", "{text(&data,\"error\")}" } }
                    } else {
                        div { class: "flex min-w-0 flex-col gap-3",
                            if summary.is_object() { div { class: "flex flex-wrap items-center gap-2 text-xs mb-1 sm:gap-3", span { class: "text-text-muted", "{mode} Test" } span { class: "px-2 py-0.5 rounded bg-emerald-500/15 text-emerald-400 font-medium", "{summary[\"passed\"]} passed" } if summary["failed"].as_u64().unwrap_or(0) > 0 { span { class: "px-2 py-0.5 rounded bg-red-500/15 text-red-400 font-medium", "{summary[\"failed\"]} failed" } } span { class: "text-text-muted sm:ml-auto", "{summary[\"total\"]} tested" } } }
                            for item in items.iter() {
                                div { class: "flex min-w-0 flex-wrap items-center gap-2 rounded-lg bg-black/[0.03] px-3 py-2 text-xs dark:bg-white/[0.03] sm:flex-nowrap", span { class: if item["valid"] == true { "material-symbols-outlined text-[16px] text-emerald-500" } else { "material-symbols-outlined text-[16px] text-red-500" }, if item["valid"] == true { "check_circle" } else { "error" } } div { class: "min-w-0 flex-[1_1_160px]", span { class: "block truncate font-medium sm:inline", "{text(item,\"connectionName\")}" } span { class: "block truncate text-text-muted sm:ml-1.5 sm:inline", "({text(item,\"provider\")})" } } if item["latencyMs"].is_number() { span { class: "shrink-0 text-text-muted font-mono tabular-nums", "{item[\"latencyMs\"]}ms" } } span { class: if item["valid"] == true { "shrink-0 text-[10px] uppercase font-bold px-1.5 py-0.5 rounded bg-emerald-500/15 text-emerald-400" } else { "shrink-0 text-[10px] uppercase font-bold px-1.5 py-0.5 rounded bg-red-500/15 text-red-400" }, if item["valid"] == true { "OK" } else { "{item[\"diagnosis\"][\"type\"].as_str().unwrap_or(\"ERROR\")}" } } }
                            }
                            if items.is_empty() { div { class: "text-center py-4 text-text-muted text-sm", "No active connections found for this group." } }
                        }
                    }
                }
            }
        }
    }
}
