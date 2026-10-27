//! Application request logs and observed throughput, using the existing APIs.
mod drawer;
mod pagination;
mod tps;

use super::{
    json_body, request_with_cache,
    shell::{dashboard_get_no_store, SessionRecheck},
    ui::{Button, CARD_CLASS},
    AbortOnDrop,
};
use dioxus::{core::Task, prelude::*};
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};
use std::rc::Rc;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{AbortController, Event};

const PREFERENCES: &str = "openproxy.logs.tps-preferences.v1";
const CARD_NONE: &str = "bg-canvas border border-hairline text-ink rounded-mini-lg shadow-none";

#[derive(Clone, Default, PartialEq)]
struct Filters {
    api_key_id: String,
    status: String,
    provider: String,
    model: String,
}

#[derive(Clone, Default, PartialEq)]
struct Preferences {
    hidden: bool,
    hidden_providers: Vec<Value>,
}

fn preferences() -> Preferences {
    let raw = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(PREFERENCES).ok().flatten());
    let Some(value) = raw.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()) else {
        return Preferences::default();
    };
    let Some(object) = value.as_object().filter(|object| object.len() == 2) else {
        return Preferences::default();
    };
    let Some(hidden) = object.get("hidden").and_then(Value::as_bool) else {
        return Preferences::default();
    };
    let Some(providers) = object.get("hiddenProviders").and_then(Value::as_array) else {
        return Preferences::default();
    };
    if !providers.iter().all(|id| id.is_null() || id.is_string()) {
        return Preferences::default();
    }
    let mut hidden_providers = Vec::new();
    for id in providers {
        if !hidden_providers.contains(id) {
            hidden_providers.push(id.clone());
        }
    }
    Preferences {
        hidden,
        hidden_providers,
    }
}

fn save_preferences(value: &Preferences) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(
            PREFERENCES,
            &json!({"hidden": value.hidden, "hiddenProviders": value.hidden_providers}).to_string(),
        );
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn number(value: &Value) -> String {
    value.as_f64().map_or_else(
        || "—".to_owned(),
        |n| {
            let locale = web_sys::window()
                .and_then(|w| w.navigator().language())
                .unwrap_or_else(|| "en-US".to_owned());
            js_sys::Number::from(n)
                .to_locale_string(&locale)
                .as_string()
                .unwrap_or_default()
        },
    )
}

fn date(value: &str) -> String {
    let locale = web_sys::window()
        .and_then(|w| w.navigator().language())
        .unwrap_or_else(|| "en-US".to_owned());
    js_sys::Date::new(&JsValue::from_str(value))
        .to_locale_string(&locale, &JsValue::UNDEFINED)
        .as_string()
        .unwrap_or_default()
}

fn encoded(value: &str) -> String {
    js_sys::encode_uri_component(value)
        .as_string()
        .unwrap_or_default()
}

fn query(page: usize, page_size: usize, filters: &Filters, include_status: bool) -> String {
    let mut query = format!("page={page}&pageSize={page_size}");
    for (key, value) in [
        ("apiKeyId", &filters.api_key_id),
        ("status", &filters.status),
        ("provider", &filters.provider),
        ("model", &filters.model),
    ] {
        let value = if include_status {
            value.as_str()
        } else {
            value.trim()
        };
        if !value.is_empty() && (include_status || key != "status") {
            query.push_str(&format!("&{key}={}", encoded(value)));
        }
    }
    query
}

async fn get(path: &str, recheck: &SessionRecheck, label: &str) -> Result<Value, String> {
    let controller = AbortController::new().map_err(|_| "Failed to fetch".to_owned())?;
    let _abort = AbortOnDrop(controller.clone());
    let response = request_with_cache("GET", path, None, Some(&controller.signal()), true)
        .await
        .map_err(|_| "Failed to fetch".to_owned())?;
    if response.status() == 401 {
        recheck.call();
    }
    if !response.ok() {
        return Err(format!("{label} ({})", response.status()));
    }
    json_body(&response)
        .await
        .ok_or_else(|| format!("Invalid {label} response"))
}

struct RefreshListeners {
    window: web_sys::Window,
    document: web_sys::Document,
    focus: Closure<dyn FnMut(Event)>,
    visibility: Closure<dyn FnMut(Event)>,
}

impl RefreshListeners {
    fn new(mut refresh: Signal<u64>) -> Option<Self> {
        let window = web_sys::window()?;
        let document = window.document()?;
        let focus = Closure::<dyn FnMut(Event)>::new(move |_| refresh += 1);
        let visible_document = document.clone();
        let visibility = Closure::<dyn FnMut(Event)>::new(move |_| {
            if visible_document.visibility_state() == web_sys::VisibilityState::Visible {
                refresh += 1;
            }
        });
        let _ = window.add_event_listener_with_callback("focus", focus.as_ref().unchecked_ref());
        let _ = document.add_event_listener_with_callback(
            "visibilitychange",
            visibility.as_ref().unchecked_ref(),
        );
        Some(Self {
            window,
            document,
            focus,
            visibility,
        })
    }
}

impl Drop for RefreshListeners {
    fn drop(&mut self) {
        let _ = self
            .window
            .remove_event_listener_with_callback("focus", self.focus.as_ref().unchecked_ref());
        let _ = self.document.remove_event_listener_with_callback(
            "visibilitychange",
            self.visibility.as_ref().unchecked_ref(),
        );
    }
}

fn visible() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_none_or(|document| document.visibility_state() == web_sys::VisibilityState::Visible)
}

#[component]
pub(super) fn ApplicationLogsPage() -> Element {
    let mut filters = use_signal(Filters::default);
    let mut page = use_signal(|| 1_usize);
    let mut page_size = use_signal(|| 20_usize);
    let mut refresh = use_signal(|| 0_u64);
    let mut prefs = use_signal(preferences);
    let mut logs = use_signal(Vec::<Value>::new);
    let mut keys = use_signal(Vec::<Value>::new);
    let mut selected = use_signal(|| None::<Value>);
    let mut loading = use_signal(|| true);
    let mut error = use_signal(String::new);
    let mut total = use_signal(|| 0_usize);
    let mut task = use_signal(|| None::<Task>);
    let recheck = use_context::<SessionRecheck>();
    let _listeners = use_hook(move || RefreshListeners::new(refresh).map(Rc::new));
    use_effect(move || save_preferences(&prefs()));

    use_future(move || async move {
        if let Ok(response) = dashboard_get_no_store("/api/keys").await
            && response.ok()
            && let Some(data) = json_body(&response).await
        {
            keys.set(
                data["keys"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|key| json!({"id":key["id"],"name":key["name"]}))
                    .collect(),
            );
        }
    });

    {
        let recheck = recheck.clone();
        use_effect(move || {
            let filters = filters();
            let page = page();
            let size = page_size();
            let _refresh = refresh();
            if let Some(previous) = *task.peek() {
                previous.cancel();
            }
            let recheck = recheck.clone();
            let next = spawn(async move {
                let path = format!("/api/request-logs?{}", query(page, size, &filters, true));
                let mut first = true;
                loop {
                    if visible() {
                        if first {
                            loading.set(true);
                        }
                        match get(&path, &recheck, "Failed to load logs").await {
                            Ok(data) => {
                                let rows = data["requests"].as_array().cloned().unwrap_or_default();
                                let selected_id = selected
                                    .peek()
                                    .as_ref()
                                    .map(|current| current["requestId"].clone());
                                if let Some(selected_id) = selected_id
                                    && let Some(updated) =
                                        rows.iter().find(|row| row["requestId"] == selected_id)
                                {
                                    selected.set(Some(updated.clone()));
                                }
                                logs.set(rows);
                                total
                                    .set(data["pagination"]["totalItems"].as_u64().unwrap_or(0)
                                        as usize);
                                error.set(String::new());
                            }
                            Err(message) => error.set(message),
                        }
                        loading.set(false);
                    }
                    first = false;
                    if page != 1 {
                        break;
                    }
                    TimeoutFuture::new(60_000).await;
                }
            });
            task.set(Some(next));
        });
    }

    let mut update_filter = move |name: &'static str, value: String| {
        page.set(1);
        let mut current = filters();
        match name {
            "key" => current.api_key_id = value,
            "status" => current.status = value,
            "provider" => current.provider = value,
            _ => current.model = value,
        }
        filters.set(current);
    };
    let f = filters();
    let preferences = prefs();
    let tps_scope = format!(
        "{}\u{0}{}\u{0}{}",
        f.api_key_id.trim(),
        f.provider.trim(),
        f.model.trim()
    );
    rsx! {
        div { class: "flex min-w-0 flex-col gap-5 px-1 sm:px-0",
            div { class: "flex flex-col justify-between gap-3 sm:flex-row sm:items-center",
                div { h1 { class: "text-2xl font-semibold text-text-main", "Application Logs" } p { class: "text-sm text-text-muted", "Provider attempts attributed to API keys · refreshes every 60 seconds" } }
                Button { label: "Refresh".to_owned(), variant: "outline", icon: "refresh".to_owned(), onclick: move |_| refresh += 1 }
            }
            div { class: CARD_CLASS,
                div { class: "grid grid-cols-1 items-end gap-3 sm:grid-cols-4",
                    select { value: "{f.api_key_id}", class: "h-10 rounded-lg border border-border bg-surface px-3 text-sm text-text-main", onchange: move |e| update_filter("key", e.value()), option { value: "", "All API keys" } for key in keys() { option { key: "{text(&key, \"id\")}", value: "{text(&key, \"id\")}", "{text(&key, \"name\")}" } } }
                    label { class: "flex flex-col gap-1", span { class: "text-xs text-text-muted", "Status (raw logs only)" } select { value: "{f.status}", class: "h-10 rounded-lg border border-border bg-surface px-3 text-sm text-text-main", onchange: move |e| update_filter("status", e.value()), option { value: "", "All statuses" } option { value: "pending", "Live" } option { value: "success", "Success" } option { value: "error", "Error" } option { value: "interrupted", "Interrupted" } } }
                    input { value: "{f.provider}", placeholder: "Provider", class: "h-10 rounded-lg border border-border bg-surface px-3 text-sm text-text-main", oninput: move |e| update_filter("provider", e.value()) }
                    input { value: "{f.model}", placeholder: "Model", class: "h-10 rounded-lg border border-border bg-surface px-3 text-sm text-text-main", oninput: move |e| update_filter("model", e.value()) }
                }
                label { class: "mt-3 flex w-fit cursor-pointer items-center gap-2 text-sm text-text-main", input { r#type: "checkbox", checked: preferences.hidden, class: "h-4 w-4 accent-primary", onchange: move |e| { let mut value = prefs(); value.hidden = e.checked(); prefs.set(value); } } "Hide Median TPS" }
            }
            if !preferences.hidden { tps::TpsPanel { key: "{tps_scope}", filters: f.clone(), refresh, preferences: prefs } }
            if !error().is_empty() { div { class: "rounded-lg border border-error/25 bg-error/5 px-4 py-3 text-sm text-error", "{error}" } }
            div { class: CARD_NONE,
                div { class: "overflow-x-auto", table { class: "w-full min-w-[1000px] text-sm",
                    thead { tr { class: "border-b border-border text-left text-xs uppercase tracking-wide text-text-muted", for header in ["Status", "Model", "Provider", "Route", "API key", "Tokens", "TPS", "Duration", "Time"] { th { class: if ["Tokens", "TPS", "Duration"].contains(&header) { "p-4 text-right" } else { "p-4" }, "{header}" } } } }
                    tbody {
                        if loading() && logs().is_empty() { tr { td { colspan: "9", class: "p-10 text-center text-text-muted", "Loading logs…" } } }
                        else if logs().is_empty() { tr { td { colspan: "9", class: "p-10 text-center text-text-muted", "No application logs yet." } } }
                        else { for log in logs() { { let status = text(&log, "status"); let status_class = match status { "success" => "bg-success/10 text-success", "error" => "bg-error/10 text-error", "pending" => "bg-primary/10 text-primary animate-pulse", _ => "bg-warning/10 text-warning" }; let status_label = if status == "pending" { "LIVE".to_owned() } else { status.to_uppercase() }; rsx! {
                            tr { key: "{text(&log, \"requestId\")}", class: "cursor-pointer border-b border-border/60 transition-colors hover:bg-primary/5", onclick: { let log = log.clone(); move |_| selected.set(Some(log.clone())) },
                                td { class: "p-4", span { class: "rounded-full px-2 py-1 text-xs font-semibold {status_class}", "{status_label}" } if status == "error" && !text(&log, "errorKind").is_empty() { div { class: "mt-1 font-mono text-xs text-text-muted", "{text(&log, \"errorKind\")}" } } }
                                td { class: "max-w-[240px] p-4", div { class: "truncate font-mono text-text-main", "{text(&log, \"model\")}" } }
                                td { class: "p-4 text-text-main", "{log[\"provider\"].as_str().filter(|v| !v.is_empty()).unwrap_or(\"—\")}" }
                                td { class: "p-4 font-mono text-text-main", "{log[\"route\"].as_str().filter(|v| !v.is_empty()).unwrap_or(\"—\")}" }
                                td { class: "p-4 text-text-main", div { class: "font-medium", "{log[\"apiKeyName\"].as_str().filter(|v| !v.is_empty()).unwrap_or(\"—\")}" } if !text(&log, "apiKeyId").is_empty() { div { class: "mt-1 max-w-[180px] truncate font-mono text-xs text-text-muted", "{text(&log, \"apiKeyId\")}" } } }
                                td { class: "p-4 text-right font-mono text-text-main", title: "Input / Output · Total when known", "{number(&log[\"inputTokens\"])}" " / " "{number(&log[\"outputTokens\"])}" if !log["totalTokens"].is_null() { div { class: "mt-1 text-xs text-text-muted", "Total " "{number(&log[\"totalTokens\"])}" } } }
                                td { class: "p-4 text-right font-mono text-text-main", if status == "success" { if let Some(tps) = log["tokensPerSecond"].as_f64() { "{tps:.1}" } else { "—" } } else { "—" } }
                                td { class: "p-4 text-right font-mono text-text-main", "{number(&log[\"durationMs\"])} ms" }
                                td { class: "whitespace-nowrap p-4 text-text-muted", "{date(text(&log, \"timestamp\"))}" }
                            }
                        } } } }
                    }
                } }
                if !loading() && total() > 0 { pagination::Pagination { page: page(), page_size: page_size(), total: total(), on_page: move |v| page.set(v), on_size: move |v| { page.set(1); page_size.set(v); } } }
            }
            if let Some(log) = selected() { drawer::LogDrawer { log, on_close: move |_| selected.set(None) } }
        }
    }
}
