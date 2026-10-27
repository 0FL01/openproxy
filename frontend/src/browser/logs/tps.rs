use super::{
    date, encoded, get, pagination::Pagination, query, visible, Filters, Preferences, CARD_NONE,
};
use crate::browser::shell::SessionRecheck;
use dioxus::{core::Task, prelude::*};
use gloo_timers::future::TimeoutFuture;
use serde_json::Value;

const WINDOWS: [&str; 3] = ["1h", "6h", "24h"];

#[derive(Clone, Default, PartialEq)]
struct State {
    scope: String,
    data: Option<Value>,
    loading: bool,
    error: String,
    providers: Option<Vec<Value>>,
}

fn timestamp(value: &Value) -> Option<f64> {
    let value = value.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    let timestamp = js_sys::Date::parse(value);
    timestamp.is_finite().then_some(timestamp)
}

fn count(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n <= 9_007_199_254_740_991)
}

fn group_id(value: &Value) -> bool {
    value.is_null() || value.is_string()
}

fn valid(payload: &Value, page: usize, page_size: usize) -> bool {
    let Some(providers) = payload["availableProviders"].as_array() else {
        return false;
    };
    let Some(groups) = payload["groups"].as_array() else {
        return false;
    };
    if timestamp(&payload["asOf"]).is_none()
        || !providers.iter().all(group_id)
        || providers
            .iter()
            .enumerate()
            .any(|(i, id)| providers[..i].contains(id))
    {
        return false;
    }
    if !WINDOWS.iter().all(|window| {
        let range = &payload["windows"][window];
        timestamp(&range["start"])
            .zip(timestamp(&range["end"]))
            .is_some_and(|(start, end)| start < end)
    }) {
        return false;
    }
    let pagination = &payload["pagination"];
    let Some(total) = count(&pagination["totalItems"]) else {
        return false;
    };
    let Some(pages) = count(&pagination["totalPages"]) else {
        return false;
    };
    if count(&pagination["page"]) != Some(page as u64)
        || count(&pagination["pageSize"]) != Some(page_size as u64)
        || !(1..=100).contains(&page_size)
        || page == 0
        || groups.len() > page_size
    {
        return false;
    }
    let expected_pages = total.div_ceil(page_size as u64);
    if (total == 0 && pages > 1) || (total > 0 && pages != expected_pages) {
        return false;
    }
    let expected = total
        .saturating_sub(((page - 1) * page_size) as u64)
        .min(page_size as u64);
    if groups.len() as u64 != expected {
        return false;
    }
    groups.iter().all(|group| {
        group.is_object()
            && group.get("provider").is_some_and(group_id)
            && group.get("model").is_some_and(group_id)
            && providers.contains(&group["provider"])
            && WINDOWS.iter().all(|window| {
                let observation = &group["windows"][window];
                let Some(samples) = count(&observation["sampleCount"]) else {
                    return false;
                };
                let Some(successes) = count(&observation["successfulAttemptCount"]) else {
                    return false;
                };
                samples <= successes
                    && observation.get("medianTps").is_some_and(|median| {
                        if median.is_null() {
                            samples == 0
                        } else {
                            samples > 0
                                && median.as_f64().is_some_and(|v| v.is_finite() && v >= 0.0)
                        }
                    })
            })
    })
}

#[component]
fn GroupId(value: Value) -> Element {
    if value.is_null() {
        rsx! { span { class: "italic text-text-muted", "Unknown" } }
    } else if value.as_str() == Some("") {
        rsx! { span { class: "italic text-text-muted", "Empty" } }
    } else {
        rsx! { "{value.as_str().unwrap_or(\"\")}" }
    }
}

fn format_tps(value: f64) -> String {
    let options = js_sys::Object::new();
    for (key, value) in [
        ("minimumFractionDigits", 1.into()),
        ("maximumFractionDigits", 1.into()),
        ("useGrouping", false.into()),
    ] {
        let _ = js_sys::Reflect::set(&options, &key.into(), &value);
    }
    let locales = js_sys::Array::new();
    locales.push(&"en-US".into());
    js_sys::Intl::NumberFormat::new(&locales, &options)
        .format()
        .call1(&wasm_bindgen::JsValue::UNDEFINED, &value.into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

#[component]
pub(super) fn TpsPanel(
    filters: Filters,
    refresh: Signal<u64>,
    mut preferences: Signal<Preferences>,
) -> Element {
    let mut page = use_signal(|| 1_usize);
    let mut size = use_signal(|| 20_usize);
    let mut state = use_signal(|| State {
        loading: true,
        ..State::default()
    });
    let mut task = use_signal(|| None::<Task>);
    let mut running = use_signal(|| false);
    let mut first = use_signal(|| true);
    let mut previous_refresh = use_signal(|| refresh());
    let recheck = use_context::<SessionRecheck>();
    {
        let recheck = recheck.clone();
        use_effect(move || {
            let requested_page = page();
            let size = size();
            let hidden = preferences().hidden_providers;
            let current_refresh = refresh();
            let refreshed = current_refresh != *previous_refresh.peek();
            previous_refresh.set(current_refresh);
            let scope = format!(
                "{requested_page}/{size}/{}",
                serde_json::to_string(&hidden).unwrap_or_default()
            );
            if state.peek().scope == scope && *running.peek() {
                return;
            }
            if let Some(previous) = *task.peek() {
                previous.cancel();
            }
            running.set(false);
            if state.peek().scope != scope {
                let providers = state.peek().providers.clone();
                state.set(State {
                    scope: scope.clone(),
                    data: None,
                    loading: true,
                    error: String::new(),
                    providers,
                });
            }
            let filters = filters.clone();
            let recheck = recheck.clone();
            let delay = if *first.peek() && !refreshed { 300 } else { 0 };
            let next = spawn(async move {
                if delay > 0 {
                    TimeoutFuture::new(delay).await;
                }
                let mut path = format!(
                    "/api/request-logs/tps-stats?{}",
                    query(requested_page, size, &filters, false)
                );
                if !hidden.is_empty() {
                    path.push_str(&format!(
                        "&hiddenProviders={}",
                        encoded(&serde_json::to_string(&hidden).unwrap_or_default())
                    ));
                }
                loop {
                    if visible() {
                        running.set(true);
                        first.set(false);
                        {
                            let mut current = state.write();
                            current.loading = true;
                            current.error.clear();
                        }
                        let result = get(&path, &recheck, "Failed to load TPS statistics").await;
                        match result {
                            Ok(data) if valid(&data, requested_page, size) => {
                                let pages =
                                    data["pagination"]["totalPages"].as_u64().unwrap_or(0) as usize;
                                let total = data["pagination"]["totalItems"].as_u64().unwrap_or(0);
                                if requested_page > pages.max(1)
                                    || (total == 0 && requested_page != 1)
                                {
                                    running.set(false);
                                    page.set(if total == 0 { 1 } else { pages.max(1) });
                                    return;
                                }
                                let providers = data["availableProviders"].as_array().cloned();
                                state.set(State {
                                    scope: scope.clone(),
                                    data: Some(data),
                                    loading: false,
                                    error: String::new(),
                                    providers,
                                });
                            }
                            Ok(_) => {
                                let mut current = state.write();
                                current.loading = false;
                                current.error = "Invalid TPS statistics response".to_owned();
                            }
                            Err(error) => {
                                let mut current = state.write();
                                current.loading = false;
                                current.error = error;
                            }
                        }
                        running.set(false);
                    }
                    TimeoutFuture::new(60_000).await;
                }
            });
            task.set(Some(next));
        });
    }
    let current = state();
    let data = current.data.clone();
    let groups = data
        .as_ref()
        .and_then(|value| value["groups"].as_array())
        .cloned()
        .unwrap_or_default();
    let hidden = preferences().hidden_providers;
    let total = data
        .as_ref()
        .and_then(|value| value["pagination"]["totalItems"].as_u64())
        .unwrap_or(0) as usize;
    rsx! {
        div { class: CARD_NONE, role: "region", aria_label: "Median TPS statistics",
            div { class: "border-b border-border p-4",
                div { class: "flex flex-wrap items-center justify-between gap-2", h2 { class: "text-base font-semibold text-text-main", "Median TPS by provider and model" } div { class: "flex flex-wrap items-center gap-2 text-xs text-text-muted", if let Some(data) = data.as_ref() { span { "As of " time { datetime: "{data[\"asOf\"].as_str().unwrap_or(\"\")}", title: "{data[\"asOf\"].as_str().unwrap_or(\"\")}", "{date(data[\"asOf\"].as_str().unwrap_or(\"\"))}" } } } if current.loading && data.is_some() { span { class: "text-primary", "Updating…" } } if !current.error.is_empty() { span { class: "text-error", if data.is_some() { "Stale" } else { "Unavailable" } } } } }
                p { class: "mt-1 text-xs font-medium text-text-muted", "Success-only · saved attempts, not all traffic. Status filters apply only to raw logs." }
                p { class: "mt-1 text-xs text-text-muted", "Sorted by highest-priority available window: 1h → 6h → 24h, TPS descending; models without measurements last." }
                p { class: "mt-1 text-xs text-text-muted", "Rolling windows use attempt start time. TPS is observed request throughput, including network, initial wait and backpressure, not GPU speed or a comparable benchmark. Coverage: eligible TPS samples / saved successful attempts." }
            }
            fieldset { class: "border-b border-border px-4 pb-4 pt-2", legend { class: "pt-3 text-xs font-medium text-text-muted", "Providers (TPS only)" } div { class: "flex flex-wrap gap-x-4 gap-y-2",
                if let Some(providers) = current.providers.clone() {
                    if providers.is_empty() { p { class: "text-xs text-text-muted", "No matching providers." } }
                    else { for provider in providers { label { key: "{provider}", class: "flex min-w-0 cursor-pointer items-center gap-2 text-sm text-text-main", input { r#type: "checkbox", checked: !hidden.contains(&provider), class: "h-4 w-4 shrink-0 accent-primary", onchange: { let provider = provider.clone(); move |event| { page.set(1); let mut prefs = preferences(); if event.checked() { prefs.hidden_providers.retain(|id| id != &provider); } else { prefs.hidden_providers.push(provider.clone()); } preferences.set(prefs); } } } span { class: "break-all", GroupId { value: provider.clone() } } } } }
                } else { p { class: "text-xs text-text-muted", "Provider choices will appear after TPS statistics load." } }
            } }
            if !current.error.is_empty() { div { role: "status", aria_live: "polite", class: "border-b border-error/25 bg-error/5 px-4 py-3 text-sm text-error", "{current.error}" if data.is_some() { " · Showing the last successful snapshot." } } }
            div { class: "overflow-x-auto", table { class: "w-full min-w-[600px] text-sm", aria_label: "Median TPS by provider and model",
                thead { tr { class: "border-b border-border text-left text-xs uppercase tracking-wide text-text-muted", th { scope: "col", class: "p-4", "Provider" } th { scope: "col", class: "p-4", "Model" } for window in WINDOWS { th { scope: "col", class: "p-4 text-right", title: data.as_ref().map(|value| format!("{} ≤ attempt start < {}", value["windows"][window]["start"].as_str().unwrap_or(""), value["windows"][window]["end"].as_str().unwrap_or(""))), "{window}" } } } }
                tbody {
                    if data.is_none() { tr { td { colspan: "5", class: "p-8 text-center text-text-muted", if current.loading { "Loading TPS statistics…" } else { "TPS statistics unavailable." } } } }
                    else if groups.is_empty() { tr { td { colspan: "5", class: "p-8 text-center text-text-muted", if current.providers.as_ref().is_some_and(|v| !v.is_empty()) { "All matching providers are hidden" } else { "No saved successful attempts in the last 24 hours for these filters." } } } }
                    else { for group in groups { tr { key: "{group[\"provider\"]}/{group[\"model\"]}", class: "border-b border-border/60", th { scope: "row", class: "max-w-[200px] break-words p-4 text-left font-normal text-text-main", GroupId { value: group["provider"].clone() } } td { class: "max-w-[320px] break-all p-4 font-mono text-text-main", GroupId { value: group["model"].clone() } } for window in WINDOWS { { let observation = &group["windows"][window]; rsx! { td { key: "{window}", class: "whitespace-nowrap p-4 text-right", div { class: "font-mono text-text-main", if let Some(tps) = observation["medianTps"].as_f64() { "{format_tps(tps)}" } else { "—" } } div { class: "mt-1 font-mono text-xs text-text-muted", title: "Eligible TPS samples / saved successful attempts", "{observation[\"sampleCount\"]}" "/" "{observation[\"successfulAttemptCount\"]}" } } } } } } } }
                }
            } }
            nav { aria_label: "TPS statistics pagination", Pagination { page: page(), page_size: size(), total, on_page: move |value| page.set(value), on_size: move |value| { page.set(1); size.set(value); } } }
        }
    }
}
