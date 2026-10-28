//! Go to Line follows Monaco 0.55.1's MIT-licensed gotoLineQuickAccess rules.
//! The full Microsoft MIT notice is shipped in assets/translator-codicon-LICENSE.txt.
use super::{multicursor, position_top};
use dioxus::{core::current_scope_id, prelude::*};
use gloo_timers::future::TimeoutFuture;
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, HtmlInputElement, HtmlTextAreaElement};

struct Destination {
    offset: Option<u32>,
    offset_mode: bool,
    label: String,
}

fn integer(value: &str) -> Option<f64> {
    let number = js_sys::parse_int(value, 10);
    number.is_finite().then_some(number)
}

fn destination(text: &str, query: &str, zero_based: bool) -> Destination {
    let units: Vec<_> = text.encode_utf16().collect();
    let mut starts = vec![0_usize];
    starts.extend(
        units
            .iter()
            .enumerate()
            .filter_map(|(i, c)| (*c == 10).then_some(i + 1)),
    );
    let query = js_sys::JsString::from(query)
        .trim()
        .as_string()
        .unwrap_or_default();
    let value = query.chars().skip(1).collect::<String>();
    let full = |line: usize, column: usize, offset_mode| Destination {
        offset: Some((starts[line - 1] + column - 1) as u32),
        offset_mode,
        label: format!("Press 'Enter' to go to line {line} at column {column}."),
    };
    if let Some(offset) = value.strip_prefix(':') {
        let Some(mut offset) = integer(offset) else {
            let (first, last) = if zero_based {
                (0, units.len() as i64 - 1)
            } else {
                (1, units.len() as i64)
            };
            return Destination {
                offset: None,
                offset_mode: true,
                label: format!("Type a character position to go to (from {first} to {last})."),
            };
        };
        let reverse = offset < 0.0;
        if !zero_based && offset != 0.0 {
            offset -= offset.signum();
        }
        if reverse {
            offset += units.len() as f64;
        }
        let offset = offset.clamp(0.0, units.len() as f64) as usize;
        let line = starts.partition_point(|start| *start <= offset);
        return full(line, offset - starts[line - 1] + 1, true);
    }
    let parts: Vec<_> = value.split([',', ':', '#']).collect();
    let Some(line) = integer(parts[0]) else {
        return Destination {
            offset: None,
            offset_mode: false,
            label: format!("Type a line number to go to (from 1 to {}).", starts.len()),
        };
    };
    let line = if line < 0.0 {
        starts.len() as f64 + 1.0 + line
    } else {
        line
    };
    let line = line.clamp(1.0, starts.len() as f64) as usize;
    let max_column = starts.get(line).map_or(units.len() + 1, |next| *next) - starts[line - 1];
    let Some(column) = parts.get(1).and_then(|value| integer(value)) else {
        return Destination {
            offset: Some(starts[line - 1] as u32),
            offset_mode: false,
            label: if parts.len() < 2 {
                format!("Press 'Enter' to go to line {line} or enter : to add a column number.")
            } else {
                format!("Press 'Enter' to go to line {line} or enter a column number (from 1 to {max_column}).")
            },
        };
    };
    let column = if column < 0.0 {
        max_column as f64 + column
    } else {
        column
    };
    full(line, column.clamp(1.0, max_column as f64) as usize, false)
}

fn center(input: &HtmlTextAreaElement, rendered: &HtmlElement, offset: u32) {
    if let Some(top) = position_top(input, rendered, offset) {
        input.set_scroll_top(
            (input.scroll_top() as f64 + top + 8.0 - input.client_height() as f64 / 2.0)
                .round()
                .max(0.0) as i32,
        );
    }
}

#[component]
pub(super) fn GoToLine(
    value: String,
    input: HtmlTextAreaElement,
    rendered: HtmlElement,
    width: i32,
    mut zero_based: Signal<bool>,
    mut preview: Signal<Option<u32>>,
    on_close: Callback<()>,
    on_accept: Callback<()>,
) -> Element {
    let mut query = use_signal(|| ":".to_owned());
    let mut root = use_signal(|| None::<HtmlElement>);
    let mut field = use_signal(|| None::<HtmlInputElement>);
    let snapshot_input = input.clone();
    let mut saved = use_signal(move || {
        (
            multicursor::selection(&snapshot_input),
            snapshot_input.scroll_top(),
            snapshot_input.scroll_left(),
        )
    });
    let id = use_hook(|| format!("translator-goto-{}", current_scope_id().0));
    let target = destination(&value, &query(), zero_based());
    let preview_input = input.clone();
    let preview_rendered = rendered.clone();
    use_effect(use_reactive((&value,), move |(value,)| {
        let target = destination(&value, &query(), zero_based());
        preview.set(target.offset);
        if let Some(offset) = target.offset {
            center(&preview_input, &preview_rendered, offset);
        }
    }));
    let cancel_input = input.clone();
    let cancel = use_callback(move |focus: bool| {
        let (selection, top, left) = saved.peek().clone();
        if multicursor::selection(&cancel_input) == selection {
            cancel_input.set_scroll_top(top);
            cancel_input.set_scroll_left(left);
        }
        preview.set(None);
        on_close.call(());
        if focus {
            let _ = cancel_input.focus();
        }
    });
    let accept_input = input.clone();
    let accept = use_callback(move |background: bool| {
        let target = destination(&accept_input.value(), &query.peek(), *zero_based.peek());
        let Some(offset) = target.offset else { return };
        let _ = accept_input.set_selection_range(offset, offset);
        if !background {
            let _ = accept_input.focus();
        }
        center(&accept_input, &rendered, offset);
        saved.set((
            multicursor::selection(&accept_input),
            accept_input.scroll_top(),
            accept_input.scroll_left(),
        ));
        on_accept.call(());
        if !background {
            preview.set(None);
            on_close.call(());
        }
    });
    let picker_width = (width as f64 * 0.62).min(600.0);
    let left = ((width as f64 - picker_width) / 2.0).round();
    rsx! {
        div { class: "translator-goto", style: "width:{picker_width}px;left:{left}px;",
            onmounted: move |event| { if let Some(element) = event.data().downcast::<web_sys::Element>() { root.set(element.clone().dyn_into().ok()); } },
            onfocusout: move |_| {
                spawn(async move {
                    TimeoutFuture::new(0).await;
                    let inside = root.peek().as_ref().is_some_and(|root| root.owner_document().and_then(|document| document.active_element()).is_some_and(|active| root.contains(Some(&active))));
                    if !inside { cancel.call(false); }
                });
            },
            onkeydown: move |event| {
                if event.is_composing() { return; }
                match event.key() {
                    Key::Escape => { event.prevent_default(); event.stop_propagation(); cancel.call(true); }
                    Key::Enter => { event.prevent_default(); event.stop_propagation(); accept.call(false); }
                    Key::ArrowRight => {
                        let at_end = field.peek().as_ref().is_some_and(|field| field.selection_start().ok().flatten() == Some(field.value().encode_utf16().count() as u32));
                        if at_end { event.prevent_default(); accept.call(true); }
                    }
                    _ => {}
                }
            },
            div { class: "goto-header",
                div { class: "goto-input-box",
                    input { r#type: "text", value: "{query}", aria_label: "{target.label}", aria_haspopup: "menu", aria_autocomplete: "list", aria_controls: "{id}-list", aria_activedescendant: "{id}-option", spellcheck: "false",
                        oninput: move |event| query.set(event.value()),
                        onmounted: move |event| async move {
                            let _ = event.set_focus(true).await;
                            if let Some(element) = event.data().downcast::<web_sys::Element>() && let Ok(input) = element.clone().dyn_into::<HtmlInputElement>() {
                                let _ = input.set_selection_range(1, 1);
                                field.set(Some(input));
                            }
                        },
                    }
                    button { r#type: "button", role: "checkbox", aria_label: "Use Zero-Based Offset", title: "Use Zero-Based Offset", aria_checked: "{zero_based}", class: "goto-offset", style: if target.offset_mode { "display:block;" } else { "display:none;" }, onclick: move |_| zero_based.set(!zero_based()), "" }
                }
            }
            div { class: "goto-results", role: "listbox", id: "{id}-list",
                button { r#type: "button", role: "option", id: "{id}-option", aria_selected: "true", onclick: move |_| accept.call(false), "{target.label}" }
            }
        }
    }
}
