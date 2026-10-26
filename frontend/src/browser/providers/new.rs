//! The reference new-provider chooser, using the checked provider registries.
use super::{compare_name, dashboard_request, json, json_body, notify, text, Toast, Value};
use crate::browser::{navigate, ui::Button, ui::INPUT_CLASS};
use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
struct Choice {
    group: &'static str,
    id: String,
    name: String,
    key: String,
    label: String,
    provider: Option<Value>,
}

fn group_meta(group: &str) -> (&'static str, &'static str) {
    match group {
        "oauth" => (
            "OAuth",
            "Connect with your subscription account via browser OAuth.",
        ),
        "free" => ("Free", "Free-tier OAuth / no-auth providers."),
        "freeTier" => ("Free Tier (API Key)", "Providers with a free API-key tier."),
        "apikey" => (
            "API Key",
            "Paste an API key here, or open the provider list to add it there.",
        ),
        "webCookie" => (
            "Web Cookie",
            "Use a browser session cookie from a subscription site.",
        ),
        "compatible" => (
            "Compatible Endpoint",
            "Add a custom OpenAI- or Anthropic-compatible base URL from the providers list.",
        ),
        _ => ("", ""),
    }
}

fn choices() -> Vec<Choice> {
    let registry: Value =
        serde_json::from_str(include_str!("../provider_data.json")).unwrap_or_default();
    let mut result = Vec::new();
    for (group, registry_key) in [
        ("oauth", "oauth"),
        ("free", "free"),
        ("freeTier", "freeTier"),
        ("apikey", "apiKey"),
        ("webCookie", "webCookie"),
    ] {
        let mut providers = registry[registry_key]
            .as_array()
            .cloned()
            .unwrap_or_default();
        providers.sort_by(|a, b| {
            a["priority"]
                .as_i64()
                .unwrap_or(999)
                .cmp(&b["priority"].as_i64().unwrap_or(999))
                .then_with(|| (b["noAuth"] == true).cmp(&(a["noAuth"] == true)))
                .then_with(|| compare_name(a, b))
        });
        for provider in providers {
            let id = text(&provider, "id").to_owned();
            let name = text(&provider, "name").to_owned();
            result.push(Choice {
                group,
                key: format!("{group}:{id}"),
                label: format!("[{}] {name}", group_meta(group).0),
                id,
                name,
                provider: Some(provider),
            });
        }
    }
    for (id, name) in [
        ("openai-compatible", "OpenAI Compatible (custom endpoint)"),
        (
            "anthropic-compatible",
            "Anthropic Compatible (custom endpoint)",
        ),
    ] {
        result.push(Choice {
            group: "compatible",
            key: format!("compatible:{id}"),
            label: format!("[Compatible Endpoint] {name}"),
            id: id.to_owned(),
            name: name.to_owned(),
            provider: None,
        });
    }
    result
}

fn js_trim(value: &str) -> String {
    js_sys::JsString::from(value)
        .trim()
        .as_string()
        .unwrap_or_default()
}

fn lower(value: &str) -> String {
    js_sys::JsString::from(value)
        .to_lower_case()
        .as_string()
        .unwrap_or_default()
}

fn encoded(value: &str) -> String {
    js_sys::encode_uri_component(value)
        .as_string()
        .unwrap_or_default()
}

#[component]
pub(in crate::browser) fn NewProviderPage() -> Element {
    let options = use_hook(choices);
    let mut selected_key = use_signal(String::new);
    let mut api_key = use_signal(String::new);
    let mut display_name = use_signal(String::new);
    let mut saving = use_signal(|| false);
    let toasts = use_context::<Signal<Vec<Toast>>>();
    let selected = options
        .iter()
        .find(|item| item.key == selected_key())
        .cloned();
    let is_key = selected.as_ref().is_some_and(|item| {
        item.provider.is_some() && matches!(item.group, "apikey" | "freeTier" | "webCookie")
    });
    let is_cookie = selected.as_ref().is_some_and(|item| {
        item.provider.as_ref().is_some_and(|provider| {
            item.group == "webCookie" || text(provider, "authType") == "cookie"
        })
    });
    let is_no_auth = selected.as_ref().is_some_and(|item| {
        item.provider
            .as_ref()
            .is_some_and(|provider| provider["noAuth"] == true)
    });
    let is_oauth = selected.as_ref().is_some_and(|item| {
        item.provider.as_ref().is_some_and(|provider| {
            item.group == "oauth" || (item.group == "free" && provider["noAuth"] != true)
        })
    });
    let has_key = !js_trim(&api_key()).is_empty();
    let primary_label = match &selected {
        None => "Connect Provider",
        Some(item) if item.group == "compatible" => "Open Compatible Setup",
        Some(_) if is_oauth => "Start OAuth",
        Some(_) if is_no_auth => "Open Provider",
        Some(_) if is_key && has_key && saving() => "Saving...",
        Some(_) if is_key && has_key && is_cookie => "Save Cookie",
        Some(_) if is_key && has_key => "Save API Key",
        Some(_) if is_key && is_cookie => "Add Cookie on List",
        Some(_) if is_key => "Add Key on List",
        Some(_) => "Continue",
    };
    let site_data: Value =
        serde_json::from_str(include_str!("../provider_sites.json")).unwrap_or_default();
    let choice_for_action = selected.clone();

    rsx! {
        div { class: "mx-auto flex w-full max-w-2xl flex-col gap-6",
            div {
                a { href: "/dashboard/providers", class: "mb-4 inline-flex items-center gap-1 text-sm text-text-muted transition-colors hover:text-primary", span { class: "material-symbols-outlined text-lg", "arrow_back" } "Back to Providers" }
                h1 { class: "text-2xl font-bold tracking-tight sm:text-3xl", "Add New Provider" }
                p { class: "mt-2 text-sm text-text-muted", "Pick a provider, then connect with OAuth, an API key, a free tier, a web cookie, or a custom compatible endpoint." }
            }
            div { class: "bg-canvas border border-hairline text-ink rounded-mini-lg shadow-none p-6",
                div { class: "flex flex-col gap-5",
                    ProviderSelect { options: options.clone(), selected: selected_key, on_change: move |value: String| { selected_key.set(value); api_key.set(String::new()); display_name.set(String::new()); } }
                    if let Some(item) = selected.clone() {
                        if let Some(provider) = item.provider.clone() {
                            ProviderSummary { provider, hint: group_meta(item.group).1 }
                        } else {
                            div { class: "rounded-xl border border-dashed border-border p-3 text-sm text-text-muted", "{group_meta(\"compatible\").1}" {" You will be taken to the providers list with the add modal open."} }
                        }
                        if is_oauth {
                            div { class: "rounded-xl border border-border bg-primary/5 p-3 text-sm text-text-muted", {"You will be redirected to complete OAuth for"} {" "} span { class: "font-medium text-text-main", "{item.name}" } "." }
                        }
                        if is_key {
                            div { class: "flex flex-col gap-3",
                                NewInput { label: "Display Name".to_owned(), placeholder: if is_cookie { "e.g. Personal cookie".to_owned() } else { "e.g. Production key".to_owned() }, value: display_name, hint: "Optional. Defaults to a generic label.".to_owned() }
                                NewInput { label: if is_cookie { "Cookie Value".to_owned() } else { "API Key".to_owned() }, password: !is_cookie, placeholder: if is_cookie { item.provider.as_ref().map_or("Paste cookie value", |provider| provider["authHint"].as_str().filter(|hint| !hint.is_empty()).unwrap_or("Paste cookie value")).to_owned() } else { concat!("sk", "-...").to_owned() }, value: api_key, hint: if has_key { "Key will be saved on this page.".to_owned() } else { "Leave blank to open the providers list with this provider pre-selected (?add=).".to_owned() } }
                                if let Some(url) = site_data[&item.id]["url"].as_str() {
                                    a { href: "{url}", target: "_blank", rel: "noopener noreferrer", class: "text-xs text-primary underline", if site_data[&item.id]["kind"] == "key" { "Get API key" } else { "Open provider site" } }
                                }
                            }
                        }
                    }
                    div { class: "flex flex-col gap-2 border-t border-border pt-4 sm:flex-row",
                        a { href: "/dashboard/providers", class: "flex-1", Button { label: "Cancel".to_owned(), variant: "ghost", full_width: true, onclick: move |_| {} } }
                        Button { label: primary_label.to_owned(), full_width: true, extra_class: "flex-1", disabled: selected.is_none() || saving(), loading: saving(), onclick: move |_| {
                            let Some(item) = choice_for_action.clone() else { return; };
                            if *saving.peek() { return; }
                            if item.group == "compatible" {
                                let variant = if item.id == "openai-compatible" { "openai" } else { "anthropic" };
                                let _ = navigate(&format!("/dashboard/providers?compatible={variant}"));
                                return;
                            }
                            let id = encoded(&item.id);
                            if is_oauth { let _ = navigate(&format!("/api/oauth/{id}/start")); return; }
                            if is_no_auth { let _ = navigate(&format!("/dashboard/providers/{id}")); return; }
                            if is_key {
                                let credential = js_trim(&api_key());
                                if credential.is_empty() { let _ = navigate(&format!("/dashboard/providers?add={id}")); return; }
                                let name = js_trim(&display_name());
                                let name = if name.is_empty() { if is_cookie { "Cookie" } else { "API Key" }.to_owned() } else { name };
                                let body = json!({"provider": item.id, "name": name, "apiKey": credential, "priority": 1, "testStatus": "unknown"});
                                saving.set(true);
                                spawn(async move {
                                    match dashboard_request("POST", "/api/providers", Some(&body)).await {
                                        Ok(response) if response.ok() => { notify(toasts, &format!("{} connected", item.name), false); let _ = navigate(&format!("/dashboard/providers/{id}")); }
                                        Ok(response) => {
                                            let data = json_body(&response).await;
                                            let error = data.as_ref().and_then(|data| data["error"].as_str()).filter(|error| !error.is_empty()).unwrap_or("Failed to add provider key");
                                            notify(toasts, error, true);
                                        }
                                        Err(_) => notify(toasts, "Failed to add provider key", true),
                                    }
                                    saving.set(false);
                                });
                                return;
                            }
                            let _ = navigate(&format!("/dashboard/providers/{id}"));
                        } }
                    }
                }
            }
        }
    }
}

#[component]
fn ProviderSelect(
    options: Vec<Choice>,
    selected: Signal<String>,
    on_change: EventHandler<String>,
) -> Element {
    let mut filter = use_signal(String::new);
    let query = lower(&js_trim(&filter()));
    let filtered: Vec<Choice> = options
        .iter()
        .filter(|option| {
            query.is_empty()
                || lower(&option.label).contains(&query)
                || lower(&option.key).contains(&query)
        })
        .cloned()
        .collect();
    let mut displayed = filtered.clone();
    if !selected().is_empty()
        && !displayed.iter().any(|option| option.key == selected())
        && let Some(option) = options.iter().find(|option| option.key == selected())
    {
        displayed.insert(0, option.clone());
    }
    let count = filtered.len();
    let total = options.len();
    rsx! {
        div { class: "flex flex-col gap-1.5",
            label { class: "text-sm font-medium text-text-main", "Provider" span { class: "text-red-500 ml-1", "*" } }
            div { class: "relative",
                span { class: "material-symbols-outlined pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-[18px] text-text-muted", "search" }
                // Let the browser retain the input value. Echoing a U+FEFF-containing
                // value through Dioxus 0.7 corrupts adjacent dynamic text updates.
                input { r#type: "text", placeholder: "Search providers...", aria_label: "Search providers...", class: "w-full py-2 pl-9 pr-3 text-sm text-text-main bg-surface-2 border border-transparent rounded-[10px] focus:outline-none focus:ring-2 focus:ring-brand-500/30 focus:border-brand-500/40 transition-all duration-150 disabled:opacity-50 disabled:cursor-not-allowed text-[16px] sm:text-sm", oninput: move |event| filter.set(event.value()) }
            }
            div { class: "relative",
                select { value: "{selected}", class: "w-full py-2.5 px-3 pr-10 text-sm text-text-main bg-surface-2 border border-transparent rounded-[10px] appearance-none focus:outline-none focus:ring-2 focus:ring-brand-500/30 focus:border-brand-500/40 transition-all duration-150 disabled:opacity-50 disabled:cursor-not-allowed text-[16px] sm:text-sm", onchange: move |event| on_change.call(event.value()),
                    option { value: "", disabled: true, "Choose a provider..." }
                    for option in displayed { option { key: "{option.key}", value: "{option.key}", "{option.label}" } }
                }
                div { class: "absolute inset-y-0 right-0 flex items-center pr-3 pointer-events-none text-text-muted", span { class: "material-symbols-outlined text-[20px]", "expand_more" } }
            }
            // Neither "of" nor "providers" is translated in the reference catalogs.
            // Keep this changing count out of the text-node observer's source cache.
            if !query.is_empty() { p { class: "text-xs text-text-muted", "data-i18n-skip": "true", "{count}" {" of "} "{total}" {" providers"} } }
            p { class: "text-xs text-text-muted", "Grouped as OAuth · Free · Free Tier · API Key · Web Cookie · Compatible" }
        }
    }
}

#[component]
fn ProviderSummary(provider: Value, hint: &'static str) -> Element {
    let mut failed_image = use_signal(|| false);
    let id = text(&provider, "id");
    let name = text(&provider, "name");
    let color = provider["color"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or("#888");
    let background = if color.len() > 7 {
        color.to_owned()
    } else {
        format!("{color}15")
    };
    let fallback = provider["textIcon"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| id.chars().take(2).collect::<String>().to_uppercase());
    let src = format!("/providers/{id}.png");
    let no_auth = provider["noAuth"] == true;
    let deprecated = provider["deprecated"] == true;
    rsx! {
        div { class: "flex items-center gap-3 rounded-xl border border-border bg-bg-subtle/40 p-3",
            div { class: "flex size-10 shrink-0 items-center justify-center rounded-lg", style: "background-color: {background};",
                if failed_image() {
                    span { class: "inline-flex items-center justify-center font-bold rounded-lg max-h-[28px] max-w-[28px] rounded-lg object-contain", style: "width: 28px; height: 28px; color: {color}; font-size: 10px;", "{fallback}" }
                } else {
                    img { src, alt: "{name}", width: "28", height: "28", class: "max-h-[28px] max-w-[28px] rounded-lg object-contain", onerror: move |_| failed_image.set(true) }
                }
            }
            div { class: "min-w-0", p { class: "truncate font-medium", "{name}" } p { class: "text-xs text-text-muted", "{hint}" if no_auth { " · No auth required" } if deprecated { " · Deprecated" } } }
        }
    }
}

#[component]
fn NewInput(
    label: String,
    placeholder: String,
    hint: String,
    mut value: Signal<String>,
    #[props(default)] password: bool,
) -> Element {
    rsx! { div { class: "flex flex-col gap-1.5",
        label { class: "type-body-sm-medium text-ink", "{label}" }
        div { class: "relative", input { r#type: if password { "password" } else { "text" }, aria_label: "{label}", placeholder: "{placeholder}", value: "{value}", class: INPUT_CLASS, oninput: move |event| value.set(event.value()) } }
        p { class: "type-body-sm text-slate", "{hint}" }
    } }
}
