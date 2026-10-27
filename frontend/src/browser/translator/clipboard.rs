use super::{insert_text, json_tokens, NativeUndo};
use serde_json::{json, Value};
use web_sys::{ClipboardEvent, HtmlTextAreaElement};

// HTML clipboard layout/escaping follows Monaco (Microsoft, MIT). The complete
// notice is shipped in public/assets/translator-codicon-LICENSE.txt.
fn html_part(text: &str, range: (usize, usize), column: &mut usize) -> String {
    let units: Vec<_> = text.encode_utf16().collect();
    let mut result = String::new();
    let mut ordinary = Vec::new();
    for (index, ch) in units.into_iter().enumerate() {
        let width = if ch == 9 {
            4 - *column % 4
        } else if matches!(ch, 0x2e80..=0xd7af | 0xf900..=0xfaff | 0xff01..=0xff5e) {
            2
        } else {
            1
        };
        *column += width;
        if index < range.0 || index >= range.1 {
            continue;
        }
        let escaped = match ch {
            0 => Some("&#00;"),
            9 => Some(""),
            13 => Some("&#8203;"),
            38 => Some("&amp;"),
            60 => Some("&lt;"),
            62 => Some("&gt;"),
            133 | 65279 | 8232 | 8233 => Some("\u{fffd}"),
            _ => None,
        };
        if let Some(escaped) = escaped {
            result.push_str(&String::from_utf16_lossy(&ordinary));
            ordinary.clear();
            if ch == 9 {
                result.extend(std::iter::repeat_n(' ', width));
            } else {
                result.push_str(escaped);
            }
        } else {
            ordinary.push(ch);
        }
    }
    result.push_str(&String::from_utf16_lossy(&ordinary));
    result
}

fn json_html(text: &str, start: usize, end: usize) -> String {
    let mut lines = vec![Vec::new()];
    for (token, color) in json_tokens(text) {
        for (index, part) in token.split('\n').enumerate() {
            if index > 0 {
                lines.push(Vec::new());
            }
            lines.last_mut().unwrap().push((part, color));
        }
    }
    let mut result = String::from("<div style=\"color: #d4d4d4;background-color: #1e1e1e;font-family: 'Droid Sans Mono', 'monospace', monospace;font-weight: normal;font-size: 12px;line-height: 16px;white-space: pre;\">");
    let mut line_start = 0;
    for line in lines {
        let length: usize = line
            .iter()
            .map(|(part, _)| part.encode_utf16().count())
            .sum();
        let line_end = line_start + length;
        if line_start > end {
            break;
        }
        if line_end >= start {
            if length == 0 {
                result.push_str("<br>");
            } else {
                result.push_str("<div>");
                let mut offset = line_start;
                let mut column = 0;
                for (part, color) in line {
                    let length = part.encode_utf16().count();
                    let content = html_part(
                        part,
                        (
                            start.saturating_sub(offset),
                            end.saturating_sub(offset).min(length),
                        ),
                        &mut column,
                    );
                    if offset + length > start && offset <= end {
                        // Rainbow brackets are view decorations, not clipboard tokens.
                        let color = if matches!(part, "{" | "}" | "[" | "]") {
                            "#dcdcdc"
                        } else {
                            color
                        };
                        result
                            .push_str(&format!("<span style=\"color: {color};\">{content}</span>"));
                    }
                    offset += length;
                    if offset >= end {
                        break;
                    }
                }
                result.push_str("</div>");
            }
        }
        line_start = line_end + 1;
    }
    result.push_str("</div>");
    result
}

pub(super) fn copy(
    input: &HtmlTextAreaElement,
    event: &ClipboardEvent,
    cut: bool,
    json: bool,
) -> Option<NativeUndo> {
    if event.default_prevented() {
        return None;
    }
    let start = input.selection_start().ok().flatten()?;
    let end = input.selection_end().ok().flatten()?;
    if start != end && !json {
        return None;
    }
    let before = input.value();
    let units: Vec<_> = before.encode_utf16().collect();
    let line_start = units[..start as usize]
        .iter()
        .rposition(|ch| *ch == 10)
        .map_or(0, |index| index + 1);
    let line_end = units[start as usize..]
        .iter()
        .position(|ch| *ch == 10)
        .map_or(units.len(), |index| start as usize + index);
    let whole_line = start == end;
    let (copy_start, copy_end) = if whole_line {
        (line_start, line_end)
    } else {
        (start as usize, end as usize)
    };
    let mut copied = String::from_utf16_lossy(&units[copy_start..copy_end]);
    if whole_line {
        copied.push('\n');
    }
    let data = event.clipboard_data()?;
    data.set_data("text/plain", &copied).ok()?;
    if json {
        data.set_data("text/html", &json_html(&before, copy_start, copy_end))
            .ok()?;
    }
    // Monaco's existing clipboard format preserves whole-line paste between
    // editors without a separate in-memory clipboard or permission request.
    data.set_data(
        "vscode-editor-data",
        &json!({"version":1,"isFromEmptySelection":whole_line,"multicursorText":null,"mode":if json { Some("json") } else { None }})
            .to_string(),
    )
    .ok()?;
    event.prevent_default();
    if !cut {
        return None;
    }
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    let (remove_start, remove_end) = if !whole_line {
        (copy_start, copy_end)
    } else if line_end < units.len() {
        (line_start, line_end + 1)
    } else {
        (line_start.saturating_sub(1), line_end)
    };
    if remove_start == remove_end || !insert_text(input, remove_start as u32, remove_end as u32, "")
    {
        return None;
    }
    Some(NativeUndo {
        before,
        after: input.value(),
        start,
        end,
        direction: direction.clone(),
        after_selection: Some((remove_start as u32, remove_start as u32, direction)),
        edits: Vec::new(),
    })
}

pub(super) fn paste_line(
    input: &HtmlTextAreaElement,
    event: &ClipboardEvent,
) -> Option<NativeUndo> {
    if event.default_prevented() {
        return None;
    }
    let start = input.selection_start().ok().flatten()?;
    let end = input.selection_end().ok().flatten()?;
    if start != end {
        return None;
    }
    let data = event.clipboard_data()?;
    let metadata: Value = serde_json::from_str(&data.get_data("vscode-editor-data").ok()?).ok()?;
    if metadata["version"] != 1 || metadata["isFromEmptySelection"] != true {
        return None;
    }
    let text = data.get_data("text/plain").ok()?;
    if text.is_empty() || text.find('\n') != text.len().checked_sub(1) {
        return None;
    }
    let before = input.value();
    let units: Vec<_> = before.encode_utf16().collect();
    let line_start = units[..start as usize]
        .iter()
        .rposition(|ch| *ch == 10)
        .map_or(0, |index| index + 1) as u32;
    let direction = input
        .selection_direction()
        .ok()
        .flatten()
        .unwrap_or_default();
    event.prevent_default();
    if !insert_text(input, line_start, line_start, &text) {
        return None;
    }
    let after = input.value();
    // Native textarea insertion normalizes clipboard line endings.
    let cursor = start + (after.encode_utf16().count() - units.len()) as u32;
    let _ = input.set_selection_range_with_direction(cursor, cursor, &direction);
    Some(NativeUndo {
        before,
        after,
        start,
        end,
        direction: direction.clone(),
        after_selection: Some((cursor, cursor, direction)),
        edits: Vec::new(),
    })
}
