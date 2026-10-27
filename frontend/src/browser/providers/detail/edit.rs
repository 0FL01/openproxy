use super::{dashboard_request, encoded, json, json_body, text, Value};
use crate::browser::ui::{Button, Modal, INPUT_CLASS};
use dioxus::prelude::*;

const SELECT_CLASS: &str = "w-full py-2.5 px-3 pr-10 text-sm text-text-main bg-surface-2 border border-transparent rounded-[10px] appearance-none focus:outline-none focus:ring-2 focus:ring-brand-500/30 focus:border-brand-500/40 transition-all duration-150 disabled:opacity-50 disabled:cursor-not-allowed text-[16px] sm:text-sm";

#[component]
pub(super) fn EditConnection(
    provider_id: String,
    mut selected: Signal<Option<Value>>,
    pools: Signal<Vec<Value>>,
    on_saved: Callback<()>,
    #[props(default)] compatible: bool,
) -> Element {
    let mut name = use_signal(String::new);
    let mut email = use_signal(String::new);
    let mut display_name = use_signal(String::new);
    let mut priority = use_signal(|| 1_f64);
    let mut pool = use_signal(|| "__none__".to_owned());
    let mut key = use_signal(String::new);
    let mut endpoint = use_signal(String::new);
    let mut version = use_signal(String::new);
    let mut deployment = use_signal(String::new);
    let mut organization = use_signal(String::new);
    let mut account = use_signal(String::new);
    let mut result = use_signal(|| None::<bool>);
    let mut fingerprint = use_signal(|| None::<Value>);
    let mut checking = use_signal(|| false);
    let mut testing = use_signal(|| false);
    let mut test_result = use_signal(|| None::<bool>);
    let mut test_generation = use_signal(|| 0_u64);
    let mut saving = use_signal(|| false);
    let mut error = use_signal(String::new);
    use_effect(move || {
        let next_test_generation = (*test_generation.peek()).wrapping_add(1);
        test_generation.set(next_test_generation);
        testing.set(false);
        test_result.set(None);
        if let Some(conn) = selected() {
            name.set(text(&conn, "name").into());
            email.set(text(&conn, "email").into());
            display_name.set(text(&conn, "displayName").into());
            priority.set(
                conn["priority"]
                    .as_f64()
                    .filter(|n| *n != 0.0)
                    .unwrap_or(1.0),
            );
            pool.set(
                conn["providerSpecificData"]["proxyPoolId"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("__none__")
                    .into(),
            );
            key.set(String::new());
            let specific = &conn["providerSpecificData"];
            endpoint.set(text(specific, "azureEndpoint").into());
            version.set(
                specific["apiVersion"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("2024-10-01-preview")
                    .into(),
            );
            deployment.set(text(specific, "deployment").into());
            organization.set(text(specific, "organization").into());
            account.set(text(specific, "accountId").into());
            result.set(None);
            fingerprint.set(None);
            error.set(String::new());
        } else {
            key.set(String::new());
            fingerprint.set(None);
        }
    });
    let current = selected();
    let is_open = current.is_some();
    let conn = current.unwrap_or_default();
    let oauth = text(&conn, "authType") == "oauth";
    let id = text(&conn, "id").to_owned();
    let azure = provider_id == "azure";
    let cloudflare = provider_id == "cloudflare-ai";
    let compatible = compatible
        || provider_id.starts_with("openai-compatible-")
        || provider_id.starts_with("anthropic-compatible-");
    let config = if azure {
        Some(
            json!({"azureEndpoint":endpoint(),"apiVersion":version(),"deployment":deployment(),"organization":organization()}),
        )
    } else if cloudflare {
        Some(json!({"accountId":account()}))
    } else {
        None
    };
    let has_key = conn["hasApiKey"] == true;
    let name_placeholder = if oauth {
        "Account name"
    } else {
        "Production Key"
    };
    let email_placeholder = if oauth {
        "Account email"
    } else {
        "Optional email"
    };
    let email_hint = if oauth {
        "OAuth account identity (editable label)"
    } else {
        "Optional identity label"
    };
    let key_placeholder = if has_key {
        "•••••••• (saved — leave blank to keep)"
    } else {
        "Enter new API key"
    };
    let key_hint = if has_key {
        "A key is already stored. Leave blank to keep it, or type a new one to replace."
    } else {
        "Required."
    };
    let check = {
        let id = id.clone();
        let provider_id = provider_id.clone();
        let config = config.clone();
        use_callback(move |()| {
            if checking() || saving() {
                return;
            }
            let id = id.clone();
            let provider = provider_id.clone();
            let key_value = key();
            let config = config.clone();
            if key_value.is_empty() && !has_key {
                return;
            }
            checking.set(true);
            result.set(None);
            fingerprint.set(None);
            spawn(async move {
                let is_saved = key_value.is_empty();
                let mut body = json!({"provider":provider,"apiKey":key_value});
                if let Some(config) = config {
                    body["providerSpecificData"] = config;
                }
                let response = if is_saved {
                    dashboard_request(
                        "POST",
                        &format!("/api/providers/{}/test", encoded(&id)),
                        None,
                    )
                    .await
                } else {
                    dashboard_request("POST", "/api/providers/validate", Some(&body)).await
                };
                let valid = if let Ok(response) = response {
                    json_body(&response)
                        .await
                        .is_some_and(|data| data["valid"] == true)
                } else {
                    false
                };
                if selected
                    .peek()
                    .as_ref()
                    .is_some_and(|conn| text(conn, "id") == id)
                    && *key.peek() == key_value
                {
                    if !is_saved && valid {
                        fingerprint.set(Some(body));
                    }
                    result.set(Some(valid));
                }
                checking.set(false);
            });
        })
    };
    let test_connection = {
        let id = id.clone();
        use_callback(move |()| {
            if *testing.peek() || id.is_empty() {
                return;
            }
            let id = id.clone();
            let generation = *test_generation.peek();
            testing.set(true);
            test_result.set(None);
            spawn(async move {
                let valid = if let Ok(response) = dashboard_request(
                    "POST",
                    &format!("/api/providers/{}/test", encoded(&id)),
                    None,
                )
                .await
                {
                    json_body(&response)
                        .await
                        .is_some_and(|data| data["valid"] == true)
                } else {
                    false
                };
                if *test_generation.peek() == generation
                    && selected
                        .peek()
                        .as_ref()
                        .is_some_and(|conn| text(conn, "id") == id)
                {
                    test_result.set(Some(valid));
                    testing.set(false);
                }
            });
        })
    };
    if !is_open {
        return rsx! {};
    }
    rsx! { Modal { title: "Edit Connection".to_owned(), on_close: move |_| selected.set(None),
        div { class: "flex flex-col gap-4",
            Field { label: "Name", value: name, placeholder: name_placeholder }
            Field { label: "Display Name", value: display_name, placeholder: "Optional secondary label", hint: "Shown under the primary name when it differs" }
            Field { label: "Email", value: email, placeholder: email_placeholder, hint: email_hint }
            div { class: "flex flex-col gap-1.5", label { class: "type-body-sm-medium text-ink", "Priority" } input { r#type: "number", aria_label: "Priority", class: INPUT_CLASS, value: "{priority}", oninput: move |e| { let n = js_sys::parse_int(&e.value(), 10); priority.set(if n.is_nan() || n == 0.0 { 1.0 } else { n }); } } }
            div { class: "flex flex-col gap-1.5", label { class: "text-sm font-medium text-text-main", "Proxy Pool" } div { class: "relative", select { class: SELECT_CLASS, aria_label: "Proxy Pool", value: "{pool}", onchange: move |e| pool.set(e.value()), option { value: "__none__", "None" } for item in pools() { option { value: "{text(&item, \"id\")}", "{text(&item, \"name\")}" } } } div { class: "absolute inset-y-0 right-0 flex items-center pr-3 pointer-events-none text-text-muted", span { class: "material-symbols-outlined text-[20px]", "expand_more" } } } }
            if pools().is_empty() { p { class: "text-xs text-text-muted", "No active proxy pools available. Create one in Proxy Pools page first." } }
            if !oauth { div { class: "flex gap-2", div { class: "flex-1", Field { label: "API Key", value: key, password: true, placeholder: key_placeholder, hint: key_hint } } div { class: "pt-6", Button { label: if checking() { "Checking...".to_owned() } else { "Check".to_owned() }, variant: "secondary", disabled: checking() || saving() || key().is_empty() && !has_key, onclick: move |_| check.call(()) } } }
                if has_key && key().is_empty() { span { class: "text-xs text-text-muted", "Key on file" } }
                if let Some(valid) = result() { span { class: if valid { "text-xs text-green-600" } else { "text-xs text-red-500" }, if key().is_empty() { if valid { "Saved key works" } else { "Saved key failed test" } } else if valid { "Valid" } else { "Invalid" } } }
            }
            if azure { div { class: "bg-sidebar/50 p-4 rounded-lg border border-accent/20", h3 { class: "font-semibold mb-3 text-sm", "Azure OpenAI Configuration" } div { class: "flex flex-col gap-3", Field { label: "Azure Endpoint", value: endpoint, placeholder: "https://your-resource.openai.azure.com" } Field { label: "Deployment Name", value: deployment, placeholder: "gpt-4" } Field { label: "API Version", value: version, placeholder: "2024-10-01-preview" } Field { label: "Organization", value: organization, placeholder: "Organization ID" } } } }
            if cloudflare { div { class: "bg-sidebar/50 p-4 rounded-lg border border-accent/20", h3 { class: "font-semibold mb-3 text-sm", "Cloudflare Workers AI" } Field { label: "Account ID", value: account, placeholder: "abc123def456..." } } }
            if !compatible && !azure && !cloudflare { div { class: "flex items-center gap-3",
                Button { label: if testing() { "Testing...".to_owned() } else { "Test Connection".to_owned() }, variant: "secondary", disabled: testing(), onclick: move |_| test_connection.call(()) }
                if let Some(valid) = test_result() { span { class: if valid { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-success-bg text-success-text px-2.5 py-1 text-[11px] tracking-wide" } else { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)] px-2.5 py-1 text-[11px] tracking-wide" }, if valid { "Valid" } else { "Failed" } } }
            } }
            if !error().is_empty() { p { role: "alert", class: "text-sm text-red-500", "{error}" } }
            div { class: "flex gap-2",
                Button { label: if saving() { "Saving...".to_owned() } else { "Save".to_owned() }, full_width: true, disabled: saving(), onclick: { let id = id.clone(); let provider = provider_id.clone(); let config = config.clone(); move |_| {
                    if saving() { return; } saving.set(true); error.set(String::new());
                    let id = id.clone(); let provider = provider.clone(); let config = config.clone(); let key_value = key();
                    let n = priority(); let priority = if (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(&n) { json!(n as i64) } else { json!(n) };
                    let mut updates = json!({"name":name(),"email":email(),"displayName":display_name(),"priority":priority,"proxyPoolId":if pool() == "__none__" { Value::Null } else { json!(pool()) }});
                    if let Some(config) = config.clone() { updates["providerSpecificData"] = config; }
                    let mut validation_body = json!({"provider":provider,"apiKey":key_value});
                    if let Some(config) = config { validation_body["providerSpecificData"] = config; }
                    let prior_valid = fingerprint() == Some(validation_body.clone()) && result() == Some(true);
                    spawn(async move {
                        if !oauth && !key_value.is_empty() {
                            updates["apiKey"] = json!(key_value);
                            let valid = if provider == "a6api" || prior_valid { true } else {
                                checking.set(true);
                                let valid = if let Ok(response) = dashboard_request("POST", "/api/providers/validate", Some(&validation_body)).await { json_body(&response).await.is_some_and(|v| v["valid"] == true) } else { false };
                                checking.set(false); result.set(Some(valid)); valid
                            };
                            if valid { updates["testStatus"] = json!("active"); updates["lastError"] = Value::Null; updates["lastErrorAt"] = Value::Null; }
                        }
                        let response = dashboard_request("PUT", &format!("/api/providers/{}", encoded(&id)), Some(&updates)).await;
                        match response {
                            Ok(response) if response.ok() => { if selected.peek().as_ref().is_some_and(|v| text(v, "id") == id) { selected.set(None); } on_saved.call(()); }
                            Ok(response) => { let body = json_body(&response).await.unwrap_or_default(); error.set(body["error"].as_str().unwrap_or("Failed to update connection").to_owned()); }
                            Err(_) => error.set("Failed to update connection".into()),
                        }
                        saving.set(false);
                    });
                } } }
                Button { label: "Cancel".to_owned(), variant: "ghost", full_width: true, onclick: move |_| selected.set(None) }
            }
        }
    } }
}

#[component]
fn Field(
    label: &'static str,
    mut value: Signal<String>,
    #[props(default)] placeholder: &'static str,
    #[props(default)] hint: &'static str,
    #[props(default)] password: bool,
) -> Element {
    rsx! { div { class: "flex flex-col gap-1.5", label { class: "type-body-sm-medium text-ink", "{label}" } div { class: "relative", input { r#type: if password { "password" } else { "text" }, aria_label: label, placeholder, class: INPUT_CLASS, value: "{value}", oninput: move |e| value.set(e.value()) } } if !hint.is_empty() { p { class: "text-xs text-text-muted", "{hint}" } } } }
}
