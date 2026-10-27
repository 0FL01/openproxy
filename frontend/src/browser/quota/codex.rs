//! Codex reset-credit controls use the existing quota endpoint and explicit confirmation.
use super::{
    dashboard_get_no_store, dashboard_request, encoded, fetch_quota, json_body, notify, text,
    Button, Modal, QuotaResult, Toast, Value,
};
use dioxus::prelude::*;
use std::collections::HashMap;

#[derive(Clone, Default, PartialEq)]
pub(super) struct State {
    preparing: Option<String>,
    resetting: Option<String>,
    confirmation: Option<(Value, Value)>,
    detail: Option<Detail>,
}

impl State {
    pub(super) fn resetting(&self, id: &str) -> bool {
        self.resetting.as_deref() == Some(id)
    }
}

#[derive(Clone, PartialEq)]
struct Detail {
    connection: Value,
    loading: bool,
    error: String,
    data: Value,
}

fn label(connection: &Value) -> String {
    ["name", "email", "displayName"]
        .into_iter()
        .map(|key| text(connection, key).trim())
        .find(|value| !value.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn count(value: &Value) -> f64 {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .filter(|n: &f64| n.is_finite())
        .unwrap_or_default()
        .max(0.0)
}

fn credit_date(value: &Value) -> String {
    let Some(value) = value.as_str().filter(|value| !value.is_empty()) else {
        return "N/A".into();
    };
    let date = js_sys::Date::new(&value.into());
    if !date.get_time().is_finite() {
        return "N/A".into();
    }
    let options = js_sys::Object::new();
    for (key, value) in [
        ("month", "short"),
        ("day", "numeric"),
        ("year", "numeric"),
        ("hour", "numeric"),
        ("minute", "2-digit"),
    ] {
        let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    let locale = web_sys::window()
        .and_then(|w| w.navigator().language())
        .unwrap_or_else(|| "en-US".into());
    date.to_locale_string(&locale, &options.into())
        .as_string()
        .unwrap_or_default()
}

fn remaining(value: &Value) -> String {
    let Some(value) = value.as_str().filter(|value| !value.is_empty()) else {
        return "N/A".into();
    };
    let diff = js_sys::Date::new(&value.into()).get_time() - js_sys::Date::now();
    if !diff.is_finite() {
        return "N/A".into();
    }
    if diff <= 0.0 {
        return "Expired".into();
    }
    let total = (diff / 3_600_000.0).ceil() as u64;
    if total >= 24 {
        format!("{}d {}h", total / 24, total % 24)
    } else {
        format!("{total}h")
    }
}

async fn credits(id: &str) -> Result<Value, String> {
    let path = format!("/api/usage/{}/codex-reset-credits", encoded(id));
    let response = dashboard_get_no_store(&path)
        .await
        .map_err(|_| "Failed to fetch".to_owned())?;
    let ok = response.ok();
    let mut data = json_body(&response).await.unwrap_or_default();
    if !ok {
        return Err(["error", "message"]
            .into_iter()
            .map(|key| text(&data, key))
            .find(|value| !value.is_empty())
            .unwrap_or("Failed to load Codex reset credits")
            .to_owned());
    }
    let mut items = data["credits"].as_array().cloned().unwrap_or_default();
    let expiry = |value: &Value| {
        value["expiresAt"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| js_sys::Date::new(&s.into()).get_time())
            .filter(|n| n.is_finite())
            .unwrap_or(f64::INFINITY)
    };
    items.sort_by(|a, b| expiry(a).total_cmp(&expiry(b)));
    data = serde_json::json!({"availableCount": data["availableCount"].clone(), "credits": items});
    Ok(data)
}

#[component]
pub(super) fn Buttons(
    connection: Value,
    data: Value,
    loading: bool,
    busy: bool,
    mut state: Signal<State>,
) -> Element {
    let id = text(&connection, "id").to_owned();
    let available = count(&data["resetCredits"]["availableCount"]);
    let current = state();
    let preparing = current.preparing.as_deref() == Some(id.as_str());
    let resetting = current.resetting.as_deref() == Some(id.as_str());
    let tooltip = if available > 0.0 {
        format!("Use one Codex reset credit. Available: {available}")
    } else {
        "No Codex reset credits available".to_owned()
    };
    let aria = if preparing {
        "Loading Codex reset credit details.".to_owned()
    } else if available > 0.0 {
        format!("Use one Codex reset credit. {available} available.")
    } else {
        "No Codex reset credits available".to_owned()
    };
    let toasts = use_context::<Signal<Vec<Toast>>>();
    rsx! {
        div { class: "relative inline-flex group",
            button { r#type: "button", aria_label: "{aria}", disabled: available <= 0.0 || loading || busy || current.preparing.is_some(), class: if available > 0.0 { "flex h-8 min-w-10 items-center justify-center gap-1 rounded-lg border px-2 text-[11px] font-medium tabular-nums transition-colors disabled:cursor-not-allowed disabled:opacity-60 border-primary/30 bg-primary/5 text-primary hover:bg-primary/10" } else { "flex h-8 min-w-10 items-center justify-center gap-1 rounded-lg border px-2 text-[11px] font-medium tabular-nums transition-colors disabled:cursor-not-allowed disabled:opacity-60 border-black/10 bg-black/[0.02] text-text-muted dark:border-white/10 dark:bg-white/[0.03]" }, onclick: { let connection = connection.clone(); let id = id.clone(); move |_| {
                if state.peek().preparing.is_some() || state.peek().resetting.is_some() { return; }
                state.write().preparing = Some(id.clone());
                let connection = connection.clone(); let id = id.clone();
                spawn(async move {
                    match credits(&id).await {
                        Ok(data) if count(&data["availableCount"]) > 0.0 => state.write().confirmation = Some((connection, data)),
                        Ok(_) => notify(toasts, "No Codex reset credits available", true),
                        Err(error) => notify(toasts, &error, true),
                    }
                    state.write().preparing = None;
                });
            } }, span { class: if preparing || resetting { "material-symbols-outlined text-[15px] animate-spin" } else { "material-symbols-outlined text-[15px]" }, if preparing || resetting { "progress_activity" } else { "restart_alt" } } span { "{available}" } }
            div { class: "pointer-events-none absolute bottom-full left-1/2 -translate-x-1/2 mb-1.5 z-50 w-max max-w-56 rounded px-2 py-1 text-[11px] leading-snug bg-gray-900 text-white opacity-0 group-hover:opacity-100 transition-opacity duration-150 whitespace-normal", "{tooltip}" }
        }
        div { class: "relative inline-flex group",
            button { r#type: "button", aria_label: "View Codex reset credit expiry", disabled: loading || busy, class: "flex h-8 w-8 items-center justify-center rounded-lg border border-black/10 text-text-muted transition-colors hover:bg-black/5 hover:text-primary disabled:cursor-not-allowed disabled:opacity-50 dark:border-white/10 dark:hover:bg-white/5", onclick: { let connection = connection.clone(); let id = id.clone(); move |_| {
                state.write().detail = Some(Detail { connection: connection.clone(), loading: true, error: String::new(), data: Value::Null });
                let connection = connection.clone(); let id = id.clone();
                spawn(async move {
                    let result = credits(&id).await;
                    state.write().detail = Some(match result { Ok(data) => Detail { connection, loading: false, error: String::new(), data }, Err(error) => Detail { connection, loading: false, error, data: Value::Null } });
                });
            } }, span { class: "material-symbols-outlined text-[17px]", "schedule" } }
            div { class: "pointer-events-none absolute bottom-full left-1/2 -translate-x-1/2 mb-1.5 z-50 w-max max-w-56 rounded px-2 py-1 text-[11px] leading-snug bg-gray-900 text-white opacity-0 group-hover:opacity-100 transition-opacity duration-150 whitespace-normal", "View Codex reset credit expiry" }
        }
    }
}

#[component]
pub(super) fn Dialogs(
    mut state: Signal<State>,
    mut quotas: Signal<HashMap<String, QuotaResult>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<Toast>>>();
    let current = state();
    rsx! {
        if let Some((connection, data)) = current.confirmation {
            { let id = text(&connection, "id").to_owned(); let name = label(&connection); let count = count(&data["availableCount"]); let available: Vec<Value> = data["credits"].as_array().cloned().unwrap_or_default().into_iter().filter(|credit| text(credit, "status").eq_ignore_ascii_case("available")).collect(); rsx! {
                Modal { title: "Reset Codex limit?".to_owned(), small: true, on_close: move |_| { if state.peek().resetting.is_none() { state.write().confirmation = None; } }, footer: rsx! {
                    Button { label: "Cancel".to_owned(), variant: "ghost", disabled: current.resetting.is_some(), onclick: move |_| state.write().confirmation = None }
                    Button { label: "Reset limit".to_owned(), variant: "danger", loading: current.resetting.is_some(), onclick: { let id = id.clone(); move |_| {
                        if state.peek().resetting.is_some() { return; }
                        state.write().resetting = Some(id.clone());
                        quotas.write().entry(id.clone()).or_default().error.clear();
                        let id = id.clone();
                        spawn(async move {
                            let path = format!("/api/usage/{}/codex-reset-credits", encoded(&id));
                            let result = match dashboard_request("POST", &path, None).await {
                                Ok(response) => { let ok = response.ok(); let data = json_body(&response).await.unwrap_or_default(); if ok { Ok(data) } else { Err(["message", "error", "code"].into_iter().map(|key| text(&data, key)).find(|value| !value.is_empty()).unwrap_or("Failed to reset Codex limit").to_owned()) } },
                                Err(_) => Err("Failed to fetch".to_owned()),
                            };
                            match result {
                                Ok(data) => { notify(toasts, if data["localOnly"] == true { "Local Codex rate-limit state cleared" } else { "Codex rate-limit reset credit used" }, false); fetch_quota(id, quotas).await; }
                                Err(error) => { quotas.write().entry(id).or_default().error = error.clone(); notify(toasts, &error, true); }
                            }
                            let mut current = state.write(); current.resetting = None; current.confirmation = None;
                        });
                    } } }
                }, p { class: "text-text-muted",
                    span { "Use 1 Codex reset credit for " if name.is_empty() { "this account" } else { "{name}" } ". This cannot be undone. Remaining credits: {count}." }
                    br {} br {} strong { "Available reset credits, soonest expiry first:" }
                    if available.is_empty() { span { br {} "No individual available reset details were returned." } } else { for (index, credit) in available.iter().enumerate() { span { key: "{index}", br {} "#" "{index + 1}" ": Expires " "{credit_date(&credit[\"expiresAt\"])}" if credit["expiresAt"].as_str().is_some_and(|value| !value.is_empty()) { " ({remaining(&credit[\"expiresAt\"])} remaining)" } } } }
                    br {} span { class: "text-xs", "The list order does not indicate which credit the reset API will consume." }
                } }
            } }
        }
        if let Some(detail) = current.detail {
            { let name = label(&detail.connection); let credits = detail.data["credits"].as_array().cloned().unwrap_or_default(); rsx! {
                div { class: "fixed inset-0 z-50 flex items-center justify-center bg-black/60 px-4 backdrop-blur-sm",
                    div { class: "w-full max-w-2xl overflow-hidden rounded-2xl border border-black/15 bg-white shadow-2xl ring-1 ring-black/10 dark:border-white/15 dark:bg-neutral-950 dark:ring-white/10",
                        div { class: "flex items-start justify-between gap-3 border-b border-black/10 bg-black/[0.03] px-4 py-3 dark:border-white/10 dark:bg-white/[0.04]",
                            div { class: "min-w-0", h3 { class: "text-base font-semibold text-text-primary", "Codex Reset Credit Expiry" } p { class: "mt-0.5 truncate text-xs text-text-muted", if name.is_empty() { "Codex account" } else { "{name}" } } }
                            button { r#type: "button", aria_label: "Close reset credit expiry modal", class: "flex h-8 w-8 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-black/5 hover:text-text-primary dark:hover:bg-white/5", onclick: move |_| state.write().detail = None, span { class: "material-symbols-outlined text-[18px]", "close" } }
                        }
                        div { class: "max-h-[70vh] overflow-auto bg-white p-4 dark:bg-neutral-950",
                            if detail.loading { div { class: "flex items-center justify-center gap-2 py-10 text-sm text-text-muted", span { class: "material-symbols-outlined animate-spin text-[20px]", "progress_activity" } "Loading reset credits..." } }
                            else if !detail.error.is_empty() { div { class: "rounded-xl border border-red-500/20 bg-red-500/10 px-3 py-2 text-sm text-red-600 dark:text-red-300", "{detail.error}" } }
                            else if credits.is_empty() { div { class: "rounded-xl border border-black/10 bg-black/[0.02] px-3 py-8 text-center text-sm text-text-muted dark:border-white/10 dark:bg-white/[0.03]", "No reset credit details returned for this account." } }
                            else { div { class: "space-y-3", div { class: "flex items-center justify-between rounded-xl border border-black/10 bg-black/[0.02] px-3 py-2 text-xs text-text-muted dark:border-white/10 dark:bg-white/[0.03]", span { "{credits.len()} reset credit" if credits.len() != 1 { "s" } } span { "{count(&detail.data[\"availableCount\"])} available" } } div { class: "overflow-x-auto rounded-xl border border-black/10 dark:border-white/10", table { class: "w-full min-w-[560px] text-left text-sm", thead { class: "bg-black/[0.03] text-xs uppercase tracking-wide text-text-muted dark:bg-white/[0.04]", tr { for heading in ["Status", "Granted At", "Expires At", "Remaining"] { th { class: "px-3 py-2 font-medium", "{heading}" } } } } tbody { for (index, credit) in credits.iter().enumerate() { tr { key: "{index}", class: "border-t border-black/5 dark:border-white/5", td { class: "px-3 py-2", span { class: "rounded-full bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary", if text(credit, "status").is_empty() { "unknown" } else { "{text(credit, \"status\")}" } } } td { class: "px-3 py-2 text-text-muted", "{credit_date(&credit[\"grantedAt\"])}" } td { class: "px-3 py-2 text-text-primary", "{credit_date(&credit[\"expiresAt\"])}" } td { class: "px-3 py-2 font-medium text-text-primary", "{remaining(&credit[\"expiresAt\"])}" } } } } } } } }
                        }
                    }
                }
            } }
        }
    }
}
