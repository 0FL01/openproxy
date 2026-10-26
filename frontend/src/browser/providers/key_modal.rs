use super::{dashboard_request, json, json_body, text, Value};
use crate::browser::{
    shell::{notify, Toast},
    ui::{Button, Modal, INPUT_CLASS},
};
use dioxus::prelude::*;

const SELECT_CLASS: &str = "w-full py-2.5 px-3 pr-10 text-sm text-text-main bg-surface-2 border border-transparent rounded-[10px] appearance-none focus:outline-none focus:ring-2 focus:ring-brand-500/30 focus:border-brand-500/40 transition-all duration-150 disabled:opacity-50 disabled:cursor-not-allowed text-[16px] sm:text-sm";

async fn refresh(mut connections: Signal<Vec<Value>>) {
    if let Ok(response) = dashboard_request("GET", "/api/providers", None).await
        && response.ok()
        && let Some(data) = json_body(&response).await
        && let Some(items) = data["connections"].as_array()
    {
        connections.set(items.clone());
    }
}

fn specific(
    id: &str,
    endpoint: &str,
    version: &str,
    deployment: &str,
    organization: &str,
    account: &str,
    region: &str,
) -> Option<Value> {
    match id {
        "azure" => Some(
            json!({"azureEndpoint": endpoint, "apiVersion": version, "deployment": deployment, "organization": organization}),
        ),
        "cloudflare-ai" => Some(json!({"accountId": account})),
        "xiaomi-tokenplan" => Some(json!({"region": region})),
        _ => None,
    }
}

async fn validate(id: &str, key: &str, specific: Option<Value>) -> bool {
    let mut body = json!({"provider": id, "apiKey": key});
    if let Some(specific) = specific {
        body["providerSpecificData"] = specific;
    }
    match dashboard_request("POST", "/api/providers/validate", Some(&body)).await {
        Ok(response) => json_body(&response)
            .await
            .is_some_and(|data| data["valid"] == true),
        Err(_) => false,
    }
}

#[component]
pub(super) fn KeyModal(
    mut provider: Signal<Option<String>>,
    pools: Signal<Vec<Value>>,
    connections: Signal<Vec<Value>>,
) -> Element {
    let name = use_signal(String::new);
    let key = use_signal(String::new);
    let mut priority = use_signal(|| 1_f64);
    let pool_id = use_signal(|| "__none__".to_owned());
    let endpoint = use_signal(String::new);
    let version = use_signal(|| "2024-10-01-preview".to_owned());
    let deployment = use_signal(String::new);
    let organization = use_signal(String::new);
    let account = use_signal(String::new);
    let region = use_signal(|| "sgp".to_owned());
    let mut validating = use_signal(|| false);
    let mut validation = use_signal(|| None::<bool>);
    let mut saving = use_signal(|| false);
    let mut bulk = use_signal(|| false);
    let mut bulk_text = use_signal(String::new);
    let mut bulk_result = use_signal(|| None::<(usize, usize)>);
    let toasts = use_context::<Signal<Vec<Toast>>>();
    let close = use_callback(move |()| {
        provider.set(None);
        spawn(async move {
            refresh(connections).await;
        });
    });
    let Some(id) = provider() else {
        return rsx! {};
    };
    let registry: Value =
        serde_json::from_str(include_str!("../provider_data.json")).unwrap_or_default();
    let name_of_provider = ["apiKey", "oauth", "freeTier", "free"]
        .into_iter()
        .find_map(|category| {
            registry[category]
                .as_array()?
                .iter()
                .find(|value| text(value, "id") == id)
                .map(|value| text(value, "name").to_owned())
        })
        .unwrap_or_else(|| id.clone());
    let title = format!("Add {name_of_provider} API Key");
    let azure = id == "azure";
    let cloudflare = id == "cloudflare-ai";
    let xiaomi = id == "xiaomi-tokenplan";
    let xai = id == "xai";
    let supports_bulk = !azure && !xai;
    let single_variant = if bulk() { "ghost" } else { "primary" };
    let bulk_variant = if bulk() { "primary" } else { "ghost" };
    let key_placeholder = if xai { "xai-..." } else { "" };
    let allowed_save = !name().is_empty()
        && !key().is_empty()
        && (!azure
            || (!endpoint().is_empty() && !deployment().is_empty() && !organization().is_empty()))
        && (!cloudflare || !account().is_empty());
    let bulk_placeholder = if cloudflare {
        concat!(
            "name1|sk",
            "-key1|acc123456\nname2|sk",
            "-key2|def789012\nsk",
            "-key-only-auto-named"
        )
    } else {
        concat!("prod|sk", "-aaa...\nstaging|sk", "-bbb...\nsk", "-ccc...")
    };
    rsx! {
        Modal { title, on_close: move |_| close.call(()),
            div { class: "flex flex-col gap-4",
                if supports_bulk {
                    div { class: "flex gap-2",
                        Button { small: true, label: "Single".to_owned(), variant: single_variant, onclick: move |_| { bulk.set(false); bulk_result.set(None); } }
                        Button { small: true, label: "Bulk Add".to_owned(), variant: bulk_variant, onclick: move |_| { bulk.set(true); bulk_result.set(None); } }
                    }
                }
                if supports_bulk && bulk() {
                    div { class: "flex flex-col gap-3",
                        p { class: "text-xs text-text-muted", if cloudflare { "One key per line. Format: " code { "name|apiKey|accountId" } " or just " code { "apiKey" } " (auto-named by index)." } else { "One key per line. Format: " code { "name|apiKey" } " or just " code { "apiKey" } " (auto-named by index)." } }
                        textarea { class: "w-full rounded border border-accent/30 bg-sidebar p-2 text-sm font-mono resize-y min-h-[140px] focus:outline-none focus:ring-1 focus:ring-primary", placeholder: bulk_placeholder, value: "{bulk_text}", oninput: move |event| bulk_text.set(event.value()) }
                        if let Some((success, failed)) = bulk_result() { div { class: if failed > 0 { "text-sm font-medium text-yellow-400" } else { "text-sm font-medium text-green-400" }, "Added {success}" if failed > 0 { ", {failed} failed" } } }
                        div { class: "flex gap-2",
                            Button { label: if saving() { "Adding...".to_owned() } else { "Add All Keys".to_owned() }, full_width: true, disabled: saving() || bulk_text().trim().is_empty(), onclick: { let id = id.clone(); move |_| {
                                if *saving.peek() || bulk_text.peek().trim().is_empty() { return; }
                                saving.set(true);
                                bulk_result.set(None);
                                let lines: Vec<String> = bulk_text().lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect();
                                let id = id.clone();
                                spawn(async move {
                                    let (mut success, mut failed) = (0_usize, 0_usize);
                                    for (i, line) in lines.iter().enumerate() {
                                        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
                                        let (item_name, api_key, account_id) = if cloudflare && parts.len() >= 3 {
                                            (format!("{} {}", if parts[0].is_empty() {"Key"} else {parts[0]}, i + 1), parts[1..parts.len()-1].join("|").trim().to_owned(), Some(parts[parts.len()-1]))
                                        } else if parts.len() >= 2 {
                                            (format!("{} {}", if parts[0].is_empty() {"Key"} else {parts[0]}, i + 1), parts[1..].join("|").trim().to_owned(), None)
                                        } else { (format!("Key {}", i+1), parts[0].to_owned(), None) };
                                        if api_key.is_empty() { failed += 1; continue; }
                                        let mut body = json!({"provider": id, "name": item_name, "apiKey": api_key, "priority": 1, "testStatus": "unknown"});
                                        if let Some(account_id) = account_id.filter(|id| !id.is_empty()) { body["providerSpecificData"] = json!({"accountId": account_id}); }
                                        if dashboard_request("POST", "/api/providers", Some(&body)).await.is_ok_and(|response| response.ok()) { success += 1; } else { failed += 1; }
                                    }
                                    saving.set(false);
                                    bulk_result.set(Some((success,failed)));
                                });
                            } } }
                            Button { label: "Close".to_owned(), variant: "ghost", full_width: true, onclick: move |_| close.call(()) }
                        }
                    }
                } else {
                    Field { label: "Name", value: name, placeholder: "Production Key" }
                    div { class: "flex gap-2",
                        Field { label: "API Key", value: key, password: true, placeholder: key_placeholder, extra_class: "flex-1" }
                        div { class: "pt-6",
                            Button { label: if validating() { "Checking...".to_owned() } else { "Check".to_owned() }, variant: "secondary", disabled: key().is_empty() || validating() || saving(), onclick: { let id = id.clone(); move |_| {
                                if key.peek().is_empty() || *validating.peek() || *saving.peek() { return; }
                                validating.set(true);
                                let id = id.clone();
                                let key_value = key();
                                let config = specific(&id, &endpoint(), &version(), &deployment(), &organization(), &account(), &region());
                                spawn(async move { let valid = validate(&id, &key_value, config).await; validation.set(Some(valid)); validating.set(false); });
                            } } }
                        }
                    }
                    if let Some(valid) = validation() {
                        span { class: if valid { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-success-bg text-success-text px-2.5 py-1 text-[11px] tracking-wide" } else { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)] px-2.5 py-1 text-[11px] tracking-wide" }, if valid { "Valid" } else { "Invalid" } }
                    }
                    if xai {
                        div { class: "bg-sidebar/50 p-4 rounded-lg border border-accent/20", h3 { class: "font-semibold mb-3 text-sm", "xAI API Key" } p { class: "text-xs text-text-muted leading-relaxed", "Use a direct xAI API key from " a { href: "https://console.x.ai", target: "_blank", rel: "noopener noreferrer", class: "text-primary underline", "console.x.ai" } ". This is separate from Grok Build OAuth." } }
                    }
                    if cloudflare {
                        div { class: "bg-sidebar/50 p-4 rounded-lg border border-accent/20", h3 { class: "font-semibold mb-3 text-sm", "Cloudflare Workers AI" } Field { label: "Account ID", value: account, placeholder: "abc123def456..." } p { class: "text-xs text-text-muted mt-2", "Find your Account ID in the right sidebar of " a { href: "https://dash.cloudflare.com", target: "_blank", rel: "noopener noreferrer", class: "text-primary underline", "dash.cloudflare.com" } } }
                    }
                    if xiaomi {
                        div { class: "bg-sidebar/50 p-4 rounded-lg border border-accent/20", h3 { class: "font-semibold mb-3 text-sm", "Xiaomi MiMo Token Plan Region" }
                            Select { label: "Region", value: region, options: vec![("sgp".to_owned(),"Singapore".to_owned()), ("cn".to_owned(),"China".to_owned()), ("ams".to_owned(),"Europe".to_owned())] }
                            p { class: "text-xs text-text-muted mt-2", "Token Plan keys are cluster-specific. Select the region matching your subscription." }
                        }
                    }
                    if azure {
                        div { class: "bg-sidebar/50 p-4 rounded-lg border border-accent/20", h3 { class: "font-semibold mb-3 text-sm", "Azure OpenAI Configuration" } div { class: "flex flex-col gap-3",
                            Field { label: "Azure Endpoint", value: endpoint, placeholder: "https://your-resource.openai.azure.com" }
                            Field { label: "Deployment Name", value: deployment, placeholder: "gpt-4" }
                            Field { label: "API Version", value: version, placeholder: "2024-10-01-preview" }
                            Field { label: "Organization", value: organization, placeholder: "Organization ID" }
                        } }
                    }
                    div { class: "flex flex-col gap-1.5", label { class: "type-body-sm-medium text-ink", "Priority" } div { class: "relative", input { r#type: "number", aria_label: "Priority", class: INPUT_CLASS, value: "{priority}", oninput: move |event| { let value = js_sys::parse_int(&event.value(), 10); priority.set(if value.is_nan() || value == 0.0 { 1.0 } else { value }); } } } }
                    Select { label: "Proxy Pool", placeholder: "None", value: pool_id, options: std::iter::once(("__none__".to_owned(),"None".to_owned())).chain(pools().iter().filter_map(|pool| Some((pool["id"].as_str()?.to_owned(),pool["name"].as_str()?.to_owned())))).collect::<Vec<_>>() }
                    if pools().is_empty() { p { class: "text-xs text-text-muted", "No active proxy pools available. Create one in Proxy Pools page first." } }
                    p { class: "text-xs text-text-muted", "Legacy manual proxy fields are still accepted by API for backward compatibility." }
                    div { class: "flex gap-2",
                        Button { label: if saving() { "Saving...".to_owned() } else { "Save".to_owned() }, full_width: true, disabled: saving() || !allowed_save, onclick: { let id = id.clone(); move |_| {
                            if *saving.peek() || !allowed_save { return; }
                            saving.set(true);
                            let id = id.clone();
                            let key_value = key();
                            let config = specific(&id, &endpoint(), &version(), &deployment(), &organization(), &account(), &region());
                            // Serialize representable integers as JSON integers so
                            // the backend's u32 deserializer accepts the full range.
                            let priority_value = priority();
                            let priority_json = if (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(&priority_value) { json!(priority_value as i64) } else { json!(priority_value) };
                            let mut body = json!({"provider": id, "name": name(), "apiKey": key_value, "priority": priority_json, "proxyPoolId": if pool_id() == "__none__" { Value::Null } else { json!(pool_id()) }});
                            if let Some(config) = config.clone() { body["providerSpecificData"] = config; }
                            spawn(async move {
                                let valid = if id == "a6api" { true } else {
                                    validating.set(true); validation.set(None);
                                    let valid = validate(&id, &key_value, config).await;
                                    validation.set(Some(valid)); validating.set(false); valid
                                };
                                body["testStatus"] = json!(if valid { "active" } else { "unknown" });
                                match dashboard_request("POST", "/api/providers", Some(&body)).await {
                                    Ok(response) => {
                                        let data = json_body(&response).await;
                                        if response.ok() { refresh(connections).await; provider.set(None); notify(toasts,"API key added",false); }
                                        else { notify(toasts,data.as_ref().and_then(|value| value["error"].as_str()).unwrap_or("Failed to save connection"),true); }
                                    }
                                    Err(_) => notify(toasts,"Failed to save connection",true),
                                }
                                saving.set(false);
                            });
                        } } }
                        Button { label: "Cancel".to_owned(), variant: "ghost", full_width: true, onclick: move |_| close.call(()) }
                    }
                }
            }
        }
    }
}

#[component]
fn Field(
    label: &'static str,
    mut value: Signal<String>,
    #[props(default)] placeholder: &'static str,
    #[props(default)] password: bool,
    #[props(default)] extra_class: &'static str,
) -> Element {
    rsx! { div { class: "flex flex-col gap-1.5 {extra_class}", label { class: "type-body-sm-medium text-ink", "{label}" } div { class: "relative", input { r#type: if password { "password" } else { "text" }, aria_label: label, placeholder, value: "{value}", class: INPUT_CLASS, oninput: move |event| value.set(event.value()) } } } }
}

#[component]
fn Select(
    label: &'static str,
    #[props(default = "Select an option")] placeholder: &'static str,
    mut value: Signal<String>,
    options: Vec<(String, String)>,
) -> Element {
    rsx! { div { class: "flex flex-col gap-1.5", label { class: "text-sm font-medium text-text-main", "{label}" } div { class: "relative",
        select { value: "{value}", aria_label: label, class: SELECT_CLASS, onchange: move |event| value.set(event.value()),
            option { value: "", disabled: true, "{placeholder}" }
            for (option_value, option_label) in options { option { key: "{option_value}", value: "{option_value}", "{option_label}" } }
        }
        div { class: "absolute inset-y-0 right-0 flex items-center pr-3 pointer-events-none text-text-muted", span { class: "material-symbols-outlined text-[20px]", "expand_more" } }
    } } }
}
