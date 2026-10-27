//! Proxy pool management using the existing authenticated dashboard APIs.
use super::{
    json, json_body,
    shell::{dashboard_get_no_store, dashboard_request, notify, notify_warning, Toast},
    ui::{Button, CardSkeleton, Modal, CARD_CLASS},
    Value, INPUT_CLASS,
};
use dioxus::prelude::*;
use futures_util::{stream, StreamExt};
use std::{collections::HashSet, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{Event, Node, Url};

#[derive(Clone, Default)]
struct PoolForm {
    name: String,
    proxy_url: String,
    no_proxy: String,
    is_active: bool,
    strict_proxy: bool,
}

impl PoolForm {
    fn new() -> Self {
        Self {
            is_active: true,
            ..Self::default()
        }
    }

    fn from_pool(pool: &Value) -> Self {
        Self {
            name: text(pool, "name").to_owned(),
            proxy_url: text(pool, "proxyUrl").to_owned(),
            no_proxy: text(pool, "noProxy").to_owned(),
            is_active: pool["isActive"] != false,
            strict_proxy: pool["strictProxy"] == true,
        }
    }
}

#[derive(Clone)]
enum Confirmation {
    Delete { id: String, name: String },
    DeleteSelected(Vec<String>),
    DisableDead { ids: Vec<String>, alive: usize },
}

struct RelayMenuGuard {
    document: web_sys::Document,
    handler: Closure<dyn FnMut(Event)>,
}

impl Drop for RelayMenuGuard {
    fn drop(&mut self) {
        let _ = self.document.remove_event_listener_with_callback(
            "mousedown",
            self.handler.as_ref().unchecked_ref(),
        );
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn js_trim(value: &str) -> String {
    js_sys::JsString::from(value)
        .trim()
        .as_string()
        .unwrap_or_default()
}

fn pool_path(id: &str) -> String {
    format!("/api/proxy-pools/{}", js_sys::encode_uri_component(id))
}

fn format_time(value: &str) -> String {
    if value.is_empty() {
        return "Never".to_owned();
    }
    let date = js_sys::Date::new(&JsValue::from_str(value));
    if date.get_time().is_nan() {
        return "Never".to_owned();
    }
    let locale = web_sys::window()
        .and_then(|window| window.navigator().language())
        .unwrap_or_else(|| "en-US".to_owned());
    date.to_locale_string(&locale, &JsValue::UNDEFINED)
        .as_string()
        .unwrap_or_else(|| "Never".to_owned())
}

async fn load_pools(
    mut pools: Signal<Vec<Value>>,
    mut selected: Signal<Vec<String>>,
    mut loading: Signal<bool>,
    toasts: Signal<Vec<Toast>>,
) {
    let list = match dashboard_get_no_store("/api/proxy-pools?includeUsage=true").await {
        Ok(response) if response.ok() => json_body(&response)
            .await
            .and_then(|value| value["proxyPools"].as_array().cloned()),
        _ => None,
    };
    if let Some(list) = list {
        selected
            .write()
            .retain(|id| list.iter().any(|pool| text(pool, "id") == id));
        pools.set(list);
    } else {
        notify(toasts, "Failed to load proxy pools", true);
    }
    loading.set(false);
}

fn parse_proxy_line(line: &str) -> Result<(String, String), &'static str> {
    let line = js_trim(line);
    if line.contains("://") {
        let url = Url::new(&line).map_err(|_| "Invalid URL")?;
        let host = if url.port().is_empty() {
            url.hostname()
        } else {
            format!("{}:{}", url.hostname(), url.port())
        };
        Ok((format!("Imported {host}"), url.href()))
    } else {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() != 4 {
            return Err("Unsupported format");
        }
        if parts.iter().any(|part| part.is_empty()) {
            return Err("Invalid host:port:user:pass format");
        }
        let raw = format!(
            "http://{}:{}@{}:{}",
            js_sys::encode_uri_component(parts[2]),
            js_sys::encode_uri_component(parts[3]),
            parts[0],
            parts[1],
        );
        let url = Url::new(&raw).map_err(|_| "Invalid URL")?;
        Ok((format!("Imported {}:{}", parts[0], parts[1]), url.href()))
    }
}

#[component]
fn PoolBadge(
    label: String,
    #[props(default = "default")] variant: &'static str,
    #[props(default)] small: bool,
    #[props(default)] dot: bool,
) -> Element {
    let color = match variant {
        "success" => "bg-success-bg text-success-text",
        "error" => "bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)]",
        _ => "bg-surface-card text-body",
    };
    let size = if small {
        "px-2 py-0.5 text-[10px] tracking-wide"
    } else {
        "px-2.5 py-1 text-[11px] tracking-wide"
    };
    let dot_color = match variant {
        "success" => "bg-success-text",
        "error" => "bg-[color:var(--color-danger)]",
        _ => "bg-stone",
    };
    rsx! { span { class: "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full {color} {size}",
        if dot { span { class: "size-1.5 rounded-full {dot_color}" } }
        "{label}"
    } }
}

#[component]
fn PoolToggle(
    checked: bool,
    label: String,
    #[props(default)] small: bool,
    #[props(default)] disabled: bool,
    onchange: EventHandler<bool>,
) -> Element {
    let track = if small { "w-8 h-4" } else { "w-11 h-6" };
    let thumb = if small { "size-3" } else { "size-5" };
    let translate = if checked {
        if small {
            "translate-x-4"
        } else {
            "translate-x-5"
        }
    } else {
        "translate-x-0.5"
    };
    let color = if checked {
        "bg-brand-500"
    } else {
        "bg-surface-3"
    };
    rsx! { div { class: if disabled { "flex items-center gap-3 opacity-50 cursor-not-allowed" } else { "flex items-center gap-3" },
        button { r#type: "button", role: "switch", aria_label: "{label}", title: "{label}", aria_checked: "{checked}", disabled,
            class: "relative inline-flex shrink-0 cursor-pointer rounded-full transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-brand-500/30 {color} {track}",
            onclick: move |_| onchange.call(!checked),
            span { class: "pointer-events-none inline-block rounded-full bg-white shadow-sm transform transition duration-200 ease-in-out {translate} {thumb} mt-0.5" }
        }
    } }
}

#[component]
fn PoolInput(
    id: &'static str,
    label: &'static str,
    placeholder: &'static str,
    value: String,
    #[props(default = "text")] kind: &'static str,
    #[props(default)] hint: Option<&'static str>,
    oninput: EventHandler<String>,
) -> Element {
    rsx! { div { class: "flex flex-col gap-1.5",
        label { r#for: id, class: "type-body-sm-medium text-ink", "{label}" }
        div { class: "relative", input { id, r#type: kind, placeholder, value: "{value}", class: INPUT_CLASS, oninput: move |event| oninput.call(event.value()) } }
        if let Some(hint) = hint { p { class: "type-body-sm text-slate", "{hint}" } }
    } }
}

#[component]
pub(super) fn ProxyPoolsPage() -> Element {
    let mut pools = use_signal(Vec::<Value>::new);
    let loading = use_signal(|| true);
    let mut selected = use_signal(Vec::<String>::new);
    let mut form_open = use_signal(|| false);
    let mut form = use_signal(PoolForm::new);
    let mut editing = use_signal(|| None::<String>);
    let mut saving = use_signal(|| false);
    let mut import_open = use_signal(|| false);
    let mut import_text = use_signal(String::new);
    let mut importing = use_signal(|| false);
    let mut vercel_open = use_signal(|| false);
    let mut vercel_token = use_signal(String::new);
    let mut project_name = use_signal(|| "vercel-relay".to_owned());
    let mut deploying = use_signal(|| false);
    let mut relay_menu = use_signal(|| false);
    let mut testing = use_signal(|| None::<String>);
    let mut health_checking = use_signal(|| false);
    let mut health_progress = use_signal(|| (0usize, 0usize));
    let mut bulk_busy = use_signal(|| false);
    let mut pending = use_signal(|| None::<Confirmation>);
    let toasts = use_context::<Signal<Vec<Toast>>>();

    use_hook(move || {
        let document = web_sys::window().and_then(|window| window.document())?;
        let menu_document = document.clone();
        let handler = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            if !relay_menu() {
                return;
            }
            let inside = menu_document
                .get_element_by_id("proxy-pools-relay-menu")
                .zip(
                    event
                        .target()
                        .and_then(|target| target.dyn_into::<Node>().ok()),
                )
                .is_some_and(|(menu, target)| menu.contains(Some(&target)));
            if !inside {
                relay_menu.set(false);
            }
        });
        let _ = document
            .add_event_listener_with_callback("mousedown", handler.as_ref().unchecked_ref());
        Some(Rc::new(RelayMenuGuard { document, handler }))
    });
    use_future(move || load_pools(pools, selected, loading, toasts));

    let open_create = use_callback(move |()| {
        editing.set(None);
        form.set(PoolForm::new());
        form_open.set(true);
    });
    let save = use_callback(move |()| {
        if saving() {
            return;
        }
        let data = form();
        let name = js_trim(&data.name);
        let url = js_trim(&data.proxy_url);
        if name.is_empty() || url.is_empty() {
            return;
        }
        let body = json!({"name": name, "proxyUrl": url, "noProxy": js_trim(&data.no_proxy), "isActive": data.is_active, "strictProxy": data.strict_proxy});
        let edit_id = editing();
        saving.set(true);
        spawn(async move {
            let path = edit_id
                .as_deref()
                .map(pool_path)
                .unwrap_or_else(|| "/api/proxy-pools".to_owned());
            let method = if edit_id.is_some() { "PUT" } else { "POST" };
            match dashboard_request(method, &path, Some(&body)).await {
                Ok(response) if response.ok() => {
                    load_pools(pools, selected, loading, toasts).await;
                    form_open.set(false);
                    editing.set(None);
                    form.set(PoolForm::new());
                    notify(
                        toasts,
                        if edit_id.is_some() {
                            "Proxy pool updated"
                        } else {
                            "Proxy pool created"
                        },
                        false,
                    );
                }
                Ok(response) => {
                    let data = json_body(&response).await.unwrap_or_default();
                    notify(
                        toasts,
                        data["error"]
                            .as_str()
                            .unwrap_or("Failed to save proxy pool"),
                        true,
                    );
                }
                Err(_) => notify(toasts, "Failed to save proxy pool", true),
            }
            saving.set(false);
        });
    });

    let test_pool = use_callback(move |id: String| {
        testing.set(Some(id.clone()));
        spawn(async move {
            let path = format!("{}/test", pool_path(&id));
            match dashboard_request("POST", &path, None).await {
                Ok(response) => {
                    let ok = response.ok();
                    let data = json_body(&response).await.unwrap_or_default();
                    if ok {
                        load_pools(pools, selected, loading, toasts).await;
                        notify(
                            toasts,
                            if data["ok"] == true {
                                "Proxy test passed"
                            } else {
                                "Proxy test failed"
                            },
                            false,
                        );
                    } else {
                        notify(
                            toasts,
                            data["error"].as_str().unwrap_or("Failed to test proxy"),
                            true,
                        );
                    }
                }
                Err(_) => notify(toasts, "Failed to test proxy", true),
            }
            if testing().as_deref() == Some(id.as_str()) {
                testing.set(None);
            }
        });
    });

    let toggle_active = use_callback(move |(id, old): (String, bool)| {
        for pool in pools
            .write()
            .iter_mut()
            .filter(|pool| text(pool, "id") == id)
        {
            pool["isActive"] = json!(!old);
        }
        spawn(async move {
            if !dashboard_request("PUT", &pool_path(&id), Some(&json!({"isActive": !old})))
                .await
                .is_ok_and(|response| response.ok())
            {
                for pool in pools
                    .write()
                    .iter_mut()
                    .filter(|pool| text(pool, "id") == id)
                {
                    pool["isActive"] = json!(old);
                }
                notify(toasts, "Failed to update active state", true);
            }
        });
    });

    let bulk_active = use_callback(move |active: bool| {
        if bulk_busy() || health_checking() {
            return;
        }
        let ids = selected();
        if ids.is_empty() {
            return;
        }
        bulk_busy.set(true);
        spawn(async move {
            let mut ok = 0;
            let mut failed = 0;
            for id in ids {
                if dashboard_request("PUT", &pool_path(&id), Some(&json!({"isActive": active})))
                    .await
                    .is_ok_and(|response| response.ok())
                {
                    ok += 1;
                } else {
                    failed += 1;
                }
            }
            load_pools(pools, selected, loading, toasts).await;
            let suffix = if failed > 0 {
                format!(", failed {failed}")
            } else {
                String::new()
            };
            notify(
                toasts,
                &format!(
                    "{} {ok}{suffix}",
                    if active { "Activated" } else { "Deactivated" }
                ),
                false,
            );
            bulk_busy.set(false);
        });
    });

    let health = use_callback(move |()| {
        if health_checking() || bulk_busy() {
            return;
        }
        let selection = selected();
        let ids: Vec<String> = pools()
            .iter()
            .filter_map(|pool| {
                let id = text(pool, "id");
                (!id.is_empty()
                    && (selection.is_empty() || selection.iter().any(|item| item == id)))
                .then(|| id.to_owned())
            })
            .collect();
        if ids.is_empty() {
            return;
        }
        health_checking.set(true);
        health_progress.set((0, ids.len()));
        spawn(async move {
            let total = ids.len();
            let mut done = 0;
            let mut alive = 0;
            let mut dead = Vec::new();
            let mut checks = stream::iter(ids.into_iter().map(|id| async move {
                let path = format!("{}/test", pool_path(&id));
                let success = match dashboard_request("POST", &path, None).await {
                    Ok(response) if response.ok() => json_body(&response)
                        .await
                        .is_some_and(|value| value["ok"] == true),
                    _ => false,
                };
                (id, success)
            }))
            .buffer_unordered(10);
            while let Some((id, success)) = checks.next().await {
                if success {
                    alive += 1;
                } else {
                    dead.push(id);
                }
                done += 1;
                health_progress.set((done, total));
            }
            load_pools(pools, selected, loading, toasts).await;
            health_checking.set(false);
            health_progress.set((0, 0));
            if dead.is_empty() {
                notify(
                    toasts,
                    &format!("Health check done. Alive: {alive}, Dead: 0"),
                    false,
                );
            } else {
                pending.set(Some(Confirmation::DisableDead { ids: dead, alive }));
            }
        });
    });

    let confirm = use_callback(move |()| {
        let Some(action) = pending() else {
            return;
        };
        pending.set(None);
        match action {
            Confirmation::Delete { id, .. } => {
                spawn(async move {
                    match dashboard_request("DELETE", &pool_path(&id), None).await {
                        Ok(response) if response.ok() => {
                            pools.write().retain(|pool| text(pool, "id") != id);
                            selected.write().retain(|item| item != &id);
                            notify(toasts, "Proxy pool deleted", false);
                        }
                        Ok(response) => {
                            let conflict = response.status() == 409;
                            let data = json_body(&response).await.unwrap_or_default();
                            if conflict {
                                let count = data["boundConnectionCount"].as_u64().unwrap_or(0);
                                notify_warning(toasts, &format!("Cannot delete: {count} connection(s) are still using this pool."), None);
                            } else {
                                notify(
                                    toasts,
                                    data["error"]
                                        .as_str()
                                        .unwrap_or("Failed to delete proxy pool"),
                                    true,
                                );
                            }
                        }
                        Err(_) => notify(toasts, "Failed to delete proxy pool", true),
                    }
                });
            }
            Confirmation::DeleteSelected(ids) => {
                bulk_busy.set(true);
                spawn(async move {
                    let (mut ok, mut blocked, mut failed) = (0, 0, 0);
                    for id in ids {
                        match dashboard_request("DELETE", &pool_path(&id), None).await {
                            Ok(response) if response.ok() => ok += 1,
                            Ok(response) if response.status() == 409 => blocked += 1,
                            _ => failed += 1,
                        }
                    }
                    load_pools(pools, selected, loading, toasts).await;
                    selected.set(Vec::new());
                    let bound = if blocked > 0 {
                        format!(", {blocked} bound")
                    } else {
                        String::new()
                    };
                    let failures = if failed > 0 {
                        format!(", {failed} failed")
                    } else {
                        String::new()
                    };
                    notify(toasts, &format!("Deleted {ok}{bound}{failures}"), false);
                    bulk_busy.set(false);
                });
            }
            Confirmation::DisableDead { ids, .. } => {
                bulk_busy.set(true);
                spawn(async move {
                    let count = ids.len();
                    let mut failed = 0;
                    for id in ids {
                        if !dashboard_request(
                            "PUT",
                            &pool_path(&id),
                            Some(&json!({"isActive": false})),
                        )
                        .await
                        .is_ok_and(|response| response.ok())
                        {
                            failed += 1;
                        }
                    }
                    load_pools(pools, selected, loading, toasts).await;
                    if failed == 0 {
                        notify(toasts, &format!("Disabled {count} dead proxies"), false);
                    } else {
                        notify(
                            toasts,
                            &format!("Disabled {} dead proxies, failed {failed}", count - failed),
                            true,
                        );
                    }
                    bulk_busy.set(false);
                });
            }
        }
    });

    let batch_import = use_callback(move |()| {
        if importing() {
            return;
        }
        let lines: Vec<String> = import_text()
            .split('\n')
            .map(js_trim)
            .filter(|line| !line.is_empty())
            .collect();
        if lines.is_empty() {
            notify_warning(toasts, "Please paste at least one proxy line.", None);
            return;
        }
        let mut entries = Vec::new();
        let mut invalid = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            match parse_proxy_line(line) {
                Ok(entry) => entries.push(entry),
                Err(error) => invalid.push(format!("Line {}: {error}", index + 1)),
            }
        }
        if !invalid.is_empty() {
            notify(
                toasts,
                &format!("Invalid proxy format:\n{}", invalid.join("\n")),
                true,
            );
            return;
        }
        let mut existing: HashSet<(String, String)> = pools()
            .iter()
            .map(|pool| {
                (
                    js_trim(text(pool, "proxyUrl")),
                    js_trim(text(pool, "noProxy")),
                )
            })
            .collect();
        importing.set(true);
        spawn(async move {
            let (mut created, mut skipped, mut failed) = (0, 0, 0);
            for (name, url) in entries {
                let key = (url.clone(), String::new());
                if existing.contains(&key) {
                    skipped += 1;
                    continue;
                }
                match dashboard_request(
                    "POST",
                    "/api/proxy-pools",
                    Some(&json!({"name": name, "proxyUrl": url, "noProxy": "", "isActive": true})),
                )
                .await
                {
                    Ok(response) if response.ok() => {
                        created += 1;
                        existing.insert(key);
                    }
                    Ok(_) => failed += 1,
                    Err(_) => {
                        notify(toasts, "Batch import failed", true);
                        importing.set(false);
                        load_pools(pools, selected, loading, toasts).await;
                        return;
                    }
                }
            }
            load_pools(pools, selected, loading, toasts).await;
            import_open.set(false);
            import_text.set(String::new());
            notify(
                toasts,
                &format!(
                    "Batch import completed: Created {created}, Skipped {skipped}, Failed {failed}"
                ),
                false,
            );
            importing.set(false);
        });
    });

    let deploy = use_callback(move |()| {
        if deploying() || js_trim(&vercel_token()).is_empty() {
            return;
        }
        let body = json!({"vercelToken": vercel_token(), "projectName": project_name()});
        deploying.set(true);
        spawn(async move {
            match dashboard_request("POST", "/api/proxy-pools/vercel-deploy", Some(&body)).await {
                Ok(response) => {
                    let ok = response.ok();
                    let data = json_body(&response).await.unwrap_or_default();
                    if ok {
                        load_pools(pools, selected, loading, toasts).await;
                        vercel_open.set(false);
                        vercel_token.set(String::new());
                        notify(
                            toasts,
                            &format!(
                                "Deployed: {}",
                                data["deployUrl"].as_str().unwrap_or("undefined")
                            ),
                            false,
                        );
                    } else {
                        notify(
                            toasts,
                            data["error"].as_str().unwrap_or("Deploy failed"),
                            true,
                        );
                    }
                }
                Err(_) => notify(toasts, "Deploy failed", true),
            }
            deploying.set(false);
        });
    });

    let list = pools();
    let chosen = selected();
    let all_selected = !list.is_empty() && chosen.len() == list.len();
    let active_count = list.iter().filter(|pool| pool["isActive"] == true).count();
    let (done, total) = health_progress();
    rsx! {
        div { class: "mx-auto flex w-full max-w-5xl flex-col gap-4 px-1 sm:gap-6 sm:px-0",
            if loading() {
                CardSkeleton {} CardSkeleton {}
            } else {
                div { class: "flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between",
                    div { class: "min-w-0", h1 { class: "text-xl font-semibold sm:text-2xl", "Proxy Pools" } }
                    div { class: "grid grid-cols-1 gap-2 sm:flex sm:items-center",
                        div { id: "proxy-pools-relay-menu", class: "relative",
                            button { r#type: "button", aria_expanded: "{relay_menu}", class: "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas bg-canvas text-ink border border-hairline hover:border-ink/40 hover:bg-surface-soft h-8 px-3 text-[12px] font-medium", onclick: move |_| relay_menu.set(!relay_menu()),
                                span { class: "material-symbols-outlined text-[18px]", "rocket_launch" } "Deploy Relay"
                                span { class: "material-symbols-outlined ml-1 text-[18px]", if relay_menu() { "expand_less" } else { "expand_more" } }
                            }
                            if relay_menu() { div { class: "absolute left-0 top-full z-50 mt-1 w-48 rounded-xl border border-black/10 bg-white p-1 shadow-xl dark:border-white/10 dark:bg-zinc-900 sm:left-auto sm:right-0",
                                button { class: "flex w-full items-center gap-2 rounded-lg px-3 py-2 text-sm text-text-main transition-colors hover:bg-black/5 dark:hover:bg-white/5", onclick: move |_| { vercel_token.set(String::new()); project_name.set("vercel-relay".to_owned()); vercel_open.set(true); relay_menu.set(false); },
                                    span { class: "material-symbols-outlined text-[20px] text-blue-500", "cloud_upload" } "Vercel Relay"
                                }
                            } }
                        }
                        Button { label: "Batch Import".to_owned(), variant: "secondary", small: true, icon: "upload".to_owned(), onclick: move |_| { import_text.set(String::new()); import_open.set(true); } }
                        Button { label: "Add Proxy Pool".to_owned(), small: true, icon: "add".to_owned(), onclick: move |_| open_create.call(()) }
                    }
                }
                div { class: CARD_CLASS,
                    div { class: "mb-4 flex flex-wrap items-center gap-2",
                        if !list.is_empty() { label { class: "flex items-center gap-1.5 text-xs text-text-muted cursor-pointer",
                            input { r#type: "checkbox", checked: all_selected, class: "size-4 rounded border-black/20 dark:border-white/20", onchange: move |_| {
                                if all_selected { selected.set(Vec::new()); } else { selected.set(pools().iter().map(|pool| text(pool, "id").to_owned()).collect()); }
                            } }
                            if all_selected { "Unselect all" } else { "Select all" }
                        } }
                        PoolBadge { label: format!("Total: {}", list.len()) }
                        PoolBadge { label: format!("Active: {active_count}"), variant: "success" }
                    }
                    if !chosen.is_empty() || health_checking() {
                        div { class: "mb-4 flex flex-wrap items-center gap-2 rounded-lg border border-primary/30 bg-primary/5 px-3 py-2",
                            span { class: "material-symbols-outlined text-[18px] text-primary", "checklist" }
                            span { class: "text-xs font-medium text-primary", if chosen.is_empty() { "All pools" } else { "{chosen.len()} selected" } }
                            div { class: "ml-auto flex flex-wrap items-center gap-2",
                                Button { label: if health_checking() { format!("Checking {done}/{total}") } else { "Health Check".to_owned() }, small: true, icon: if health_checking() { "progress_activity".to_owned() } else { "health_and_safety".to_owned() }, disabled: health_checking() || bulk_busy() || list.is_empty(), onclick: move |_| health.call(()) }
                                if !chosen.is_empty() {
                                    Button { label: "Activate".to_owned(), variant: "secondary", small: true, icon: "toggle_on".to_owned(), disabled: bulk_busy() || health_checking(), onclick: move |_| bulk_active.call(true) }
                                    Button { label: "Deactivate".to_owned(), variant: "secondary", small: true, icon: "toggle_off".to_owned(), disabled: bulk_busy() || health_checking(), onclick: move |_| bulk_active.call(false) }
                                    Button { label: "Delete".to_owned(), variant: "secondary", small: true, icon: "delete".to_owned(), disabled: bulk_busy() || health_checking(), onclick: move |_| pending.set(Some(Confirmation::DeleteSelected(selected()))) }
                                    Button { label: "Clear".to_owned(), variant: "ghost", small: true, disabled: bulk_busy() || health_checking(), onclick: move |_| selected.set(Vec::new()) }
                                }
                            }
                        }
                    }
                    if list.is_empty() {
                        div { class: "text-center py-10",
                            p { class: "text-text-main font-medium mb-1", "No proxy pool entries yet" }
                            p { class: "text-sm text-text-muted mb-4", "Create a proxy pool entry, then assign it to connections." }
                            Button { label: "Add Proxy Pool".to_owned(), icon: "add".to_owned(), onclick: move |_| open_create.call(()) }
                        }
                    } else {
                        div { class: "flex flex-col divide-y divide-black/[0.04] dark:divide-white/[0.05]",
                            for pool in list {
                                { let id = text(&pool, "id").to_owned(); let name = text(&pool, "name").to_owned(); let url = text(&pool, "proxyUrl").to_owned(); let no_proxy = text(&pool, "noProxy").to_owned(); let active = pool["isActive"] == true; let active_variant = if active { "success" } else { "default" }; let state = text(&pool, "testStatus"); let state = if state.is_empty() { "unknown".to_owned() } else { state.to_owned() }; let variant = match state.as_str() { "active" => "success", "error" => "error", _ => "default" }; let kind = text(&pool, "type").to_owned(); let bound = pool["boundConnectionCount"].as_u64().unwrap_or(0); let tested = format_time(text(&pool, "lastTestedAt")); let last_error = text(&pool, "lastError").to_owned(); let is_testing = testing().as_deref() == Some(id.as_str()); let is_selected = chosen.contains(&id); rsx! {
                                div { key: "{id}", class: "flex flex-col gap-3 py-3 sm:flex-row sm:items-center sm:justify-between",
                                    div { class: "flex items-start gap-3 min-w-0 flex-1",
                                        input { r#type: "checkbox", aria_label: "Select {name}", checked: is_selected, class: "mt-1 size-4 shrink-0 rounded border-black/20 dark:border-white/20", onchange: { let id = id.clone(); move |_| { if selected().contains(&id) { selected.write().retain(|item| item != &id); } else { selected.write().push(id.clone()); } } } }
                                        div { class: "min-w-0 flex-1",
                                            div { class: "flex items-center gap-2 flex-wrap",
                                                p { class: "min-w-0 max-w-full truncate text-sm font-medium sm:max-w-[18rem]", "{name}" }
                                                PoolBadge { label: state, variant: variant, small: true, dot: true }
                                                PoolBadge { label: if active { "active".to_owned() } else { "inactive".to_owned() }, variant: active_variant, small: true }
                                                if kind == "vercel" { PoolBadge { label: "vercel relay".to_owned(), small: true } }
                                                if kind == "cloudflare" { PoolBadge { label: "cloudflare relay".to_owned(), small: true } }
                                                span { class: "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-surface-card text-body px-2 py-0.5 text-[10px] tracking-wide", "{bound}" " bound" }
                                            }
                                            p { class: "text-xs text-text-muted truncate mt-1", "{url}" }
                                            if !no_proxy.is_empty() { p { class: "text-xs text-text-muted truncate", "No proxy: " "{no_proxy}" } }
                                            p { class: "text-[11px] text-text-muted mt-1", "Last tested: " "{tested}" if !last_error.is_empty() { " · {last_error}" } }
                                        }
                                    }
                                    div { class: "flex items-center justify-end gap-1",
                                        PoolToggle { checked: active, small: true, label: format!("{} {name}", if active { "Disable" } else { "Enable" }), onchange: { let id = id.clone(); move |_| toggle_active.call((id.clone(), active)) } }
                                        button { title: "Test proxy", aria_label: "Test proxy {name}", disabled: is_testing, class: "p-2 rounded hover:bg-black/5 dark:hover:bg-white/5 text-text-muted hover:text-primary", onclick: { let id = id.clone(); move |_| test_pool.call(id.clone()) },
                                            span { class: if is_testing { "material-symbols-outlined text-[18px] animate-spin" } else { "material-symbols-outlined text-[18px]" }, if is_testing { "progress_activity" } else { "science" } }
                                        }
                                        button { title: "Edit", aria_label: "Edit {name}", class: "p-2 rounded hover:bg-black/5 dark:hover:bg-white/5 text-text-muted hover:text-primary", onclick: { let id = id.clone(); let pool = pool.clone(); move |_| { editing.set(Some(id.clone())); form.set(PoolForm::from_pool(&pool)); form_open.set(true); } }, span { class: "material-symbols-outlined text-[18px]", "edit" } }
                                        button { title: "Delete", aria_label: "Delete {name}", class: "p-2 rounded hover:bg-red-500/10 text-red-500", onclick: { let id = id.clone(); let name = name.clone(); move |_| pending.set(Some(Confirmation::Delete { id: id.clone(), name: name.clone() })) }, span { class: "material-symbols-outlined text-[18px]", "delete" } }
                                    }
                                }
                            } }
                            }
                        }
                    }
                }
            }
        }
        if import_open() { Modal { title: "Batch Import Proxies".to_owned(), on_close: move |_| { if !importing() { import_open.set(false); import_text.set(String::new()); } },
            div { class: "flex flex-col gap-4",
                div { label { r#for: "proxy-batch-input", class: "text-sm font-medium text-text-main mb-1 block", "Paste Proxy List (One per line)" }
                    textarea { id: "proxy-batch-input", value: "{import_text}", oninput: move |event| import_text.set(event.value()), placeholder: "http://user:pass@127.0.0.1:7897\n127.0.0.1:7897:user:pass", class: "w-full min-h-[180px] py-2 px-3 text-sm text-text-main bg-white dark:bg-white/5 border border-black/10 dark:border-white/10 rounded-md focus:ring-1 focus:ring-primary/30 focus:border-primary/50 focus:outline-none transition-all" }
                    p { class: "text-xs text-text-muted mt-1", "Supported formats: protocol://user:pass@host:port, host:port:user:pass" }
                }
                div { class: "grid grid-cols-1 gap-2 sm:grid-cols-2",
                    Button { label: if importing() { "Importing...".to_owned() } else { "Import".to_owned() }, full_width: true, disabled: js_trim(&import_text()).is_empty() || importing(), onclick: move |_| batch_import.call(()) }
                    Button { label: "Cancel".to_owned(), full_width: true, variant: "ghost", disabled: importing(), onclick: move |_| { import_open.set(false); import_text.set(String::new()); } }
                }
            }
        } }
        if vercel_open() { Modal { title: "Deploy Vercel Relay".to_owned(), on_close: move |_| { if !deploying() { vercel_open.set(false); vercel_token.set(String::new()); } },
            div { class: "flex flex-col gap-4",
                div { class: "rounded-lg bg-blue-500/5 border border-blue-500/10 p-3 flex flex-col gap-1.5",
                    p { class: "text-sm text-text-main font-medium", "What is Vercel Relay?" }
                    p { class: "text-xs text-text-muted", "Deploys an edge relay function to Vercel. All AI provider requests will be forwarded through Vercel's edge network, masking your real IP from providers." }
                    ul { class: "text-xs text-text-muted list-disc pl-4 space-y-0.5",
                        li { "Your IP is replaced by Vercel's dynamic edge IPs (hundreds of IPs across 20+ global regions)" }
                        li { "Vercel serves millions of apps — providers can't block Vercel IPs without affecting legitimate traffic" }
                        li { "Free tier: 100GB bandwidth/month, 500K edge invocations" }
                        li { "Deploy multiple relays on different accounts for more IP diversity" }
                    }
                }
                div { class: "flex flex-col gap-1.5",
                    label { r#for: "vercel-token", class: "type-body-sm-medium text-ink", "Vercel API Token" }
                    div { class: "relative", input { id: "vercel-token", r#type: "password", placeholder: "your-vercel-api-token", value: "{vercel_token}", class: INPUT_CLASS, oninput: move |event| vercel_token.set(event.value()) } }
                    p { class: "type-body-sm text-slate", "Token is used once for deployment and not stored. " a { href: "https://vercel.com/account/tokens", target: "_blank", rel: "noopener noreferrer", class: "text-primary hover:underline", "Get token →" } }
                }
                PoolInput { id: "vercel-project-name", label: "Project Name", placeholder: "my-relay", value: project_name(), hint: "Unique name for your Vercel project. Leave empty for auto-generated name.", oninput: move |value| project_name.set(value) }
                div { class: "grid grid-cols-1 gap-2 sm:grid-cols-2",
                    Button { label: if deploying() { "Deploying... (may take ~1 min)".to_owned() } else { "Deploy".to_owned() }, full_width: true, disabled: js_trim(&vercel_token()).is_empty() || deploying(), onclick: move |_| deploy.call(()) }
                    Button { label: "Cancel".to_owned(), full_width: true, variant: "ghost", disabled: deploying(), onclick: move |_| { vercel_open.set(false); vercel_token.set(String::new()); } }
                }
            }
        } }
        if form_open() { Modal { title: if editing().is_some() { "Edit Proxy Pool".to_owned() } else { "Add Proxy Pool".to_owned() }, on_close: move |_| { form_open.set(false); editing.set(None); form.set(PoolForm::new()); },
            div { class: "flex flex-col gap-4",
                PoolInput { id: "pool-name", label: "Name", placeholder: "Office Proxy", value: form().name, oninput: move |value| form.write().name = value }
                PoolInput { id: "pool-url", label: "Proxy URL", placeholder: "http://127.0.0.1:7897", value: form().proxy_url, oninput: move |value| form.write().proxy_url = value }
                PoolInput { id: "pool-no-proxy", label: "No Proxy", placeholder: "localhost,127.0.0.1,.internal", value: form().no_proxy, hint: "Comma-separated hosts/domains to bypass proxy", oninput: move |value| form.write().no_proxy = value }
                div { class: "flex flex-col gap-3 rounded-lg border border-border/50 p-3 sm:flex-row sm:items-center sm:justify-between",
                    div { p { class: "font-medium text-sm", "Active" } p { class: "text-xs text-text-muted", "Inactive pools are ignored by runtime resolution." } }
                    PoolToggle { checked: form().is_active, label: "Active".to_owned(), disabled: saving(), onchange: move |value| form.write().is_active = value }
                }
                div { class: "flex flex-col gap-3 rounded-lg border border-border/50 p-3 sm:flex-row sm:items-center sm:justify-between",
                    div { p { class: "font-medium text-sm", "Strict Proxy" } p { class: "text-xs text-text-muted", "Fail request if proxy is unreachable instead of falling back to direct." } }
                    PoolToggle { checked: form().strict_proxy, label: "Strict Proxy".to_owned(), disabled: saving(), onchange: move |value| form.write().strict_proxy = value }
                }
                div { class: "grid grid-cols-1 gap-2 sm:grid-cols-2",
                    Button { label: if saving() { "Saving...".to_owned() } else { "Save".to_owned() }, full_width: true, disabled: js_trim(&form().name).is_empty() || js_trim(&form().proxy_url).is_empty() || saving(), onclick: move |_| save.call(()) }
                    Button { label: "Cancel".to_owned(), full_width: true, variant: "ghost", disabled: saving(), onclick: move |_| { form_open.set(false); editing.set(None); form.set(PoolForm::new()); } }
                }
            }
        } }
        if let Some(action) = pending() {
            { let (title, message) = match &action {
                Confirmation::Delete { name, .. } => ("Delete Proxy Pool", format!("Delete proxy pool \"{name}\"?")),
                Confirmation::DeleteSelected(ids) => ("Delete Proxy Pools", format!("Delete {} proxy pool(s)?", ids.len())),
                Confirmation::DisableDead { ids, alive } => ("Disable Dead Proxies", format!("Alive: {alive}, Dead: {}.\n\nDisable {} dead proxies?", ids.len(), ids.len())),
            }; rsx! {
                Modal { title: title.to_owned(), small: true, on_close: move |_| pending.set(None), footer: rsx! {
                    Button { label: "Cancel".to_owned(), variant: "ghost", onclick: move |_| pending.set(None) }
                    Button { label: "Confirm".to_owned(), variant: "danger", onclick: move |_| confirm.call(()) }
                }, p { class: "text-text-muted", "{message}" } }
            } }
        }
    }
}
