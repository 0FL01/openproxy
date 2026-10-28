//! Translator stages and their same-origin dashboard API actions.
mod clipboard;
mod cursor_undo;
mod format;
mod grapheme;
mod multicursor;

use super::{endpoint::copy_text, json_body, shell::dashboard_request, ui::Button};
use dioxus::prelude::*;
use serde_json::{json, Value};
use std::{collections::HashSet, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

#[used]
static FIND_STYLES: Asset = asset!(
    "/assets/translator-find.css",
    AssetOptions::css()
        .with_minify(false)
        .with_static_head(true)
);

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
    let pattern = if whole_word && !regex {
        // Monaco treats every character except its configured separators as a word character.
        const SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/? \t\r\n";
        const WORD_CHARACTER: &str = r#"[^`~!@#$%^&*()\-=+\[\]{}\\|;:'",.<>/? \t\r\n]"#;
        let left = if query.starts_with(|ch| SEPARATORS.contains(ch)) {
            String::new()
        } else {
            format!("(?<!{WORD_CHARACTER})")
        };
        let right = if query.ends_with(|ch| SEPARATORS.contains(ch)) {
            String::new()
        } else {
            format!("(?!{WORD_CHARACTER})")
        };
        format!("{left}(?:{pattern}){right}")
    } else {
        pattern
    };
    let flags = if match_case { "gm" } else { "gim" };
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

fn find_matches(
    text: &str,
    query: &str,
    match_case: bool,
    whole_word: bool,
    regex: bool,
    scope: Option<(u32, u32)>,
) -> Vec<(u32, u32, js_sys::Array<js_sys::JsString>)> {
    let Some(pattern) = find_regex(query, match_case, whole_word, regex) else {
        return Vec::new();
    };
    let scoped_text = scope.map(|(start, end)| {
        js_sys::JsString::from(text)
            .slice(start, end)
            .as_string()
            .unwrap_or_default()
    });
    let offset = scope.map_or(0, |(start, _)| start);
    let text = scoped_text.as_deref().unwrap_or(text);
    let js_text = js_sys::JsString::from(text);
    let mut ranges = Vec::new();
    let text_length = text.encode_utf16().count() as u32;
    while let Some(found) = pattern.exec(text) {
        let end = pattern.last_index();
        let length = found
            .get(0)
            .as_string()
            .unwrap_or_default()
            .encode_utf16()
            .count() as u32;
        let start = end - length;
        let word = |index| {
            char::from_u32(js_text.char_code_at(index) as u32)
                .is_none_or(|ch| !"`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/? \t\r\n".contains(ch))
        };
        if !whole_word
            || !regex
            || ((start == 0 || !word(start - 1) || (length > 0 && !word(start)))
                && (end == text_length || !word(end) || (length > 0 && !word(end - 1))))
        {
            ranges.push((offset + start, offset + end, found));
        }
        if length == 0 {
            if end >= text_length {
                break;
            }
            pattern.set_last_index(end + 1);
        }
    }
    ranges
}

fn find_ranges(
    text: &str,
    query: &str,
    match_case: bool,
    whole_word: bool,
    regex: bool,
    scope: Option<(u32, u32)>,
) -> Vec<(u32, u32)> {
    find_matches(text, query, match_case, whole_word, regex, scope)
        .into_iter()
        .map(|(start, end, _)| (start, end))
        .collect()
}

fn regex_replacement(template: &str, captures: &js_sys::Array<js_sys::JsString>) -> (String, bool) {
    let mut result = String::new();
    let mut dynamic = false;
    let mut chars = template.chars().peekable();
    let mut case_ops = Vec::new();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek().copied() {
                Some('\\' | 'n' | 't') => {
                    result.push(match chars.next().unwrap() {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                    continue;
                }
                Some('u' | 'U' | 'l' | 'L') => {
                    case_ops.push(chars.next().unwrap());
                    continue;
                }
                _ => {}
            }
        } else if ch == '$' {
            match chars.peek().copied() {
                Some('$') => {
                    chars.next();
                    result.push('$');
                    continue;
                }
                Some('&' | '0'..='9') => {
                    dynamic = true;
                    let marker = chars.next().unwrap();
                    let mut index = if marker == '&' {
                        0
                    } else {
                        marker.to_digit(10).unwrap()
                    };
                    if marker != '&'
                        && marker != '0'
                        && chars.peek().is_some_and(char::is_ascii_digit)
                    {
                        index = index * 10 + chars.next().unwrap().to_digit(10).unwrap();
                    }
                    let mut remainder = String::new();
                    while index > 0 && index >= captures.length() {
                        remainder.insert(0, char::from_digit(index % 10, 10).unwrap());
                        index /= 10;
                    }
                    let mut matched = if index == 0 && marker != '0' && marker != '&' {
                        format!("${remainder}")
                    } else {
                        format!(
                            "{}{remainder}",
                            captures.get(index).as_string().unwrap_or_default()
                        )
                    };
                    if !case_ops.is_empty() {
                        let mut transformed = String::new();
                        let mut op = 0;
                        for ch in matched.chars() {
                            match case_ops.get(op) {
                                Some('u' | 'U') => transformed.extend(ch.to_uppercase()),
                                Some('l' | 'L') => transformed.extend(ch.to_lowercase()),
                                _ => transformed.push(ch),
                            }
                            if matches!(case_ops.get(op), Some('u' | 'l')) {
                                op += 1;
                            }
                        }
                        matched = transformed;
                    }
                    case_ops.clear();
                    result.push_str(&matched);
                    continue;
                }
                _ => {}
            }
        }
        result.push(ch);
    }
    (result, dynamic)
}

fn preserve_replacement_case(matched: &str, replacement: &str) -> String {
    if matched.is_empty() {
        return replacement.to_owned();
    }
    let same_parts = |separator| {
        matched.contains(separator)
            && replacement.contains(separator)
            && matched.split(separator).count() == replacement.split(separator).count()
    };
    let hyphens = same_parts('-');
    let underscores = same_parts('_');
    if hyphens != underscores {
        let separator = if hyphens { '-' } else { '_' };
        return matched
            .split(separator)
            .zip(replacement.split(separator))
            .map(|(matched, replacement)| preserve_replacement_case(matched, replacement))
            .collect::<Vec<_>>()
            .join(&separator.to_string());
    }
    let upper = |value: &str| {
        js_sys::JsString::from(value)
            .to_upper_case()
            .as_string()
            .unwrap_or_default()
    };
    let lower = |value: &str| {
        js_sys::JsString::from(value)
            .to_lower_case()
            .as_string()
            .unwrap_or_default()
    };
    if upper(matched) == matched {
        return upper(replacement);
    }
    if lower(matched) == matched {
        return lower(replacement);
    }
    let (Some(first_match), Some(first_replacement)) =
        (matched.chars().next(), replacement.chars().next())
    else {
        return replacement.to_owned();
    };
    // Monaco checks the first UTF-16 code unit; a surrogate is neither upper nor lower case.
    if first_match.len_utf16() != 1 || first_replacement.len_utf16() != 1 {
        return replacement.to_owned();
    }
    let first_match = first_match.to_string();
    let first_replacement = first_replacement.to_string();
    let rest = &replacement[first_replacement.len()..];
    if lower(&first_match) != first_match {
        format!("{}{rest}", upper(&first_replacement))
    } else if upper(&first_match) != first_match {
        format!("{}{rest}", lower(&first_replacement))
    } else {
        replacement.to_owned()
    }
}

fn adjust_find_scope(before: &str, after: &str, (start, end): (u32, u32)) -> (u32, u32) {
    let edit = CompositionEdit::new(before, after);
    let changed_at = before[..edit.start].encode_utf16().count() as u32;
    let old_end = changed_at + edit.removed.encode_utf16().count() as u32;
    let new_end = changed_at + edit.inserted.encode_utf16().count() as u32;
    let shift = |position: u32| position - old_end + new_end;
    let start = if start <= changed_at {
        start
    } else if start >= old_end {
        shift(start)
    } else {
        changed_at
    };
    let end = if end < changed_at {
        end
    } else if end >= old_end {
        shift(end)
    } else {
        new_end
    };
    (start, end)
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

fn backspace_indent(input: &web_sys::HtmlTextAreaElement) -> bool {
    let Some(start) = input.selection_start().ok().flatten() else {
        return false;
    };
    if input.selection_end().ok().flatten() != Some(start) {
        return false;
    }
    let before = js_sys::JsString::from(input.value())
        .slice(0, start)
        .as_string()
        .unwrap_or_default();
    let indent = before.rsplit('\n').next().unwrap_or("");
    if indent.is_empty() || !indent.bytes().all(|ch| matches!(ch, b' ' | b'\t')) {
        return false;
    }
    let columns = indent.bytes().fold(0, |column, ch| {
        column + if ch == b'\t' { 4 - column % 4 } else { 1 }
    });
    let target = (columns - 1) / 4 * 4;
    let mut column = 0;
    let mut keep = 0;
    for ch in indent.bytes() {
        let next = column + if ch == b'\t' { 4 - column % 4 } else { 1 };
        if next > target {
            break;
        }
        column = next;
        keep += 1;
    }
    let count = indent.len() - keep;
    if count <= 1 {
        return false;
    }
    // Native backward deletions preserve both the original caret and the
    // browser's consecutive-delete undo group; do not replace a selection.
    let mut changed = false;
    for _ in 0..count {
        if !editing_command("delete") {
            break;
        }
        changed = true;
    }
    changed
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

fn line_indentation(before: &str) -> String {
    before
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .collect()
}

fn json_bracket_newline(before: &str, after: &str) -> Option<(String, u32)> {
    if !matches!(
        (before.chars().last(), after.chars().next()),
        (Some('{'), Some('}')) | (Some('['), Some(']'))
    ) {
        return None;
    }
    if in_json_string(before) {
        return None;
    }
    let indentation = line_indentation(before);
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
    let cursor = 1 + indentation.encode_utf16().count() as u32 + spaces;
    Some((replacement, cursor))
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
        .slice(end, text.length())
        .as_string()
        .unwrap_or_default();
    let Some((replacement, offset)) = json_bracket_newline(&before, &after) else {
        return false;
    };
    if insert_text(input, start, end, &replacement) {
        let cursor = start + offset;
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
    let indentation = line_indentation(&before);
    !indentation.is_empty() && insert_text(input, start, end, &format!("\n{indentation}"))
}

struct AutoIndent {
    text: String,
    start: usize,
    end: usize,
}

fn insert_line(
    input: &web_sys::HtmlTextAreaElement,
    above: bool,
    json: bool,
    automatic: Option<AutoIndent>,
) -> Option<(NativeUndo, Option<AutoIndent>)> {
    let before = input.value();
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    let end = input.selection_end().ok().flatten().unwrap_or(start);
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    let cursor = if direction == "backward" { start } else { end } as usize;
    let units: Vec<_> = before.encode_utf16().collect();
    let line_start = units[..cursor]
        .iter()
        .rposition(|ch| *ch == 10)
        .map_or(0, |index| index + 1);
    let position = if above {
        line_start.saturating_sub(1)
    } else {
        cursor
            + units[cursor..]
                .iter()
                .position(|ch| *ch == 10)
                .unwrap_or(units.len() - cursor)
    };
    let prefix = String::from_utf16_lossy(&units[..position]);
    let line = prefix.rsplit('\n').next().unwrap_or("");
    let mut columns = line
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .fold(0, |column, ch| {
            column + if ch == '\t' { 4 - column % 4 } else { 1 }
        });
    if json {
        let line_start = prefix.len() - line.len();
        let mut offset = 0;
        let mut last = None;
        for (part, _) in json_tokens(&prefix) {
            offset += part.len();
            if offset > line_start
                && !part.trim().is_empty()
                && !part.starts_with("//")
                && !part.starts_with("/*")
            {
                last = Some(part);
            }
        }
        if matches!(last, Some("{" | "[")) {
            columns += 4 - columns % 4;
        }
    } else if line.trim_end().ends_with(['{', '[', '(']) {
        columns += 4 - columns % 4;
    }
    let at_start = above && line_start == 0;
    let text = if at_start {
        "\n".to_owned()
    } else {
        format!("\n{}", " ".repeat(columns))
    };
    let trim = automatic.filter(|indent| {
        indent.text == before
            && indent.start < indent.end
            && indent.end <= units.len()
            && (indent.start == 0 || units[indent.start - 1] == 10)
            && (indent.end == units.len() || units[indent.end] == 10)
            && units[indent.start..indent.end]
                .iter()
                .all(|ch| matches!(ch, 9 | 32))
            && (position < indent.start
                || position > indent.end
                || position == indent.end && text.starts_with('\n')
                || position == indent.start && text.ends_with('\n'))
    });
    let mut replace_start = position;
    let mut replace_end = position;
    let mut replacement = text.clone();
    let mut removed_before = 0;
    if let Some(indent) = trim {
        replace_start = position.min(indent.start);
        replace_end = position.max(indent.end);
        let mut content = units[replace_start..replace_end].to_vec();
        let insertion = position - replace_start;
        content.splice(insertion..insertion, text.encode_utf16());
        let shift = if position <= indent.start {
            text.len()
        } else {
            0
        };
        content.drain(indent.start - replace_start + shift..indent.end - replace_start + shift);
        replacement = String::from_utf16_lossy(&content);
        if indent.end <= position {
            removed_before = indent.end - indent.start;
        }
    }
    if !insert_text(
        input,
        replace_start as u32,
        replace_end as u32,
        &replacement,
    ) {
        return None;
    }
    let after_cursor = if at_start {
        0
    } else {
        (position + text.len() - removed_before) as u32
    };
    let _ = input.set_selection_range(after_cursor, after_cursor);
    let after = input.value();
    let automatic = (!at_start && columns > 0).then(|| AutoIndent {
        text: after.clone(),
        start: after_cursor as usize - columns,
        end: after_cursor as usize,
    });
    let edit = CompositionEdit::new(&before, &after);
    Some((
        NativeUndo {
            before,
            after,
            start,
            end,
            direction,
            after_selection: Some((after_cursor, after_cursor, "none".into())),
            edits: vec![edit],
        },
        automatic,
    ))
}

fn character_top(line: &web_sys::HtmlElement, mut index: usize) -> Option<f64> {
    let children = line.child_nodes();
    for child in 0..children.length() {
        let Some(span) = children
            .item(child)
            .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
            .filter(|element| element.tag_name() == "SPAN")
        else {
            continue;
        };
        let Some(text) = span.first_child() else {
            continue;
        };
        let length =
            js_sys::JsString::from(text.text_content().unwrap_or_default()).length() as usize;
        if index < length {
            let range = web_sys::window()?.document()?.create_range().ok()?;
            range.set_start(&text, index as u32).ok()?;
            range.set_end(&text, index as u32 + 1).ok()?;
            return Some(range.get_bounding_client_rect().top());
        }
        index -= length;
    }
    None
}

fn ascii_visual_line_ends(line: &web_sys::HtmlElement) -> Vec<usize> {
    let text = line.text_content().unwrap_or_default();
    if !text.is_ascii() || text.len() <= 50 {
        return Vec::new();
    }
    let mut ends = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let Some(top) = character_top(line, start) else {
            return Vec::new();
        };
        let mut low = start + 1;
        let mut high = text.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if character_top(line, middle).is_some_and(|next| next > top) {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        ends.push(low);
        start = low;
    }
    ends
}

fn ascii_token_parts<'a>(text: &'a str, offset: usize, ends: &[usize]) -> Vec<&'a str> {
    if ends.is_empty() || !text.is_ascii() {
        return vec![text];
    }
    // Monaco bounds ASCII spans to 50 characters separately on each visual row.
    // Keep shaping-sensitive non-ASCII text on its existing rendering path.
    let mut parts = Vec::new();
    let mut start = 0;
    for end in ends
        .iter()
        .copied()
        .chain(std::iter::once(offset + text.len()))
    {
        let end = end.saturating_sub(offset).min(text.len());
        while start < end {
            let next = (start + 50).min(end);
            parts.push(&text[start..next]);
            start = next;
        }
        if start == text.len() {
            break;
        }
    }
    parts
}

fn cursor_top(
    input: &web_sys::HtmlTextAreaElement,
    rendered: &web_sys::HtmlElement,
) -> Option<f64> {
    let start = input.selection_start().ok().flatten()?;
    let end = input.selection_end().ok().flatten()?;
    let position = if input.selection_direction().ok().flatten().as_deref() == Some("backward") {
        start
    } else {
        end
    };
    let text = input.value();
    let before = js_sys::JsString::from(text.as_str())
        .slice(0, position)
        .as_string()?;
    let line_number = before.bytes().filter(|ch| *ch == b'\n').count();
    let line_text = text.split('\n').nth(line_number)?;
    let line = rendered
        .child_nodes()
        .item(line_number as u32)?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()?;
    if line.text_content().as_deref() != Some(line_text) {
        return None;
    }
    let length = line_text.encode_utf16().count();
    let top = if length == 0 {
        line.get_bounding_client_rect().top()
    } else {
        let column = before.rsplit('\n').next()?.encode_utf16().count();
        character_top(&line, column.min(length - 1))?
    };
    Some(top - input.get_bounding_client_rect().top())
}

fn smart_home(
    input: &web_sys::HtmlTextAreaElement,
    rendered: Option<&web_sys::HtmlElement>,
    select: bool,
) -> bool {
    let start = input.selection_start().ok().flatten().unwrap_or_default() as usize;
    let end = input.selection_end().ok().flatten().unwrap_or(start as u32) as usize;
    let backward = input.selection_direction().ok().flatten().as_deref() == Some("backward");
    let cursor = if backward { start } else { end };
    let text: Vec<u16> = input.value().encode_utf16().collect();
    let line_start = text[..cursor]
        .iter()
        .rposition(|&ch| ch == b'\n' as u16)
        .map_or(0, |index| index + 1);
    let line_end = text[cursor..]
        .iter()
        .position(|&ch| ch == b'\n' as u16)
        .map_or(text.len(), |index| cursor + index);
    let line_number = text[..line_start]
        .iter()
        .filter(|&&ch| ch == b'\n' as u16)
        .count();
    let Some(line) = rendered.and_then(|element| {
        let children = element.child_nodes();
        (0..children.length())
            .filter_map(|index| {
                children
                    .item(index)?
                    .dyn_into::<web_sys::HtmlElement>()
                    .ok()
            })
            .nth(line_number)
    }) else {
        return false;
    };
    // Leave native navigation untouched when the rendered layout is stale.
    if line.text_content().as_deref()
        != Some(&String::from_utf16_lossy(&text[line_start..line_end]))
    {
        return false;
    }
    if line.offset_height() > 16 && cursor > line_start {
        let offset = cursor - line_start;
        let Some(first_top) = character_top(&line, 0) else {
            return false;
        };
        let Some(current_top) = character_top(&line, offset) else {
            return false;
        };
        if current_top != first_top {
            let Some(previous_top) = character_top(&line, offset - 1) else {
                return false;
            };
            if previous_top == current_top {
                // Native Home first moves to the beginning of this visual line.
                return false;
            }
        }
    }
    let first = text[line_start..line_end]
        .iter()
        .position(|&ch| ch != b' ' as u16 && ch != b'\t' as u16)
        .map_or(line_start, |offset| line_start + offset);
    let target = if cursor == first { line_start } else { first } as u32;
    if !select {
        return input.set_selection_range(target, target).is_ok();
    }
    let anchor = if backward { end } else { start } as u32;
    let direction = if target < anchor {
        "backward"
    } else {
        "forward"
    };
    input
        .set_selection_range_with_direction(target.min(anchor), target.max(anchor), direction)
        .is_ok()
}

fn select_line(input: &web_sys::HtmlTextAreaElement) {
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    let end = input.selection_end().ok().flatten().unwrap_or(start);
    let text = js_sys::JsString::from(input.value());
    let before = text.slice(0, start).as_string().unwrap_or_default();
    let after = text
        .slice(end, text.length())
        .as_string()
        .unwrap_or_default();
    let line_start = start
        - before
            .rsplit('\n')
            .next()
            .unwrap_or("")
            .encode_utf16()
            .count() as u32;
    let line_end = after.find('\n').map_or(text.length(), |index| {
        end + after[..index].encode_utf16().count() as u32 + 1
    });
    let _ = input.set_selection_range(line_start, line_end);
}

fn delete_lines(input: &web_sys::HtmlTextAreaElement) {
    let start = input.selection_start().ok().flatten().unwrap_or_default() as usize;
    let end = input.selection_end().ok().flatten().unwrap_or(start as u32) as usize;
    let text: Vec<u16> = input.value().encode_utf16().collect();
    if text.is_empty() {
        return;
    }
    let newline = b'\n' as u16;
    let line_start = text[..start]
        .iter()
        .rposition(|ch| *ch == newline)
        .map_or(0, |index| index + 1);
    let focus = if input.selection_direction().ok().flatten().as_deref() == Some("backward") {
        start
    } else {
        end
    };
    let focus_start = text[..focus]
        .iter()
        .rposition(|ch| *ch == newline)
        .map_or(0, |index| index + 1);
    let column = focus - focus_start;
    let selected_end = if end > start && text[end - 1] == newline {
        end - 1
    } else {
        end
    };
    let line_end = text[selected_end..]
        .iter()
        .position(|ch| *ch == newline)
        .map_or(text.len(), |index| selected_end + index);
    let (delete_start, delete_end, cursor) = if line_end < text.len() {
        let next = line_end + 1;
        let next_length = text[next..]
            .iter()
            .position(|ch| *ch == newline)
            .unwrap_or(text.len() - next);
        (line_start, next, line_start + column.min(next_length))
    } else if line_start > 0 {
        let previous_end = line_start - 1;
        let previous_start = text[..previous_end]
            .iter()
            .rposition(|ch| *ch == newline)
            .map_or(0, |index| index + 1);
        (
            previous_end,
            text.len(),
            previous_start + column.min(previous_end - previous_start),
        )
    } else {
        (0, text.len(), 0)
    };
    if insert_text(input, delete_start as u32, delete_end as u32, "") {
        let _ = input.set_selection_range(cursor as u32, cursor as u32);
    }
}

fn move_lines(input: &web_sys::HtmlTextAreaElement, down: bool) {
    let start = input.selection_start().ok().flatten().unwrap_or_default() as usize;
    let end = input.selection_end().ok().flatten().unwrap_or(start as u32) as usize;
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    let text: Vec<u16> = input.value().encode_utf16().collect();
    let newline = b'\n' as u16;
    let line_start = text[..start]
        .iter()
        .rposition(|ch| *ch == newline)
        .map_or(0, |index| index + 1);
    let selected_end = if end > start && text[end - 1] == newline {
        end - 1
    } else {
        end
    };
    let line_end = text[selected_end..]
        .iter()
        .position(|ch| *ch == newline)
        .map_or(text.len(), |index| selected_end + index);
    let (replace_start, replace_end, replacement, new_start, new_end) = if down {
        if !text[end..].contains(&newline) {
            return;
        }
        let next_start = line_end + 1;
        let next_end = text[next_start..]
            .iter()
            .position(|ch| *ch == newline)
            .map_or(text.len(), |index| next_start + index);
        let mut replacement = text[next_start..next_end].to_vec();
        replacement.push(newline);
        replacement.extend_from_slice(&text[line_start..line_end]);
        let delta = next_end - next_start + 1;
        (
            line_start,
            next_end,
            replacement,
            start + delta,
            (end + delta).min(text.len()),
        )
    } else {
        if line_start == 0 {
            return;
        }
        let previous_end = line_start - 1;
        let previous_start = text[..previous_end]
            .iter()
            .rposition(|ch| *ch == newline)
            .map_or(0, |index| index + 1);
        let mut replacement = text[line_start..line_end].to_vec();
        replacement.push(newline);
        replacement.extend_from_slice(&text[previous_start..previous_end]);
        let delta = line_start - previous_start;
        (
            previous_start,
            line_end,
            replacement,
            start - delta,
            end - delta,
        )
    };
    if insert_text(
        input,
        replace_start as u32,
        replace_end as u32,
        &String::from_utf16_lossy(&replacement),
    ) {
        let _ =
            input.set_selection_range_with_direction(new_start as u32, new_end as u32, &direction);
    }
}

fn copy_lines(input: &web_sys::HtmlTextAreaElement, down: bool) {
    let start = input.selection_start().ok().flatten().unwrap_or_default() as usize;
    let end = input.selection_end().ok().flatten().unwrap_or(start as u32) as usize;
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    let text: Vec<u16> = input.value().encode_utf16().collect();
    let newline = b'\n' as u16;
    let line_start = text[..start]
        .iter()
        .rposition(|ch| *ch == newline)
        .map_or(0, |index| index + 1);
    let selected_end = if end > start && text[end - 1] == newline {
        end - 1
    } else {
        end
    };
    let line_end = text[selected_end..]
        .iter()
        .position(|ch| *ch == newline)
        .map_or(text.len(), |index| selected_end + index);
    let selected_lines = String::from_utf16_lossy(&text[line_start..line_end]);
    let (position, replacement, delta) = if down {
        (
            line_start,
            format!("{selected_lines}\n"),
            line_end - line_start + 1,
        )
    } else {
        (line_end, format!("\n{selected_lines}"), 0)
    };
    if insert_text(input, position as u32, position as u32, &replacement) {
        let _ = input.set_selection_range_with_direction(
            (start + delta) as u32,
            (end + delta) as u32,
            &direction,
        );
    }
}

fn toggle_json_block_comment(input: &web_sys::HtmlTextAreaElement) {
    let start = input.selection_start().ok().flatten().unwrap_or_default() as usize;
    let end = input.selection_end().ok().flatten().unwrap_or(start as u32) as usize;
    let text: Vec<u16> = input.value().encode_utf16().collect();
    let newline = b'\n' as u16;
    let start_line = text[..start]
        .iter()
        .rposition(|ch| *ch == newline)
        .map_or(0, |index| index + 1);
    let start_line_end = text[start..]
        .iter()
        .position(|ch| *ch == newline)
        .map_or(text.len(), |index| start + index);
    let end_line = text[..end]
        .iter()
        .rposition(|ch| *ch == newline)
        .map_or(0, |index| index + 1);
    let end_line_end = text[end..]
        .iter()
        .position(|ch| *ch == newline)
        .map_or(text.len(), |index| end + index);
    let open = text[start_line..start_line_end]
        .windows(2)
        .enumerate()
        .rev()
        .find(|(index, pair)| {
            start_line + index <= start + 2 && *pair == [b'/' as u16, b'*' as u16]
        })
        .map(|(index, _)| start_line + index);
    let close = text[end_line..end_line_end]
        .windows(2)
        .enumerate()
        .find(|(index, pair)| {
            end_line + index >= end.saturating_sub(2) && *pair == [b'*' as u16, b'/' as u16]
        })
        .map(|(index, _)| end_line + index);
    let has_close = |slice: &[u16]| {
        slice
            .windows(2)
            .any(|pair| pair == [b'*' as u16, b'/' as u16])
    };
    if let (Some(open), Some(close)) = (open, close)
        && open + 2 <= close
        && !(if start_line == end_line {
            has_close(&text[open + 2..close])
        } else {
            has_close(&text[open + 2..start_line_end]) || has_close(&text[end_line..close])
        })
    {
        let inner_start = open + 2 + usize::from(text.get(open + 2) == Some(&(b' ' as u16)));
        let inner_end = close - usize::from(close > inner_start && text[close - 1] == b' ' as u16);
        let inner = String::from_utf16_lossy(&text[inner_start..inner_end]);
        if insert_text(input, open as u32, (close + 2) as u32, &inner) {
            let _ = input.set_selection_range(open as u32, (open + inner_end - inner_start) as u32);
        }
    } else {
        let selected = String::from_utf16_lossy(&text[start..end]);
        if insert_text(
            input,
            start as u32,
            end as u32,
            &format!("/* {selected} */"),
        ) {
            let _ = input.set_selection_range((start + 3) as u32, (end + 3) as u32);
        }
    }
}

fn toggle_json_line_comments(input: &web_sys::HtmlTextAreaElement) {
    let start = input.selection_start().ok().flatten().unwrap_or_default() as usize;
    let end = input.selection_end().ok().flatten().unwrap_or(start as u32) as usize;
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    let text: Vec<u16> = input.value().encode_utf16().collect();
    let newline = b'\n' as u16;
    let line_start = text[..start]
        .iter()
        .rposition(|ch| *ch == newline)
        .map_or(0, |index| index + 1);
    let selected_end = if end > start && text[end - 1] == newline {
        end - 1
    } else {
        end
    };
    let line_end = text[selected_end..]
        .iter()
        .position(|ch| *ch == newline)
        .map_or(text.len(), |index| selected_end + index);
    let lines: Vec<_> = text[line_start..line_end]
        .split(|ch| *ch == newline)
        .collect();
    let indents: Vec<_> = lines
        .iter()
        .map(|line| {
            line.iter()
                .take_while(|ch| **ch == b' ' as u16 || **ch == b'\t' as u16)
                .count()
        })
        .collect();
    let has_content = lines
        .iter()
        .zip(&indents)
        .any(|(line, indent)| *indent < line.len());
    let remove = has_content
        && lines.iter().zip(&indents).all(|(line, indent)| {
            *indent == line.len() || line[*indent..].starts_with(&[b'/' as u16, b'/' as u16])
        });
    let column = lines
        .iter()
        .zip(&indents)
        .filter(|(line, indent)| !has_content || **indent < line.len())
        .map(|(line, indent)| {
            line[..*indent].iter().fold(0, |column, ch| {
                if *ch == b'\t' as u16 {
                    column + 4 - column % 4
                } else {
                    column + 1
                }
            })
        })
        .min()
        .unwrap_or_default()
        / 4
        * 4;
    let mut updated = Vec::new();
    let mut edits = Vec::new();
    let mut offset = line_start;
    for (index, (line, indent)) in lines.iter().zip(&indents).enumerate() {
        if index > 0 {
            updated.push(newline);
        }
        if has_content && *indent == line.len() {
            updated.extend_from_slice(line);
        } else if remove {
            let count = 2 + usize::from(line.get(indent + 2) == Some(&(b' ' as u16)));
            updated.extend_from_slice(&line[..*indent]);
            updated.extend_from_slice(&line[indent + count..]);
            edits.push((offset + indent, count, 0));
        } else {
            let mut width = 0;
            let mut at = 0;
            while at < *indent && width < column {
                let next = if line[at] == b'\t' as u16 {
                    width + 4 - width % 4
                } else {
                    width + 1
                };
                if next > column {
                    break;
                }
                width = next;
                at += 1;
            }
            updated.extend_from_slice(&line[..at]);
            updated.extend_from_slice(&[b'/' as u16, b'/' as u16, b' ' as u16]);
            updated.extend_from_slice(&line[at..]);
            edits.push((offset + at, 0, 3));
        }
        offset += line.len() + 1;
    }
    let position_after = |position: usize| {
        let mut result = position as isize;
        for (at, removed, added) in &edits {
            if position >= at + removed {
                result += *added as isize - *removed as isize;
            } else if position > *at {
                result += *added as isize - (position - at) as isize;
            }
        }
        result as u32
    };
    if insert_text(
        input,
        line_start as u32,
        line_end as u32,
        &String::from_utf16_lossy(&updated),
    ) {
        let _ = input.set_selection_range_with_direction(
            position_after(start),
            position_after(end),
            &direction,
        );
    }
}

fn trim_trailing_whitespace(
    input: &web_sys::HtmlTextAreaElement,
    json: bool,
) -> Option<NativeUndo> {
    let before = input.value();
    let start = input.selection_start().ok().flatten()?;
    let end = input.selection_end().ok().flatten()?;
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut strings = Vec::new();
    if json {
        let mut offset = 0;
        for (text, _) in json_tokens(&before) {
            if text.starts_with('"') {
                strings.push((offset, offset + text.len()));
            }
            offset += text.len();
        }
    }
    let mut string_index = 0;
    let mut offset = 0;
    let mut utf16_offset = 0_u32;
    let mut removed = Vec::new();
    let mut after = String::with_capacity(before.len());
    for line in before.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let mut keep = content.trim_end_matches([' ', '\t']).len();
        let at = offset + keep;
        while strings.get(string_index).is_some_and(|(_, end)| *end <= at) {
            string_index += 1;
        }
        // The reference's default does not trim whitespace tokenized as a string.
        if strings
            .get(string_index)
            .is_some_and(|(start, end)| *start <= at && at < *end)
        {
            keep = content.len();
        }
        if keep < content.len() {
            let at = utf16_offset + content[..keep].encode_utf16().count() as u32;
            removed.push((at, at + (content.len() - keep) as u32));
        }
        after.push_str(&content[..keep]);
        if line.ends_with('\n') {
            after.push('\n');
        }
        offset += line.len();
        utf16_offset += line.encode_utf16().count() as u32;
    }
    if removed.is_empty() {
        return None;
    }
    let mapped = |position: u32| {
        position
            - removed
                .iter()
                .map(|(start, end)| position.saturating_sub(*start).min(end - start))
                .sum::<u32>()
    };
    let selection = (mapped(start), mapped(end), direction.clone());
    let top = input.scroll_top();
    let left = input.scroll_left();
    if !insert_text(input, 0, before.encode_utf16().count() as u32, &after) {
        return None;
    }
    let _ = input.set_selection_range_with_direction(selection.0, selection.1, &selection.2);
    input.set_scroll_top(top);
    input.set_scroll_left(left);
    Some(NativeUndo {
        before,
        after: input.value(),
        start,
        end,
        direction,
        after_selection: Some(selection),
        edits: Vec::new(),
    })
}

fn indent_selection(input: &web_sys::HtmlTextAreaElement, backwards: bool) -> Option<NativeUndo> {
    const TAB_SIZE: u32 = 4;
    let before_value = input.value();
    let text = js_sys::JsString::from(before_value.as_str());
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    let end = input.selection_end().ok().flatten().unwrap_or(start);
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    let before = text.slice(0, start).as_string().unwrap_or_default();
    let line_prefix = before.rsplit('\n').next().unwrap_or("");
    let selected = text.slice(start, end).as_string().unwrap_or_default();
    let after = text
        .slice(end, text.length())
        .as_string()
        .unwrap_or_default();
    if backwards
        || start != end
            && (selected.contains('\n')
                || line_prefix.is_empty() && (after.is_empty() || after.starts_with('\n')))
    {
        return indent_lines(input, backwards);
    }
    let column = line_prefix.chars().fold(0_u32, |column, ch| {
        if ch == '\t' {
            column + TAB_SIZE - column % TAB_SIZE
        } else {
            column + ch.len_utf16() as u32
        }
    });
    let spaces = TAB_SIZE - column % TAB_SIZE;
    if !insert_text(input, start, end, &" ".repeat(spaces as usize)) {
        return None;
    }
    Some(NativeUndo {
        before: before_value,
        after: input.value(),
        start,
        end,
        direction,
        after_selection: Some((start + spaces, start + spaces, "none".into())),
        edits: Vec::new(),
    })
}

fn indent_lines(input: &web_sys::HtmlTextAreaElement, backwards: bool) -> Option<NativeUndo> {
    let before = input.value();
    let text = js_sys::JsString::from(before.as_str());
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    let end = input.selection_end().ok().flatten().unwrap_or(start);
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    let prefix = text.slice(0, start).as_string().unwrap_or_default();
    let line_start = start
        - prefix
            .rsplit('\n')
            .next()
            .unwrap_or("")
            .encode_utf16()
            .count() as u32;
    let last = if end > start && text.slice(end - 1, end).as_string().as_deref() == Some("\n") {
        end - 1
    } else {
        end
    };
    let suffix = text
        .slice(last, text.length())
        .as_string()
        .unwrap_or_default();
    let line_end = last
        + suffix
            .split('\n')
            .next()
            .unwrap_or("")
            .encode_utf16()
            .count() as u32;
    let lines = text
        .slice(line_start, line_end)
        .as_string()
        .unwrap_or_default();
    let multiple = lines.contains('\n');
    let mut offset = line_start;
    let mut change = 0_i64;
    let mut new_start = start as i64;
    let mut new_end = end as i64;
    let mut first_indent = None;
    let updated = lines
        .split('\n')
        .map(|line| {
            let old_indent = line
                .bytes()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            let columns = line[..old_indent].bytes().fold(0_usize, |column, byte| {
                if byte == b'\t' {
                    column + 4 - column % 4
                } else {
                    column + 1
                }
            });
            let columns = if backwards {
                columns.saturating_sub(1) / 4 * 4
            } else {
                (columns / 4 + 1) * 4
            };
            let indent = if multiple && line.is_empty() {
                String::new()
            } else {
                " ".repeat(columns)
            };
            first_indent.get_or_insert(old_indent as u32);
            let old_end = offset + old_indent as u32;
            let delta = indent.len() as i64 - old_indent as i64;
            let map = |original, mapped| {
                if original >= old_end {
                    mapped + delta
                } else if original >= offset {
                    offset as i64 + change + indent.len() as i64
                } else {
                    mapped
                }
            };
            new_start = map(start, new_start);
            new_end = map(end, new_end);
            change += delta;
            offset += line.encode_utf16().count() as u32 + 1;
            format!("{indent}{}", &line[old_indent..])
        })
        .collect::<Vec<_>>()
        .join("\n");
    if updated == lines || !insert_text(input, line_start, line_end, &updated) {
        return None;
    }
    // A selected range starting in the leading whitespace retains its start
    // column on indent; outdent may still pull it toward the new line start.
    if start != end && start - line_start <= first_indent.unwrap_or_default() {
        new_start = new_start.min(start as i64);
    }
    let after_selection = (
        new_start as u32,
        new_end.max(new_start) as u32,
        direction.clone(),
    );
    let _ =
        input.set_selection_range_with_direction(after_selection.0, after_selection.1, &direction);
    Some(NativeUndo {
        before,
        after: input.value(),
        start,
        end,
        direction,
        after_selection: Some(after_selection),
        edits: Vec::new(),
    })
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
            b'/' if bytes.get(offset + 1) == Some(&b'/') => {
                offset += 2;
                while offset < bytes.len() && bytes[offset] != b'\n' {
                    offset += 1;
                }
                "#608b4e"
            }
            b'/' if bytes.get(offset + 1) == Some(&b'*') => {
                offset += 2;
                while offset + 1 < bytes.len()
                    && !(bytes[offset] == b'*' && bytes[offset + 1] == b'/')
                {
                    offset += 1;
                }
                offset = if offset + 1 < bytes.len() {
                    offset + 2
                } else {
                    bytes.len()
                };
                "#608b4e"
            }
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

fn json_guides(token_lines: &[Vec<(&str, &str)>]) -> Vec<Vec<(usize, bool)>> {
    let mut lines = Vec::new();
    let mut depth = 0_usize;
    for segments in token_lines {
        let mut guides = Vec::new();
        let mut leading_closers = 0;
        for (token, color) in segments {
            if leading_closers == 0 && token.trim().is_empty() {
                continue;
            }
            if matches!(*color, "#ffd700" | "#da70d6" | "#179fff") && matches!(*token, "}" | "]") {
                leading_closers += 1;
            } else {
                break;
            }
        }
        for level in 0..depth.saturating_sub(leading_closers) {
            guides.push((level, false));
        }
        let previous = depth;
        for (token, color) in segments {
            if matches!(*color, "#ffd700" | "#da70d6" | "#179fff") {
                match *token {
                    "{" | "[" => depth += 1,
                    "}" | "]" => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
        for level in previous..depth {
            guides.push((level, true));
        }
        lines.push(guides);
    }
    lines
}

fn matching_brackets(tokens: &[(&str, &str)], cursor: u32, json: bool) -> Vec<(u32, u32)> {
    let mut stack = Vec::<(char, u32)>::new();
    let mut pairs = Vec::new();
    let mut offset = 0_u32;
    for &(text, color) in tokens {
        let structural = !json || matches!(color, "#ffd700" | "#da70d6" | "#179fff");
        for ch in text.chars() {
            if structural {
                match ch {
                    '{' | '[' | '(' if !json || ch != '(' => stack.push((ch, offset)),
                    '}' | ']' | ')' if !json || ch != ')' => {
                        let opening = match ch {
                            '}' => '{',
                            ']' => '[',
                            _ => '(',
                        };
                        if let Some(index) = stack.iter().rposition(|(ch, _)| *ch == opening) {
                            pairs.push((stack[index].1, offset));
                            stack.truncate(index);
                        }
                    }
                    _ => {}
                }
            }
            offset += ch.len_utf16() as u32;
        }
    }
    let near = pairs
        .iter()
        .filter_map(|&(open, close)| {
            [open, close]
                .into_iter()
                .filter(|position| *position == cursor || *position + 1 == cursor)
                .max()
                .map(|position| (position, open, close))
        })
        .max_by_key(|(position, _, _)| *position)
        .map(|(_, open, close)| (open, close));
    let pair = near.or_else(|| {
        pairs
            .into_iter()
            .filter(|(open, close)| *open < cursor && cursor <= *close)
            .max_by_key(|(open, _)| *open)
    });
    pair.map_or_else(Vec::new, |(open, close)| {
        vec![(open, open + 1), (close, close + 1)]
    })
}

type HighlightedToken<'a> = (&'a str, &'static str, Option<usize>, bool);

fn jump_to_bracket(
    input: &web_sys::HtmlTextAreaElement,
    json: bool,
    rendered: Option<&web_sys::HtmlElement>,
) {
    let text = input.value();
    let tokens = if json {
        json_tokens(&text)
    } else {
        vec![(text.as_str(), "#d4d4d4")]
    };
    // Monaco navigates from the selection's start, not its active end.
    let position = input.selection_start().ok().flatten().unwrap_or_default();
    let pair = matching_brackets(&tokens, position, json);
    let target = if let &[(open, _), (close, closing_end)] = pair.as_slice() {
        if close <= position && position <= closing_end {
            open
        } else {
            close
        }
    } else {
        let mut offset = 0;
        let mut next = None;
        'search: for (token, color) in &tokens {
            let structural = !json || matches!(*color, "#ffd700" | "#da70d6" | "#179fff");
            for ch in token.chars() {
                if structural
                    && offset >= position
                    && matches!(ch, '{' | '}' | '[' | ']' | '(' | ')')
                {
                    next = Some(offset);
                    break 'search;
                }
                offset += ch.len_utf16() as u32;
            }
        }
        next.unwrap_or(position)
    };
    let _ = input.set_selection_range(target, target);
    if let Some(top) = rendered.and_then(|rendered| cursor_top(input, rendered)) {
        let bottom = top + 16.0 - input.client_height() as f64;
        let delta = if top < 0.0 { top } else { bottom.max(0.0) };
        input.set_scroll_top(input.scroll_top() + delta.ceil() as i32);
    }
}

fn remove_brackets(input: &web_sys::HtmlTextAreaElement, json: bool) -> Option<NativeUndo> {
    let before = input.value();
    let start = input.selection_start().ok().flatten()?;
    let end = input.selection_end().ok().flatten()?;
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_else(|| "none".into());
    let position = if direction == "backward" { start } else { end };
    let tokens = if json {
        json_tokens(&before)
    } else {
        vec![(before.as_str(), "#d4d4d4")]
    };
    let pair = matching_brackets(&tokens, position, json);
    let &[(open, _), (close, closing_end)] = pair.as_slice() else {
        return None;
    };
    let inside = js_sys::JsString::from(before.as_str())
        .slice(open + 1, close)
        .as_string()?;
    if !insert_text(input, open, closing_end, &inside) {
        return None;
    }
    let map = |offset| offset - u32::from(offset > open) - u32::from(offset > close);
    let after_selection = (map(start), map(end), direction.clone());
    let _ =
        input.set_selection_range_with_direction(after_selection.0, after_selection.1, &direction);
    Some(NativeUndo {
        before,
        after: input.value(),
        start,
        end,
        direction,
        after_selection: Some(after_selection),
        edits: Vec::new(),
    })
}

fn highlight_find_matches<'a>(
    lines: &[Vec<(&'a str, &'static str)>],
    matches: &[(u32, u32)],
    brackets: &[(u32, u32)],
) -> Vec<Vec<HighlightedToken<'a>>> {
    let mut result = Vec::with_capacity(lines.len());
    let mut offset = 0_u32;
    let mut match_index = 0;
    let mut bracket_index = 0;
    for line in lines {
        let mut highlighted = Vec::new();
        for &(text, color) in line {
            let mut piece_start = 0;
            let mut piece_match = None;
            let mut piece_bracket = false;
            for (byte, ch) in text.char_indices() {
                while matches
                    .get(match_index)
                    .is_some_and(|(_, end)| *end <= offset)
                {
                    match_index += 1;
                }
                let end = offset + ch.len_utf16() as u32;
                let next_match = matches
                    .get(match_index)
                    .filter(|(start, finish)| *start < end && *finish > offset)
                    .map(|_| match_index);
                while brackets
                    .get(bracket_index)
                    .is_some_and(|(_, finish)| *finish <= offset)
                {
                    bracket_index += 1;
                }
                let next_bracket = brackets
                    .get(bracket_index)
                    .is_some_and(|(start, finish)| *start < end && *finish > offset);
                if next_match != piece_match || next_bracket != piece_bracket {
                    if byte > piece_start {
                        highlighted.push((
                            &text[piece_start..byte],
                            color,
                            piece_match,
                            piece_bracket,
                        ));
                    }
                    piece_start = byte;
                    piece_match = next_match;
                    piece_bracket = next_bracket;
                }
                offset = end;
            }
            if piece_start < text.len() {
                highlighted.push((&text[piece_start..], color, piece_match, piece_bracket));
            }
        }
        result.push(highlighted);
        offset += 1;
    }
    result
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

struct EditorWheel {
    root: web_sys::Element,
    callback: Closure<dyn FnMut(web_sys::WheelEvent)>,
}

impl EditorWheel {
    fn new(input: web_sys::HtmlTextAreaElement) -> Option<Self> {
        let root = input.parent_element()?.parent_element()?.parent_element()?;
        let user_agent = web_sys::window()?
            .navigator()
            .user_agent()
            .unwrap_or_default();
        let mac = user_agent.contains("Macintosh");
        let firefox_lines = user_agent.contains("Firefox") && !mac;
        let callback =
            Closure::<dyn FnMut(web_sys::WheelEvent)>::new(move |event: web_sys::WheelEvent| {
                if event.default_prevented() {
                    return;
                }
                // Monaco's default wheel normalization and 50px sensitivity. The
                // configured wrapped editor consumes horizontal wheel input too.
                let delta = |property: &str, value: f64| {
                    js_sys::Reflect::get(event.as_ref(), &JsValue::from_str(property))
                        .ok()
                        .and_then(|value| value.as_f64())
                        .map(|value| -value * 50.0 / 120.0)
                        .unwrap_or_else(|| {
                            value * 50.0
                                / if event.delta_mode() == 1 {
                                    if firefox_lines {
                                        3.0
                                    } else {
                                        1.0
                                    }
                                } else {
                                    40.0
                                }
                        })
                };
                let x = delta("wheelDeltaX", event.delta_x());
                let mut y = delta("wheelDeltaY", event.delta_y());
                if x.abs() > y.abs() || event.shift_key() && !mac {
                    y = 0.0;
                }
                if event.alt_key() {
                    y *= 5.0;
                }
                event.prevent_default();
                event.stop_propagation();
                input.set_scroll_top(
                    (input.scroll_top() as f64 + y.signum() * y.abs().ceil()) as i32,
                );
            });
        let options = web_sys::AddEventListenerOptions::new();
        options.set_passive(false);
        root.add_event_listener_with_callback_and_add_event_listener_options(
            "wheel",
            callback.as_ref().unchecked_ref(),
            &options,
        )
        .ok()?;
        Some(Self { root, callback })
    }
}

impl Drop for EditorWheel {
    fn drop(&mut self) {
        let _ = self
            .root
            .remove_event_listener_with_callback("wheel", self.callback.as_ref().unchecked_ref());
    }
}

#[derive(Clone)]
struct NativeUndo {
    before: String,
    after: String,
    start: u32,
    end: u32,
    direction: String,
    after_selection: Option<(u32, u32, String)>,
    edits: Vec<CompositionEdit>,
}

#[derive(Default)]
struct NativeSelectionHistory {
    undo: Vec<NativeSelection>,
    redo: Vec<NativeSelection>,
    undo_after: Option<String>,
    redo_before: Option<String>,
}

struct NativeSelection {
    change: CompositionEdit,
    // Ordinary native edits between this command and the preceding command.
    since_previous: Option<CompositionEdit>,
    selection_before: (u32, u32, String),
    selection_after: (u32, u32, String),
    secondary_before: Vec<multicursor::Selection>,
    secondary_after: Vec<multicursor::Selection>,
    native_edits: Vec<CompositionEdit>,
    typing: bool,
}

struct RestoredSelection {
    primary: multicursor::Selection,
    secondary: Vec<multicursor::Selection>,
}

impl NativeSelectionHistory {
    fn record(&mut self, operation: NativeUndo) {
        let Some(after) = operation.after_selection else {
            return;
        };
        // Keep compact changes and one text anchor per history direction, not
        // two whole document copies for every command's selection metadata.
        let since_previous = self
            .undo_after
            .as_ref()
            .map(|previous| CompositionEdit::new(previous, &operation.before));
        self.undo.push(NativeSelection {
            change: CompositionEdit::new(&operation.before, &operation.after),
            since_previous,
            selection_before: (operation.start, operation.end, operation.direction),
            selection_after: after,
            secondary_before: Vec::new(),
            secondary_after: Vec::new(),
            native_edits: Vec::new(),
            typing: false,
        });
        self.undo_after = Some(operation.after);
        self.clear_redo();
    }

    fn clear_redo(&mut self) {
        self.redo.clear();
        self.redo_before = None;
    }

    fn stop_typing(&mut self) {
        if let Some(operation) = self.undo.last_mut() {
            operation.typing = false;
        }
    }

    fn record_multi(
        &mut self,
        operation: NativeUndo,
        before: Vec<multicursor::Selection>,
        after: Vec<multicursor::Selection>,
        typing: bool,
    ) {
        let change = CompositionEdit::new(&operation.before, &operation.after);
        if typing
            && self.undo_after.as_ref() == Some(&operation.before)
            && let Some(previous) = self.undo.last_mut()
            && previous.typing
            && previous.secondary_after == before
            && previous.selection_after
                == (operation.start, operation.end, operation.direction.clone())
            && let Some(original) = previous.change.apply(&operation.before, true)
            && let Some(selection) = operation.after_selection.clone()
        {
            previous.change = CompositionEdit::new(&original, &operation.after);
            previous.native_edits.push(change);
            previous.selection_after = selection;
            previous.secondary_after = after;
            self.undo_after = Some(operation.after);
            self.clear_redo();
            return;
        }
        self.record(operation);
        if let Some(last) = self.undo.last_mut() {
            last.secondary_before = before;
            last.secondary_after = after;
            last.native_edits.push(change);
            last.typing = typing;
        }
    }

    fn multi_history_steps(
        &self,
        value: &str,
        undo: bool,
    ) -> Option<(String, Vec<CompositionEdit>)> {
        let (operations, anchor) = if undo {
            (&self.undo, self.undo_after.as_deref()?)
        } else {
            (&self.redo, self.redo_before.as_deref()?)
        };
        let operation = operations.last()?;
        if value != anchor || operation.native_edits.is_empty() {
            return None;
        }
        let target = operation.change.apply(value, undo)?;
        let edits = if undo {
            operation.native_edits.iter().rev().cloned().collect()
        } else {
            operation.native_edits.clone()
        };
        Some((target, edits))
    }

    fn restore(&mut self, before: &str, after: &str, undo: bool) -> Option<RestoredSelection> {
        let (source, anchor) = if undo {
            (&mut self.undo, self.undo_after.as_deref()?)
        } else {
            (&mut self.redo, self.redo_before.as_deref()?)
        };
        let operation = source.last()?;
        if before != anchor {
            return None;
        }
        let expected = operation.change.apply(anchor, undo)?;
        if after != expected {
            return None;
        }
        let operation = source.pop()?;
        let selection = RestoredSelection {
            primary: if undo {
                operation.selection_before.clone()
            } else {
                operation.selection_after.clone()
            },
            secondary: if undo {
                operation.secondary_before.clone()
            } else {
                operation.secondary_after.clone()
            },
        };
        if undo {
            self.undo_after = operation
                .since_previous
                .as_ref()
                .and_then(|change| change.apply(after, true));
            self.redo_before = Some(expected);
            self.redo.push(operation);
        } else {
            self.redo_before = self
                .redo
                .last()
                .and_then(|next| next.since_previous.as_ref())
                .and_then(|change| change.apply(after, false));
            self.undo_after = Some(expected);
            self.undo.push(operation);
        }
        Some(selection)
    }
}

#[derive(Clone)]
struct CompositionEdit {
    start: usize,
    removed: String,
    inserted: String,
}

impl CompositionEdit {
    fn new(before: &str, after: &str) -> Self {
        let start = before
            .chars()
            .zip(after.chars())
            .take_while(|(left, right)| left == right)
            .map(|(ch, _)| ch.len_utf8())
            .sum::<usize>();
        let suffix = before[start..]
            .chars()
            .rev()
            .zip(after[start..].chars().rev())
            .take_while(|(left, right)| left == right)
            .map(|(ch, _)| ch.len_utf8())
            .sum::<usize>();
        Self {
            start,
            removed: before[start..before.len() - suffix].to_owned(),
            inserted: after[start..after.len() - suffix].to_owned(),
        }
    }

    fn apply(&self, text: &str, undo: bool) -> Option<String> {
        let (from, to) = if undo {
            (&self.inserted, &self.removed)
        } else {
            (&self.removed, &self.inserted)
        };
        let end = self.start.checked_add(from.len())?;
        if text.get(self.start..end)? != from {
            return None;
        }
        Some(format!(
            "{}{to}{}",
            text.get(..self.start)?,
            text.get(end..)?
        ))
    }
}

fn editing_command(command: &str) -> bool {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return false;
    };
    js_sys::Reflect::get(document.as_ref(), &"execCommand".into())
        .ok()
        .and_then(|value| value.dyn_into::<js_sys::Function>().ok())
        .and_then(|function| function.call1(document.as_ref(), &command.into()).ok())
        .and_then(|value| value.as_bool())
        == Some(true)
}

#[component]
fn Editor(
    value: String,
    json: bool,
    mut tab_moves_focus: Signal<bool>,
    mut preserve_case_setting: Signal<bool>,
    on_change: EventHandler<String>,
) -> Element {
    let mut textarea = use_signal(|| None::<web_sys::HtmlTextAreaElement>);
    let mut size_observer = use_signal(|| None::<Rc<EditorSizeObserver>>);
    let mut wheel_listener = use_signal(|| None::<Rc<EditorWheel>>);
    let mut editor_width = use_signal(|| 0_i32);
    let mut rendered = use_signal(|| None::<web_sys::HtmlElement>);
    let mut line_tops = use_signal(Vec::<i32>::new);
    let mut visual_line_ends = use_signal(Vec::<Vec<usize>>::new);
    let initial_value = value.clone();
    let mut pair_text = use_signal(move || initial_value);
    let initial_value = value.clone();
    let mut last_input = use_signal(move || initial_value);
    let mut composition_start = use_signal(|| None::<NativeUndo>);
    let mut composition_undo = use_signal(|| None::<NativeUndo>);
    let mut command_selections = use_signal(NativeSelectionHistory::default);
    let mut cursors = use_signal(multicursor::Cursors::default);
    let mut cursor_boxes = use_signal(Vec::<multicursor::Box>::new);
    let mut multi_input = use_signal(|| None::<Rc<multicursor::InputListener>>);
    let mut cursor_history = use_signal(cursor_undo::CursorUndo::default);
    let mut auto_indent = use_signal(|| None::<AutoIndent>);
    let mut composition_extendable = use_signal(|| false);
    let mut composition_cursor = use_signal(|| None::<(u32, u32)>);
    let mut adjusting_history = use_signal(|| false);
    let mut auto_pairs = use_signal(Vec::<(char, u32)>::new);
    let mut ruler = use_signal(|| None::<web_sys::HtmlCanvasElement>);
    let mut has_changed = use_signal(|| !value.is_empty());
    let mut find_open = use_signal(|| false);
    let mut replace_open = use_signal(|| false);
    let mut tab_focus_announcement = use_signal(String::new);
    let mut command_chord = use_signal(|| false);
    let mut query = use_signal(String::new);
    let mut replacement = use_signal(String::new);
    let mut match_index = use_signal(|| 0_usize);
    let mut current_find = use_signal(|| None::<(u32, u32)>);
    let mut match_case = use_signal(|| false);
    let mut whole_word = use_signal(|| false);
    let mut regex = use_signal(|| false);
    let mut preserve_case = use_signal(|| *preserve_case_setting.peek());
    let mut find_scope = use_signal(|| None::<(u32, u32)>);
    let matches = find_ranges(
        &value,
        &query(),
        match_case(),
        whole_word(),
        regex(),
        find_scope(),
    );
    let mut active_line = use_signal(|| 0_usize);
    let mut active_visual_top = use_signal(|| 0_i32);
    let mut has_selection = use_signal(|| false);
    let mut cursor = use_signal(|| 0_u32);
    let mut widget_focused = use_signal(|| false);
    let mut scroll_top = use_signal(|| 0_i32);
    let mut applied_find_padding = use_signal(|| 0_i32);
    let find_padding = if find_open() {
        if replace_open() {
            62
        } else {
            33
        }
    } else {
        0
    };
    let tokens = if json {
        json_tokens(&value)
    } else {
        vec![(value.as_str(), "#d4d4d4")]
    };
    let brackets = if widget_focused() && !has_selection() {
        matching_brackets(&tokens, cursor(), json)
    } else {
        Vec::new()
    };
    let overview_brackets = if brackets
        .iter()
        .any(|(start, end)| *start == cursor() || *end == cursor())
    {
        brackets.clone()
    } else {
        Vec::new()
    };
    use_effect(use_reactive((&find_padding,), move |(padding,)| {
        if let Some(input) = textarea() {
            let previous = *applied_find_padding.peek();
            if padding != previous {
                input.set_scroll_top((*scroll_top.peek() + padding - previous).max(0));
                scroll_top.set(input.scroll_top());
                applied_find_padding.set(padding);
            }
        }
    }));
    let ranges = (matches.clone(), overview_brackets);
    use_effect(use_reactive((&value, &ranges), move |(value, ranges)| {
        let (matches, brackets) = ranges;
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
        let line = active_line();
        let padding = if find_open() {
            if replace_open() {
                62
            } else {
                33
            }
        } else {
            0
        };
        if !brackets.is_empty() || (find_open() && !matches.is_empty()) {
            let lines: Vec<_> = value.split('\n').collect();
            let mut starts = vec![0_u32];
            for (index, unit) in value.encode_utf16().enumerate() {
                if unit == b'\n' as u16 {
                    starts.push(index as u32 + 1);
                }
            }
            let tops = line_tops();
            let rendered = rendered();
            let position_top = |offset: u32| {
                let line_number = starts.partition_point(|start| *start <= offset) - 1;
                let base = tops
                    .get(line_number)
                    .copied()
                    .unwrap_or(line_number as i32 * 16);
                let visual = rendered
                    .as_ref()
                    .and_then(|element| {
                        let line = element
                            .child_nodes()
                            .item(line_number as u32)?
                            .dyn_into::<web_sys::HtmlElement>()
                            .ok()?;
                        let text = lines[line_number];
                        if line.offset_height() <= 16
                            || line.text_content().as_deref() != Some(text)
                        {
                            return None;
                        }
                        let length = text.encode_utf16().count();
                        if length == 0 {
                            return None;
                        }
                        let first = character_top(&line, 0)?;
                        let current = character_top(
                            &line,
                            ((offset - starts[line_number]) as usize).min(length - 1),
                        )?;
                        Some((current - first).round().max(0.0) as i32)
                    })
                    .unwrap_or(0);
                base + visual + padding
            };
            let ratio = height as f64 / document_height;
            let half_min = (6.0 * scale) as i32 / 2;
            let draw = |start: i32, end: i32| {
                context.fill_rect(
                    1.0,
                    start as f64,
                    (width.saturating_sub(1) / 2) as f64,
                    (end - start) as f64,
                );
            };
            let find_matches = if find_open() { matches.as_slice() } else { &[] };
            for (color, ranges) in [
                ("#a0a0a0", brackets.as_slice()),
                ("rgba(209,134,22,0.49)", find_matches),
            ] {
                context.set_fill_style_str(color);
                let mut combined: Option<(i32, i32)> = None;
                for &(start, end) in ranges {
                    let mut y1 = (position_top(start).max(0) as f64 * ratio) as i32;
                    let mut y2 = ((position_top(end.saturating_sub(u32::from(end > start))) + 16)
                        .max(0) as f64
                        * ratio) as i32;
                    if y2 - y1 < half_min * 2 {
                        let center = ((y1 + y2) / 2).clamp(half_min, height as i32 - half_min);
                        y1 = center - half_min;
                        y2 = center + half_min;
                    }
                    if let Some((previous_start, previous_end)) = combined {
                        if y1 <= previous_end + 1 {
                            combined = Some((previous_start, previous_end.max(y2)));
                        } else {
                            draw(previous_start, previous_end);
                            combined = Some((y1, y2));
                        }
                    } else {
                        combined = Some((y1, y2));
                    }
                }
                if let Some((start, end)) = combined {
                    draw(start, end);
                }
            }
        }
        let top = line_tops().get(line).copied().unwrap_or(line as i32 * 16)
            + active_visual_top()
            + padding;
        let y = ((top.max(0) as f64 * height as f64 / document_height) as u32)
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
            if !*adjusting_history.peek()
                && cursors
                    .peek()
                    .ranges
                    .first()
                    .is_some_and(|expected| *expected != multicursor::selection(input))
            {
                cursors.set(multicursor::Cursors::default());
                command_selections.write().stop_typing();
            }
            cursor_history
                .write()
                .observe(input, &cursors.peek().ranges);
            let start = input.selection_start().ok().flatten().unwrap_or_default();
            let end = input.selection_end().ok().flatten().unwrap_or(start);
            if current_find
                .peek()
                .is_some_and(|selected| selected != (start, end))
            {
                current_find.set(None);
            }
            if *composition_extendable.peek()
                && composition_start.peek().is_none()
                && !*adjusting_history.peek()
                && *composition_cursor.peek() != Some((start, end))
            {
                composition_extendable.set(false);
            }
            let position =
                if input.selection_direction().ok().flatten().as_deref() == Some("backward") {
                    start
                } else {
                    end
                };
            cursor.set(position);
            let value = input.value();
            let before = js_sys::JsString::from(value.as_str())
                .slice(0, position)
                .as_string()
                .unwrap_or_default();
            let line_number = before.bytes().filter(|byte| *byte == b'\n').count();
            active_line.set(line_number);
            has_selection.set(start != end);
            let column =
                js_sys::JsString::from(before.rsplit('\n').next().unwrap_or("")).length() as usize;
            let line_text = value.split('\n').nth(line_number).unwrap_or("");
            let top = rendered
                .peek()
                .as_ref()
                .and_then(|element| {
                    let line = element
                        .child_nodes()
                        .item(line_number as u32)?
                        .dyn_into::<web_sys::HtmlElement>()
                        .ok()?;
                    if line.text_content().as_deref() != Some(line_text) {
                        return None;
                    }
                    let length = js_sys::JsString::from(line_text).length() as usize;
                    if length == 0 {
                        return Some(0);
                    }
                    let first = character_top(&line, 0)?;
                    let current = character_top(&line, column.min(length - 1))?;
                    Some((current - first).round().max(0.0) as i32)
                })
                .unwrap_or(0);
            active_visual_top.set(top);
        }
    });
    let finish_multi_edit = use_callback(
        move |(operation, ranges): (NativeUndo, Vec<multicursor::Selection>)| {
            let after = operation.after.clone();
            let next = cursors.peek().clone();
            if next.starts_typing {
                command_selections.write().stop_typing();
            }
            auto_pairs.set(next.auto_pairs());
            pair_text.set(after.clone());
            command_selections.write().record_multi(
                operation,
                ranges.into_iter().skip(1).collect(),
                next.ranges.into_iter().skip(1).collect(),
                next.typing,
            );
            let previous = last_input.peek().clone();
            let scope = *find_scope.peek();
            if let Some(scope) = scope {
                find_scope.set(Some(adjust_find_scope(&previous, &after, scope)));
            }
            auto_indent.set(None);
            composition_extendable.set(false);
            current_find.set(None);
            last_input.set(after.clone());
            on_change.call(after);
            update_cursor.call(());
        },
    );
    let before_input = use_callback(move |event: web_sys::InputEvent| {
        if *adjusting_history.peek() || event.is_composing() || !event.cancelable() {
            return;
        }
        let kind = event.input_type();
        if !matches!(
            kind.as_str(),
            "insertText"
                | "insertLineBreak"
                | "insertParagraph"
                | "deleteContentBackward"
                | "deleteContentForward"
        ) {
            return;
        }
        let Some(input) = textarea.peek().clone() else {
            return;
        };
        let ranges = cursors.peek().ranges.clone();
        if ranges.len() < 2 || ranges.first() != Some(&multicursor::selection(&input)) {
            return;
        }
        event.prevent_default();
        adjusting_history.set(true);
        let operation = if json && matches!(kind.as_str(), "insertLineBreak" | "insertParagraph") {
            cursors.write().enter_json(&input)
        } else if json {
            cursors
                .write()
                .apply_json(&input, &kind, &event.data().unwrap_or_default())
        } else {
            cursors
                .write()
                .apply(&input, &kind, &event.data().unwrap_or_default())
        };
        adjusting_history.set(false);
        if let Some(operation) = operation {
            finish_multi_edit.call((operation, ranges));
        }
    });
    use_effect(use_reactive((&value,), move |(value,)| {
        let ranges = cursors().ranges;
        let _ = scroll_top();
        let _ = editor_width();
        let _ = applied_find_padding();
        let Some(input) = textarea() else { return };
        let Some(element) = rendered() else { return };
        if ranges.len() < 2 {
            cursor_boxes.set(Vec::new());
            return;
        }
        spawn(async move {
            for _ in 0..2 {
                let frame = js_sys::Promise::new(&mut |resolve, _| {
                    if let Some(window) = web_sys::window() {
                        let _ = window.request_animation_frame(&resolve);
                    }
                });
                let _ = JsFuture::from(frame).await;
            }
            if cursors.peek().ranges == ranges && input.value() == value {
                cursor_boxes.set(multicursor::boxes(&input, &element, &ranges[1..]));
            }
        });
    }));
    let on_clipboard = use_callback(move |event: ClipboardEvent| {
        if composition_start.peek().is_some() {
            return;
        }
        let data = event.data();
        let Some(native) = data
            .downcast::<web_sys::Event>()
            .and_then(|event| event.dyn_ref::<web_sys::ClipboardEvent>())
        else {
            return;
        };
        let Some(input) = textarea.peek().clone() else {
            return;
        };
        let ranges = cursors.peek().ranges.clone();
        if ranges.len() > 1 && ranges.first() == Some(&multicursor::selection(&input)) {
            adjusting_history.set(true);
            let operation = match native.type_().as_str() {
                "copy" => clipboard::copy_multiple(&input, native, false, &mut cursors.write()),
                "cut" => clipboard::copy_multiple(&input, native, true, &mut cursors.write()),
                "paste" => clipboard::paste_multiple(&input, native, &mut cursors.write()),
                _ => None,
            };
            adjusting_history.set(false);
            if let Some(operation) = operation {
                finish_multi_edit.call((operation, ranges));
            }
            return;
        }
        let undo = match native.type_().as_str() {
            "copy" => clipboard::copy(&input, native, false, json),
            "cut" => clipboard::copy(&input, native, true, json),
            "paste" => clipboard::paste_line(&input, native),
            _ => None,
        };
        if let Some(undo) = undo {
            command_selections.write().record(undo);
            composition_extendable.set(false);
            update_cursor.call(());
        }
    });
    use_effect(use_reactive((&value,), move |_| {
        if let Some(input) = textarea() {
            cursor_history.write().reset(&input, &cursors.peek().ranges);
            if input.value() != *last_input.peek() {
                cursors.set(multicursor::Cursors::default());
            }
        }
    }));
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
            let lines: Vec<_> = (0..children.length())
                .filter_map(|index| {
                    children
                        .item(index)?
                        .dyn_into::<web_sys::HtmlElement>()
                        .ok()
                })
                .collect();
            line_tops.set(lines.iter().map(|line| line.offset_top()).collect());
            visual_line_ends.set(lines.iter().map(ascii_visual_line_ends).collect());
            update_cursor.call(());
        });
    }));
    let select_match = use_callback(move |next: bool| {
        if let Some(input) = textarea.peek().as_ref() {
            let ranges = find_ranges(
                &input.value(),
                &query.peek(),
                *match_case.peek(),
                *whole_word.peek(),
                *regex.peek(),
                *find_scope.peek(),
            );
            if !ranges.is_empty() {
                let start = input.selection_start().ok().flatten().unwrap_or_default();
                let end = input.selection_end().ok().flatten().unwrap_or(start);
                let mut index = if next {
                    ranges
                        .iter()
                        .position(|(start, _)| *start >= end)
                        .unwrap_or(0)
                } else {
                    ranges
                        .iter()
                        .rposition(|(_, end)| *end <= start)
                        .unwrap_or(ranges.len() - 1)
                };
                if start == end && ranges[index] == (start, end) {
                    index = if next {
                        (index + 1) % ranges.len()
                    } else {
                        (index + ranges.len() - 1) % ranges.len()
                    };
                }
                match_index.set(index);
                let (start, end) = ranges[index];
                let _ = input.set_selection_range(start, end);
                current_find.set(Some((start, end)));
                update_cursor.call(());
            }
        }
    });
    let reset_match = use_callback(move |_| {
        match_index.set(0);
        current_find.set(None);
        if let Some(input) = textarea.peek().as_ref()
            && let Some((start, end)) = find_ranges(
                &input.value(),
                &query.peek(),
                *match_case.peek(),
                *whole_word.peek(),
                *regex.peek(),
                *find_scope.peek(),
            )
            .first()
            .copied()
        {
            let _ = input.set_selection_range(start, end);
            current_find.set(Some((start, end)));
            update_cursor.call(());
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
    let toggle_scope = use_callback(move |_| {
        let current = *find_scope.peek();
        let scope = if current.is_some() {
            None
        } else {
            textarea.peek().as_ref().and_then(|input| {
                let start = input.selection_start().ok().flatten()?;
                let mut end = input.selection_end().ok().flatten()?;
                let text = js_sys::JsString::from(input.value());
                if end > start && text.slice(end - 1, end).as_string().as_deref() == Some("\n") {
                    end -= 1;
                }
                (start < end).then_some((start, end))
            })
        };
        find_scope.set(scope);
        reset_match.call(());
    });
    let close_find = use_callback(move |_| {
        find_open.set(false);
        find_scope.set(None);
        current_find.set(None);
        if let Some(input) = textarea.peek().as_ref() {
            let _ = input.focus();
        }
    });
    let toggle_preserve = use_callback(move |_| {
        let next = !*preserve_case.peek();
        preserve_case.set(next);
        preserve_case_setting.set(next);
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
        let matches = find_matches(
            &text,
            &query,
            *match_case.peek(),
            *whole_word.peek(),
            *regex.peek(),
            *find_scope.peek(),
        );
        if matches.is_empty() {
            return;
        }
        let input_text = js_sys::JsString::from(text);
        let current = *match_index.peek() % matches.len();
        let (start, end) = if all {
            (*find_scope.peek()).unwrap_or((0, input_text.length()))
        } else {
            (matches[current].0, matches[current].1)
        };
        let replacement = if *regex.peek() || *preserve_case.peek() {
            let chosen = if all {
                &matches[..]
            } else {
                &matches[current..current + 1]
            };
            let mut result = String::new();
            let mut previous = start;
            for (match_start, match_end, captures) in chosen {
                result.push_str(
                    &input_text
                        .slice(previous, *match_start)
                        .as_string()
                        .unwrap_or_default(),
                );
                let (replace, dynamic) = if *regex.peek() {
                    regex_replacement(&replacement.peek(), captures)
                } else {
                    (replacement.peek().clone(), false)
                };
                if *preserve_case.peek() && !dynamic {
                    let matched = captures.get(0).as_string().unwrap_or_default();
                    result.push_str(&preserve_replacement_case(&matched, &replace));
                } else {
                    result.push_str(&replace);
                }
                previous = *match_end;
            }
            result.push_str(
                &input_text
                    .slice(previous, end)
                    .as_string()
                    .unwrap_or_default(),
            );
            result
        } else if all {
            input_text
                .slice(start, end)
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
            *find_scope.peek(),
        );
        if let Some(index) = ranges
            .iter()
            .position(|(next, _)| *next >= input.selection_end().ok().flatten().unwrap_or_default())
            .or_else(|| (!ranges.is_empty()).then_some(0))
        {
            match_index.set(index);
            let _ = input.set_selection_range(ranges[index].0, ranges[index].1);
            current_find.set(Some(ranges[index]));
            update_cursor.call(());
        } else {
            current_find.set(None);
        }
    });
    let count = if matches.is_empty() {
        "No results".to_owned()
    } else {
        format!(
            "{} of {}",
            (match_index() % matches.len()) + 1,
            matches.len()
        )
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
    let text_top = find_padding - scroll;
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
        json_guides(&lines)
    } else {
        Vec::new()
    };
    let current_match = if matches.is_empty() {
        0
    } else {
        match_index() % matches.len()
    };
    let selected_match = find_open()
        && textarea().as_ref().is_some_and(|input| {
            matches.get(current_match).is_some_and(|(start, end)| {
                *current_find.read() == Some((*start, *end))
                    && input.selection_start().ok().flatten() == Some(*start)
                    && input.selection_end().ok().flatten() == Some(*end)
            })
        });
    let selected_lines = if selected_match {
        let (start, end) = matches[current_match];
        let text = js_sys::JsString::from(value.as_str());
        let before = text.slice(0, start).as_string().unwrap_or_default();
        let matched = text.slice(start, end).as_string().unwrap_or_default();
        let first = before.bytes().filter(|byte| *byte == b'\n').count();
        let mut last = first + matched.bytes().filter(|byte| *byte == b'\n').count();
        if last > first && matched.ends_with('\n') {
            last -= 1;
        }
        Some(first..=last)
    } else {
        None
    };
    let find_matches = if find_open() { matches.as_slice() } else { &[] };
    let mut highlighted = (!find_matches.is_empty() || !brackets.is_empty())
        .then(|| highlight_find_matches(&lines, find_matches, &brackets));
    let ends = visual_line_ends.read();
    if let Some(highlighted) = &mut highlighted {
        for (line, tokens) in highlighted.iter_mut().enumerate() {
            let Some(ends) = ends.get(line) else { continue };
            let mut offset = 0;
            *tokens = tokens
                .iter()
                .flat_map(|&(text, color, matched, bracket)| {
                    let parts = ascii_token_parts(text, offset, ends);
                    offset += text.len();
                    parts
                        .into_iter()
                        .map(move |part| (part, color, matched, bracket))
                })
                .collect();
        }
    } else {
        for (line, tokens) in lines.iter_mut().enumerate() {
            let Some(ends) = ends.get(line) else { continue };
            let mut offset = 0;
            *tokens = tokens
                .iter()
                .flat_map(|&(text, color)| {
                    let parts = ascii_token_parts(text, offset, ends);
                    offset += text.len();
                    parts.into_iter().map(move |part| (part, color))
                })
                .collect();
        }
    }
    let root_width = editor_width() + 76;
    let find_layout = if root_width <= 328 {
        "reduced narrow collapsed"
    } else if root_width <= 378 {
        "reduced narrow"
    } else if root_width <= 447 {
        "reduced"
    } else {
        ""
    };
    let replacing = if replace_open() { "replacing" } else { "" };
    rsx! {
        div { style: "height:400px;position:relative;background:#1e1e1e;color:#d4d4d4;",
            onfocusin: move |_| { widget_focused.set(true); update_cursor.call(()); },
            onfocusout: move |_| widget_focused.set(false),
            div { style: "position:relative;overflow:hidden;width:100%;height:400px;",
                div { aria_hidden: "true", style: "position:absolute;transform:translate3d(0,0,0);contain:strict;top:0;height:400px;width:62px;background:#1e1e1e;font-family:'Droid Sans Mono',monospace;font-size:12px;line-height:16px;",
                    for line in 0..lines.len() {
                        { let top = line_tops().get(line).copied().unwrap_or(line as i32 * 16) + find_padding - scroll; let color = if value.is_empty() { "rgba(133,133,133,.4)" } else if line == active_line() { "#c6c6c6" } else { "#858585" }; rsx! {
                            div { key: "{line}", style: "position:absolute;top:{top}px;left:0;width:36px;text-align:right;color:{color};pointer-events:none;", "{line + 1}" }
                        } }
                    }
                }
                div { style: "position:absolute;overflow:hidden;left:62px;right:0;height:400px;background:#1e1e1e;transform:translate3d(0,0,0);contain:paint;",
                    for (index, rect) in cursor_boxes().iter().enumerate() {
                        { let width = if rect.caret { 2.0 } else { rect.width }; let color = if rect.caret { "#aeafad" } else { "#264f78" }; rsx! {
                            div { key: "{index}", aria_hidden: "true", class: if rect.caret { "multicursor-cursor" } else { "multicursor-selection" }, style: "position:absolute;pointer-events:none;left:{rect.left}px;top:{rect.top}px;width:{width}px;height:{rect.height}px;background:{color};" }
                        } }
                    }
                    pre { aria_hidden: "true", style: "position:absolute;top:{text_top}px;left:0;right:0;margin:0;white-space:pre-wrap;overflow-wrap:anywhere;pointer-events:none;font-family:'Droid Sans Mono',monospace;font-size:12px;line-height:16px;tab-size:4;",
                        onmounted: move |event| { if let Some(element) = event.data().downcast::<web_sys::Element>() && let Ok(element) = element.clone().dyn_into::<web_sys::HtmlElement>() { rendered.set(Some(element)); } },
                        for (line, segments) in lines.iter().enumerate() {
                            { let in_range = selected_lines.as_ref().is_some_and(|range| range.contains(&line)); let layer = if in_range { "z-index:0;" } else { "" }; rsx! {
                            div { key: "{line}", style: "position:relative;width:{wrapping_width};min-height:16px;padding-left:{wrapping_indent}px;text-indent:-{wrapping_indent}px;{layer}",
                                if in_range { div { class: "find-range-highlight", style: "position:absolute;z-index:-1;top:0;left:0;width:{current_line_width}px;height:100%;background:rgba(255,255,255,.04);" } }
                                if let Some(line_guides) = guides.get(line) {
                                    for (index, (level, start)) in line_guides.iter().enumerate() {
                                        { let vertical = if *start { "bottom:0;height:2px" } else { "top:0;height:100%;z-index:1" }; rsx! { div { key: "{index}", style: "position:absolute;left:{*level as f64 * indent as f64 * 7.2246}px;{vertical};width:1px;background:#707070;" } } }
                                    }
                                }
                                if line == active_line() && !has_selection() { div { style: "position:absolute;top:{active_visual_top}px;left:0;width:{current_line_width}px;height:16px;border:2px solid #282828;" } }
                                if let Some(highlighted) = &highlighted {
                                    for (index, (text, color, matched, bracket)) in highlighted[line].iter().enumerate() {
                                        { let background = match (matched, bracket) { (Some(index), true) if selected_match && *index == current_match => "linear-gradient(rgba(0,100,0,.1),rgba(0,100,0,.1)),#515c6a", (Some(_), true) => "linear-gradient(rgba(0,100,0,.1),rgba(0,100,0,.1)),rgba(234,92,0,.33)", (None, true) => "rgba(0,100,0,.1)", (Some(index), false) if selected_match && *index == current_match => "#515c6a", (Some(_), false) => "rgba(234,92,0,.33)", (None, false) => "transparent" }; let padding = if *bracket || matched.is_some() { "1px 0" } else { "0" }; let border = if *bracket { "inset 0 0 0 1px #888" } else { "none" }; rsx! {
                                            span { key: "{index}", class: if *bracket && matched.is_some() { "find-match bracket-match" } else if *bracket { "bracket-match" } else if matched.is_some() { "find-match" } else { "" }, style: "color:{color};background:{background};padding:{padding};box-shadow:{border};position:relative;z-index:1", "{text}" }
                                        } }
                                    }
                                } else {
                                    for (index, (text, color)) in segments.iter().enumerate() { span { key: "{index}", class: "", style: "color:{color};background:transparent;padding:0;box-shadow:none;position:relative;z-index:1", "{text}" } }
                                }
                            }
                            } }
                        }
                    }
                    textarea { aria_label: "Translator editor", spellcheck: "false", value: "{value}", class: if cursors.read().ranges.is_empty() { "" } else { "translator-occurrences" },
                        oncopy: move |event| on_clipboard.call(event),
                        oncut: move |event| on_clipboard.call(event),
                        onpaste: move |event| on_clipboard.call(event),
                        style: "position:absolute;inset:0 14px 0 0;width:calc(100% - 14px);height:100%;resize:none;border:0;outline:0;padding:{find_padding}px {right_padding}px 0 {wrapping_indent}px;text-indent:-{wrapping_indent}px each-line;background:transparent;color:transparent;caret-color:white;font-family:'Droid Sans Mono',monospace;font-size:12px;line-height:16px;white-space:pre-wrap;overflow-wrap:anywhere;tab-size:4;",
                        onmounted: move |event| {
                            if let Some(element) = event.data().downcast::<web_sys::Element>() && let Ok(input) = element.clone().dyn_into::<web_sys::HtmlTextAreaElement>() && let Some(container) = input.parent_element() {
                                editor_width.set(container.client_width().saturating_sub(14));
                                let observed = container.clone();
                                let callback = Closure::<dyn FnMut(js_sys::Array, web_sys::ResizeObserver)>::new(move |_, _| editor_width.set(observed.client_width().saturating_sub(14)));
                                if let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
                                    observer.observe(&container);
                                    size_observer.set(Some(Rc::new(EditorSizeObserver { observer, _callback: callback })));
                                }
                                wheel_listener.set(EditorWheel::new(input.clone()).map(Rc::new));
                                multi_input.set(multicursor::InputListener::new(input.clone(), before_input));
                                textarea.set(Some(input));
                            }
                        },
                        onkeydown: move |event| {
                            if event.is_composing() { return; }
                            let key = event.key().to_string();
                            if let Some(input) = textarea.peek().as_ref() {
                                cursor_history.write().observe(input, &cursors.peek().ranges);
                            }
                            if (event.modifiers().ctrl() || event.modifiers().meta()) && !event.modifiers().alt() && !event.modifiers().shift() && key.eq_ignore_ascii_case("u") {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() {
                                    let restored = cursor_history.write().undo(input, &cursors.peek().ranges);
                                    if let Some(ranges) = restored { cursors.write().restore(ranges); command_selections.write().stop_typing(); }
                                }
                                update_cursor.call(());
                                return;
                            }
                            let modifiers = event.modifiers();
                            let control = modifiers.ctrl() || modifiers.meta();
                            if !control && !modifiers.alt() && matches!(event.key(), Key::ArrowUp | Key::ArrowDown) && cursors.peek().ranges.len() > 1 {
                                let changed = if let Some(input) = textarea.peek().as_ref()
                                    && let Some(element) = rendered.peek().as_ref() {
                                    cursors.write().vertical(input, element, event.key() == Key::ArrowUp, modifiers.shift())
                                } else { None };
                                if let Some(changed) = changed {
                                    event.prevent_default();
                                    if changed {
                                        command_selections.write().stop_typing();
                                        composition_extendable.set(false);
                                    }
                                    update_cursor.call(());
                                }
                                return;
                            }
                            if !control && !modifiers.alt() && matches!(event.key(), Key::ArrowLeft | Key::ArrowRight) && cursors.peek().ranges.len() > 1 {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() {
                                    if cursors.write().horizontal(input, event.key() == Key::ArrowLeft, modifiers.shift()) {
                                        command_selections.write().stop_typing();
                                        composition_extendable.set(false);
                                    }
                                    update_cursor.call(());
                                }
                                return;
                            }
                            if modifiers.shift() && modifiers.alt() && !control && key.eq_ignore_ascii_case("i") {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() {
                                    let changed = cursors.write().line_ends(input);
                                    if changed {
                                        command_selections.write().stop_typing();
                                        update_cursor.call(());
                                    }
                                }
                                return;
                            }
                            if control && !modifiers.alt() && ((!modifiers.shift() && key.eq_ignore_ascii_case("d")) || (modifiers.shift() && key.eq_ignore_ascii_case("l"))) {
                                event.prevent_default();
                                command_selections.write().stop_typing();
                                if let Some(input) = textarea.peek().as_ref() {
                                    cursors.write().choose(input, json, *match_case.peek(), *whole_word.peek(), modifiers.shift());
                                    update_cursor.call(());
                                }
                                return;
                            }
                            if event.key() == Key::Escape && cursors.peek().ranges.len() > 1 {
                                event.prevent_default();
                                cursors.set(multicursor::Cursors::default());
                                command_selections.write().stop_typing();
                                return;
                            }
                            if control && !modifiers.alt() && (key.eq_ignore_ascii_case("z") || key.eq_ignore_ascii_case("y"))
                                && let Some(input) = textarea.peek().as_ref() {
                                let before = input.value();
                                let undo = key.eq_ignore_ascii_case("z") && !modifiers.shift();
                                let work = command_selections.peek().multi_history_steps(&before, undo);
                                if let Some((target, edits)) = work {
                                    event.prevent_default();
                                    adjusting_history.set(true);
                                    let mut after = before.clone();
                                    for edit in edits {
                                        // Identical replacement still changes selection and can
                                        // consume a native history entry without changing text.
                                        let Some(expected) = edit.apply(&after, undo) else { break };
                                        if !editing_command(if undo { "undo" } else { "redo" }) { break; }
                                        after = input.value();
                                        if after != expected {
                                            if after != target {
                                                let _ = editing_command(if undo { "redo" } else { "undo" });
                                                after = input.value();
                                            }
                                            break;
                                        }
                                    }
                                    adjusting_history.set(false);
                                    let restored = command_selections.write().restore(&before, &after, undo);
                                    if let Some(restored) = restored {
                                        let (start, end, direction) = &restored.primary;
                                        let _ = input.set_selection_range_with_direction(*start, *end, direction);
                                        let mut ranges = vec![multicursor::selection(input)]; ranges.extend(restored.secondary);
                                        cursors.write().restore(ranges);
                                    } else { cursors.set(multicursor::Cursors::default()); }
                                    let scope = *find_scope.peek();
                                    if let Some(scope) = scope { find_scope.set(Some(adjust_find_scope(&before, &after, scope))); }
                                    composition_extendable.set(false);
                                    auto_indent.set(None);
                                    cursor_history.write().reset(input, &cursors.peek().ranges);
                                    last_input.set(after.clone());
                                    on_change.call(after);
                                    update_cursor.call(());
                                    return;
                                }
                            }
                            let chord = *command_chord.peek() && control && !modifiers.shift() && !modifiers.alt();
                            let selection_format = chord && key.eq_ignore_ascii_case("f");
                            if !matches!(event.key(), Key::Control | Key::Meta | Key::Shift | Key::Alt) { command_chord.set(false); }
                            if chord && key.eq_ignore_ascii_case("x") {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() && let Some(undo) = trim_trailing_whitespace(input, json) {
                                    command_selections.write().record(undo);
                                    composition_extendable.set(false);
                                    update_cursor.call(());
                                }
                                return;
                            }
                            let document_format = if web_sys::window().and_then(|window| window.navigator().user_agent().ok()).is_some_and(|agent| agent.contains("Linux")) {
                                modifiers.ctrl() && modifiers.shift() && !modifiers.meta() && !modifiers.alt() && key.eq_ignore_ascii_case("i")
                            } else { modifiers.shift() && modifiers.alt() && !control && key.eq_ignore_ascii_case("f") };
                            if json && (document_format || selection_format) {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() && let Some(undo) = format::apply(input, selection_format, rendered.peek().clone()) {
                                    command_selections.write().record(undo);
                                    composition_extendable.set(false);
                                    update_cursor.call(());
                                }
                                return;
                            }
                            if control && !modifiers.shift() && !modifiers.alt() && key.eq_ignore_ascii_case("k") {
                                event.prevent_default();
                                command_chord.set(true);
                                return;
                            }
                            if control && !modifiers.alt() && event.key() == Key::Enter {
                                event.prevent_default();
                                let automatic = auto_indent.write().take();
                                if let Some(input) = textarea.peek().as_ref() && let Some((undo, automatic)) = insert_line(input, modifiers.shift(), json, automatic) {
                                    auto_indent.set(automatic);
                                    command_selections.write().record(undo);
                                    composition_extendable.set(false);
                                    update_cursor.call(());
                                }
                                return;
                            }
                            if control && !modifiers.shift() && !modifiers.alt() && matches!(key.as_str(), "[" | "]") {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() && let Some(undo) = indent_lines(input, key == "[") {
                                    command_selections.write().record(undo);
                                    composition_extendable.set(false);
                                    update_cursor.call(());
                                }
                                return;
                            }
                            if control && modifiers.shift() && !modifiers.alt() && matches!(key.as_str(), "\\" | "|") {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() {
                                    jump_to_bracket(input, json, rendered.peek().as_ref());
                                    update_cursor.call(());
                                }
                                return;
                            }
                            if control && modifiers.alt() && !modifiers.shift() && event.key() == Key::Backspace {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() && let Some(undo) = remove_brackets(input, json) {
                                    command_selections.write().record(undo);
                                    composition_extendable.set(false);
                                    update_cursor.call(());
                                }
                                return;
                            }
                            if key == "Backspace" && !control && !modifiers.alt() && let Some(input) = textarea.peek().as_ref() && backspace_indent(input) {
                                event.prevent_default();
                                composition_extendable.set(false);
                                update_cursor.call(());
                                return;
                            }
                            if json && cursors.peek().ranges.len() > 1 && !control && !modifiers.alt() && matches!(key.as_str(), "{" | "[" | "\"") && let Some(input) = textarea.peek().clone() {
                                let ranges = cursors.peek().ranges.clone();
                                adjusting_history.set(true);
                                let operation = cursors.write().surround_json(&input, &key);
                                adjusting_history.set(false);
                                if let Some(operation) = operation {
                                    event.prevent_default();
                                    finish_multi_edit.call((operation, ranges));
                                }
                                return;
                            }
                            if json && cursors.peek().ranges.len() < 2 && !event.modifiers().ctrl() && !event.modifiers().meta() && !event.modifiers().alt() && let Some(input) = textarea.peek().clone() {
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
                            if json && cursors.peek().ranges.len() < 2 && event.key() == Key::Enter && !event.modifiers().ctrl() && !event.modifiers().meta() && !event.modifiers().alt() && !event.modifiers().shift() && textarea.peek().as_ref().is_some_and(|input| expand_json_brackets(input) || indent_json_newline(input)) {
                                event.prevent_default();
                            } else if event.key() == Key::Home && !event.modifiers().ctrl() && !event.modifiers().meta() && !event.modifiers().alt() {
                                if let Some(input) = textarea.peek().as_ref() && smart_home(input, rendered.peek().as_ref(), event.modifiers().shift()) {
                                    event.prevent_default();
                                    update_cursor.call(());
                                }
                            } else if event.modifiers().ctrl() && !event.modifiers().meta() && !event.modifiers().alt() && key.eq_ignore_ascii_case("m")
                                && event.modifiers().shift() == web_sys::window().and_then(|window| window.navigator().user_agent().ok()).is_some_and(|agent| agent.contains("Macintosh")) {
                                event.prevent_default();
                                let enabled = !*tab_moves_focus.peek();
                                tab_moves_focus.set(enabled);
                                tab_focus_announcement.set(if enabled { "Pressing Tab will now move focus to the next focusable element" } else { "Pressing Tab will now insert the tab character" }.to_owned());
                            } else if event.key() == Key::Tab && !event.modifiers().ctrl() && !event.modifiers().meta() && !event.modifiers().alt() && !*tab_moves_focus.peek() {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() && let Some(undo) = indent_selection(input, event.modifiers().shift()) {
                                    command_selections.write().record(undo);
                                    composition_extendable.set(false);
                                }
                                update_cursor.call(());
                            } else if event.modifiers().alt() && !event.modifiers().ctrl() && !event.modifiers().shift() && key.eq_ignore_ascii_case("p")
                                && event.modifiers().meta() == web_sys::window().and_then(|window| window.navigator().user_agent().ok()).is_some_and(|agent| agent.contains("Macintosh")) {
                                event.prevent_default();
                                toggle_preserve.call(());
                            } else if (event.modifiers().ctrl() || event.modifiers().meta()) && !event.modifiers().shift() && !event.modifiers().alt() && key.eq_ignore_ascii_case("l") {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() { select_line(input); }
                                update_cursor.call(());
                            } else if (event.modifiers().ctrl() || event.modifiers().meta()) && event.modifiers().shift() && !event.modifiers().alt() && key.eq_ignore_ascii_case("k") {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() { delete_lines(input); }
                                update_cursor.call(());
                            } else if event.modifiers().alt() && !event.modifiers().ctrl() && !event.modifiers().meta() && !event.modifiers().shift() && matches!(event.key(), Key::ArrowUp | Key::ArrowDown) {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() { move_lines(input, event.key() == Key::ArrowDown); }
                                update_cursor.call(());
                            } else if event.modifiers().alt() && event.modifiers().shift() && !event.modifiers().meta() && matches!(event.key(), Key::ArrowUp | Key::ArrowDown)
                                && event.modifiers().ctrl() == web_sys::window().and_then(|window| window.navigator().user_agent().ok()).is_some_and(|agent| agent.contains("Linux")) {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() { copy_lines(input, event.key() == Key::ArrowDown); }
                                update_cursor.call(());
                            } else if json && (event.modifiers().ctrl() || event.modifiers().meta()) && !event.modifiers().shift() && !event.modifiers().alt() && key == "/" {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() { toggle_json_line_comments(input); }
                                update_cursor.call(());
                            } else if json && event.modifiers().shift() && !event.modifiers().meta() && key.eq_ignore_ascii_case("a")
                                && if web_sys::window().and_then(|window| window.navigator().user_agent().ok()).is_some_and(|agent| agent.contains("Linux")) { event.modifiers().ctrl() && !event.modifiers().alt() } else { event.modifiers().alt() && !event.modifiers().ctrl() } {
                                event.prevent_default();
                                if let Some(input) = textarea.peek().as_ref() { toggle_json_block_comment(input); }
                                update_cursor.call(());
                            } else if (event.modifiers().ctrl() || event.modifiers().meta()) && (key.eq_ignore_ascii_case("f") || key.eq_ignore_ascii_case("h")) {
                                event.prevent_default();
                                current_find.set(None);
                                if let Some(input) = textarea.peek().as_ref() {
                                    let seed = find_seed(input);
                                    let cursor = input.selection_start().ok().flatten().unwrap_or_default();
                                    let ranges = find_ranges(&input.value(), &seed, *match_case.peek(), *whole_word.peek(), *regex.peek(), *find_scope.peek());
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
                        oncompositionstart: move |_| {
                            if let Some(input) = textarea.peek().as_ref() {
                                let before = input.value();
                                last_input.set(before.clone());
                                let start = input.selection_start().ok().flatten().unwrap_or_default();
                                let end = input.selection_end().ok().flatten().unwrap_or(start);
                                if *composition_extendable.peek() && *composition_cursor.peek() != Some((start, end)) {
                                    composition_extendable.set(false);
                                }
                                composition_start.set(Some(NativeUndo {
                                    before,
                                    after: String::new(),
                                    start,
                                    end,
                                    direction: input.selection_direction().ok().flatten().unwrap_or_default(),
                                    after_selection: None,
                                    edits: Vec::new(),
                                }));
                            }
                        },
                        oncompositionend: move |_| {
                            if let Some(mut composition) = composition_start.take() && let Some(input) = textarea.peek().as_ref() {
                                composition.after = input.value();
                                if composition.before != composition.after {
                                    let edit = CompositionEdit::new(&composition.before, &composition.after);
                                    let previous = composition_undo.peek().clone();
                                    if *composition_extendable.peek() && let Some(mut group) = previous && group.after == composition.before {
                                        group.after = composition.after;
                                        group.edits.push(edit);
                                        composition_undo.set(Some(group));
                                    } else {
                                        composition.edits.push(edit);
                                        composition_undo.set(Some(composition));
                                    }
                                    composition_extendable.set(true);
                                    let start = input.selection_start().ok().flatten().unwrap_or_default();
                                    let end = input.selection_end().ok().flatten().unwrap_or(start);
                                    composition_cursor.set(Some((start, end)));
                                }
                            }
                        },
                        oninput: move |event| {
                            if *adjusting_history.peek() { return; }
                            auto_indent.set(None);
                            let mut value = event.value();
                            let previous = last_input.peek().clone();
                            if previous != value {
                                current_find.set(None);
                                if let Some(input) = textarea.peek().as_ref() { cursor_history.write().reset(input, &cursors.peek().ranges); }
                            }
                            let scope = *find_scope.peek();
                            if let Some(scope) = scope && previous != value {
                                find_scope.set(Some(adjust_find_scope(&previous, &value, scope)));
                            }
                            let input_type = event.data().downcast::<web_sys::Event>()
                                .and_then(|event| event.dyn_ref::<web_sys::InputEvent>())
                                .map(|event| event.input_type()).unwrap_or_default();
                            if input_type != "insertCompositionText" && value != *last_input.peek() { composition_extendable.set(false); }
                            let undo = input_type == "historyUndo";
                            let redo = input_type == "historyRedo";
                            let selection = if undo || redo {
                                command_selections.write().restore(&previous, &value, undo)
                            } else {
                                command_selections.write().clear_redo();
                                None
                            };
                            if let Some(restored) = selection {
                                if let Some(input) = textarea.peek().as_ref() {
                                    let (start, end, direction) = restored.primary;
                                    let _ = input.set_selection_range_with_direction(start, end, &direction);
                                    let mut ranges = vec![multicursor::selection(input)]; ranges.extend(restored.secondary);
                                    cursors.write().restore(ranges);
                                }
                            } else if (undo || redo) && let Some(group) = composition_undo.peek().clone()
                                && let Some(input) = textarea.peek().clone() {
                                let edits: Vec<_> = if undo { group.edits.iter().rev().collect() } else { group.edits.iter().collect() };
                                let initial = if undo { &group.after } else { &group.before };
                                let final_value = if undo { &group.before } else { &group.after };
                                if *last_input.peek() == *initial && (value == *final_value || edits.first().and_then(|edit| edit.apply(initial, undo)).as_ref() == Some(&value)) {
                                    if value != *final_value {
                                        // Additional native history events must run after this handler returns.
                                        spawn(async move {
                                            let first_value = value.clone();
                                            adjusting_history.set(true);
                                            let edits: Vec<_> = if undo { group.edits.iter().rev().collect() } else { group.edits.iter().collect() };
                                            let final_value = if undo { &group.before } else { &group.after };
                                            for edit in edits.iter().skip(1) {
                                                if value == *final_value { break; }
                                                let Some(expected) = edit.apply(&value, undo) else { break };
                                                if !editing_command(if undo { "undo" } else { "redo" }) { break; }
                                                value = input.value();
                                                if value != expected && value != *final_value {
                                                    let _ = editing_command(if undo { "redo" } else { "undo" });
                                                    value = input.value();
                                                    break;
                                                }
                                            }
                                            adjusting_history.set(false);
                                            let scope = *find_scope.peek();
                                            if let Some(scope) = scope && first_value != value {
                                                find_scope.set(Some(adjust_find_scope(&first_value, &value, scope)));
                                            }
                                            if undo && value == group.before {
                                                let _ = input.set_selection_range_with_direction(group.start, group.end, &group.direction);
                                            } else if redo && value == group.after && let Some((start, end, direction)) = &group.after_selection {
                                                let _ = input.set_selection_range_with_direction(*start, *end, direction);
                                            }
                                            last_input.set(value.clone());
                                            on_change.call(value);
                                            update_cursor.call(());
                                        });
                                        return;
                                    } else if undo {
                                        let _ = input.set_selection_range_with_direction(group.start, group.end, &group.direction);
                                    } else if let Some((start, end, direction)) = &group.after_selection {
                                        let _ = input.set_selection_range_with_direction(*start, *end, direction);
                                    }
                                }
                            }
                            last_input.set(value.clone());
                            if !undo && !redo && !cursors.peek().ranges.is_empty() { cursors.set(multicursor::Cursors::default()); }
                            on_change.call(value);
                            update_cursor.call(());
                        },
                        onselect: move |_| update_cursor.call(()),
                        onmouseup: move |_| update_cursor.call(()),
                        onkeyup: move |_| update_cursor.call(()),
                        onscroll: move |_| { if let Some(input) = textarea.peek().as_ref() { cursor_history.write().observe(input, &cursors.peek().ranges); scroll_top.set(input.scroll_top()); } }
                    }
                }
                canvas { aria_hidden: "true", style: "position:absolute;transform:translate3d(0,0,0);contain:strict;top:0;right:0;width:14px;height:400px;pointer-events:none;", onmounted: move |event| { if let Some(element) = event.data().downcast::<web_sys::Element>() && let Ok(canvas) = element.clone().dyn_into::<web_sys::HtmlCanvasElement>() { ruler.set(Some(canvas)); } } }
                if !tab_focus_announcement().is_empty() { div { role: "alert", aria_atomic: "true", style: "position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0;", "{tab_focus_announcement}" } }
                if find_open() { div { role: "dialog", aria_label: "Find / Replace", class: "translator-find {find_layout} {replacing}",
                    button { r#type: "button", class: "find-icon find-toggle", aria_label: "Toggle Replace", aria_expanded: "{replace_open}", onclick: move |_| { let next = !*replace_open.peek(); replace_open.set(next); }, if replace_open() { "" } else { "" } }
                    div { class: "find-row",
                    div { class: "find-box",
                    textarea { aria_label: "Find", placeholder: "Find", value: "{query}", spellcheck: "false", class: "find-input",
                        onmounted: move |event| async move { let _ = event.set_focus(true).await; if let Some(element) = event.data().downcast::<web_sys::Element>() && let Ok(input) = element.clone().dyn_into::<web_sys::HtmlTextAreaElement>() { input.select(); } },
                        oninput: move |event| { query.set(event.value()); reset_match.call(()); },
                        onkeydown: move |event| { if event.key() == Key::Enter { event.prevent_default(); select_match.call(!event.modifiers().shift()); } else if event.key() == Key::Escape { event.prevent_default(); close_find.call(()); } else if event.modifiers().alt() { let option = match event.key().to_string().to_lowercase().as_str() { "c" => Some(0), "w" => Some(1), "r" => Some(2), _ => None }; if let Some(option) = option { event.prevent_default(); toggle_option.call(option); } else if event.key().to_string().eq_ignore_ascii_case("l") { event.prevent_default(); toggle_scope.call(()); } else if event.key().to_string().eq_ignore_ascii_case("p") && !event.modifiers().ctrl() && !event.modifiers().shift() && event.modifiers().meta() == web_sys::window().and_then(|window| window.navigator().user_agent().ok()).is_some_and(|agent| agent.contains("Macintosh")) { event.prevent_default(); toggle_preserve.call(()); } } }
                    }
                    div { class: "find-options",
                        button { r#type: "button", class: "find-icon find-option", role: "checkbox", aria_label: "Match Case (Alt+C)", aria_checked: "{match_case}", onclick: move |_| toggle_option.call(0), "" }
                        button { r#type: "button", class: "find-icon find-option", role: "checkbox", aria_label: "Match Whole Word (Alt+W)", aria_checked: "{whole_word}", onclick: move |_| toggle_option.call(1), "" }
                        button { r#type: "button", class: "find-icon find-option", role: "checkbox", aria_label: "Use Regular Expression (Alt+R)", aria_checked: "{regex}", onclick: move |_| toggle_option.call(2), "" }
                    }
                    }
                    span { class: "find-count", "{count}" }
                    button { r#type: "button", class: "find-icon", aria_label: "Previous Match (Shift+Enter)", onclick: move |_| select_match.call(false), "" }
                    button { r#type: "button", class: "find-icon", aria_label: "Next Match (Enter)", onclick: move |_| select_match.call(true), "" }
                    button { r#type: "button", class: "find-icon find-scope", role: "checkbox", aria_label: "Find in Selection (Alt+L)", aria_checked: "{find_scope().is_some()}", onclick: move |_| toggle_scope.call(()), "" }
                    }
                    button { r#type: "button", class: "find-icon find-close", aria_label: "Close (Escape)", onclick: move |_| close_find.call(()), "" }
                    if replace_open() { div { class: "find-row",
                        div { class: "find-box replace-box", textarea { aria_label: "Replace", placeholder: "Replace", value: "{replacement}", spellcheck: "false", class: "find-input", oninput: move |event| replacement.set(event.value()), onkeydown: move |event| { if event.key() == Key::Enter { event.prevent_default(); replace_matches.call(false); } else if event.key() == Key::Escape { event.prevent_default(); close_find.call(()); } else if event.modifiers().alt() && event.key().to_string().eq_ignore_ascii_case("l") { event.prevent_default(); toggle_scope.call(()); } else if event.modifiers().alt() && event.key().to_string().eq_ignore_ascii_case("p") && !event.modifiers().ctrl() && !event.modifiers().shift() && event.modifiers().meta() == web_sys::window().and_then(|window| window.navigator().user_agent().ok()).is_some_and(|agent| agent.contains("Macintosh")) { event.prevent_default(); toggle_preserve.call(()); } } }
                            div { class: "find-options", button { r#type: "button", class: "find-icon find-option", role: "checkbox", aria_label: "Preserve Case (Alt+P)", title: "Preserve Case (Alt+P)", aria_checked: "{preserve_case}", onclick: move |_| toggle_preserve.call(()), "" } }
                        }
                        button { r#type: "button", class: "find-icon", aria_label: "Replace (Ctrl+Shift+1)", onclick: move |_| replace_matches.call(false), "" }
                        button { r#type: "button", class: "find-icon", aria_label: "Replace All (Ctrl+Alt+Enter)", onclick: move |_| replace_matches.call(true), "" }
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
    let tab_moves_focus = use_signal(|| false);
    let preserve_case_setting = use_signal(|| false);
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
                                div { class: "border border-hairline rounded-mini-lg overflow-hidden", section { style: "display:flex;position:relative;text-align:initial;width:100%;height:400px;", div { style: "width:100%;", Editor { value: content.clone(), json: step < 4, tab_moves_focus, preserve_case_setting, on_change: move |value: String| { let changed = contents.peek()[step] != value; if changed { contents.write()[step] = value.clone(); if step == 0 { detect_meta(&value, meta); } } } } } } }
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
