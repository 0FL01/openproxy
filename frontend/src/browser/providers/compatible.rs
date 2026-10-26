use super::{dashboard_request, json, json_body, Value, BADGE};
use crate::browser::ui::{Button, Modal, INPUT_CLASS};
use dioxus::prelude::*;

#[component]
pub(super) fn CompatibleModal(
    mut open: Signal<bool>,
    kind: &'static str,
    mut nodes: Signal<Vec<Value>>,
) -> Element {
    let anthropic = kind == "anthropic-compatible";
    let default_url = if anthropic {
        "https://api.anthropic.com/v1"
    } else {
        "https://api.openai.com/v1"
    };
    let title = if anthropic {
        "Add Anthropic Compatible"
    } else {
        "Add OpenAI Compatible"
    };
    let mut name = use_signal(String::new);
    let mut prefix = use_signal(String::new);
    let mut api_type = use_signal(|| "chat".to_owned());
    let mut base_url = use_signal(|| default_url.to_owned());
    let mut check_key = use_signal(String::new);
    let mut model = use_signal(String::new);
    let mut submitting = use_signal(|| false);
    let mut validating = use_signal(|| false);
    let mut validation = use_signal(|| None::<Value>);
    use_effect(move || {
        if anthropic && open() {
            check_key.set(String::new());
            model.set(String::new());
            validation.set(None);
        }
    });
    if !open() {
        return rsx! {};
    }

    let check_disabled = check_key().is_empty() || validating() || base_url().trim().is_empty();
    let create_disabled = name().trim().is_empty()
        || prefix().trim().is_empty()
        || base_url().trim().is_empty()
        || submitting();
    let name_placeholder = if anthropic {
        "Anthropic Compatible (Prod)"
    } else {
        "OpenAI Compatible (Prod)"
    };
    let prefix_placeholder = if anthropic { "ac-prod" } else { "oc-prod" };
    let base_hint = if anthropic {
        "Use the base URL (ending in /v1) for your Anthropic-compatible API. The system will append /messages."
    } else {
        "Use the base URL (ending in /v1) for your OpenAI-compatible API."
    };
    let model_placeholder = if anthropic {
        "e.g. claude-3-opus"
    } else {
        "e.g. gpt-4, claude-3-opus"
    };
    rsx! {
        Modal { title: title.to_owned(), on_close: move |_| open.set(false),
            div { class: "flex flex-col gap-4",
                Field { label: "Name", value: name, placeholder: name_placeholder, hint: "Required. A friendly label for this node." }
                Field { label: "Prefix", value: prefix, placeholder: prefix_placeholder, hint: "Required. Used as the provider prefix for model IDs." }
                if !anthropic {
                    div { class: "flex flex-col gap-1.5",
                        label { class: "text-sm font-medium text-text-main", "API Type" }
                        div { class: "relative",
                            select { value: "{api_type}", aria_label: "API Type", class: "w-full py-2.5 px-3 pr-10 text-sm text-text-main bg-surface-2 border border-transparent rounded-[10px] appearance-none focus:outline-none focus:ring-2 focus:ring-brand-500/30 focus:border-brand-500/40 transition-all duration-150 disabled:opacity-50 disabled:cursor-not-allowed text-[16px] sm:text-sm", onchange: move |event| { api_type.set(event.value()); base_url.set(default_url.to_owned()); },
                                option { value: "", disabled: true, "Select an option" }
                                option { value: "chat", "Chat Completions" }
                                option { value: "responses", "Responses API" }
                            }
                            div { class: "absolute inset-y-0 right-0 flex items-center pr-3 pointer-events-none text-text-muted", span { class: "material-symbols-outlined text-[20px]", "expand_more" } }
                        }
                    }
                }
                Field { label: "Base URL", value: base_url, placeholder: default_url, hint: base_hint }
                Field { label: "API Key (for Check)", value: check_key, password: true }
                Field { label: "Model ID (optional)", value: model, placeholder: model_placeholder, hint: "If provider lacks /models endpoint, enter a model ID to validate via chat/completions instead." }
                div { class: "flex flex-col gap-3 sm:flex-row sm:items-center",
                    Button { label: if validating() { "Checking...".to_owned() } else { "Check".to_owned() }, variant: "secondary", disabled: check_disabled, extra_class: "w-full sm:w-auto", onclick: move |_| {
                        if check_key.peek().is_empty() || base_url.peek().trim().is_empty() || *validating.peek() { return; }
                        validating.set(true);
                        let mut body = json!({"baseUrl": base_url(), "apiKey": check_key(), "type": kind});
                        let model_id = model().trim().to_owned();
                        if !model_id.is_empty() { body["modelId"] = json!(model_id); }
                        spawn(async move {
                            let data = match dashboard_request("POST", "/api/provider-nodes/validate", Some(&body)).await {
                                Ok(response) => json_body(&response).await,
                                Err(_) => None,
                            }.unwrap_or_else(|| json!({"valid":false,"error":"Network error"}));
                            validation.set(Some(data));
                            validating.set(false);
                        });
                    } }
                    if let Some(result) = validation() {
                        if result["valid"] == true {
                            span { class: "{BADGE} bg-success-bg text-success-text px-2.5 py-1 text-[11px] tracking-wide", "Valid" }
                            if result["method"] == "chat" { span { class: "text-sm text-text-muted", "(via inference test)" } }
                        } else {
                            div { class: "flex flex-col gap-1",
                                span { class: "{BADGE} bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)] px-2.5 py-1 text-[11px] tracking-wide", "Invalid" }
                                if let Some(error) = result["error"].as_str() { span { class: "text-sm text-red-500", "{error}" } }
                            }
                        }
                    }
                }
                div { class: "flex flex-col gap-2 sm:flex-row",
                    Button { label: if submitting() { "Creating...".to_owned() } else { "Create".to_owned() }, full_width: true, disabled: create_disabled, onclick: move |_| {
                        if name.peek().trim().is_empty() || prefix.peek().trim().is_empty() || base_url.peek().trim().is_empty() || *submitting.peek() { return; }
                        submitting.set(true);
                        let mut body = json!({"name": name(), "prefix": prefix(), "baseUrl": base_url(), "type": kind});
                        if !anthropic { body["apiType"] = json!(api_type()); }
                        spawn(async move {
                            if let Ok(response) = dashboard_request("POST", "/api/provider-nodes", Some(&body)).await
                                && response.ok()
                                && let Some(data) = json_body(&response).await
                                && data["node"]["id"].as_str().is_some()
                            {
                                nodes.write().push(data["node"].clone());
                                open.set(false);
                                name.set(String::new());
                                prefix.set(String::new());
                                base_url.set(default_url.to_owned());
                                api_type.set("chat".to_owned());
                                check_key.set(String::new());
                                validation.set(None);
                            }
                            submitting.set(false);
                        });
                    } }
                    Button { label: "Cancel".to_owned(), variant: "ghost", full_width: true, onclick: move |_| open.set(false) }
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
    #[props(default)] hint: &'static str,
    #[props(default)] password: bool,
) -> Element {
    rsx! { div { class: "flex flex-col gap-1.5",
        label { class: "type-body-sm-medium text-ink", "{label}" }
        div { class: "relative", input { r#type: if password { "password" } else { "text" }, aria_label: label, placeholder, value: "{value}", class: INPUT_CLASS, oninput: move |event| value.set(event.value()) } }
        if !hint.is_empty() { p { class: "type-body-sm text-slate", "{hint}" } }
    } }
}
