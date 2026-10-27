use super::{js_trim, json, text, Value};
use crate::browser::ui::{Button, Modal, INPUT_CLASS};
use dioxus::prelude::*;

const SELECT_CLASS: &str = "w-full py-2.5 px-3 pr-10 text-sm text-text-main bg-surface-2 border border-transparent rounded-[10px] appearance-none focus:outline-none focus:ring-2 focus:ring-brand-500/30 focus:border-brand-500/40 transition-all duration-150 disabled:opacity-50 disabled:cursor-not-allowed text-[16px] sm:text-sm";

#[derive(Clone)]
pub(super) struct NodeRequest {
    pub attempt: u64,
    pub body: Value,
}

#[component]
pub(super) fn EditCompatibleNode(
    node: Value,
    anthropic: bool,
    attempt: u64,
    saving: Signal<bool>,
    save_error: Signal<String>,
    validating: Signal<bool>,
    validation: Signal<Option<bool>>,
    close: Callback<u64>,
    changed: Callback<()>,
    save: Callback<NodeRequest>,
    check: Callback<NodeRequest>,
) -> Element {
    let default_url = if anthropic {
        "https://api.anthropic.com/v1"
    } else {
        "https://api.openai.com/v1"
    };
    let name = use_signal(|| text(&node, "name").to_owned());
    let prefix = use_signal(|| text(&node, "prefix").to_owned());
    let mut api_type = use_signal(|| {
        node["apiType"]
            .as_str()
            .filter(|value| !value.is_empty())
            .unwrap_or("chat")
            .to_owned()
    });
    let base_url = use_signal(|| {
        node["baseUrl"]
            .as_str()
            .filter(|value| !value.is_empty())
            .unwrap_or(default_url)
            .to_owned()
    });
    // Never populate a check key from a saved connection.
    let check_key = use_signal(String::new);
    let model_id = use_signal(String::new);
    let required_missing = js_trim(&name()).is_empty()
        || js_trim(&prefix()).is_empty()
        || js_trim(&base_url()).is_empty();
    let check_disabled = check_key().is_empty() || validating() || js_trim(&base_url()).is_empty();
    let title = if anthropic {
        "Edit Anthropic Compatible"
    } else {
        "Edit OpenAI Compatible"
    };
    let name_placeholder = if anthropic {
        "Anthropic Compatible (Prod)"
    } else {
        "OpenAI Compatible (Prod)"
    };
    let prefix_placeholder = if anthropic { "ac-prod" } else { "oc-prod" };
    let base_hint = if anthropic {
        "Use the base URL (ending in /v1) for your Anthropic-compatible API."
    } else {
        "Use the base URL (ending in /v1) for your OpenAI-compatible API."
    };
    rsx! {
        Modal { title: title.to_owned(), on_close: move |_| close.call(attempt),
            div { class: "flex flex-col gap-4",
                Field { label: "Name", value: name, placeholder: name_placeholder, hint: "Required. A friendly label for this node.", changed }
                Field { label: "Prefix", value: prefix, placeholder: prefix_placeholder, hint: "Required. Used as the provider prefix for model IDs.", changed }
                if !anthropic { div { class: "flex flex-col gap-1.5",
                    label { class: "text-sm font-medium text-text-main", "API Type" }
                    div { class: "relative", select { class: SELECT_CLASS, aria_label: "API Type", value: "{api_type}", onchange: move |event| { api_type.set(event.value()); changed.call(()); },
                        option { value: "", disabled: true, "Select an option" }
                        option { value: "chat", "Chat Completions" }
                        option { value: "responses", "Responses API" }
                    } div { class: "absolute inset-y-0 right-0 flex items-center pr-3 pointer-events-none text-text-muted", span { class: "material-symbols-outlined text-[20px]", "expand_more" } } }
                } }
                Field { label: "Base URL", value: base_url, placeholder: default_url, hint: base_hint, changed }
                div { class: "flex gap-2",
                    Field { label: "API Key (for Check)", value: check_key, password: true, extra_class: "flex-1", changed }
                    div { class: "pt-6", Button { label: if validating() { "Checking...".to_owned() } else { "Check".to_owned() }, variant: "secondary", disabled: check_disabled, onclick: move |_| {
                        if check_key.peek().is_empty() || js_trim(&base_url.peek()).is_empty() || *validating.peek() { return; }
                        let mut body = json!({"baseUrl":base_url(), "apiKey":check_key(), "type":if anthropic { "anthropic-compatible" } else { "openai-compatible" }});
                        let model = js_trim(&model_id());
                        if !model.is_empty() { body["modelId"] = json!(model); }
                        check.call(NodeRequest { attempt, body });
                    } } }
                }
                Field { label: "Model ID (optional)", value: model_id, placeholder: "e.g. my-model-id", hint: "If provider lacks /models endpoint, enter a model ID to validate via chat/completions instead.", changed }
                if let Some(valid) = validation() { span { class: if valid { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-success-bg text-success-text px-2.5 py-1 text-[11px] tracking-wide" } else { "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)] px-2.5 py-1 text-[11px] tracking-wide" }, if valid { "Valid" } else { "Invalid" } } }
                if !save_error().is_empty() { p { role: "alert", class: "text-sm text-red-500", "{save_error}" } }
                div { class: "flex gap-2",
                    Button { label: if saving() { "Saving...".to_owned() } else { "Save".to_owned() }, full_width: true, disabled: required_missing || saving(), onclick: move |_| {
                        if js_trim(&name.peek()).is_empty() || js_trim(&prefix.peek()).is_empty() || js_trim(&base_url.peek()).is_empty() || *saving.peek() { return; }
                        let mut body = json!({"name":name(), "prefix":prefix(), "baseUrl":base_url()});
                        if !anthropic { body["apiType"] = json!(api_type()); }
                        save.call(NodeRequest { attempt, body });
                    } }
                    Button { label: "Cancel".to_owned(), variant: "ghost", full_width: true, onclick: move |_| close.call(attempt) }
                }
            }
        }
    }
}

#[component]
fn Field(
    label: &'static str,
    mut value: Signal<String>,
    changed: Callback<()>,
    #[props(default)] placeholder: &'static str,
    #[props(default)] hint: &'static str,
    #[props(default)] extra_class: &'static str,
    #[props(default)] password: bool,
) -> Element {
    rsx! { div { class: "flex flex-col gap-1.5 {extra_class}",
        label { class: "type-body-sm-medium text-ink", "{label}" }
        div { class: "relative", input { r#type: if password { "password" } else { "text" }, aria_label: label, placeholder, value: "{value}", class: INPUT_CLASS, oninput: move |event| { value.set(event.value()); changed.call(()); } } }
        if !hint.is_empty() { p { class: "type-body-sm text-slate", "{hint}" } }
    } }
}
