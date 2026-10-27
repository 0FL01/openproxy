//! Translator stages and their same-origin dashboard API actions.
use super::{endpoint::copy_text, json_body, shell::dashboard_request, ui::Button};
use dioxus::prelude::*;
use serde_json::{json, Value};
use std::{collections::HashSet, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

const STEPS: [(&str, &str); 7] = [
    ("Client Request", "1_req_client.json"),
    ("Source Body", "2_req_source.json"),
    ("OpenAI Intermediate", "3_req_openai.json"),
    ("Target Request", "4_req_target.json"),
    ("Provider Response", "5_res_provider.txt"),
    ("OpenAI Response", "6_res_openai.txt"),
    ("Client Response", "7_res_client.txt"),
];

fn alert(message: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.alert_with_message(message);
    }
}

fn pretty(value: &Value) -> String {
    js_sys::JSON::parse(&value.to_string())
        .and_then(|value| {
            js_sys::JSON::stringify_with_replacer_and_space(
                &value,
                &JsValue::NULL,
                &JsValue::from_f64(2.0),
            )
        })
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

fn save(file: &'static str, content: String) {
    spawn(async move {
        let _ = dashboard_request(
            "POST",
            "/api/translator/save",
            Some(&json!({"file": file, "content": content})),
        )
        .await;
    });
}

async fn translate(step: u8, body: Value) -> Result<Value, String> {
    let response = dashboard_request(
        "POST",
        "/api/translator/translate",
        Some(&json!({"step": step, "body": body})),
    )
    .await
    .map_err(|_| "Failed to fetch".to_owned())?;
    let data = json_body(&response)
        .await
        .ok_or("Invalid response".to_owned())?;
    if data["success"] == true {
        Ok(data["result"].clone())
    } else {
        Err(data["error"]
            .as_str()
            .unwrap_or("Translation failed")
            .to_owned())
    }
}

fn detect_meta(content: &str, mut meta: Signal<Option<Value>>) {
    if let Ok(body) = serde_json::from_str::<Value>(content) {
        spawn(async move {
            if let Ok(value) = translate(1, body).await {
                meta.set(Some(value));
            }
        });
    }
}

fn open_step(mut expanded: Signal<Vec<bool>>, step: usize) {
    let mut next = vec![false; STEPS.len()];
    next[step] = true;
    expanded.set(next);
}

async fn receive_response(
    response: web_sys::Response,
    mut contents: Signal<Vec<String>>,
    expanded: Signal<Vec<bool>>,
) -> Result<(), JsValue> {
    let stream = response
        .body()
        .ok_or_else(|| JsValue::from_str("Response has no stream"))?;
    let reader: web_sys::ReadableStreamDefaultReader = stream.get_reader().unchecked_into();
    let decoder = web_sys::TextDecoder::new()?;
    let options = web_sys::TextDecodeOptions::new();
    options.set_stream(true);
    let mut content = String::new();
    loop {
        let result = JsFuture::from(reader.read()).await?;
        if js_sys::Reflect::get(&result, &"done".into())?.as_bool() == Some(true) {
            break;
        }
        let bytes = js_sys::Uint8Array::new(&js_sys::Reflect::get(&result, &"value".into())?);
        content.push_str(&decoder.decode_with_js_u8_array_and_options(&bytes, &options)?);
    }
    contents.write()[4] = content.clone();
    open_step(expanded, 4);
    dashboard_request(
        "POST",
        "/api/translator/save",
        Some(&json!({"file": STEPS[4].1, "content": content})),
    )
    .await?;
    Ok(())
}

fn find_regex(
    query: &str,
    match_case: bool,
    whole_word: bool,
    regex: bool,
) -> Option<js_sys::RegExp> {
    if query.is_empty() {
        return None;
    }
    let pattern: String = if regex {
        query.to_owned()
    } else {
        query
            .chars()
            .flat_map(|ch| {
                if "\\^$.*+?()[]{}|".contains(ch) {
                    vec!['\\', ch]
                } else {
                    vec![ch]
                }
            })
            .collect()
    };
    let pattern = if whole_word {
        format!("\\b(?:{pattern})\\b")
    } else {
        pattern
    };
    let flags = if match_case { "g" } else { "gi" };
    let constructor = js_sys::Reflect::get(&js_sys::global(), &"RegExp".into())
        .ok()
        .and_then(|value| value.dyn_into::<js_sys::Function>().ok())?;
    let regex = js_sys::Reflect::construct(
        &constructor,
        &js_sys::Array::of2(&pattern.into(), &flags.into()),
    )
    .ok()?;
    Some(regex.unchecked_into())
}

fn find_ranges(
    text: &str,
    query: &str,
    match_case: bool,
    whole_word: bool,
    regex: bool,
) -> Vec<(u32, u32)> {
    let Some(regex) = find_regex(query, match_case, whole_word, regex) else {
        return Vec::new();
    };
    let mut ranges = Vec::new();
    let text_length = text.encode_utf16().count() as u32;
    while let Some(found) = regex.exec(text) {
        let end = regex.last_index();
        let length = found
            .get(0)
            .as_string()
            .unwrap_or_default()
            .encode_utf16()
            .count() as u32;
        ranges.push((end - length, end));
        if length == 0 {
            if end >= text_length {
                break;
            }
            regex.set_last_index(end + 1);
        }
    }
    ranges
}

fn find_seed(input: &web_sys::HtmlTextAreaElement) -> String {
    let text = input.value();
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    let end = input.selection_end().ok().flatten().unwrap_or(start);
    if start != end {
        return js_sys::JsString::from(text)
            .slice(start, end)
            .as_string()
            .unwrap_or_default();
    }
    let byte = text
        .char_indices()
        .find_map(|(offset, _)| {
            (text[..offset].encode_utf16().count() >= start as usize).then_some(offset)
        })
        .unwrap_or(text.len());
    let word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let left = text[..byte]
        .char_indices()
        .rev()
        .take_while(|(_, ch)| word(*ch))
        .last()
        .map_or(byte, |(offset, _)| offset);
    let right = text[byte..]
        .char_indices()
        .find_map(|(offset, ch)| (!word(ch)).then_some(byte + offset))
        .unwrap_or(text.len());
    text[left..right].to_owned()
}

fn insert_text(input: &web_sys::HtmlTextAreaElement, start: u32, end: u32, text: &str) -> bool {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return false;
    };
    let Some(command) = js_sys::Reflect::get(document.as_ref(), &"execCommand".into())
        .ok()
        .and_then(|value| value.dyn_into::<js_sys::Function>().ok())
    else {
        return false;
    };
    let _ = input.focus();
    let _ = input.set_selection_range(start, end);
    command
        .call3(
            document.as_ref(),
            &"insertText".into(),
            &false.into(),
            &text.into(),
        )
        .ok()
        .and_then(|value| value.as_bool())
        == Some(true)
}

fn in_json_string(text: &str) -> bool {
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            escaped = false;
        } else if in_string && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            in_string = !in_string;
        }
    }
    in_string
}

fn close_json_pair(input: &web_sys::HtmlTextAreaElement, key: &str) -> bool {
    let closing = match key {
        "{" => '}',
        "[" => ']',
        "\"" => '"',
        _ => return false,
    };
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    let end = input.selection_end().ok().flatten().unwrap_or(start);
    let text = js_sys::JsString::from(input.value());
    if start != end {
        let selected = text.slice(start, end).as_string().unwrap_or_default();
        if insert_text(input, start, end, &format!("{key}{selected}{closing}")) {
            let _ = input.set_selection_range(start + 1, end + 1);
            return true;
        }
        return false;
    }
    let before = text.slice(0, start).as_string().unwrap_or_default();
    let after = text
        .slice(start, text.length())
        .as_string()
        .unwrap_or_default();
    if in_json_string(&before)
        || after
            .chars()
            .next()
            .is_some_and(|ch| !";:.,=}])> \n\t\r".contains(ch))
    {
        return false;
    }
    if insert_text(input, start, end, &format!("{key}{closing}")) {
        let _ = input.set_selection_range(start + 1, start + 1);
        true
    } else {
        false
    }
}

fn update_auto_pairs(before: &str, after: &str, pairs: &mut Vec<(char, u32)>) {
    let old: Vec<u16> = before.encode_utf16().collect();
    let new: Vec<u16> = after.encode_utf16().collect();
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    pairs.retain_mut(|(closing, position)| {
        let index = *position as usize;
        if index >= old_end {
            *position = (index - old_end + new_end) as u32;
        } else if index >= prefix {
            return false;
        }
        new.get(*position as usize) == Some(&(*closing as u16))
    });
}

fn expand_json_brackets(input: &web_sys::HtmlTextAreaElement) -> bool {
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    let end = input.selection_end().ok().flatten().unwrap_or(start);
    if start != end {
        return false;
    }
    let text = js_sys::JsString::from(input.value());
    let before = text.slice(0, start).as_string().unwrap_or_default();
    let after = text
        .slice(start, text.length())
        .as_string()
        .unwrap_or_default();
    if !matches!(
        (before.chars().last(), after.chars().next()),
        (Some('{'), Some('}')) | (Some('['), Some(']'))
    ) {
        return false;
    }
    if in_json_string(&before) {
        return false;
    }
    let indentation: String = before
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .collect();
    let column = indentation.chars().fold(0_u32, |column, ch| {
        if ch == '\t' {
            column + 4 - column % 4
        } else {
            column + 1
        }
    });
    let spaces = 4 - column % 4;
    let replacement = format!(
        "\n{indentation}{}\n{indentation}",
        " ".repeat(spaces as usize)
    );
    if insert_text(input, start, end, &replacement) {
        let cursor = start + 1 + indentation.encode_utf16().count() as u32 + spaces;
        let _ = input.set_selection_range(cursor, cursor);
        true
    } else {
        false
    }
}

fn indent_json_newline(input: &web_sys::HtmlTextAreaElement) -> bool {
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    let end = input.selection_end().ok().flatten().unwrap_or(start);
    let before = js_sys::JsString::from(input.value())
        .slice(0, start)
        .as_string()
        .unwrap_or_default();
    let indentation: String = before
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .collect();
    !indentation.is_empty() && insert_text(input, start, end, &format!("\n{indentation}"))
}

fn indent_selection(input: &web_sys::HtmlTextAreaElement, backwards: bool) {
    const TAB_SIZE: u32 = 4;
    let text = js_sys::JsString::from(input.value());
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    let end = input.selection_end().ok().flatten().unwrap_or(start);
    let before = text.slice(0, start).as_string().unwrap_or_default();
    let line_prefix = before.rsplit('\n').next().unwrap_or("");
    if start == end && !backwards {
        let column = line_prefix.chars().fold(0_u32, |column, ch| {
            if ch == '\t' {
                column + TAB_SIZE - column % TAB_SIZE
            } else {
                column + ch.len_utf16() as u32
            }
        });
        let spaces = " ".repeat((TAB_SIZE - column % TAB_SIZE) as usize);
        let _ = insert_text(input, start, end, &spaces);
        return;
    }

    let line_start = start - line_prefix.encode_utf16().count() as u32;
    let last = if end > start && text.slice(end - 1, end).as_string().as_deref() == Some("\n") {
        end - 1
    } else {
        end
    };
    let after = text
        .slice(last, text.length())
        .as_string()
        .unwrap_or_default();
    let line_end = last
        + after
            .split('\n')
            .next()
            .unwrap_or("")
            .encode_utf16()
            .count() as u32;
    let lines = text
        .slice(line_start, line_end)
        .as_string()
        .unwrap_or_default();
    let mut first_removed = 0_u32;
    let mut removed = 0_u32;
    let mut count = 0_u32;
    let updated = lines
        .split('\n')
        .map(|line| {
            count += 1;
            if backwards {
                let remove = if line.starts_with('\t') {
                    1
                } else {
                    line.bytes()
                        .take_while(|byte| *byte == b' ')
                        .count()
                        .min(TAB_SIZE as usize)
                };
                if count == 1 {
                    first_removed = remove as u32;
                }
                removed += remove as u32;
                line[remove..].to_owned()
            } else {
                format!("    {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if backwards && removed == 0 {
        return;
    }
    if insert_text(input, line_start, line_end, &updated) {
        let new_start = if backwards {
            start.saturating_sub(first_removed.min(start - line_start))
        } else {
            start + TAB_SIZE
        };
        let new_end = if backwards {
            end.saturating_sub(removed)
        } else {
            end + TAB_SIZE * count
        };
        let _ = input.set_selection_range(new_start, new_end.max(new_start));
    }
}

fn json_tokens(text: &str) -> Vec<(&str, &'static str)> {
    const BASE: &str = "#d4d4d4";
    const BRACKETS: [&str; 3] = ["#ffd700", "#da70d6", "#179fff"];
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut offset = 0;
    let mut depth = 0_usize;
    while offset < bytes.len() {
        let start = offset;
        let color = match bytes[offset] {
            b'"' => {
                offset += 1;
                while offset < bytes.len() {
                    if bytes[offset] == b'\\' {
                        offset = (offset + 2).min(bytes.len());
                    } else if bytes[offset] == b'"' {
                        offset += 1;
                        break;
                    } else {
                        offset += 1;
                    }
                }
                if text[offset..].trim_start().starts_with(':') {
                    "#9cdcfe"
                } else {
                    "#ce9178"
                }
            }
            b'{' | b'[' => {
                offset += 1;
                let color = BRACKETS[depth % BRACKETS.len()];
                depth += 1;
                color
            }
            b'}' | b']' => {
                offset += 1;
                depth = depth.saturating_sub(1);
                BRACKETS[depth % BRACKETS.len()]
            }
            b':' | b',' => {
                offset += 1;
                "#dcdcdc"
            }
            b'-' | b'0'..=b'9' => {
                offset += 1;
                while offset < bytes.len()
                    && matches!(
                        bytes[offset],
                        b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                    )
                {
                    offset += 1;
                }
                "#b5cea8"
            }
            b't' | b'f' | b'n'
                if ["true", "false", "null"]
                    .iter()
                    .any(|literal| text[offset..].starts_with(literal)) =>
            {
                let literal = ["true", "false", "null"]
                    .into_iter()
                    .find(|literal| text[offset..].starts_with(literal))
                    .unwrap();
                offset += literal.len();
                "#ce9178"
            }
            _ => {
                offset += text[offset..].chars().next().unwrap().len_utf8();
                BASE
            }
        };
        tokens.push((&text[start..offset], color));
    }
    tokens
}

fn json_guides(text: &str) -> Vec<Vec<(usize, bool)>> {
    let mut lines = Vec::new();
    let mut depth = 0_usize;
    for source in text.split('\n') {
        let mut guides = Vec::new();
        let leading_closers = source
            .trim_start()
            .chars()
            .take_while(|ch| matches!(ch, '}' | ']'))
            .count();
        for level in 0..depth.saturating_sub(leading_closers) {
            guides.push((level, false));
        }
        let previous = depth;
        for (token, _) in json_tokens(source) {
            match token {
                "{" | "[" => depth += 1,
                "}" | "]" => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        for level in previous..depth {
            guides.push((level, true));
        }
        lines.push(guides);
    }
    lines
}

struct EditorSizeObserver {
    observer: web_sys::ResizeObserver,
    _callback: Closure<dyn FnMut(js_sys::Array, web_sys::ResizeObserver)>,
}

impl Drop for EditorSizeObserver {
    fn drop(&mut self) {
        self.observer.disconnect();
    }
}

#[component]
fn Editor(value: String, json: bool, on_change: EventHandler<String>) -> Element {
    let mut textarea = use_signal(|| None::<web_sys::HtmlTextAreaElement>);
    let mut size_observer = use_signal(|| None::<Rc<EditorSizeObserver>>);
    let mut editor_width = use_signal(|| 0_i32);
    let mut rendered = use_signal(|| None::<web_sys::HtmlElement>);
    let mut line_tops = use_signal(Vec::<i32>::new);
    let initial_value = value.clone();
    let mut pair_text = use_signal(move || initial_value);
    let mut auto_pairs = use_signal(Vec::<(char, u32)>::new);
    let mut ruler = use_signal(|| None::<web_sys::HtmlCanvasElement>);
    let mut has_changed = use_signal(|| !value.is_empty());
    let mut find_open = use_signal(|| false);
    let mut replace_open = use_signal(|| false);
    let mut query = use_signal(String::new);
    let mut replacement = use_signal(String::new);
    let mut match_index = use_signal(|| 0_usize);
    let mut match_case = use_signal(|| false);
    let mut whole_word = use_signal(|| false);
    let mut regex = use_signal(|| false);
    let mut active_line = use_signal(|| 0_usize);
    let mut scroll_top = use_signal(|| 0_i32);
    use_effect(use_reactive((&value,), move |_| {
        let _ = editor_width();
        let Some(element) = rendered() else { return };
        spawn(async move {
            let frame = js_sys::Promise::new(&mut |resolve, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.request_animation_frame(&resolve);
                }
            });
            let _ = JsFuture::from(frame).await;
            let children = element.child_nodes();
            let tops = (0..children.length())
                .filter_map(|index| {
                    children
                        .item(index)?
                        .dyn_into::<web_sys::HtmlElement>()
                        .ok()
                })
                .map(|line| line.offset_top())
                .collect();
            line_tops.set(tops);
        });
    }));
    use_effect(use_reactive((&value,), move |(value,)| {
        if !value.is_empty() && !*has_changed.peek() {
            has_changed.set(true);
        }
        let Some(canvas) = ruler() else { return };
        let Some(context) = canvas
            .get_context("2d")
            .ok()
            .flatten()
            .and_then(|ctx| ctx.dyn_into::<web_sys::CanvasRenderingContext2d>().ok())
        else {
            return;
        };
        let scale = web_sys::window().map_or(1.0, |window| window.device_pixel_ratio());
        let width = (14.0 * scale) as u32;
        let height = (400.0 * scale) as u32;
        canvas.set_width(width);
        canvas.set_height(height);
        let document_height = textarea()
            .as_ref()
            .map_or(400, |input| input.scroll_height())
            .max(400) as f64;
        let cursor_height = (2.0 * scale) as u32;
        let half = cursor_height / 2;
        let y = ((active_line() as f64 * 16.0 * height as f64 / document_height) as u32)
            .clamp(half, height - half)
            - half;
        context.set_fill_style_str(if has_changed() {
            "rgba(174,175,173,0.7)"
        } else {
            "rgba(0,0,0,0.7)"
        });
        context.fill_rect(
            1.0,
            y as f64,
            width.saturating_sub(1) as f64,
            cursor_height as f64,
        );
        context.begin_path();
        context.set_line_width(1.0);
        context.set_stroke_style_str("rgba(127,127,127,0.3)");
        context.move_to(0.0, 0.0);
        context.line_to(0.0, height as f64);
        context.move_to(1.0, 0.0);
        context.line_to(width as f64, 0.0);
        context.stroke();
    }));
    let update_cursor = use_callback(move |_| {
        if let Some(input) = textarea.peek().as_ref() {
            let cursor = input.selection_start().ok().flatten().unwrap_or_default();
            let before = js_sys::JsString::from(input.value())
                .slice(0, cursor)
                .as_string()
                .unwrap_or_default();
            active_line.set(before.bytes().filter(|byte| *byte == b'\n').count());
        }
    });
    let select_match = use_callback(move |next: bool| {
        if let Some(input) = textarea.peek().as_ref() {
            let ranges = find_ranges(
                &input.value(),
                &query.peek(),
                *match_case.peek(),
                *whole_word.peek(),
                *regex.peek(),
            );
            if !ranges.is_empty() {
                let current = *match_index.peek();
                let index = if next {
                    (current + 1) % ranges.len()
                } else {
                    (current + ranges.len() - 1) % ranges.len()
                };
                match_index.set(index);
                let (start, end) = ranges[index];
                let _ = input.set_selection_range(start, end);
            }
        }
    });
    let reset_match = use_callback(move |_| {
        match_index.set(0);
        if let Some(input) = textarea.peek().as_ref()
            && let Some((start, end)) = find_ranges(
                &input.value(),
                &query.peek(),
                *match_case.peek(),
                *whole_word.peek(),
                *regex.peek(),
            )
            .first()
            .copied()
        {
            let _ = input.set_selection_range(start, end);
        }
    });
    let toggle_option = use_callback(move |option: u8| {
        match option {
            0 => {
                let next = !*match_case.peek();
                match_case.set(next);
            }
            1 => {
                let next = !*whole_word.peek();
                whole_word.set(next);
            }
            _ => {
                let next = !*regex.peek();
                regex.set(next);
            }
        }
        reset_match.call(());
    });
    let replace_matches = use_callback(move |all: bool| {
        let Some(input) = textarea.peek().as_ref().cloned() else {
            return;
        };
        let query = query.peek().clone();
        let Some(pattern) = find_regex(
            &query,
            *match_case.peek(),
            *whole_word.peek(),
            *regex.peek(),
        ) else {
            return;
        };
        let text = input.value();
        let ranges = find_ranges(
            &text,
            &query,
            *match_case.peek(),
            *whole_word.peek(),
            *regex.peek(),
        );
        if ranges.is_empty() {
            return;
        }
        let input_text = js_sys::JsString::from(text);
        let (start, end) = if all {
            (0, input_text.length())
        } else {
            ranges[*match_index.peek() % ranges.len()]
        };
        let selected = input_text.slice(start, end);
        let replacement = if *regex.peek() {
            selected
                .replace_by_pattern(&pattern, &replacement.peek())
                .as_string()
                .unwrap_or_default()
        } else if all {
            selected
                .replace_by_pattern(&pattern, &replacement.peek().replace('$', "$$"))
                .as_string()
                .unwrap_or_default()
        } else {
            replacement.peek().clone()
        };
        if !insert_text(&input, start, end, &replacement) {
            alert("Could not replace text");
            return;
        }
        let ranges = find_ranges(
            &input.value(),
            &query,
            *match_case.peek(),
            *whole_word.peek(),
            *regex.peek(),
        );
        if let Some(index) = ranges
            .iter()
            .position(|(next, _)| *next >= input.selection_end().ok().flatten().unwrap_or_default())
            .or_else(|| (!ranges.is_empty()).then_some(0))
        {
            match_index.set(index);
            let _ = input.set_selection_range(ranges[index].0, ranges[index].1);
        }
    });
    let matches = find_ranges(&value, &query(), match_case(), whole_word(), regex());
    let count = if matches.is_empty() {
        "No results".to_owned()
    } else {
        format!(
            "{} of {}",
            (match_index() % matches.len()) + 1,
            matches.len()
        )
    };
    let tokens = if json {
        json_tokens(&value)
    } else {
        vec![(value.as_str(), "#d4d4d4")]
    };
    let mut lines = vec![Vec::new()];
    for (text, color) in tokens {
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                lines.push(Vec::new());
            }
            if !part.is_empty() {
                lines.last_mut().unwrap().push((part, color));
            }
        }
    }
    let scroll = scroll_top();
    let mut wrapping_indent = None::<f64>;
    let mut wrapping_width = editor_width() as f64;
    if let Some(context) = ruler()
        .as_ref()
        .and_then(|canvas| canvas.get_context("2d").ok().flatten())
        .and_then(|value| value.dyn_into::<web_sys::CanvasRenderingContext2d>().ok())
    {
        context.set_font("12px 'Droid Sans Mono', monospace");
        let width = editor_width() as f64;
        if width > 0.0 {
            // Monaco reserves two pixels for the cursor and wraps on whole columns.
            if let Ok(character) = context.measure_text("n") {
                let character_width = (character.width() * 256.0).round() / 256.0;
                if character_width > 0.0 {
                    wrapping_width =
                        ((width - 2.0) / character_width).floor().max(1.0) * character_width;
                }
            }
            for line in value.split('\n') {
                if context
                    .measure_text(line)
                    .is_ok_and(|metrics| metrics.width() > wrapping_width)
                {
                    let prefix = line.len() - line.trim_start_matches(' ').len();
                    let indent = context
                        .measure_text(&line[..prefix])
                        .map_or(0.0, |metrics| (metrics.width() * 64.0).round() / 64.0);
                    // One textarea can share the overlay's hanging indent only if
                    // all wrapped lines use the same indentation.
                    if wrapping_indent.is_some_and(|previous| (previous - indent).abs() > 0.01) {
                        wrapping_indent = None;
                        break;
                    }
                    wrapping_indent = Some(indent);
                }
            }
        }
    }
    let wrapping_indent = wrapping_indent.unwrap_or(0.0);
    let right_padding = (editor_width() as f64 - wrapping_width).max(0.0);
    let wrapping_width = if wrapping_width > 0.0 {
        format!("{wrapping_width}px")
    } else {
        "calc(100% - 14px)".to_owned()
    };
    let current_line_width = editor_width() + 14;
    let indent = value
        .lines()
        .filter_map(|line| {
            let spaces = line.chars().take_while(|ch| *ch == ' ').count();
            (spaces > 0).then_some(spaces)
        })
        .min()
        .unwrap_or(4);
    let guides = if json {
        json_guides(&value)
    } else {
        Vec::new()
    };
    rsx! {
        div { style: "height:400px;position:relative;background:#1e1e1e;color:#d4d4d4;",
            div { style: "position:relative;overflow:hidden;width:100%;height:400px;",
                div { aria_hidden: "true", style: "position:absolute;transform:translate3d(0,0,0);contain:strict;top:0;height:400px;width:62px;background:#1e1e1e;font-family:'Droid Sans Mono',monospace;font-size:12px;line-height:16px;",
                    for line in 0..lines.len() {
                        { let top = line_tops().get(line).copied().unwrap_or(line as i32 * 16) - scroll; let color = if value.is_empty() { "rgba(133,133,133,.4)" } else if line == active_line() { "#c6c6c6" } else { "#858585" }; rsx! {
                            div { key: "{line}", style: "position:absolute;top:{top}px;left:0;width:36px;text-align:right;color:{color};pointer-events:none;", "{line + 1}" }
                        } }
                    }
                }
                div { style: "position:absolute;overflow:hidden;left:62px;right:0;height:400px;",
                    pre { aria_hidden: "true", style: "position:absolute;top:-{scroll}px;left:0;right:0;margin:0;white-space:pre-wrap;overflow-wrap:anywhere;pointer-events:none;font-family:'Droid Sans Mono',monospace;font-size:12px;line-height:16px;",
                        onmounted: move |event| { if let Some(element) = event.data().downcast::<web_sys::Element>() && let Ok(element) = element.clone().dyn_into::<web_sys::HtmlElement>() { rendered.set(Some(element)); } },
                        for (line, segments) in lines.iter().enumerate() {
                            div { key: "{line}", style: "position:relative;width:{wrapping_width};min-height:16px;padding-left:{wrapping_indent}px;text-indent:-{wrapping_indent}px;",
                                if let Some(line_guides) = guides.get(line) {
                                    for (index, (level, start)) in line_guides.iter().enumerate() {
                                        { let vertical = if *start { "bottom:0;height:2px" } else { "top:0;height:100%" }; rsx! { div { key: "{index}", style: "position:absolute;left:{*level as f64 * indent as f64 * 7.2246}px;{vertical};width:1px;background:#707070;" } } }
                                    }
                                }
                                if line == active_line() { div { style: "position:absolute;top:0;left:0;width:{current_line_width}px;height:16px;border:2px solid #282828;" } }
                                for (index, (text, color)) in segments.iter().enumerate() { span { key: "{index}", style: "color:{color}", "{text}" } }
                            }
                        }
                    }
                    textarea { aria_label: "Translator editor", spellcheck: "false", value: "{value}",
                        style: "position:absolute;inset:0 14px 0 0;width:calc(100% - 14px);height:100%;resize:none;border:0;outline:0;padding:0 {right_padding}px 0 {wrapping_indent}px;text-indent:-{wrapping_indent}px each-line;background:transparent;color:transparent;caret-color:white;font-family:'Droid Sans Mono',monospace;font-size:12px;line-height:16px;white-space:pre-wrap;overflow-wrap:anywhere;",
                        onmounted: move |event| {
                            if let Some(element) = event.data().downcast::<web_sys::Element>() && let Ok(input) = element.clone().dyn_into::<web_sys::HtmlTextAreaElement>() && let Some(container) = input.parent_element() {
                                editor_width.set(container.client_width().saturating_sub(14));
                                let observed = container.clone();
                                let callback = Closure::<dyn FnMut(js_sys::Array, web_sys::ResizeObserver)>::new(move |_, _| editor_width.set(observed.client_width().saturating_sub(14)));
                                if let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
                                    observer.observe(&container);
                                    size_observer.set(Some(Rc::new(EditorSizeObserver { observer, _callback: callback })));
                                }
                                textarea.set(Some(input));
                            }
                        },
                        onkeydown: move |event| {
                            if event.is_composing() { return; }
                            let key = event.key().to_string();
                            if json && !event.modifiers().ctrl() && !event.modifiers().meta() && !event.modifiers().alt() {
                                if let Some(input) = textarea.peek().clone() {
                                    let before = input.value();
                                    update_auto_pairs(&pair_text.peek(), &before, &mut auto_pairs.write());
                                    pair_text.set(before.clone());
                                    let start = input.selection_start().ok().flatten().unwrap_or_default();
                                    let end = input.selection_end().ok().flatten().unwrap_or(start);
                                    if key == "Backspace" && start == end && start > 0 {
                                        let pair = auto_pairs.peek().iter().find(|(_, position)| *position == start).copied();
                                        if let Some((closing, _)) = pair {
                                            let opening = match closing { '}' => '{', ']' => '[', '"' => '"', _ => '\0' };
                                            let text = js_sys::JsString::from(before.clone());
                                            if text.slice(start - 1, start).as_string().as_deref() == Some(&opening.to_string()) && insert_text(&input, start - 1, start + 1, "") {
                                                let after = input.value();
                                                update_auto_pairs(&before, &after, &mut auto_pairs.write());
                                                pair_text.set(after);
                                                event.prevent_default();
                                                update_cursor.call(());
                                                return;
                                            }
                                        }
                                    }
                                    if start == end && key.chars().count() == 1 {
                                        let closing = key.chars().next().unwrap();
                                        let index = auto_pairs.peek().iter().position(|pair| *pair == (closing, start));
                                        if let Some(index) = index {
                                            auto_pairs.write().remove(index);
                                            let _ = input.set_selection_range(start + 1, start + 1);
                                            event.prevent_default();
                                            update_cursor.call(());
                                            return;
                                        }
                                    }
                                    if close_json_pair(&input, &key) {
                                        let after = input.value();
                                        update_auto_pairs(&before, &after, &mut auto_pairs.write());
                                        pair_text.set(after);
                                        if start == end && let Some(closing) = match key.as_str() { "{" => Some('}'), "[" => Some(']'), "\"" => Some('"'), _ => None } {
                                            auto_pairs.write().push((closing, start + 1));
                                        }
                                        event.prevent_default();
                                        update_cursor.call(());
                                        return;
                                    }
                                }
                            }
                            if json && event.key() == Key::Enter && !event.modifiers().ctrl() && !event.modifiers().meta() && !event.modifiers().alt() && !event.modifiers().shift() && textarea.peek().as_ref().is_some_and(|input| expand_json_brackets(input) || indent_json_newline(input)) {
                                event.prevent_default();
                            } else if event.key() == Key::Tab && !event.modifiers().ctrl() && !event.modifiers().meta() && !event.modifiers().alt() {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() { indent_selection(input, event.modifiers().shift()); }
                                update_cursor.call(());
                            } else if (event.modifiers().ctrl() || event.modifiers().meta()) && (key.eq_ignore_ascii_case("f") || key.eq_ignore_ascii_case("h")) {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() {
                                    let seed = find_seed(input);
                                    let cursor = input.selection_start().ok().flatten().unwrap_or_default();
                                    let ranges = find_ranges(&input.value(), &seed, *match_case.peek(), *whole_word.peek(), *regex.peek());
                                    let index = ranges.iter().position(|(start, end)| *start <= cursor && cursor <= *end)
                                        .or_else(|| ranges.iter().position(|(start, _)| *start >= cursor)).unwrap_or(0);
                                    query.set(seed);
                                    match_index.set(index);
                                }
                                replace_open.set(key.eq_ignore_ascii_case("h"));
                                find_open.set(true);
                            } else if event.key() == Key::F3 && find_open() {
                                event.prevent_default();
                                select_match.call(!event.modifiers().shift());
                            }
                        },
                        oninput: move |event| { on_change.call(event.value()); update_cursor.call(()); },
                        onselect: move |_| update_cursor.call(()),
                        onkeyup: move |_| update_cursor.call(()),
                        onscroll: move |_| { if let Some(input) = textarea.peek().as_ref() { scroll_top.set(input.scroll_top()); } }
                    }
                }
                canvas { aria_hidden: "true", style: "position:absolute;transform:translate3d(0,0,0);contain:strict;top:0;right:0;width:14px;height:400px;pointer-events:none;", onmounted: move |event| { if let Some(element) = event.data().downcast::<web_sys::Element>() && let Ok(canvas) = element.clone().dyn_into::<web_sys::HtmlCanvasElement>() { ruler.set(Some(canvas)); } } }
                if find_open() { div { role: "dialog", aria_label: "Find / Replace", style: "position:absolute;z-index:5;top:0;right:28px;width:min(419px,calc(100% - 28px));padding:4px;background:#252526;color:#cccccc;font:12px sans-serif;box-shadow:0 2px 8px #0008;",
                    div { style: "display:flex;align-items:center;gap:4px;height:25px;",
                    button { r#type: "button", aria_label: "Toggle Replace", onclick: move |_| { let next = !*replace_open.peek(); replace_open.set(next); }, if replace_open() { "⌄" } else { "›" } }
                    input { aria_label: "Find", placeholder: "Find", value: "{query}", spellcheck: "false", style: "min-width:0;flex:1;height:24px;padding:3px 6px;background:#3c3c3c;color:#cccccc;border:1px solid #007fd4;outline:none;",
                        onmounted: move |event| async move { let _ = event.set_focus(true).await; },
                        oninput: move |event| { query.set(event.value()); reset_match.call(()); },
                        onkeydown: move |event| { if event.key() == Key::Enter { event.prevent_default(); select_match.call(!event.modifiers().shift()); } else if event.key() == Key::Escape { event.prevent_default(); find_open.set(false); if let Some(input) = textarea.peek().as_ref() { let _ = input.focus(); } } else if event.modifiers().alt() { let option = match event.key().to_string().to_lowercase().as_str() { "c" => Some(0), "w" => Some(1), "r" => Some(2), _ => None }; if let Some(option) = option { event.prevent_default(); toggle_option.call(option); } } }
                    }
                    button { r#type: "button", role: "checkbox", aria_label: "Match Case (Alt+C)", aria_checked: "{match_case}", style: if match_case() { "background:#004a77;color:#fff;" } else { "color:#ccc;" }, onclick: move |_| toggle_option.call(0), "Aa" }
                    button { r#type: "button", role: "checkbox", aria_label: "Match Whole Word (Alt+W)", aria_checked: "{whole_word}", style: if whole_word() { "background:#004a77;color:#fff;" } else { "color:#ccc;" }, onclick: move |_| toggle_option.call(1), "ab" }
                    button { r#type: "button", role: "checkbox", aria_label: "Use Regular Expression (Alt+R)", aria_checked: "{regex}", style: if regex() { "background:#004a77;color:#fff;" } else { "color:#ccc;" }, onclick: move |_| toggle_option.call(2), ".*" }
                    span { style: "min-width:55px;text-align:center;", "{count}" }
                    button { r#type: "button", aria_label: "Previous Match (Shift+Enter)", onclick: move |_| select_match.call(false), "↑" }
                    button { r#type: "button", aria_label: "Next Match (Enter)", onclick: move |_| select_match.call(true), "↓" }
                    button { r#type: "button", aria_label: "Close (Escape)", onclick: move |_| { find_open.set(false); if let Some(input) = textarea.peek().as_ref() { let _ = input.focus(); } }, "×" }
                    }
                    if replace_open() { div { style: "display:flex;align-items:center;gap:4px;height:25px;margin-left:14px;",
                        input { aria_label: "Replace", placeholder: "Replace", value: "{replacement}", spellcheck: "false", style: "min-width:0;flex:1;height:24px;padding:3px 6px;background:#3c3c3c;color:#cccccc;border:1px solid #3c3c3c;outline:none;", oninput: move |event| replacement.set(event.value()), onkeydown: move |event| { if event.key() == Key::Enter { event.prevent_default(); replace_matches.call(false); } else if event.key() == Key::Escape { event.prevent_default(); find_open.set(false); if let Some(input) = textarea.peek().as_ref() { let _ = input.focus(); } } } }
                        button { r#type: "button", aria_label: "Replace (Ctrl+Shift+1)", onclick: move |_| replace_matches.call(false), "Replace" }
                        button { r#type: "button", aria_label: "Replace All (Ctrl+Alt+Enter)", onclick: move |_| replace_matches.call(true), "Replace All" }
                    } }
                } }
            }
        }
    }
}

#[component]
fn Action(step: usize, loading: bool, on_action: EventHandler<usize>) -> Element {
    let (label, icon) = match step {
        0 => ("OpenAI", "arrow_forward"),
        2 => ("Target", "arrow_forward"),
        3 => ("Send", "send"),
        _ => return rsx! {},
    };
    rsx! { Button { label: label.to_owned(), icon: icon.to_owned(), small: true, loading, onclick: move |_| on_action.call(step) } }
}

#[component]
fn MetaBadge(label: &'static str, value: String, color: &'static str) -> Element {
    let class =
        format!("inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-mono {color}");
    rsx! { span { class, span { class: "text-body/70 font-sans text-[10px]", "{label}:" } "{value}" } }
}

#[component]
pub(super) fn TranslatorPage() -> Element {
    let mut contents = use_signal(|| vec![String::new(); STEPS.len()]);
    let mut expanded = use_signal(|| vec![true, false, false, false, false, false, false]);
    let mut loading = use_signal(HashSet::<usize>::new);
    let meta = use_signal(|| None::<Value>);
    let copied = use_signal(String::new);
    let generation = use_signal(|| 0_u32);

    let load = use_callback(move |step: usize| {
        if loading.peek().contains(&(step + 10)) {
            return;
        }
        loading.write().insert(step + 10);
        spawn(async move {
            let path = format!("/api/translator/load?file={}", STEPS[step].1);
            match dashboard_request("GET", &path, None).await {
                Ok(response) => match json_body(&response).await {
                    Some(data) if data["success"] == true => {
                        let content = data["content"].as_str().unwrap_or("").to_owned();
                        contents.write()[step] = content.clone();
                        if step == 0 {
                            detect_meta(&content, meta);
                        }
                    }
                    Some(data) => alert(data["error"].as_str().unwrap_or("File not found")),
                    None => alert("Invalid response"),
                },
                Err(_) => alert("Failed to fetch"),
            }
            loading.write().remove(&(step + 10));
        });
    });

    let action = use_callback(move |step: usize| {
        if loading.peek().contains(&step) {
            return;
        }
        loading.write().insert(step);
        let raw = contents.peek()[step].clone();
        let current_meta = meta.peek().clone();
        spawn(async move {
            if !raw.is_empty() {
                match serde_json::from_str::<Value>(&raw) {
                    Ok(body) => {
                        if step == 0 {
                            save(STEPS[0].1, raw);
                            let timestamp = js_sys::Date::new_0()
                                .to_iso_string()
                                .as_string()
                                .unwrap_or_default();
                            save(
                                STEPS[1].1,
                                pretty(
                                    &json!({"timestamp": timestamp, "headers": {}, "body": body.get("body").filter(|v| !v.is_null()).unwrap_or(&body)}),
                                ),
                            );
                            match translate(2, body).await {
                                Ok(result) => {
                                    contents.write()[2] = pretty(&result["body"]);
                                    open_step(expanded, 2);
                                }
                                Err(error) => alert(&error),
                            }
                        } else if step == 2 {
                            save(STEPS[2].1, raw);
                            let mut body = body;
                            if let (Some(object), Some(meta)) =
                                (body.as_object_mut(), current_meta.as_ref())
                            {
                                for key in ["provider", "model"] {
                                    if let Some(value) = meta.get(key) {
                                        object.insert(key.to_owned(), value.clone());
                                    }
                                }
                            }
                            match translate(3, body).await {
                                Ok(mut result) => {
                                    if let (Some(object), Some(meta)) =
                                        (result.as_object_mut(), current_meta.as_ref())
                                    {
                                        for key in ["provider", "model"] {
                                            if let Some(value) = meta.get(key) {
                                                object.insert(key.to_owned(), value.clone());
                                            }
                                        }
                                    }
                                    contents.write()[3] = pretty(&result);
                                    open_step(expanded, 3);
                                }
                                Err(error) => alert(&error),
                            }
                        } else if step == 3 {
                            save(STEPS[3].1, raw);
                            let provider = body["provider"].as_str().or_else(|| {
                                current_meta
                                    .as_ref()
                                    .and_then(|meta| meta["provider"].as_str())
                            });
                            let model = body["model"].as_str().or_else(|| {
                                current_meta
                                    .as_ref()
                                    .and_then(|meta| meta["model"].as_str())
                            });
                            if let (Some(provider), Some(model)) = (provider, model) {
                                let input = json!({"provider": provider, "model": model, "body": body.get("body").filter(|v| !v.is_null()).unwrap_or(&body)});
                                match dashboard_request(
                                    "POST",
                                    "/api/translator/send",
                                    Some(&input),
                                )
                                .await
                                {
                                    Ok(response) if response.ok() => {
                                        if receive_response(response, contents, expanded)
                                            .await
                                            .is_err()
                                        {
                                            alert("Failed to read response stream");
                                        }
                                    }
                                    Ok(response) => {
                                        let fallback = response.status_text();
                                        let data = json_body(&response).await.unwrap_or_default();
                                        alert(data["error"].as_str().unwrap_or(&fallback));
                                    }
                                    Err(_) => alert("Failed to fetch"),
                                }
                            } else {
                                alert("Missing provider or model. Please run step 1 first to detect them.");
                            }
                        }
                    }
                    Err(error) => alert(&error.to_string()),
                }
            }
            loading.write().remove(&step);
        });
    });

    let metadata = meta();
    rsx! {
        div { class: "p-8 space-y-3",
            div { class: "flex items-center justify-between mb-2",
                div { h1 { class: "text-2xl font-bold text-ink", "Translator Debug" } p { class: "text-sm text-body mt-1", "Replay request flow — matches log files" } }
                if let Some(meta) = metadata { div { class: "flex items-center gap-2 flex-wrap justify-end",
                    MetaBadge { label: "src", value: meta["sourceFormat"].as_str().unwrap_or("").to_owned(), color: "bg-blue-500/10 text-blue-500" }
                    span { class: "material-symbols-outlined text-body text-[14px]", "arrow_forward" }
                    MetaBadge { label: "dst", value: meta["targetFormat"].as_str().unwrap_or("").to_owned(), color: "bg-orange-500/10 text-orange-500" }
                    MetaBadge { label: "provider", value: meta["provider"].as_str().unwrap_or("").to_owned(), color: "bg-green-500/10 text-green-500" }
                    MetaBadge { label: "model", value: meta["model"].as_str().unwrap_or("").to_owned(), color: "bg-purple-500/10 text-purple-500" }
                } }
            }
            for (step, (label, file)) in STEPS.iter().enumerate() {
                { let content = contents()[step].clone(); let is_expanded = expanded()[step]; let busy = loading(); rsx! {
                    div { key: "{step}", class: "bg-canvas border border-hairline text-ink rounded-mini-lg shadow-none",
                        div { class: "p-4 space-y-3",
                            div { class: "flex items-center justify-between",
                                button { class: "flex items-center gap-2 flex-1 text-left group", onclick: move |_| { let next = !expanded.peek()[step]; expanded.write()[step] = next; },
                                    span { class: "material-symbols-outlined text-[20px] text-body group-hover:text-ink transition-colors", if is_expanded { "expand_more" } else { "chevron_right" } }
                                    span { class: "text-xs font-mono text-body/60 w-4", "{step + 1}" }
                                    h3 { class: "text-sm font-semibold text-ink", "{label}" }
                                    span { class: "text-xs text-body/60 font-mono", "{file}" }
                                    if !content.is_empty() { span { class: "text-xs text-green-500", "(" "{content.encode_utf16().count()}" " chars)" } }
                                }
                                if !is_expanded { div { class: "flex gap-1 shrink-0",
                                    Button { label: "".to_owned(), icon: "folder_open".to_owned(), variant: "ghost", small: true, loading: busy.contains(&(step + 10)), onclick: move |_| load.call(step) }
                                    Action { step, loading: busy.contains(&step), on_action: action }
                                } }
                            }
                            if is_expanded {
                                div { class: "border border-hairline rounded-mini-lg overflow-hidden", section { style: "display:flex;position:relative;text-align:initial;width:100%;height:400px;", div { style: "width:100%;", Editor { value: content.clone(), json: step < 4, on_change: move |value: String| { let changed = contents.peek()[step] != value; if changed { contents.write()[step] = value.clone(); if step == 0 { detect_meta(&value, meta); } } } } } } }
                                div { class: "flex gap-2 flex-wrap",
                                    Button { label: "Load".to_owned(), icon: "folder_open".to_owned(), variant: "outline", small: true, loading: busy.contains(&(step + 10)), onclick: move |_| load.call(step) }
                                    Button { label: "Format".to_owned(), icon: "data_object".to_owned(), variant: "outline", small: true, onclick: move |_| { let raw = contents.peek()[step].clone(); if let Ok(value) = serde_json::from_str::<Value>(&raw) { contents.write()[step] = pretty(&value); } } }
                                    Button { label: "Copy".to_owned(), icon: "content_copy".to_owned(), variant: "outline", small: true, onclick: move |_| { let text = contents.peek()[step].clone(); if !text.is_empty() { copy_text(text, format!("translator-step-{}", step + 1), copied, generation); } } }
                                    Action { step, loading: busy.contains(&step), on_action: action }
                                }
                            }
                        }
                    }
                } }
            }
        }
    }
}
