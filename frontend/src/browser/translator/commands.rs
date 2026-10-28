//! Command Palette's editing actions follow Monaco 0.55.1 (Microsoft, MIT).
//! Full notice: public/assets/translator-codicon-LICENSE.txt.
use dioxus::{core::current_scope_id, prelude::*};
use gloo_timers::future::TimeoutFuture;
use js_sys::{JsString, RegExp};
use wasm_bindgen::{closure::Closure, JsCast};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Command {
    Deduplicate,
    Duplicate,
    Join,
    Reverse,
    SortAscending,
    SortDescending,
    Camel,
    Kebab,
    Lower,
    Pascal,
    Snake,
    Title,
    Upper,
}

impl Command {
    const ALL: [Self; 13] = [
        Self::Deduplicate,
        Self::Duplicate,
        Self::Join,
        Self::Reverse,
        Self::SortAscending,
        Self::SortDescending,
        Self::Camel,
        Self::Kebab,
        Self::Lower,
        Self::Pascal,
        Self::Snake,
        Self::Title,
        Self::Upper,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Deduplicate => "Delete Duplicate Lines",
            Self::Duplicate => "Duplicate Selection",
            Self::Join => "Join Lines",
            Self::Reverse => "Reverse lines",
            Self::SortAscending => "Sort Lines Ascending",
            Self::SortDescending => "Sort Lines Descending",
            Self::Camel => "Transform to Camel Case",
            Self::Kebab => "Transform to Kebab Case",
            Self::Lower => "Transform to Lowercase",
            Self::Pascal => "Transform to Pascal Case",
            Self::Snake => "Transform to Snake Case",
            Self::Title => "Transform to Title Case",
            Self::Upper => "Transform to Uppercase",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Deduplicate => "editor.action.removeDuplicateLines",
            Self::Duplicate => "editor.action.duplicateSelection",
            Self::Join => "editor.action.joinLines",
            Self::Reverse => "editor.action.reverseLines",
            Self::SortAscending => "editor.action.sortLinesAscending",
            Self::SortDescending => "editor.action.sortLinesDescending",
            Self::Camel => "editor.action.transformToCamelcase",
            Self::Kebab => "editor.action.transformToKebabcase",
            Self::Lower => "editor.action.transformToLowercase",
            Self::Pascal => "editor.action.transformToPascalcase",
            Self::Snake => "editor.action.transformToSnakecase",
            Self::Title => "editor.action.transformToTitlecase",
            Self::Upper => "editor.action.transformToUppercase",
        }
    }

    pub fn is_line_action(self) -> bool {
        matches!(
            self,
            Self::Deduplicate | Self::Reverse | Self::SortAscending | Self::SortDescending
        )
    }

    pub fn transform(self, text: &str) -> String {
        if self.is_line_action() {
            let mut lines: Vec<_> = text.split('\n').collect();
            match self {
                Self::Deduplicate => {
                    let mut seen = std::collections::HashSet::new();
                    lines.retain(|line| seen.insert(*line));
                }
                Self::Reverse => lines.reverse(),
                Self::SortAscending | Self::SortDescending => {
                    let collator =
                        js_sys::Intl::Collator::new(&js_sys::Array::new(), &js_sys::Object::new());
                    let compare = collator.compare();
                    lines.sort_by(|a, b| {
                        let result = compare
                            .call2(
                                &wasm_bindgen::JsValue::UNDEFINED,
                                &(*a).into(),
                                &(*b).into(),
                            )
                            .expect("Intl.Collator compares strings")
                            .as_f64()
                            .expect("Intl.Collator returns a number");
                        if result < 0.0 {
                            std::cmp::Ordering::Less
                        } else if result > 0.0 {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    });
                    if self == Self::SortDescending {
                        lines.reverse();
                    }
                }
                _ => unreachable!(),
            }
            return lines.join("\n");
        }
        let text = JsString::from(text);
        let transformed = match self {
            Self::Upper => text.to_locale_upper_case(None),
            Self::Lower => text.to_locale_lower_case(None),
            Self::Title => replace_case(
                &text.to_locale_lower_case(None),
                r"(^|[^\p{L}\p{N}']|((^|\P{L})'))\p{L}",
                true,
            ),
            Self::Snake => text
                .replace_by_pattern(&RegExp::new(r"(\p{Ll})(\p{Lu})", "gmu"), "$1_$2")
                .replace_by_pattern(
                    &RegExp::new(r"(\p{Lu}|\p{N})(\p{Lu})(\p{Ll})", "gmu"),
                    "$1_$2$3",
                )
                .to_locale_lower_case(None),
            Self::Kebab => text
                .replace_by_pattern(&RegExp::new(r"(\S)(_)(\S)", "gm"), "$1-$3")
                .replace_by_pattern(&RegExp::new(r"(\p{Ll})(\p{Lu})", "gmu"), "$1-$2")
                .replace_by_pattern(
                    &RegExp::new(r"(\p{Lu}|\p{N})(\p{Lu}\p{Ll})", "gmu"),
                    "$1-$2",
                )
                .to_locale_lower_case(None),
            Self::Camel => {
                let boundary = if text
                    .as_string()
                    .is_some_and(|text| text.contains(['\r', '\n']))
                {
                    r"[_-]+"
                } else {
                    r"[_\s-]+"
                };
                let words = text.split_by_pattern(&RegExp::new(boundary, "gm"));
                let result = words
                    .iter()
                    .enumerate()
                    .map(|(index, word)| {
                        let word = JsString::from(word);
                        if index == 0 {
                            replace_case(&word, r"^(\p{Lu}[^\p{Lu}])", false)
                        } else {
                            capitalize(&word)
                        }
                        .as_string()
                        .unwrap_or_default()
                    })
                    .collect::<String>();
                JsString::from(result)
            }
            Self::Pascal => {
                let boundary = RegExp::new(r"[_ \t-]", "gm");
                let all_upper = RegExp::new(r"^\p{Lu}+$", "mu");
                let mut result = String::new();
                for word in text.split_by_pattern(&RegExp::new(r"(?<=\.)", "gm")).iter() {
                    for word in JsString::from(word).split_by_pattern(&boundary).iter() {
                        let word = capitalize(&JsString::from(word));
                        let word = if word.length() > 1
                            && all_upper.test(&word.as_string().unwrap_or_default())
                        {
                            word.char_at(0).concat(
                                word.slice(1, word.length())
                                    .to_locale_lower_case(None)
                                    .as_ref(),
                            )
                        } else {
                            word
                        };
                        result.push_str(&word.as_string().unwrap_or_default());
                    }
                }
                JsString::from(result)
            }
            _ => unreachable!(),
        };
        transformed.as_string().unwrap_or_default()
    }
}

fn replace_case(text: &JsString, pattern: &str, upper: bool) -> JsString {
    let replacement = Closure::<dyn Fn(JsString) -> JsString>::new(move |value: JsString| {
        if upper {
            value.to_locale_upper_case(None)
        } else {
            value.to_locale_lower_case(None)
        }
    });
    text.replace_by_pattern_with_function(
        &RegExp::new(pattern, "gmu"),
        replacement.as_ref().unchecked_ref(),
    )
}

fn capitalize(word: &JsString) -> JsString {
    word.char_at(0)
        .to_locale_upper_case(None)
        .concat(word.slice(1, word.length()).as_ref())
}

#[component]
pub(super) fn CommandPalette(
    width: i32,
    on_cancel: Callback<bool>,
    on_run: Callback<Command>,
) -> Element {
    let mut query = use_signal(|| ">".to_owned());
    let mut selected = use_signal(|| 0_usize);
    let mut root = use_signal(|| None::<web_sys::HtmlElement>);
    let id = use_hook(|| format!("translator-commands-{}", current_scope_id().0));
    let search = query().trim_start_matches('>').trim().to_lowercase();
    let matches: Vec<_> = Command::ALL
        .into_iter()
        .filter(|command| {
            command.label().to_lowercase().contains(&search)
                || command.id().eq_ignore_ascii_case(&search)
        })
        .collect();
    let choices = matches.clone();
    let empty = matches.is_empty();
    let picker_width = (f64::from(width) * 0.62).min(600.0);
    let left = ((f64::from(width) - picker_width) / 2.0).round();
    rsx! {
        div { class: "translator-goto translator-commands", style: "width:{picker_width}px;left:{left}px;",
            onmounted: move |event| { if let Some(element) = event.data().downcast::<web_sys::Element>() { root.set(element.clone().dyn_into().ok()); } },
            onfocusout: move |_| {
                spawn(async move {
                    TimeoutFuture::new(0).await;
                    let inside = root.peek().as_ref().is_some_and(|root| root.owner_document().and_then(|document| document.active_element()).is_some_and(|active| root.contains(Some(&active))));
                    if !inside { on_cancel.call(false); }
                });
            },
            onkeydown: move |event| {
                if event.is_composing() { return; }
                match event.key() {
                    Key::Escape => { event.prevent_default(); event.stop_propagation(); on_cancel.call(true); }
                    Key::Enter => { event.prevent_default(); event.stop_propagation(); if let Some(command) = choices.get(selected()) { on_run.call(*command); } }
                    Key::ArrowDown | Key::ArrowUp => {
                        event.prevent_default(); event.stop_propagation();
                        if !choices.is_empty() {
                            let next = if event.key() == Key::ArrowUp { (selected() + choices.len() - 1) % choices.len() } else { (selected() + 1) % choices.len() };
                            selected.set(next);
                        }
                    }
                    _ => {}
                }
            },
            div { class: "goto-header",
                div { class: "goto-input-box",
                    input { r#type: "text", value: "{query}", aria_label: "Type the name of an action to run.", aria_haspopup: "listbox", aria_autocomplete: "list", aria_controls: "{id}-list", aria_activedescendant: if empty { String::new() } else { format!("{id}-{}", selected()) }, spellcheck: "false",
                        oninput: move |event| { query.set(event.value()); selected.set(0); },
                        onmounted: move |event| async move { let _ = event.set_focus(true).await; },
                    }
                }
            }
            div { class: "goto-results", role: "listbox", id: "{id}-list",
                for (index, command) in matches.into_iter().enumerate() {
                    button { r#type: "button", role: "option", tabindex: "-1", id: "{id}-{index}", aria_selected: "{selected() == index}", onmousedown: move |event| event.prevent_default(), onclick: move |_| on_run.call(command), "{command.label()}" }
                }
                if empty { div { role: "status", "No matching commands" } }
            }
        }
    }
}
