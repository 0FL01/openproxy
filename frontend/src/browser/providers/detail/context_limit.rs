use super::{dashboard_get_no_store, dashboard_request, json, json_body, notify, Toast, Value};
use dioxus::prelude::*;
use wasm_bindgen::JsCast;

const DEFAULT_LIMIT: &str = "500000";
const CODEX_TITLE: &str = "Context metadata advertised to clients. Codex compatibility metadata reserves 50,000 input tokens and reports 128,000 output tokens; OpenCode owns compaction policy. Proxy memory byte limits are separate.";
const TITLE: &str = "Context metadata advertised to clients; a model's smaller native context window still applies. Proxy memory byte limits are separate.";

async fn settings() -> Result<Value, &'static str> {
    let response = dashboard_get_no_store("/api/settings")
        .await
        .map_err(|_| "Failed to load context limits")?;
    if !response.ok() {
        return Err("Failed to load context limits");
    }
    json_body(&response)
        .await
        .filter(Value::is_object)
        .ok_or("Failed to load context limits")
}

#[component]
pub(super) fn ProviderContextLimit(provider_id: String, on_saved: Callback<()>) -> Element {
    let mut draft = use_signal(|| DEFAULT_LIMIT.to_owned());
    let mut saved = use_signal(|| DEFAULT_LIMIT.to_owned());
    let mut loading = use_signal(|| true);
    let mut saving = use_signal(|| false);
    let toasts = use_context::<Signal<Vec<Toast>>>();

    let title = if provider_id == "codex" {
        CODEX_TITLE
    } else {
        TITLE
    };
    let provider_for_load = provider_id.clone();
    use_future(move || {
        let provider_id = provider_for_load.clone();
        async move {
            if let Ok(data) = settings().await {
                let value = data["providerContextLimits"][&provider_id]
                    .as_u64()
                    .filter(|value| *value > 0)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| DEFAULT_LIMIT.to_owned());
                draft.set(value.clone());
                saved.set(value);
            }
            loading.set(false);
        }
    });

    let save = use_callback(move |()| {
        if *loading.peek() || *saving.peek() || *draft.peek() == *saved.peek() {
            return;
        }
        // Number inputs accept exponent and decimal notation. Match the
        // reference's Number/Number.isInteger check, not just digit strings.
        let value = draft.peek().parse::<f64>().ok().filter(|value| {
            value.is_finite() && value.fract() == 0.0 && (1.0..=1_000_000.0).contains(value)
        });
        let Some(value) = value else {
            draft.set(saved.peek().clone());
            notify(
                toasts,
                "Context limit must be a whole number from 1 to 1,000,000",
                true,
            );
            return;
        };
        let value = value as u32;
        saving.set(true);
        let provider_id = provider_id.clone();
        spawn(async move {
            let result = async {
                let data = settings().await?;
                let mut limits = match data.get("providerContextLimits") {
                    None | Some(Value::Null) => serde_json::Map::new(),
                    Some(Value::Object(limits)) => limits.clone(),
                    _ => return Err("Failed to load context limits"),
                };
                limits.insert(provider_id, json!(value));
                let response = dashboard_request(
                    "PATCH",
                    "/api/settings",
                    Some(&json!({"providerContextLimits": limits})),
                )
                .await
                .map_err(|_| "Failed to save context limit")?;
                if !response.ok() {
                    return Err("Failed to save context limit");
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => {
                    let value = value.to_string();
                    saved.set(value.clone());
                    draft.set(value);
                    on_saved.call(());
                }
                Err(error) => {
                    draft.set(saved.peek().clone());
                    notify(toasts, error, true);
                }
            }
            saving.set(false);
        });
    });

    rsx! {
        label { class: "flex items-center gap-2", title,
            span { class: "text-xs text-text-muted font-medium", "Advertised Context" }
            input {
                r#type: "number", min: "1", max: "1000000", step: "1",
                value: "{draft}", disabled: loading() || saving(),
                class: "w-24 px-2 py-1 text-xs border border-border rounded-md bg-background focus:outline-none focus:border-primary disabled:opacity-60",
                oninput: move |event| draft.set(event.value()),
                onblur: move |_| save.call(()),
                onkeydown: move |event| {
                    if event.key() == Key::Enter {
                        event.prevent_default();
                        if let Some(element) = web_sys::window().and_then(|window| window.document()).and_then(|document| document.active_element()).and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok()) {
                            let _ = element.blur();
                        }
                    }
                },
            }
        }
    }
}
