use super::{date, number, text};
use dioxus::prelude::*;
use serde_json::Value;
use std::rc::Rc;
use wasm_bindgen::{closure::Closure, JsCast};

struct DrawerGuard {
    document: web_sys::Document,
    handler: Closure<dyn FnMut(web_sys::KeyboardEvent)>,
}

impl Drop for DrawerGuard {
    fn drop(&mut self) {
        let _ = self
            .document
            .remove_event_listener_with_callback("keydown", self.handler.as_ref().unchecked_ref());
        if let Some(body) = self.document.body() {
            let _ = body.style().remove_property("overflow");
        }
    }
}

#[component]
fn Field(label: &'static str, value: Option<String>) -> Element {
    let value = value.as_deref().unwrap_or("—");
    rsx! { div { class: "min-w-0", div { class: "text-xs font-medium uppercase tracking-wide text-text-muted", "{label}" } div { class: "mt-1 break-all font-mono text-sm text-text-main", "{value}" } } }
}

fn display(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        _ => Some(value.to_string()),
    }
}

#[component]
fn Counts(title: &'static str, values: Value) -> Element {
    let entries = values.as_object().cloned().unwrap_or_default();
    if entries.is_empty() {
        return rsx! {};
    }
    rsx! { div { div { class: "text-xs font-medium uppercase tracking-wide text-text-muted", "{title}" } div { class: "mt-1 grid grid-cols-1 gap-1 sm:grid-cols-2", for (key, value) in entries { div { key: "{key}", class: "flex items-baseline justify-between gap-2 font-mono text-xs text-text-main", span { class: "break-all", "{key}" } span { class: "text-text-muted", "×{value}" } } } } } }
}

#[component]
fn Trace(trace: Value) -> Element {
    let names = trace["toolNames"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let entries = trace["entries"].as_array().cloned().unwrap_or_default();
    rsx! { details { class: "rounded-lg border border-border bg-bg-subtle p-4",
        summary { class: "cursor-pointer text-sm font-semibold text-text-main", "Stream trace" }
        div { class: "mt-3 space-y-3",
            div { class: "grid grid-cols-2 gap-3 sm:grid-cols-4",
                Field { label: "Stop reason", value: display(&trace["stopReason"]) }
                Field { label: "Finish reason", value: display(&trace["finishReason"]) }
                Field { label: "Completed", value: display(&trace["completedCount"]) }
                Field { label: "Errors", value: display(&trace["errorCount"]) }
                Field { label: "After completed", value: display(&trace["framesAfterCompleted"]) }
                Field { label: "[DONE] sent", value: Some(if trace["doneSent"] == true { "yes" } else { "no" }.to_owned()) }
                Field { label: "Overflowed", value: Some(display(&trace["overflowed"]).unwrap_or_else(|| "0".to_owned())) }
                if !names.is_empty() { Field { label: "Tool calls", value: Some(names.join(", ")) } }
            }
            Counts { title: "Upstream events", values: trace["upstreamEvents"].clone() }
            Counts { title: "Emitted events", values: trace["emittedEvents"].clone() }
            Counts { title: "Item types", values: trace["itemTypes"].clone() }
            div { div { class: "text-xs font-medium uppercase tracking-wide text-text-muted", "Event order" } ol { class: "mt-1 space-y-0.5 font-mono text-xs text-text-main", for (index, entry) in entries.iter().enumerate() { li { key: "{index}", class: "break-all", span { class: "text-text-muted", "{index + 1:02}" } " {entry.as_str().unwrap_or(\"\")}" } } } }
        }
    } }
}

#[component]
pub(super) fn LogDrawer(log: Value, on_close: EventHandler<()>) -> Element {
    use_hook(move || {
        let document = web_sys::window().and_then(|w| w.document())?;
        if let Some(body) = document.body() {
            let _ = body.style().set_property("overflow", "hidden");
        }
        let handler = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
            move |event: web_sys::KeyboardEvent| {
                if event.key() == "Escape" {
                    on_close.call(());
                }
            },
        );
        let _ =
            document.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
        Some(Rc::new(DrawerGuard { document, handler }))
    });
    let throughput = text(&log, "status") == "success"
        && log["tokensPerSecond"].as_f64().is_some()
        && log["generatedOutputTokens"].as_f64().is_some()
        && log["upstreamDurationMs"].as_f64().is_some();
    let tps = log["tokensPerSecond"].as_f64().unwrap_or_default();
    rsx! { div { class: "fixed inset-0 z-50",
        div { class: "absolute inset-0 bg-black/50 backdrop-blur-[2px] fade-in cursor-pointer", aria_hidden: "true", onclick: move |_| on_close.call(()) }
        div { class: "absolute right-0 top-0 h-full bg-surface flex flex-col shadow-[var(--shadow-elev)] slide-in-right border-l border-border-subtle w-[600px]",
            div { class: "flex items-center justify-between p-6 border-b border-border-subtle flex-shrink-0", div { class: "flex items-center gap-3", h2 { class: "text-lg font-semibold text-text-main", "Request details" } } button { r#type: "button", class: "p-1.5 rounded-[10px] text-text-muted hover:bg-surface-2 hover:text-text-main transition-colors", onclick: move |_| on_close.call(()), span { class: "material-symbols-outlined text-[20px]", "close" } } }
            div { class: "flex-1 overflow-y-auto p-6 custom-scrollbar", div { class: "space-y-6",
                div { class: "grid grid-cols-1 gap-4 sm:grid-cols-2",
                    Field { label: "Status", value: Some(text(&log, "status").to_uppercase()) }
                    Field { label: "HTTP status", value: display(&log["statusCode"]) }
                    if !text(&log, "errorKind").is_empty() { Field { label: "Error kind", value: display(&log["errorKind"]) } }
                    if !text(&log, "errorCode").is_empty() { Field { label: "Error code", value: display(&log["errorCode"]) } }
                    Field { label: "Request ID", value: display(&log["requestId"]) }
                    Field { label: "Route", value: display(&log["route"]) }
                    Field { label: "Model", value: display(&log["model"]) }
                    Field { label: "Provider", value: display(&log["provider"]) }
                    Field { label: "API key", value: display(&log["apiKeyName"]) }
                    Field { label: "API key ID", value: display(&log["apiKeyId"]) }
                    Field { label: "Started", value: Some(date(text(&log, "timestamp"))) }
                    Field { label: "Duration", value: Some(format!("{} ms", log["durationMs"])) }
                }
                if !text(&log, "errorMessage").is_empty() { div { class: "rounded-lg border border-error/25 bg-error/5 p-4", div { class: "text-xs font-medium uppercase tracking-wide text-text-muted", "Error message" } p { class: "mt-1 whitespace-pre-wrap break-words font-mono text-sm text-text-main", "{text(&log, \"errorMessage\")}" } } }
                if log["streamTrace"].is_object() { Trace { trace: log["streamTrace"].clone() } }
                div { class: "rounded-lg border border-border bg-bg-subtle p-4", h3 { class: "mb-3 text-sm font-semibold text-text-main", "Observed throughput" }
                    if throughput { div { class: "grid grid-cols-2 gap-4 sm:grid-cols-3", Field { label: "Generated output", value: Some(number(&log["generatedOutputTokens"])) } Field { label: "Upstream time", value: Some(format!("{} ms", log["upstreamDurationMs"])) } Field { label: "TPS", value: Some(format!("{tps:.1}")) } } p { class: "mt-3 break-all font-mono text-xs text-text-muted", "{log[\"generatedOutputTokens\"]}" " ÷ (" "{log[\"upstreamDurationMs\"]}" " / 1000) = " "{tps:.1}" " tokens/s" } }
                    else { p { class: "text-sm text-text-muted", "— · Final original usage and completion timing are unavailable." } }
                    p { class: "mt-3 text-xs text-text-muted", "Original provider-generated output divided by observed upstream time. Reasoning is included only when the original provider completion counter includes it. Includes network, initial wait and downstream backpressure; this is observed throughput, not GPU decode speed." }
                }
                div { class: "rounded-lg border border-border bg-bg-subtle p-4", h3 { class: "mb-3 text-sm font-semibold text-text-main", "Tokens" } div { class: "grid grid-cols-2 gap-4 sm:grid-cols-3", for (label, key) in [("Input", "inputTokens"), ("Output", "outputTokens"), ("Cache Read", "cachedTokens"), ("Reasoning", "reasoningTokens"), ("Total", "totalTokens")] { Field { key: "{key}", label, value: if log[key].is_null() { None } else { Some(number(&log[key])) } } } } p { class: "mt-3 text-xs text-text-muted", "Counters come from the selected attempt’s usage. Cache read and reasoning may overlap input/output; do not sum these fields to calculate Total. Missing counters are shown as —." } }
            } }
        }
    } }
}
