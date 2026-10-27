use super::{shell::dashboard_request, ui::CARD_CLASS};
use dioxus::prelude::*;
use serde_json::Value;
use std::rc::Rc;
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::{EventSource, MessageEvent};

const MAX_LINES: usize = 200;

struct ConsoleStream {
    source: EventSource,
    _message: Closure<dyn FnMut(MessageEvent)>,
}

impl ConsoleStream {
    fn new(mut logs: Signal<Vec<String>>) -> Option<Self> {
        let source = EventSource::new("/api/translator/console-logs/stream").ok()?;
        let message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let Some(data) = event.data().as_string() else {
                return;
            };
            let Ok(data) = serde_json::from_str::<Value>(&data) else {
                return;
            };
            match data["type"].as_str() {
                Some("init") => {
                    let Some(lines) = data["logs"].as_array() else {
                        return;
                    };
                    logs.set(
                        lines
                            .iter()
                            .rev()
                            .take(MAX_LINES)
                            .rev()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                    );
                }
                Some("line") => {
                    let Some(line) = data["line"].as_str() else {
                        return;
                    };
                    let mut lines = logs();
                    lines.push(line.to_owned());
                    if lines.len() > MAX_LINES {
                        lines.drain(..lines.len() - MAX_LINES);
                    }
                    logs.set(lines);
                }
                Some("clear") => logs.set(Vec::new()),
                _ => {}
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        source.set_onmessage(Some(message.as_ref().unchecked_ref()));
        Some(Self {
            source,
            _message: message,
        })
    }
}

impl Drop for ConsoleStream {
    fn drop(&mut self) {
        self.source.set_onmessage(None);
        self.source.close();
    }
}

fn line_color(line: &str) -> &'static str {
    let level = line
        .split('[')
        .skip(1)
        .filter_map(|part| part.split_once(']').map(|(tag, _)| tag))
        .filter(|tag| {
            !tag.is_empty() && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .nth(1);
    match level {
        Some("INFO") => "text-blue-400",
        Some("WARN") => "text-yellow-400",
        Some("ERROR") => "text-red-400",
        Some("DEBUG") => "text-purple-400",
        _ => "text-green-400",
    }
}

#[component]
pub(super) fn ConsoleLogPage() -> Element {
    let logs = use_signal(Vec::<String>::new);
    let _stream = use_hook(move || ConsoleStream::new(logs).map(Rc::new));
    use_effect(move || {
        let _ = logs.read();
        if let Some(element) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("console-log-output"))
        {
            element.set_scroll_top(element.scroll_height());
        }
    });

    rsx! {
        div { class: "",
            div { class: CARD_CLASS,
                div { class: "flex items-center justify-end px-4 pt-3 pb-2",
                    button { class: "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas bg-transparent text-ink border border-ink/80 hover:bg-ink/5 disabled:border-hairline disabled:text-muted h-8 px-3 text-[12px] font-medium", onclick: move |_| { spawn(async move { let _ = dashboard_request("DELETE", "/api/translator/console-logs", None).await; }); },
                        span { class: "material-symbols-outlined text-[18px]", "delete" } "Clear"
                    }
                }
                div { id: "console-log-output", class: "bg-black rounded-b-lg p-4 text-xs font-mono h-[calc(100vh-220px)] overflow-y-auto",
                    if logs().is_empty() { span { class: "text-text-muted", "No console logs yet." } }
                    else { div { class: "space-y-0.5", for (index, line) in logs().into_iter().enumerate() { div { key: "{index}", span { class: line_color(&line), "{line}" } } } } }
                }
            }
        }
    }
}
