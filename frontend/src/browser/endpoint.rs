use super::{
    json_body,
    shell::{dashboard_request, notify, Toast},
    ui::{Button, CardSkeleton, Modal, Toggle, CARD_CLASS, INPUT_CLASS},
};
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};
use std::collections::HashSet;
use wasm_bindgen::{JsCast, JsValue};

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

async fn fetch_keys(mut keys: Signal<Vec<Value>>, mut loading: Signal<bool>) {
    if let Ok(response) = dashboard_request("GET", "/api/keys", None).await {
        let data = json_body(&response).await;
        if response.ok() && data.as_ref().is_some_and(|v| !v.is_null()) {
            let mut list = data
                .as_ref()
                .and_then(|v| v["keys"].as_array())
                .cloned()
                .unwrap_or_default();
            if list.is_empty()
                && let Ok(created) =
                    dashboard_request("POST", "/api/keys", Some(&json!({"name": "Default Key"})))
                        .await
                && created.ok()
                && let Ok(refetch) = dashboard_request("GET", "/api/keys", None).await
            {
                let data = json_body(&refetch).await;
                if refetch.ok() {
                    list = data
                        .and_then(|v| v["keys"].as_array().cloned())
                        .unwrap_or_default();
                }
            }
            keys.set(list);
        }
    }
    loading.set(false);
}

pub(super) fn copy_text(
    value: String,
    id: String,
    mut copied: Signal<String>,
    mut generation: Signal<u32>,
) {
    if let Some(window) = web_sys::window() {
        let navigator = window.navigator();
        let clipboard = js_sys::Reflect::get(navigator.as_ref(), &JsValue::from_str("clipboard"))
            .ok()
            .and_then(|v| v.dyn_into::<web_sys::Clipboard>().ok());
        if let Some(clipboard) = clipboard {
            let _ = clipboard.write_text(&value);
        } else if let Some(document) = window.document()
            && let Ok(element) = document.create_element("textarea")
            && let Ok(textarea) = element.dyn_into::<web_sys::HtmlTextAreaElement>()
        {
            textarea.set_value(&value);
            let _ = textarea.style().set_property("position", "fixed");
            let _ = textarea.style().set_property("opacity", "0");
            if let Some(body) = document.body() {
                let _ = body.append_child(&textarea);
                textarea.select();
                if let Ok(function) =
                    js_sys::Reflect::get(document.as_ref(), &JsValue::from_str("execCommand"))
                        .and_then(|v| v.dyn_into::<js_sys::Function>())
                {
                    let _ = function.call1(document.as_ref(), &JsValue::from_str("copy"));
                }
                let _ = body.remove_child(&textarea);
            }
        }
    }
    copied.set(id);
    let next = generation().wrapping_add(1);
    generation.set(next);
    spawn(async move {
        TimeoutFuture::new(2000).await;
        if generation() == next {
            copied.set(String::new());
        }
    });
}

fn date_string(date: &str) -> String {
    let date = js_sys::Date::new(&JsValue::from_str(date));
    js_sys::Reflect::get(date.as_ref(), &JsValue::from_str("toLocaleDateString"))
        .and_then(|value| value.dyn_into::<js_sys::Function>())
        .and_then(|f| f.call0(date.as_ref()))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

fn mask_key(key: &str) -> String {
    let units: Vec<u16> = key.encode_utf16().collect();
    if units.len() > 8 {
        format!("{}...", String::from_utf16_lossy(&units[..8]))
    } else {
        key.to_owned()
    }
}

fn is_remote_host() -> bool {
    let host = web_sys::window()
        .and_then(|w| w.location().hostname().ok())
        .unwrap_or_default()
        .to_lowercase();
    !matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]" | "::1")
        && !host.ends_with(".localhost")
}

#[component]
pub fn EndpointPage() -> Element {
    let mut keys = use_signal(Vec::<Value>::new);
    let loading = use_signal(|| true);
    let mut add = use_signal(|| false);
    let mut name = use_signal(String::new);
    let mut created = use_signal(String::new);
    let mut require_key = use_signal(|| true);
    let mut require_login = use_signal(|| true);
    let mut has_password = use_signal(|| true);
    let mut visible = use_signal(HashSet::<String>::new);
    let mut delete_target = use_signal(|| None::<Value>);
    let mut pause_target = use_signal(|| None::<Value>);
    let copied = use_signal(String::new);
    let generation = use_signal(|| 0_u32);
    let toasts = use_context::<Signal<Vec<Toast>>>();
    use_future(move || async move {
        fetch_keys(keys, loading).await;
    });
    use_future(move || async move {
        if let Ok(res) = dashboard_request("GET", "/api/settings", None).await
            && res.ok()
            && let Some(data) = json_body(&res).await
        {
            require_key.set(data["requireApiKey"] != false);
            require_login.set(data["requireLogin"] != false);
            has_password.set(data["hasPassword"] == true);
        }
    });
    let base_url = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .map_or_else(|| "/v1".to_owned(), |origin| format!("{origin}/v1"));
    if loading() {
        return rsx! { div { class: "flex flex-col gap-8", CardSkeleton {} CardSkeleton {} } };
    }
    rsx! {
        div { class: "flex flex-col gap-8",
            div { class: CARD_CLASS,
                h2 { class: "text-lg font-semibold mb-4 flex items-center gap-2", span { class: "material-symbols-outlined text-primary", "api" } "API Endpoint" }
                div { class: "flex flex-col gap-2",
                    div { class: "flex items-center gap-2",
                        span { class: "text-xs font-mono px-1.5 py-0.5 rounded shrink-0 min-w-[88px] text-center bg-surface-2 text-text-muted", "Local" }
                        div { class: "flex flex-col gap-1.5 flex-1 font-mono text-sm", div { class: "relative", input { r#type: "text", value: "{base_url}", readonly: true, class: INPUT_CLASS, aria_label: "Local endpoint URL" } } }
                        button { class: "p-2 hover:bg-black/5 dark:hover:bg-white/5 rounded text-text-muted hover:text-primary transition-colors shrink-0", aria_label: "Copy endpoint URL", onclick: { let base_url = base_url.clone(); move |_| copy_text(base_url.clone(), "local_url".into(), copied, generation) },
                            span { class: "material-symbols-outlined text-[18px]", if copied() == "local_url" { "check" } else { "content_copy" } }
                        }
                    }
                }
                if !require_login() || !has_password() || !require_key() {
                    div { class: "mt-4 flex flex-col gap-2",
                        if !require_login() || !has_password() {
                            SecurityWarning { message: if !require_login() { "Require login is disabled — enable it and set a password to protect your dashboard." } else { "Dashboard password is not set — set a strong password in Settings." }, label: "Open Settings", href: "/dashboard/profile" }
                        }
                        if !require_key() {
                            SecurityWarning { message: if is_remote_host() { "Endpoint is exposed without an API key." } else { "Require API key is disabled — your endpoint is accessible without authentication." }, label: "Enable", href: "#require-api-key" }
                        }
                    }
                }
            }
            div { id: "require-api-key", class: CARD_CLASS,
                div { class: "flex items-center justify-between mb-4",
                    h2 { class: "text-lg font-semibold flex items-center gap-2", span { class: "material-symbols-outlined text-primary", "vpn_key" } "API Keys" }
                    Button { label: "Create Key".to_owned(), icon: Some("add".to_owned()), onclick: move |_| add.set(true) }
                }
                div { class: "flex items-center justify-between pb-4 mb-4 border-b border-border",
                    div { p { class: "font-medium", "Require API key" } p { class: "text-sm text-text-muted", "Requests without a valid key will be rejected" } }
                    Toggle { checked: require_key(), label: "Require API key".to_owned(), onchange: move |value| {
                        spawn(async move {
                            let result = dashboard_request("PATCH", "/api/settings", Some(&json!({"requireApiKey": value}))).await;
                            if result.is_ok_and(|res| res.ok()) { require_key.set(value); } else { notify(toasts, "Failed to save API key requirement", true); }
                        });
                    } }
                }
                if keys().is_empty() {
                    div { class: "text-center py-12",
                        div { class: "inline-flex items-center justify-center w-16 h-16 rounded-full bg-primary/10 text-primary mb-4", span { class: "material-symbols-outlined text-[32px]", "vpn_key" } }
                        p { class: "text-text-main font-medium mb-1", "No API keys yet" }
                        p { class: "text-sm text-text-muted mb-4", "Create your first API key to get started" }
                        Button { label: "Create Key".to_owned(), icon: Some("add".to_owned()), onclick: move |_| add.set(true) }
                    }
                } else {
                    div { class: "flex flex-col",
                        for key in keys() {
                            KeyRow { key: "{text(&key, \"id\")}", data: key, visible, copied, generation, pause_target, delete_target, keys }
                        }
                    }
                }
            }
            if add() {
                Modal { title: "Create API Key".to_owned(), on_close: move |_| { add.set(false); name.set(String::new()); },
                    div { class: "flex flex-col gap-4",
                        div { class: "flex flex-col gap-1.5",
                            label { class: "type-body-sm-medium text-ink", r#for: "new-key-name", "Key Name" }
                            div { class: "relative", input { id: "new-key-name", r#type: "text", placeholder: "Production Key", value: "{name}", class: INPUT_CLASS, oninput: move |event| name.set(event.value()) } }
                        }
                        div { class: "flex gap-2",
                            Button { label: "Create".to_owned(), full_width: true, disabled: name().trim().is_empty(), onclick: move |_| {
                                let submitted = name();
                                if submitted.trim().is_empty() { return; }
                                spawn(async move {
                                    if let Ok(res) = dashboard_request("POST", "/api/keys", Some(&json!({"name": submitted}))).await {
                                        let data = json_body(&res).await.unwrap_or(Value::Null);
                                        if res.ok() && !data.is_null() {
                                            created.set(text(&data, "key").to_owned());
                                            fetch_keys(keys, loading).await;
                                            name.set(String::new()); add.set(false);
                                        }
                                    }
                                });
                            } }
                            Button { label: "Cancel".to_owned(), variant: "ghost", full_width: true, onclick: move |_| { add.set(false); name.set(String::new()); } }
                        }
                    }
                }
            }
            if !created().is_empty() {
                Modal { title: "API Key Created".to_owned(), on_close: move |_| created.set(String::new()),
                    div { class: "flex flex-col gap-4",
                        div { class: "bg-yellow-50 dark:bg-yellow-900/20 border border-yellow-200 dark:border-yellow-800 rounded-lg p-4",
                            p { class: "text-sm text-yellow-800 dark:text-yellow-200 mb-2 font-medium", "Save this key now!" }
                            p { class: "text-sm text-yellow-700 dark:text-yellow-300", "This is the only time you will see this key. Store it securely." }
                        }
                        div { class: "flex gap-2",
                            div { class: "flex flex-col gap-1.5 flex-1 font-mono text-sm", div { class: "relative", input { r#type: "text", value: "{created}", readonly: true, class: INPUT_CLASS, aria_label: "Created API key" } } }
                            Button { label: if copied() == "created_key" { "Copied!".to_owned() } else { "Copy".to_owned() }, variant: "secondary", icon: Some(if copied() == "created_key" { "check".to_owned() } else { "content_copy".to_owned() }), onclick: move |_| copy_text(created(), "created_key".into(), copied, generation) }
                        }
                        Button { label: "Done".to_owned(), full_width: true, onclick: move |_| created.set(String::new()) }
                    }
                }
            }
            if let Some(target) = delete_target() {
                Modal { title: "Delete API key".to_owned(), small: true, on_close: move |_| delete_target.set(None),
                    footer: rsx! {
                        Button { label: "Cancel".to_owned(), variant: "ghost", onclick: move |_| delete_target.set(None) }
                        Button { label: "Delete".to_owned(), variant: "danger", onclick: move |_| {
                            let Some(target) = delete_target() else { return; };
                            let id = text(&target, "id").to_owned();
                            spawn(async move {
                                let path = format!("/api/keys/{id}");
                                if dashboard_request("DELETE", &path, None).await.is_ok_and(|res| res.ok()) {
                                    keys.write().retain(|key| text(key, "id") != id);
                                    visible.write().remove(&id);
                                    notify(toasts, "API key deleted", false);
                                } else { notify(toasts, "Failed to delete API key", true); }
                                delete_target.set(None);
                            });
                        } }
                    },
                    p { class: "text-text-muted", "Delete API key " code { "{text(&target, \"name\")}" } "? This cannot be undone." }
                }
            }
            if let Some(target) = pause_target() {
                Modal { title: "Pause API key".to_owned(), small: true, on_close: move |_| pause_target.set(None),
                    footer: rsx! {
                        Button { label: "Cancel".to_owned(), variant: "ghost", onclick: move |_| pause_target.set(None) }
                        Button { label: "Pause".to_owned(), variant: "danger", onclick: move |_| {
                            let Some(target) = pause_target() else { return; };
                            let id = text(&target, "id").to_owned();
                            spawn(async move { toggle_key(id, false, keys).await; pause_target.set(None); });
                        } }
                    },
                    p { class: "text-text-muted", "Pause API key " code { "{text(&target, \"name\")}" } "? This key will stop working immediately but can be resumed later." }
                }
            }
        }
    }
}

async fn toggle_key(id: String, active: bool, mut keys: Signal<Vec<Value>>) {
    let path = format!("/api/keys/{id}");
    if dashboard_request("PUT", &path, Some(&json!({"isActive": active})))
        .await
        .is_ok_and(|response| response.ok())
    {
        for key in keys.write().iter_mut() {
            if text(key, "id") == id {
                key["isActive"] = json!(active);
            }
        }
    }
}

#[component]
fn KeyRow(
    data: Value,
    mut visible: Signal<HashSet<String>>,
    copied: Signal<String>,
    generation: Signal<u32>,
    mut pause_target: Signal<Option<Value>>,
    mut delete_target: Signal<Option<Value>>,
    keys: Signal<Vec<Value>>,
) -> Element {
    let id = text(&data, "id").to_owned();
    let name = text(&data, "name").to_owned();
    let value = text(&data, "key").to_owned();
    let is_visible = visible().contains(&id);
    let active = data["isActive"] != false;
    let active_explicit = data["isActive"] == true;
    let display = if is_visible {
        value.clone()
    } else {
        mask_key(&value)
    };
    let date = date_string(text(&data, "createdAt"));
    let row_class = if active {
        "group flex items-center justify-between py-3 border-b border-black/[0.03] dark:border-white/[0.03] last:border-b-0"
    } else {
        "group flex items-center justify-between py-3 border-b border-black/[0.03] dark:border-white/[0.03] last:border-b-0 opacity-60"
    };
    rsx! {
        div { class: row_class,
            div { class: "flex-1 min-w-0",
                p { class: "text-sm font-medium", "{name}" }
                div { class: "flex items-center gap-2 mt-1",
                    code { class: "text-xs text-text-muted font-mono", "{display}" }
                    button { class: "p-1 hover:bg-black/5 dark:hover:bg-white/5 rounded text-text-muted hover:text-primary opacity-0 group-hover:opacity-100 transition-all", title: if is_visible { "Hide key" } else { "Show key" }, aria_label: if is_visible { format!("Hide key {name}") } else { format!("Show key {name}") }, onclick: { let id = id.clone(); move |_| { let mut set = visible.write(); if !set.remove(&id) { set.insert(id.clone()); } } },
                        span { class: "material-symbols-outlined text-[14px]", if is_visible { "visibility_off" } else { "visibility" } }
                    }
                    button { class: "p-1 hover:bg-black/5 dark:hover:bg-white/5 rounded text-text-muted hover:text-primary opacity-0 group-hover:opacity-100 transition-all", aria_label: "Copy key {name}", onclick: { let id = id.clone(); let value = value.clone(); move |_| copy_text(value.clone(), id.clone(), copied, generation) },
                        span { class: "material-symbols-outlined text-[14px]", if copied() == id { "check" } else { "content_copy" } }
                    }
                }
                p { class: "text-xs text-text-muted mt-1", {"Created "} "{date}" }
                if !active { p { class: "text-xs text-orange-500 mt-1", "Paused" } }
            }
            div { class: "flex items-center gap-2",
                Toggle { small: true, checked: active, label: if active { format!("Pause key {name}") } else { format!("Resume key {name}") }, onchange: { let id = id.clone(); let data = data.clone(); move |checked: bool| {
                    if active_explicit && !checked { pause_target.set(Some(data.clone())); } else { let id = id.clone(); spawn(async move { toggle_key(id, checked, keys).await; }); }
                } } }
                button { class: "p-2 hover:bg-red-500/10 rounded text-red-500 opacity-0 group-hover:opacity-100 transition-all", aria_label: "Delete key {name}", onclick: move |_| delete_target.set(Some(data.clone())), span { class: "material-symbols-outlined text-[18px]", "delete" } }
            }
        }
    }
}

#[component]
fn SecurityWarning(message: String, label: &'static str, href: &'static str) -> Element {
    rsx! {
        div { class: "flex items-center gap-2 px-3 py-2 rounded-lg bg-amber-500/10 border border-amber-500/20 text-amber-700 dark:text-amber-400",
            span { class: "material-symbols-outlined text-[16px] shrink-0 mt-0.5", "warning" }
            p { class: "text-xs flex-1", "{message}" }
            a { href, class: "text-xs font-medium underline shrink-0 hover:opacity-80", onclick: move |event| {
                if let Some(id) = href.strip_prefix('#') {
                    event.prevent_default();
                    if let Some(element) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id(id)) {
                        let options = web_sys::ScrollIntoViewOptions::new();
                        options.set_behavior(web_sys::ScrollBehavior::Smooth);
                        element.scroll_into_view_with_scroll_into_view_options(&options);
                    }
                }
            }, "{label}" }
        }
    }
}
