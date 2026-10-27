use super::{insert_text, NativeUndo};
use serde_json::{json, Value};
use web_sys::{ClipboardEvent, HtmlTextAreaElement};

pub(super) fn copy_line(
    input: &HtmlTextAreaElement,
    event: &ClipboardEvent,
    cut: bool,
) -> Option<NativeUndo> {
    if event.default_prevented() {
        return None;
    }
    let start = input.selection_start().ok().flatten()?;
    let end = input.selection_end().ok().flatten()?;
    if start != end {
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
    let copied = format!(
        "{}\n",
        String::from_utf16_lossy(&units[line_start..line_end])
    );
    let data = event.clipboard_data()?;
    data.set_data("text/plain", &copied).ok()?;
    // Monaco's existing clipboard format preserves whole-line paste between
    // editors without a separate in-memory clipboard or permission request.
    data.set_data(
        "vscode-editor-data",
        &json!({"version":1,"isFromEmptySelection":true,"multicursorText":null,"mode":null})
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
    let (remove_start, remove_end) = if line_end < units.len() {
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
