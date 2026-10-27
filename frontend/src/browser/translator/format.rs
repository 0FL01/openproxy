//! Whitespace edits for the editor's default JSON formatting commands.
//!
//! Adapted from Microsoft's jsonc-parser formatter (MIT), distributed with
//! Monaco 0.55.1. Copyright (c) Microsoft Corporation. Permission and warranty
//! notice: frontend/public/assets/translator-codicon-LICENSE.txt.

use super::{cursor_top, insert_text, json_tokens, CompositionEdit, NativeUndo};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    OpenObject,
    CloseObject,
    OpenArray,
    CloseArray,
    Comma,
    Colon,
    String,
    Literal,
    LineComment,
    BlockComment,
    Unknown,
    End,
}

struct Token {
    kind: Kind,
    start: usize,
    end: usize,
    line_break: bool,
    error: bool,
}

struct Edit {
    start: usize,
    end: usize,
    text: String,
}

fn tokens(text: &str, base: usize) -> Vec<Token> {
    let mut result = Vec::new();
    let mut offset = base;
    let mut line_break = false;
    for (part, _) in json_tokens(text) {
        let start = offset;
        offset += part.len();
        if part
            .bytes()
            .all(|ch| matches!(ch, b' ' | b'\t' | b'\r' | b'\n'))
        {
            line_break |= part.contains(['\r', '\n']);
            continue;
        }
        let kind = match part {
            "{" => Kind::OpenObject,
            "}" => Kind::CloseObject,
            "[" => Kind::OpenArray,
            "]" => Kind::CloseArray,
            "," => Kind::Comma,
            ":" => Kind::Colon,
            "true" | "false" | "null" => Kind::Literal,
            part if part.starts_with("//") => Kind::LineComment,
            part if part.starts_with("/*") => Kind::BlockComment,
            part if part.starts_with('"') => Kind::String,
            part if matches!(part.as_bytes()[0], b'-' | b'0'..=b'9') => Kind::Literal,
            _ => Kind::Unknown,
        };
        let error = match kind {
            Kind::Unknown => true,
            Kind::String => !valid_string(part),
            Kind::BlockComment => !part.ends_with("*/"),
            Kind::Literal if part != "true" && part != "false" && part != "null" => {
                part.parse::<f64>().is_err()
            }
            _ => false,
        };
        result.push(Token {
            kind,
            start,
            end: offset,
            line_break,
            error,
        });
        line_break = false;
    }
    result.push(Token {
        kind: Kind::End,
        start: offset,
        end: offset,
        line_break,
        error: false,
    });
    result
}

fn valid_string(text: &str) -> bool {
    if text.len() < 2 || !text.ends_with('"') {
        return false;
    }
    let mut bytes = text.as_bytes()[1..text.len() - 1].iter().copied();
    while let Some(ch) = bytes.next() {
        if ch < 0x20 {
            return false;
        }
        if ch == b'\\' {
            match bytes.next() {
                Some(b'u') => {
                    for _ in 0..4 {
                        if !bytes.next().is_some_and(|ch| ch.is_ascii_hexdigit()) {
                            return false;
                        }
                    }
                }
                Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {}
                _ => return false,
            }
        }
    }
    true
}

fn edits(text: &str, range: Option<(usize, usize)>) -> Vec<Edit> {
    let (start, end, initial_indent) = if let Some((start, end)) = range {
        let line_start = text[..start]
            .rfind(['\r', '\n'])
            .map_or(0, |index| index + 1);
        let line_end = text[end..]
            .find(['\r', '\n'])
            .map_or(text.len(), |index| end + index);
        let columns: usize = text[line_start..]
            .bytes()
            .take_while(|ch| matches!(ch, b' ' | b'\t'))
            .map(|ch| if ch == b'\t' { 4 } else { 1 })
            .sum();
        (line_start, line_end, columns / 4)
    } else {
        (0, text.len(), 0)
    };
    let eol = text
        .find(['\r', '\n'])
        .map(|index| {
            if text[index..].starts_with("\r\n") {
                "\r\n"
            } else {
                &text[index..index + 1]
            }
        })
        .unwrap_or("\n");
    let newline = |indent: i32| {
        format!(
            "{eol}{}",
            "    ".repeat((initial_indent as i32 + indent).max(0) as usize)
        )
    };
    let tokens = tokens(&text[start..end], start);
    let mut edits = Vec::new();
    let mut add = |start, end, content: String, error: bool| {
        if !error && range.is_none_or(|(a, b)| start < b && end > a) && text[start..end] != content
        {
            edits.push(Edit {
                start,
                end,
                text: content,
            });
        }
    };
    let mut index = 0;
    let mut indent = 0_i32;
    if tokens[0].kind != Kind::End {
        add(
            start,
            tokens[0].start,
            "    ".repeat(initial_indent),
            tokens[0].error,
        );
    }
    while tokens[index].kind != Kind::End {
        let first = &tokens[index];
        let mut first_end = first.end;
        index += 1;
        let mut second = &tokens[index];
        let mut content = String::new();
        let mut needs_break = false;
        while !second.line_break && matches!(second.kind, Kind::LineComment | Kind::BlockComment) {
            add(first_end, second.start, " ".into(), second.error);
            first_end = second.end;
            needs_break = second.kind == Kind::LineComment;
            content = if needs_break {
                newline(indent)
            } else {
                String::new()
            };
            index += 1;
            second = &tokens[index];
        }
        let mut error = second.error;
        match second.kind {
            Kind::CloseObject | Kind::CloseArray => {
                let opening = if second.kind == Kind::CloseObject {
                    Kind::OpenObject
                } else {
                    Kind::OpenArray
                };
                if first.kind != opening {
                    indent -= 1;
                    content = newline(indent);
                }
            }
            _ => {
                match first.kind {
                    Kind::OpenObject | Kind::OpenArray => {
                        indent += 1;
                        content = newline(indent);
                    }
                    Kind::Comma | Kind::LineComment => content = newline(indent),
                    Kind::BlockComment if second.line_break => content = newline(indent),
                    Kind::BlockComment | Kind::Colon if !needs_break => content = " ".into(),
                    Kind::String if second.kind == Kind::Colon && !needs_break => content.clear(),
                    Kind::Literal | Kind::CloseObject | Kind::CloseArray => {
                        if !matches!(
                            second.kind,
                            Kind::Comma | Kind::End | Kind::LineComment | Kind::BlockComment
                        ) {
                            error = true;
                        }
                    }
                    Kind::Unknown => error = true,
                    _ => {}
                }
                if second.line_break
                    && matches!(second.kind, Kind::LineComment | Kind::BlockComment)
                {
                    content = newline(indent);
                }
            }
        }
        if second.kind == Kind::End {
            content.clear();
        }
        add(first_end, second.start, content, error);
    }
    edits
}

pub(super) fn apply(
    input: &web_sys::HtmlTextAreaElement,
    selection: bool,
    rendered: Option<web_sys::HtmlElement>,
) -> Option<NativeUndo> {
    let before = input.value();
    let start = input.selection_start().ok().flatten()?;
    let end = input.selection_end().ok().flatten()?;
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    let byte_offset = |position: u32| {
        let mut offset = 0;
        before
            .char_indices()
            .find_map(|(index, ch)| {
                let found = (offset >= position).then_some(index);
                offset += ch.len_utf16() as u32;
                found
            })
            .unwrap_or(before.len())
    };
    let range = selection.then(|| {
        let a = byte_offset(start);
        let b = byte_offset(end);
        if start == end {
            (
                before[..a].rfind(['\r', '\n']).map_or(0, |index| index + 1),
                before[b..]
                    .find(['\r', '\n'])
                    .map_or(before.len(), |index| b + index),
            )
        } else {
            (a, b)
        }
    });
    let edits = edits(&before, range);
    if edits.is_empty() {
        return None;
    }
    let map = |position: u32| {
        let mut shift = 0_i64;
        for edit in &edits {
            let a = before[..edit.start].encode_utf16().count() as u32;
            let b = a + before[edit.start..edit.end].encode_utf16().count() as u32;
            let inserted = edit.text.encode_utf16().count() as i64;
            if position < a {
                break;
            }
            if position <= b {
                return (a as i64 + shift + inserted) as u32;
            }
            shift += inserted - (b - a) as i64;
        }
        (position as i64 + shift) as u32
    };
    let after_start = map(start);
    let after_end = map(end);
    let mut after = before.clone();
    for edit in edits.iter().rev() {
        after.replace_range(edit.start..edit.end, &edit.text);
    }
    let scroll = input.scroll_top();
    let top = rendered
        .as_ref()
        .and_then(|element| cursor_top(input, element));
    if !insert_text(input, 0, before.encode_utf16().count() as u32, &after) {
        return None;
    }
    let _ = input.set_selection_range_with_direction(after_start, after_end, &direction);
    input.set_scroll_top(scroll);
    if let Some((rendered, top)) = rendered.zip(top) {
        let input = input.clone();
        let expected = after.clone();
        dioxus::prelude::spawn(async move {
            // Wait for the syntax overlay and wrapped-line layout to catch up.
            for _ in 0..2 {
                let frame = js_sys::Promise::new(&mut |resolve, _| {
                    if let Some(window) = web_sys::window() {
                        let _ = window.request_animation_frame(&resolve);
                    }
                });
                let _ = wasm_bindgen_futures::JsFuture::from(frame).await;
            }
            // A later edit, navigation or scroll owns the viewport instead.
            if input.is_connected()
                && input.value() == expected
                && input.selection_start().ok().flatten() == Some(after_start)
                && input.selection_end().ok().flatten() == Some(after_end)
                && input.scroll_top() == scroll
                && let Some(current) = cursor_top(&input, &rendered)
            {
                input.set_scroll_top((scroll as f64 + current - top).round() as i32);
            }
        });
    }
    let edit = CompositionEdit::new(&before, &after);
    Some(NativeUndo {
        before,
        after,
        start,
        end,
        direction: direction.clone(),
        after_selection: Some((after_start, after_end, direction)),
        edits: vec![edit],
    })
}
