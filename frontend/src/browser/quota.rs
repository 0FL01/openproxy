//! Provider quota overview. Further account controls are migrated separately.
mod codex;
use super::{
    json_body,
    providers::{
        detail::{
            auto_ping_connections, auto_ping_eligible, auto_ping_key, initialize_auto_ping,
            load_auto_ping_settings, save_auto_ping,
        },
        EditConnection,
    },
    shell::{dashboard_get_no_store, dashboard_request, notify, Toast},
    ui::{Button, Modal, Toggle},
};
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};
use std::collections::HashMap;

const SUPPORTED: &[&str] = &[
    "claude",
    "antigravity",
    "github",
    "codex",
    "kimi",
    "kimi-coding",
    "deepseek",
    "ollama",
    "glm",
    "glm-cn",
    "minimax",
    "opencode-go",
    "commandcode",
    "a6api",
];
const API_KEY: &[&str] = &[
    "glm",
    "glm-cn",
    "minimax",
    "kimi",
    "deepseek",
    "opencode-go",
    "commandcode",
    "a6api",
];

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn eligible(connection: &Value) -> bool {
    let provider = text(connection, "provider");
    SUPPORTED.contains(&provider)
        && (text(connection, "authType") == "oauth" || API_KEY.contains(&provider))
}

fn encoded(value: &str) -> String {
    js_sys::encode_uri_component(value)
        .as_string()
        .unwrap_or_default()
}

#[derive(Clone, Default, PartialEq)]
struct QuotaResult {
    loading: bool,
    data: Option<Value>,
    error: String,
}

#[derive(Clone, Default, PartialEq)]
struct AutoPing {
    ready: bool,
    saving: Vec<String>,
    configs: HashMap<String, serde_json::Map<String, Value>>,
}

async fn connections() -> Vec<Value> {
    match dashboard_request("GET", "/api/providers", None).await {
        Ok(response) if response.ok() => json_body(&response)
            .await
            .and_then(|value| value["connections"].as_array().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

async fn fetch_quota(id: String, mut quotas: Signal<HashMap<String, QuotaResult>>) {
    quotas.write().entry(id.clone()).or_default().loading = true;
    let path = format!("/api/usage/{}", encoded(&id));
    let result = match dashboard_request("GET", &path, None).await {
        Ok(response) if response.ok() => QuotaResult {
            data: json_body(&response).await,
            ..Default::default()
        },
        Ok(response) if response.status() == 404 => QuotaResult::default(),
        Ok(response) => {
            let status = response.status();
            let status_text = response.status_text();
            let body = json_body(&response).await.unwrap_or_default();
            let message = body["error"].as_str().unwrap_or(&status_text);
            if status == 401 {
                QuotaResult {
                    data: Some(json!({"message": message})),
                    ..Default::default()
                }
            } else {
                QuotaResult {
                    error: format!("HTTP {status}: {message}"),
                    ..Default::default()
                }
            }
        }
        Err(_) => QuotaResult {
            error: "Failed to fetch".into(),
            ..Default::default()
        },
    };
    quotas.write().insert(id, result);
}

async fn refresh_all(
    mut rows: Signal<Vec<Value>>,
    mut quotas: Signal<HashMap<String, QuotaResult>>,
    mut loading: Signal<bool>,
    mut refreshing: Signal<bool>,
) {
    if *refreshing.peek() {
        return;
    }
    refreshing.set(true);
    let current = connections().await;
    let ids: Vec<String> = current
        .iter()
        .filter(|row| eligible(row))
        .map(|row| text(row, "id").to_owned())
        .collect();
    {
        let mut values = quotas.write();
        for id in &ids {
            values.entry(id.clone()).or_default().loading = true;
        }
    }
    rows.set(current);
    loading.set(false);
    futures_util::future::join_all(ids.into_iter().map(|id| fetch_quota(id, quotas))).await;
    refreshing.set(false);
}

fn quota_entries(provider: &str, data: &Value) -> Vec<Value> {
    let Some(entries) = data["quotas"].as_object() else {
        return if provider == "claude" && data["message"].is_string() {
            vec![json!({"name":"error","used":0,"total":0})]
        } else {
            Vec::new()
        };
    };
    entries
        .iter()
        .filter_map(|(name, value)| {
            let mut quota = value.as_object()?.clone();
            if provider == "commandcode" {
                if quota.get("kind").and_then(Value::as_str) == Some("balance") {
                    quota.get("remaining").and_then(Value::as_f64)?;
                } else {
                    quota.get("used").and_then(Value::as_f64)?;
                    quota.get("total").and_then(Value::as_f64)?;
                }
            } else if provider == "a6api" && quota.get("unlimited") != Some(&Value::Bool(true)) {
                quota.get("used").and_then(Value::as_f64)?;
                quota.get("total").and_then(Value::as_f64)?;
            }
            let label = match provider {
                "antigravity" => quota.get("displayName"),
                "commandcode" => quota.get("label"),
                _ => None,
            }
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(name)
            .to_owned();
            quota.insert("name".into(), json!(label));
            if !matches!(
                provider,
                "antigravity" | "kimi" | "deepseek" | "commandcode" | "a6api" | "claude"
            ) {
                quota.remove("remainingPercentage");
            }
            if provider == "a6api" {
                quota.insert("recurring".into(), json!(false));
            }
            Some(Value::Object(quota))
        })
        .collect()
}

fn percentage(quota: &Value) -> f64 {
    if quota["unlimited"] == true {
        return 100.0;
    }
    if let Some(value) = quota["remainingPercentage"].as_f64() {
        return (value + 0.5).floor();
    }
    let used = quota["used"].as_f64().unwrap_or_default();
    let total = quota["total"].as_f64().unwrap_or_default();
    if total == 0.0 {
        0.0
    } else if used <= 0.0 {
        100.0
    } else if used >= total {
        0.0
    } else {
        (((total - used) / total) * 100.0 + 0.5).floor()
    }
}

fn number(value: f64) -> String {
    let locale = web_sys::window()
        .and_then(|w| w.navigator().language())
        .unwrap_or_else(|| "en-US".into());
    js_sys::Number::from(value)
        .to_locale_string(&locale)
        .as_string()
        .unwrap_or_default()
}

fn reset_label(quota: &Value) -> String {
    let Some(reset) = quota["resetAt"].as_str() else {
        return "N/A".into();
    };
    let diff = js_sys::Date::new(&reset.into()).get_time() - js_sys::Date::now();
    if !diff.is_finite() {
        return "N/A".into();
    }
    if diff <= 0.0 {
        return "Expired".into();
    }
    let minutes = (diff / 60000.0).ceil() as u64;
    let duration = if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 1440 {
        format!("{}h {}m", minutes / 60, minutes % 60)
    } else {
        format!(
            "{}d {}h {}m",
            minutes / 1440,
            (minutes / 60) % 24,
            minutes % 60
        )
    };
    format!(
        "{} in {duration}",
        if quota["recurring"] == false {
            "Expires"
        } else {
            "Reset"
        }
    )
}

#[component]
fn QuotaTable(provider: String, data: Value) -> Element {
    let entries = quota_entries(&provider, &data);
    rsx! {
        if !entries.is_empty() { div { class: "overflow-x-auto", table { class: "w-full table-fixed text-left", tbody {
            for (index, quota) in entries.into_iter().enumerate() {
                { let remaining = percentage(&quota); let balance = quota["kind"] == "balance"; let unlimited = quota["unlimited"] == true;
                  let (color, bar, light) = if remaining > 70.0 { ("text-green-600 dark:text-green-400", "bg-green-500", "bg-green-500/10") } else if remaining >= 30.0 { ("text-yellow-600 dark:text-yellow-400", "bg-yellow-500", "bg-yellow-500/10") } else { ("text-red-600 dark:text-red-400", "bg-red-500", "bg-red-500/10") };
                  let used = number(quota["used"].as_f64().unwrap_or_default()); let total = quota["total"].as_f64().unwrap_or_default(); let label = reset_label(&quota);
                  let dot = if balance { "bg-text-muted" } else { bar }; let border = if remaining == 0.0 { "border-black/10 dark:border-white/10" } else { "border-transparent" }; let total_label = if total > 0.0 { number(total) } else { "∞".into() }; let balance_value = quota["remaining"].as_f64().map(number).unwrap_or_else(|| "N/A".into()); let unit = quota["unit"].as_str().unwrap_or("credit value");
                  rsx! { tr { key: "{index}", class: "border-b border-black/5 dark:border-white/5 hover:bg-black/[0.02] dark:hover:bg-white/[0.02] transition-colors",
                    td { class: "py-1 px-1.5 w-[30%]", div { class: "flex items-center gap-1.5 min-w-0", span { class: "w-2 h-2 rounded-full shrink-0 {dot}" } span { class: "text-[11px] font-medium text-text-primary truncate", "{text(&quota, \"name\")}" } } }
                    td { class: "py-1 px-1.5 w-[45%]", if unlimited { div { class: "text-[11px] font-medium text-green-600 dark:text-green-400", "Unlimited" } } else if balance { div { class: "text-[11px] font-medium text-text-primary", "{balance_value} {unit}" } } else { div { class: "space-y-1", div { class: "h-1 rounded-full overflow-hidden border {light} {border}", div { class: "h-full transition-all duration-300 {bar}", style: "width: {remaining.min(100.0)}%" } } div { class: "flex items-center justify-between text-[10px]", span { class: "text-text-muted", "{used} / {total_label}" if let Some(unit) = quota["unit"].as_str().filter(|s| !s.is_empty()) { " {unit}" } } span { class: "font-medium {color}", "{remaining}%" } } } } }
                    td { class: "py-1 px-1.5 w-[25%]", if balance { div { class: "text-[11px] text-text-muted", "Available" } } else if label == "N/A" { div { class: "text-[11px] text-text-muted italic", "N/A" } } else { div { class: "text-[11px] text-text-primary font-medium truncate", "{label}" } } }
                  } }
                }
            }
        } } } }
    }
}

#[component]
fn QuotaCard(
    connection: Value,
    quota: QuotaResult,
    busy: bool,
    refresh: EventHandler<String>,
    edit: EventHandler<Value>,
    delete: EventHandler<String>,
    toggle: EventHandler<(String, bool)>,
    ping: AutoPing,
    codex: Signal<codex::State>,
    toggle_ping: EventHandler<(String, String, bool)>,
) -> Element {
    let provider = text(&connection, "provider").to_owned();
    let id = text(&connection, "id").to_owned();
    let name = ["name", "email", "displayName"]
        .into_iter()
        .map(|key| text(&connection, key).trim())
        .find(|value| !value.is_empty())
        .unwrap_or("")
        .to_owned();
    let data = quota.data.unwrap_or_default();
    let entries = quota_entries(&provider, &data);
    let opacity = if connection["isActive"] == false {
        "opacity-60"
    } else {
        ""
    };
    let spin = if quota.loading { "animate-spin" } else { "" };
    let active = connection["isActive"] != false;
    let ping_eligible = auto_ping_eligible(&provider, text(&connection, "authType"))
        && (provider != "codex"
            || entries
                .iter()
                .any(|entry| matches!(text(entry, "name"), "session" | "weekly")));
    let ping_on = ping
        .configs
        .get(&provider)
        .and_then(|config| config.get(&id))
        == Some(&Value::Bool(true));
    let ping_saving = ping.saving.contains(&provider);
    let ping_tooltip = if provider == "codex" {
        "Auto-starts the next available Codex quota window after reset with a tiny gpt-6-luna request. Consumes a small amount of quota."
    } else {
        "Auto-starts the next GLM 5-hour window after reset with a tiny glm-5.3-flash request. Consumes a small amount of Coding Plan quota."
    };
    rsx! {
        div { class: "bg-canvas border border-hairline text-ink rounded-mini-lg shadow-none min-w-0 {opacity}",
            div { class: "px-3 py-2 border-b border-black/10 dark:border-white/10", div { class: "flex items-center justify-between gap-2",
                div { class: "flex items-center gap-2 min-w-0", div { class: "w-8 h-8 shrink-0 rounded-md flex items-center justify-center overflow-hidden", img { src: "/providers/{provider}.png", alt: "{provider}", width: "32", height: "32", class: "object-contain" } }
                    div { class: "min-w-0", div { class: "flex items-center gap-1.5", h3 { class: "text-sm font-semibold text-text-primary capitalize truncate", "{provider}" } if let Some(plan) = data["plan"].as_str().filter(|plan| !plan.is_empty() && *plan != "unknown") { span { class: "shrink-0 rounded bg-primary/10 px-1.5 py-0.5 text-[9px] font-medium capitalize text-primary", "{plan}" } } } if !name.is_empty() { p { class: "text-xs text-text-muted truncate", "{name}" } } }
                }
                div { class: "flex items-center gap-1 shrink-0",
                    if ping.ready && ping_eligible { div { class: "relative inline-flex group",
                        button { r#type: "button", disabled: ping_saving, aria_pressed: "{ping_on}", aria_label: format!("{} auto-ping for {}", if ping_on { "Disable" } else { "Enable" }, if name.is_empty() { &id } else { &name }), title: "Toggle auto-ping warmup", class: if ping_on { "flex h-8 w-8 items-center justify-center rounded-lg transition-colors hover:bg-black/5 dark:hover:bg-white/5 disabled:cursor-not-allowed disabled:opacity-50 text-primary" } else { "flex h-8 w-8 items-center justify-center rounded-lg transition-colors hover:bg-black/5 dark:hover:bg-white/5 disabled:cursor-not-allowed disabled:opacity-50 text-text-muted" }, onclick: { let provider = provider.clone(); let id = id.clone(); move |_| toggle_ping.call((provider.clone(), id.clone(), !ping_on)) }, span { class: "material-symbols-outlined text-[18px]", "bolt" } }
                        div { class: "pointer-events-none absolute bottom-full left-1/2 -translate-x-1/2 mb-1.5 z-50 w-max max-w-56 rounded px-2 py-1 text-[11px] leading-snug bg-gray-900 text-white opacity-0 group-hover:opacity-100 transition-opacity duration-150 whitespace-normal", "{ping_tooltip}" }
                    } }
                    if provider == "codex" { codex::Buttons { connection: connection.clone(), data: data.clone(), loading: quota.loading, busy, state: codex } }
                    button { r#type: "button", disabled: quota.loading || busy, title: "Refresh quota", class: "p-1.5 rounded-lg hover:bg-black/5 dark:hover:bg-white/5 transition-colors disabled:opacity-50", onclick: { let id = id.clone(); move |_| refresh.call(id.clone()) }, span { class: "material-symbols-outlined text-[18px] text-text-muted {spin}", "refresh" } }
                    button { r#type: "button", disabled: busy, title: "Edit connection", class: "p-1.5 rounded-lg hover:bg-black/5 dark:hover:bg-white/5 text-text-muted hover:text-primary transition-colors disabled:opacity-50", onclick: { let connection = connection.clone(); move |_| edit.call(connection.clone()) }, span { class: "material-symbols-outlined text-[18px]", "edit" } }
                    button { r#type: "button", disabled: busy, title: "Delete connection", class: "p-1.5 rounded-lg hover:bg-red-500/10 text-red-500 transition-colors disabled:opacity-50", onclick: { let id = id.clone(); move |_| delete.call(id.clone()) }, span { class: "material-symbols-outlined text-[18px]", "delete" } }
                    div { class: "inline-flex items-center pl-0.5", title: if active { "Disable connection" } else { "Enable connection" }, Toggle { checked: active, small: true, disabled: busy, label: if active { "Disable connection".to_owned() } else { "Enable connection".to_owned() }, onchange: { let id = id.clone(); move |next| toggle.call((id.clone(), next)) } } }
                }
            } }
            div { class: "px-2 py-1.5", if quota.loading { div { class: "text-center py-5 text-text-muted", span { class: "material-symbols-outlined text-[28px] animate-spin", "progress_activity" } } } else if !quota.error.is_empty() { div { class: "text-center py-5", span { class: "material-symbols-outlined text-[28px] text-red-500", "error" } p { class: "mt-1.5 text-xs text-text-muted", "{quota.error}" } } } else if let Some(message) = data["message"].as_str().filter(|m| !m.is_empty() && entries.is_empty()) { div { class: "text-center py-5", p { class: "text-xs text-text-muted", "{message}" } } } else { div { class: "space-y-2", if let Some(message) = data["message"].as_str().filter(|m| !m.is_empty()) { p { class: "text-[11px] text-text-muted", "{message}" } } QuotaTable { provider: provider.clone(), data } } } }
        }
    }
}

#[component]
pub(super) fn QuotaPage() -> Element {
    let mut rows = use_signal(Vec::<Value>::new);
    let mut quotas = use_signal(HashMap::<String, QuotaResult>::new);
    let loading = use_signal(|| true);
    let refreshing = use_signal(|| false);
    let mut auto_refresh = use_signal(|| {
        web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item("quotaAutoRefresh").ok().flatten())
            .is_none_or(|v| v == "true")
    });
    let mut countdown = use_signal(|| 60_u32);
    let mut filter = use_signal(|| "all".to_owned());
    let mut menu = use_signal(|| false);
    let mut expiring = use_signal(|| false);
    let mut bulk = use_signal(|| false);
    let mut pending = use_signal(|| None::<String>);
    let mut delete_id = use_signal(|| None::<String>);
    let mut editing = use_signal(|| None::<Value>);
    let mut editing_id = use_signal(String::new);
    let mut pools = use_signal(Vec::<Value>::new);
    let mut ping = use_signal(AutoPing::default);
    let codex = use_signal(codex::State::default);
    let toasts = use_context::<Signal<Vec<Toast>>>();
    use_future(move || async move {
        let result = async {
            let codex = initialize_auto_ping(Some("codexAutoPing")).await?;
            let glm = initialize_auto_ping(Some("glmAutoPing")).await?;
            Ok::<_, String>(HashMap::from([
                ("codex".to_owned(), codex),
                ("glm".to_owned(), glm),
            ]))
        }
        .await;
        match result {
            Ok(configs) => ping.set(AutoPing {
                ready: true,
                configs,
                ..Default::default()
            }),
            Err(error) => notify(
                toasts,
                &format!("Auto-ping settings unavailable: {error}"),
                true,
            ),
        }
    });
    use_future(move || async move {
        if let Ok(response) = dashboard_get_no_store("/api/proxy-pools?isActive=true").await
            && response.ok()
            && let Some(data) = json_body(&response).await
            && let Some(items) = data["proxyPools"].as_array()
        {
            pools.set(items.clone());
        }
    });
    use_future(move || async move {
        refresh_all(rows, quotas, loading, refreshing).await;
    });
    use_effect(move || {
        if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
            let _ = storage.set_item(
                "quotaAutoRefresh",
                if auto_refresh() { "true" } else { "false" },
            );
        }
    });
    use_future(move || async move {
        loop {
            TimeoutFuture::new(1000).await;
            if *auto_refresh.peek()
                && web_sys::window()
                    .and_then(|w| w.document())
                    .is_none_or(|d| !d.hidden())
            {
                let left = *countdown.peek();
                if left <= 1 {
                    countdown.set(60);
                    spawn(async move {
                        refresh_all(rows, quotas, loading, refreshing).await;
                    });
                } else {
                    countdown.set(left - 1);
                }
            }
        }
    });
    let quota_values = quotas();
    let eligible_rows: Vec<Value> = rows().into_iter().filter(eligible).collect();
    let mut providers: Vec<String> = eligible_rows
        .iter()
        .map(|row| text(row, "provider").to_owned())
        .collect();
    providers.sort();
    providers.dedup();
    let mut current: Vec<Value> = eligible_rows
        .into_iter()
        .filter(|row| filter() == "all" || text(row, "provider") == filter())
        .collect();
    current.sort_by(|a, b| {
        let rank = |row: &Value| {
            SUPPORTED
                .iter()
                .position(|provider| *provider == text(row, "provider"))
                .unwrap_or(usize::MAX)
        };
        let expiry = |row: &Value| {
            quota_values
                .get(text(row, "id"))
                .and_then(|q| q.data.as_ref())
                .map(|data| quota_entries(text(row, "provider"), data))
                .unwrap_or_default()
                .iter()
                .filter_map(|entry| entry["resetAt"].as_str())
                .map(|value| js_sys::Date::new(&value.into()).get_time())
                .filter(|time| time.is_finite())
                .min_by(f64::total_cmp)
                .unwrap_or(f64::INFINITY)
        };
        if expiring() {
            expiry(a)
                .total_cmp(&expiry(b))
                .then_with(|| rank(a).cmp(&rank(b)))
        } else {
            rank(a).cmp(&rank(b))
        }
    });
    let toggle = use_callback(move |(id, next): (String, bool)| {
        if pending().is_some() {
            return;
        }
        pending.set(Some(id.clone()));
        spawn(async move {
            let path = format!("/api/providers/{}", encoded(&id));
            if dashboard_request("PUT", &path, Some(&json!({"isActive": next})))
                .await
                .is_ok_and(|response| response.ok())
            {
                let updated = connections().await;
                let should_refresh = next
                    && updated
                        .iter()
                        .any(|row| text(row, "id") == id && eligible(row));
                rows.set(updated);
                if should_refresh {
                    fetch_quota(id.clone(), quotas).await;
                }
            }
            pending.set(None);
        });
    });
    let toggle_ping = use_callback(move |(provider, id, enabled): (String, String, bool)| {
        let Some(key) = auto_ping_key(&provider) else {
            return;
        };
        if !ping.peek().ready || ping.peek().saving.contains(&provider) {
            return;
        }
        let previous = ping
            .peek()
            .configs
            .get(&provider)
            .cloned()
            .unwrap_or_default();
        {
            let mut state = ping.write();
            state.saving.push(provider.clone());
            state
                .configs
                .entry(provider.clone())
                .or_default()
                .insert(id.clone(), json!(enabled));
        }
        spawn(async move {
            let baseline = match load_auto_ping_settings().await {
                Ok(settings) => auto_ping_connections(&settings[key]).0,
                Err(error) => {
                    let mut state = ping.write();
                    state.configs.insert(provider.clone(), previous);
                    state.saving.retain(|value| value != &provider);
                    drop(state);
                    notify(toasts, &error, true);
                    return;
                }
            };
            let mut next = baseline.clone();
            next.insert(id, json!(enabled));
            ping.write().configs.insert(provider.clone(), next.clone());
            let result = save_auto_ping(key, &next).await;
            let mut state = ping.write();
            if result.is_err() {
                state.configs.insert(provider.clone(), baseline);
            }
            state.saving.retain(|value| value != &provider);
            drop(state);
            if let Err(error) = result {
                notify(toasts, &error, true);
            }
        });
    });
    let auto_color = if auto_refresh() {
        "text-primary"
    } else {
        "text-text-muted"
    };
    let spin = if refreshing() { "animate-spin" } else { "" };
    rsx! {
        if !loading() && current.is_empty() { div { class: "bg-canvas border border-hairline text-ink rounded-mini-lg shadow-none p-6", div { class: "text-center py-12", span { class: "material-symbols-outlined text-[64px] text-text-muted opacity-20", "cloud_off" } h3 { class: "mt-4 text-lg font-semibold text-text-primary", "No Providers Connected" } p { class: "mt-2 text-sm text-text-muted max-w-md mx-auto", "Connect to providers with OAuth to track your API quota limits and usage." } } } }
        else { div { class: "space-y-6", div { class: "flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between", div { class: "flex flex-col gap-1 sm:flex-row sm:items-center sm:gap-3", h2 { class: "text-xl font-semibold text-text-primary", "Provider Limits" } } div { class: "flex flex-wrap items-center gap-1.5",
            div { class: "relative", button { r#type: "button", title: "Filter quota providers", aria_haspopup: "menu", aria_expanded: "{menu}", class: "flex h-8 items-center justify-between gap-1 rounded-lg border border-black/10 bg-black/[0.02] px-2 text-xs text-text-primary transition-colors hover:bg-black/5 dark:border-white/10 dark:bg-white/[0.03] dark:hover:bg-white/10", onclick: move |_| menu.set(!menu()), span { class: "flex min-w-0 items-center gap-1.5", if filter() == "all" { span { class: "material-symbols-outlined text-[14px] text-text-muted", "apps" } } else { img { src: "/providers/{filter}.png", alt: "{filter}", width: "18", height: "18", class: "size-[18px] rounded object-contain" } } span { class: "truncate capitalize hidden lg:inline", if filter() == "all" { "All providers" } else { "{filter}" } } } span { class: "material-symbols-outlined text-[14px] text-text-muted", "expand_more" } }
                if menu() { button { r#type: "button", class: "fixed inset-0 z-30 bg-transparent", aria_label: "Close provider filter", onclick: move |_| menu.set(false) } div { class: "absolute left-0 z-40 mt-2 w-64 overflow-hidden rounded-2xl border border-black/10 bg-surface/95 p-1.5 shadow-xl shadow-black/10 backdrop-blur dark:border-white/10 dark:bg-surface/95 sm:w-72", button { r#type: "button", class: if filter() == "all" { "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm transition-colors bg-primary/10 text-primary" } else { "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm transition-colors text-text-primary hover:bg-black/5 dark:hover:bg-white/10" }, onclick: move |_| { filter.set("all".into()); menu.set(false); }, span { class: "material-symbols-outlined text-[22px]", "apps" } span { class: "font-medium", "All providers" } if filter() == "all" { span { class: "material-symbols-outlined ml-auto text-[20px]", "check" } } } div { class: "my-1 h-px bg-black/10 dark:bg-white/10" } div { class: "max-h-72 overflow-y-auto pr-1", for provider in providers { button { key: "{provider}", r#type: "button", class: if filter() == provider { "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm transition-colors bg-primary/10 text-primary" } else { "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm transition-colors text-text-primary hover:bg-black/5 dark:hover:bg-white/10" }, onclick: { let provider = provider.clone(); move |_| { filter.set(provider.clone()); menu.set(false); } }, img { src: "/providers/{provider}.png", alt: "{provider}", width: "24", height: "24", class: "size-6 rounded-md object-contain" } span { class: "font-medium capitalize", "{provider}" } if filter() == provider { span { class: "material-symbols-outlined ml-auto text-[20px]", "check" } } } } } } }
            }
            button { r#type: "button", title: "Sort accounts by earliest quota reset time", class: if expiring() { "flex h-8 shrink-0 items-center gap-1 rounded-lg border px-2 text-xs transition-colors border-amber-500/40 bg-amber-500/10 text-amber-500" } else { "flex h-8 shrink-0 items-center gap-1 rounded-lg border px-2 text-xs transition-colors border-black/10 text-text-primary hover:bg-black/5 dark:border-white/10 dark:hover:bg-white/5" }, onclick: move |_| expiring.set(!expiring()), span { class: "material-symbols-outlined text-[14px]", "hourglass_top" } span { class: "hidden sm:inline", "Expiring first" } }
            for target in [false, true] { button { key: "{target}", r#type: "button", disabled: bulk(), title: if target { "Enable connections that still have quota (within current filter)" } else { "Disable connections with depleted quota (within current filter)" }, class: if target { "flex h-8 shrink-0 items-center gap-1 rounded-lg border border-emerald-500/30 px-2 text-xs text-emerald-500 transition-colors hover:bg-emerald-500/10 disabled:opacity-50" } else { "flex h-8 shrink-0 items-center gap-1 rounded-lg border border-red-500/30 px-2 text-xs text-red-500 transition-colors hover:bg-red-500/10 disabled:opacity-50" }, onclick: { let ids: Vec<String> = current.iter().filter(|row| { let depleted = quota_values.get(text(row, "id")).and_then(|q| q.data.as_ref()).map(|data| quota_entries(text(row, "provider"), data)).unwrap_or_default().iter().any(|q| q["total"].as_f64().is_some_and(|total| total > 0.0 && percentage(&json!({"used":q["used"], "total":total})) <= 5.0)); if target { row["isActive"] == false && !depleted } else { row["isActive"] != false && depleted } }).map(|row| text(row, "id").to_owned()).collect(); move |_| { if ids.is_empty() || bulk() { return; } bulk.set(true); let ids = ids.clone(); spawn(async move { let results = futures_util::future::join_all(ids.iter().map(|id| async move { let path = format!("/api/providers/{}", encoded(id)); (id.clone(), dashboard_request("PUT", &path, Some(&json!({"isActive":target}))).await.is_ok_and(|r| r.ok())) })).await; for (id, ok) in results { if ok { for row in rows.write().iter_mut().filter(|row| text(row, "id") == id) { row["isActive"] = json!(target); } } } bulk.set(false); }); } }, span { class: "material-symbols-outlined text-[14px]", if target { "check_circle" } else { "block" } } span { class: "hidden sm:inline", if target { "Turn on Available" } else { "Turn off Empty" } } } }
            button { onclick: move |_| auto_refresh.set(!auto_refresh()), title: if auto_refresh() { "Disable auto-refresh" } else { "Enable auto-refresh" }, class: "flex h-8 shrink-0 items-center gap-1 rounded-lg border border-black/10 px-2 text-xs transition-colors hover:bg-black/5 dark:border-white/10 dark:hover:bg-white/5", span { class: "material-symbols-outlined text-[14px] {auto_color}", if auto_refresh() { "toggle_on" } else { "toggle_off" } } span { class: "hidden text-text-primary sm:inline", "Auto-refresh" } if auto_refresh() { span { class: "text-[10px] text-text-muted tabular-nums", "({countdown}s)" } } }
            button { r#type: "button", title: "Refresh all", disabled: refreshing(), class: "flex h-8 shrink-0 items-center gap-1 rounded-lg border border-black/10 px-2 text-xs text-text-primary transition-colors hover:bg-black/5 dark:border-white/10 dark:hover:bg-white/5 disabled:opacity-50", onclick: move |_| { countdown.set(60); spawn(async move { refresh_all(rows, quotas, loading, refreshing).await; }); }, span { class: "material-symbols-outlined text-[14px] {spin}", "refresh" } }
        } }
        div { class: "grid grid-cols-1 md:grid-cols-2 gap-3", for row in current { { let id = text(&row, "id").to_owned(); let quota = quota_values.get(&id).cloned().unwrap_or_default(); let busy = pending().as_deref() == Some(id.as_str()) || codex().resetting(&id); rsx! { QuotaCard { key: "{id}", connection: row, quota, busy, ping: ping(), codex, toggle_ping, refresh: move |id| { spawn(async move { fetch_quota(id, quotas).await; }); }, edit: move |row: Value| { editing_id.set(text(&row, "id").to_owned()); editing.set(Some(row)); }, delete: move |id| delete_id.set(Some(id)), toggle } } } } }
        codex::Dialogs { state: codex, quotas }
        if let Some(row) = editing() { EditConnection { provider_id: text(&row, "provider").to_owned(), selected: editing, pools, on_saved: move |_| { let id = editing_id(); spawn(async move { rows.set(connections().await); fetch_quota(id, quotas).await; }); } } }
        if let Some(id) = delete_id() { Modal { title: "Delete connection".to_owned(), small: true, on_close: move |_| { if pending().is_none() { delete_id.set(None); } }, footer: rsx! { Button { label: "Cancel".to_owned(), variant: "ghost", disabled: pending().is_some(), onclick: move |_| delete_id.set(None) } Button { label: "Delete".to_owned(), variant: "danger", loading: pending().is_some(), onclick: { let id = id.clone(); move |_| { if pending().is_some() { return; } pending.set(Some(id.clone())); let id = id.clone(); spawn(async move { let path = format!("/api/providers/{}", encoded(&id)); if dashboard_request("DELETE", &path, None).await.is_ok_and(|r| r.ok()) { quotas.write().remove(&id); rows.set(connections().await); } pending.set(None); delete_id.set(None); }); } } } }, p { class: "text-text-muted", "Are you sure you want to delete this connection? This action cannot be undone." } } }
        } }
    }
}
