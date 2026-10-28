//! Occurrence selections layered over the existing native text input/history.
//! Rules follow Monaco 0.55.1 (Copyright Microsoft Corporation, MIT).
//! Permission/warranty notice: public/assets/translator-codicon-LICENSE.txt.
use super::{
    find_ranges, in_json_string, insert_text, json_bracket_newline, line_indentation, NativeUndo,
};
use dioxus::prelude::Callback;
use std::{collections::HashSet, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast};

pub(super) type Selection = (u32, u32, String);

pub(super) fn selection(input: &web_sys::HtmlTextAreaElement) -> Selection {
    let start = input.selection_start().ok().flatten().unwrap_or_default();
    (
        start,
        input.selection_end().ok().flatten().unwrap_or(start),
        input
            .selection_direction()
            .ok()
            .flatten()
            .unwrap_or_default(),
    )
}

fn normalize(ranges: &mut Vec<Selection>) {
    let last_added = ranges.len().saturating_sub(1);
    let mut ordered: Vec<_> = std::mem::take(ranges).into_iter().enumerate().collect();
    ordered.sort_by_key(|(_, range)| (range.0, range.1));
    let mut merged: Vec<(usize, Selection, bool)> = Vec::new();
    for (index, range) in ordered {
        if let Some((owner, previous, contains_last)) = merged.last_mut()
            && (range.0 < previous.1
                || range.0 == previous.1 && (range.0 == range.1 || previous.0 == previous.1))
        {
            // Keep the original primary/order; the last-added selection wins
            // direction when overlapping, as in Monaco's cursor collection.
            if index == last_added || !*contains_last && index < *owner {
                previous.2 = range.2;
            }
            previous.0 = previous.0.min(range.0);
            previous.1 = previous.1.max(range.1);
            *owner = (*owner).min(index);
            *contains_last |= index == last_added;
        } else {
            merged.push((index, range, index == last_added));
        }
    }
    merged.sort_by_key(|(owner, _, _)| *owner);
    *ranges = merged.into_iter().map(|(_, range, _)| range).collect();
}

#[derive(Clone, Default)]
pub(super) struct Cursors {
    pub ranges: Vec<Selection>,
    pub typing: bool,
    pub starts_typing: bool,
    query: Option<(String, bool, bool)>,
    auto_closed: Vec<Vec<ClosedPair>>,
}

#[derive(Clone)]
struct ClosedPair {
    open: u32,
    close: u32,
    character: char,
}

impl Cursors {
    pub fn auto_pairs(&self) -> Vec<(char, u32)> {
        self.auto_closed
            .iter()
            .flatten()
            .map(|pair| (pair.character, pair.close))
            .collect()
    }

    fn validate_pairs(&mut self, units: &[u16]) {
        let mut ranges = self.ranges.clone();
        ranges.sort_by_key(|range| (range.0, range.1));
        self.auto_closed.retain(|group| {
            ranges.len() <= group.len()
                && group.iter().all(|pair| {
                    pair.open < pair.close
                        && !units[pair.open as usize..=pair.close as usize].contains(&10)
                })
                && ranges
                    .iter()
                    .zip(group)
                    .all(|(range, pair)| pair.open < range.0 && range.1 <= pair.close)
        });
    }

    pub fn restore(&mut self, mut ranges: Vec<Selection>) {
        normalize(&mut ranges);
        *self = Self {
            ranges,
            ..Self::default()
        };
    }

    pub fn line_ends(&mut self, input: &web_sys::HtmlTextAreaElement) -> bool {
        let primary = selection(input);
        let ranges = if self.ranges.first() == Some(&primary) {
            self.ranges.clone()
        } else {
            vec![primary]
        };
        let units: Vec<_> = input.value().encode_utf16().collect();
        let mut ends = Vec::new();
        'selections: for (start, end, _) in ranges {
            if start == end {
                continue;
            }
            let mut offset = start as usize;
            while let Some(next) = units[offset..end as usize].iter().position(|ch| *ch == 10) {
                offset += next;
                ends.push((offset as u32, offset as u32, "forward".into()));
                if ends.len() == 10_000 {
                    break 'selections;
                }
                offset += 1;
            }
            // The final line uses the selection endpoint, not its full length;
            // column one on a following line is excluded.
            if offset < end as usize {
                ends.push((end, end, "forward".into()));
                if ends.len() == 10_000 {
                    break;
                }
            }
        }
        if !ends.is_empty() {
            normalize(&mut ends);
            self.ranges = ends;
            self.query = None;
            self.typing = false;
            self.starts_typing = false;
            self.validate_pairs(&units);
            let primary = &self.ranges[0];
            let _ = input.set_selection_range(primary.0, primary.1);
            true
        } else {
            false
        }
    }

    pub fn choose(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        json: bool,
        case: bool,
        whole: bool,
        all: bool,
    ) {
        let primary = selection(input);
        if self.ranges.first() != Some(&primary) {
            self.ranges.clear();
            self.query = None;
        }
        let text = input.value();
        let string = js_sys::JsString::from(text.as_str());
        if self.query.is_none() {
            let (start, end) = if primary.0 == primary.1 {
                let before = string.slice(0, primary.0).as_string().unwrap_or_default();
                let line_start = before
                    .rfind('\n')
                    .map_or(0, |byte| before[..=byte].encode_utf16().count() as u32);
                let line = js_sys::JsString::from(text.as_str())
                    .slice(line_start, string.length())
                    .as_string()
                    .unwrap_or_default();
                let line = line.split('\n').next().unwrap_or_default();
                let pattern = if json {
                    r#"(-?\d*\.\d\w*)|([^\[\{\]\}\:\",\s]+)"#
                } else {
                    r#"(-?\d*\.\d\w*)|([^`~!@#$%^&*()\-=+\[\{\]\}\\|;:'\",.<>/?\s]+)"#
                };
                let words = js_sys::RegExp::new(pattern, "g");
                let mut word = None;
                while let Some(found) = words.exec(line) {
                    let end = words.last_index() + line_start;
                    let start = end - found.get(0).length();
                    if start <= primary.0 && primary.0 <= end {
                        word = Some((start, end));
                        break;
                    }
                }
                let Some(word) = word else { return };
                word
            } else {
                (primary.0, primary.1)
            };
            let term = string.slice(start, end).as_string().unwrap_or_default();
            if term.is_empty() {
                return;
            }
            self.query = Some((
                term,
                case || primary.0 == primary.1,
                whole || primary.0 == primary.1,
            ));
            if self.ranges.len() < 2 || primary.0 == primary.1 {
                self.ranges = vec![(start, end, primary.2.clone())];
            }
            if !all && primary.0 == primary.1 {
                let _ = input.set_selection_range(start, end);
                self.ranges[0] = selection(input);
                self.typing = false;
                return;
            }
        }
        let (term, case, whole) = self.query.as_ref().unwrap();
        let matches = find_ranges(&text, term, *case, *whole, false, None);
        if all {
            if !matches.is_empty() {
                let primary = self.ranges[0].clone();
                // Monaco's default editor.multiCursorLimit is 10,000.
                self.ranges = matches
                    .into_iter()
                    .take(10_000)
                    .map(|(start, end)| (start, end, "forward".into()))
                    .collect();
                if let Some(index) = self
                    .ranges
                    .iter()
                    .position(|r| (r.0, r.1) == (primary.0, primary.1))
                {
                    self.ranges.remove(index);
                    self.ranges.insert(0, primary);
                }
            }
        } else if self.ranges.len() < 10_000 {
            let end = self.ranges.last().unwrap().1;
            let next = matches
                .iter()
                .find(|(start, _)| *start >= end)
                .or_else(|| matches.first());
            if let Some(&(start, end)) = next
                && !self.ranges.iter().any(|r| (r.0, r.1) == (start, end))
            {
                self.ranges.push((start, end, "forward".into()));
            }
        }
        normalize(&mut self.ranges);
        let primary = &self.ranges[0];
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        self.typing = false;
    }

    pub fn apply(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        kind: &str,
        data: &str,
    ) -> Option<NativeUndo> {
        if self.ranges.len() < 2 {
            return None;
        }
        let before = input.value();
        let original = selection(input);
        if self.ranges.first() != Some(&original) {
            self.ranges.clear();
            return None;
        }
        let units: Vec<u16> = before.encode_utf16().collect();
        let inserted = match kind {
            "insertText" => data,
            "insertLineBreak" | "insertParagraph" => "\n",
            "deleteContentBackward" | "deleteContentForward" => "",
            _ => return None,
        };
        let mut replacements = Vec::with_capacity(self.ranges.len());
        for &(start, end, _) in &self.ranges {
            let (mut start, mut end) = (start as usize, end as usize);
            if end > units.len() {
                return None;
            }
            if start == end && kind == "deleteContentBackward" && start > 0 {
                start -= 1;
                if start > 0
                    && (0xdc00..=0xdfff).contains(&units[start])
                    && (0xd800..=0xdbff).contains(&units[start - 1])
                {
                    start -= 1;
                }
            } else if start == end && kind == "deleteContentForward" && end < units.len() {
                end += 1;
                if end < units.len()
                    && (0xd800..=0xdbff).contains(&units[end - 1])
                    && (0xdc00..=0xdfff).contains(&units[end])
                {
                    end += 1;
                }
            }
            replacements.push((start as u32, end as u32, inserted.to_owned()));
        }
        let operation = self.replace(input, replacements, false)?;
        self.typing = kind == "insertText" && !data.contains(char::is_whitespace);
        Some(operation)
    }

    pub fn apply_json(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        kind: &str,
        data: &str,
    ) -> Option<NativeUndo> {
        let text = input.value();
        let units: Vec<_> = text.encode_utf16().collect();
        let collapsed = self.ranges.iter().all(|range| range.0 == range.1);
        if collapsed && (kind == "insertText" || kind == "deleteContentBackward") {
            self.validate_pairs(&units);
            let closing = data.chars().next().filter(|_| data.len() == 1);
            let marked_positions: HashSet<_> = self
                .auto_closed
                .iter()
                .flatten()
                .map(|pair| (pair.close, pair.character))
                .collect();
            let marked = self.ranges.iter().all(|range| {
                let character = if kind == "deleteContentBackward" {
                    units
                        .get(range.0 as usize)
                        .and_then(|ch| char::from_u32(*ch as u32))
                } else {
                    closing
                };
                character.is_some_and(|ch| marked_positions.contains(&(range.0, ch)))
            });
            let overtype = kind == "insertText"
                && (closing != Some('"')
                    || self
                        .ranges
                        .iter()
                        .all(|range| range.0 == 0 || units[range.0 as usize - 1] != b'\\' as u16));
            let delete_pair = kind == "deleteContentBackward"
                && self.ranges.iter().all(|range| {
                    range.0 > 0
                        && units
                            .get(range.0 as usize - 1..=range.0 as usize)
                            .is_some_and(|pair| matches!(pair, [123, 125] | [91, 93] | [34, 34]))
                });
            if marked && (overtype || delete_pair) {
                let deleting = kind == "deleteContentBackward";
                let replacements = self
                    .ranges
                    .iter()
                    .map(|range| {
                        (
                            range.0 - u32::from(deleting),
                            range.1 + 1,
                            if deleting {
                                String::new()
                            } else {
                                data.to_owned()
                            },
                        )
                    })
                    .collect();
                let operation = self.replace(input, replacements, false)?;
                self.typing = !deleting;
                return Some(operation);
            }
            if kind == "insertText"
                && let Some(close) = match data {
                    "{" => Some('}'),
                    "[" => Some(']'),
                    "\"" => Some('"'),
                    _ => None,
                }
            {
                let string = js_sys::JsString::from(text.as_str());
                let allowed = self.ranges.iter().all(|range| {
                    let before = string.slice(0, range.0).as_string().unwrap_or_default();
                    let after = units.get(range.0 as usize).copied();
                    !in_json_string(&before)
                        && after.is_none_or(|ch| {
                            ";:.,=}])> \n\t\r"
                                .encode_utf16()
                                .any(|allowed| allowed == ch)
                        })
                        && (close != '"'
                            || before.chars().last().is_none_or(|ch| {
                                "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/? \t\r\n".contains(ch)
                            }))
                });
                if allowed {
                    let replacements = self
                        .ranges
                        .iter()
                        .map(|range| (range.0, range.1, format!("{data}{close}")))
                        .collect();
                    let mut operation = self.replace(input, replacements, false)?;
                    for range in &mut self.ranges {
                        range.0 -= 1;
                        range.1 = range.0;
                    }
                    let mut group: Vec<_> = self
                        .ranges
                        .iter()
                        .map(|range| ClosedPair {
                            open: range.0 - 1,
                            close: range.0,
                            character: close,
                        })
                        .collect();
                    group.sort_by_key(|pair| pair.open);
                    self.auto_closed.push(group);
                    let primary = self.ranges[0].clone();
                    let _ =
                        input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
                    operation.after_selection = Some(primary);
                    self.typing = true;
                    self.starts_typing = true;
                    return Some(operation);
                }
            }
        }
        self.apply(input, kind, data)
    }

    pub fn enter_json(&mut self, input: &web_sys::HtmlTextAreaElement) -> Option<NativeUndo> {
        let text = js_sys::JsString::from(input.value());
        let mut replacements = Vec::with_capacity(self.ranges.len());
        let mut tails = Vec::with_capacity(self.ranges.len());
        for &(start, end, _) in &self.ranges {
            let before = text.slice(0, start).as_string().unwrap_or_default();
            let after = text
                .slice(end, text.length())
                .as_string()
                .unwrap_or_default();
            let (inserted, cursor) = (start == end)
                .then(|| json_bracket_newline(&before, &after))
                .flatten()
                .unwrap_or_else(|| {
                    let inserted = format!("\n{}", line_indentation(&before));
                    let cursor = inserted.encode_utf16().count() as u32;
                    (inserted, cursor)
                });
            tails.push(inserted.encode_utf16().count() as u32 - cursor);
            replacements.push((start, end, inserted));
        }
        let mut operation = self.replace(input, replacements, false)?;
        // replace keeps cursor order; an expanded pair leaves its caret on the
        // inner line, not after the closing-line indentation.
        for (range, tail) in self.ranges.iter_mut().zip(tails) {
            range.0 -= tail;
            range.1 = range.0;
        }
        normalize(&mut self.ranges);
        let primary = self.ranges[0].clone();
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        operation.after_selection = Some(primary);
        Some(operation)
    }

    pub fn surround_json(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        key: &str,
    ) -> Option<NativeUndo> {
        let closing = match key {
            "{" => "}",
            "[" => "]",
            "\"" => "\"",
            _ => return None,
        };
        let text = js_sys::JsString::from(input.value());
        let original = self.ranges.clone();
        let mut replacements = Vec::with_capacity(original.len());
        for (start, end, _) in &original {
            let selected = text.slice(*start, *end).as_string()?;
            // Monaco surrounds only when every selection contains non-whitespace;
            // typing a quote over a selected single quote is ordinary replacement.
            if !selected
                .chars()
                .any(|ch| !matches!(ch, ' ' | '\t' | '\n' | '\r'))
                || key == "\"" && matches!(selected.as_str(), "\"" | "'" | "`")
            {
                return None;
            }
            replacements.push((*start, *end, format!("{key}{selected}{closing}")));
        }
        let mut operation = self.replace(input, replacements, false)?;
        for (range, (start, end, direction)) in self.ranges.iter_mut().zip(original) {
            range.1 -= 1;
            range.0 = range.1 - (end - start);
            range.2 = direction;
        }
        normalize(&mut self.ranges);
        let primary = self.ranges[0].clone();
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        operation.after_selection = Some(primary);
        Some(operation)
    }

    // Clipboard and language-aware edits can differ per cursor. Share the same
    // atomic native edit and selection mapping rather than editing cursors in turn.
    pub fn replace(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        replacements: Vec<(u32, u32, String)>,
        document_order: bool,
    ) -> Option<NativeUndo> {
        let original = selection(input);
        if self.ranges.first() != Some(&original) || replacements.len() != self.ranges.len() {
            return None;
        }
        let before = input.value();
        let units: Vec<_> = before.encode_utf16().collect();
        let mut edits: Vec<_> = replacements.into_iter().enumerate().collect();
        edits.sort_by_key(|(_, (start, end, _))| (*start, *end));
        let mut output = Vec::new();
        let mut last = 0;
        let mut last_deleted = false;
        let mut deletion_position = 0;
        let mut positions: Vec<(u32, u32, u32)> = Vec::with_capacity(edits.len());
        let mut ranges = self.ranges.clone();
        for (index, (start, end, text)) in edits {
            let (start, end) = (start as usize, end as usize);
            if start > end || end > units.len() {
                return None;
            }
            if start < last {
                if !text.is_empty() || !last_deleted {
                    return None;
                }
                // Whole-line cuts from cursors on the same/adjacent lines can
                // overlap. Remove the union once and merge the resulting carets.
                last = last.max(end);
                if let Some(previous) = positions.last_mut() {
                    previous.1 = last as u32;
                }
                ranges[index] = (deletion_position, deletion_position, "forward".into());
                continue;
            }
            output.extend_from_slice(&units[last..start]);
            output.extend(text.encode_utf16());
            let position = output.len() as u32;
            ranges[index] = (position, position, "forward".into());
            last = end;
            last_deleted = text.is_empty();
            deletion_position = position;
            positions.push((start as u32, end as u32, position));
        }
        output.extend_from_slice(&units[last..]);
        let after = String::from_utf16(&output).ok()?;
        let scroll = input.scroll_top();
        if !insert_text(input, 0, units.len() as u32, &after) {
            return None;
        }
        if document_order {
            ranges.sort_by_key(|range| (range.0, range.1));
        }
        normalize(&mut ranges);
        let primary = ranges[0].clone();
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        input.set_scroll_top(scroll);
        self.ranges = ranges;
        self.query = None;
        self.typing = false;
        self.starts_typing = false;
        let map = |position: u32| {
            let index = positions.partition_point(|(start, _, _)| *start <= position);
            if index == 0 {
                Some(position)
            } else {
                let (_, end, after) = positions[index - 1];
                (position >= end).then(|| after + position - end)
            }
        };
        // Keep marker coordinates through the actual simultaneous edits, not
        // one prefix/suffix diff which would discard unchanged interior pairs.
        for group in &mut self.auto_closed {
            group.retain_mut(|pair| {
                let (Some(open), Some(close)) = (map(pair.open), map(pair.close)) else {
                    return false;
                };
                pair.open = open;
                pair.close = close;
                true
            });
        }
        self.validate_pairs(&output);
        Some(NativeUndo {
            before,
            after: input.value(),
            start: original.0,
            end: original.1,
            direction: original.2,
            after_selection: Some(primary),
            edits: Vec::new(),
        })
    }
}

pub(super) struct InputListener {
    input: web_sys::HtmlTextAreaElement,
    callback: Closure<dyn Fn(web_sys::InputEvent)>,
}

impl InputListener {
    pub fn new(
        input: web_sys::HtmlTextAreaElement,
        handler: Callback<web_sys::InputEvent>,
    ) -> Option<Rc<Self>> {
        let busy = std::cell::Cell::new(false);
        let callback = Closure::<dyn Fn(web_sys::InputEvent)>::new(move |event| {
            if !busy.replace(true) {
                handler.call(event);
                busy.set(false);
            }
        });
        input
            .add_event_listener_with_callback("beforeinput", callback.as_ref().unchecked_ref())
            .ok()?;
        Some(Rc::new(Self { input, callback }))
    }
}

impl Drop for InputListener {
    fn drop(&mut self) {
        let _ = self.input.remove_event_listener_with_callback(
            "beforeinput",
            self.callback.as_ref().unchecked_ref(),
        );
    }
}

#[derive(Clone)]
pub(super) struct Box {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub caret: bool,
}

pub(super) fn boxes(
    input: &web_sys::HtmlTextAreaElement,
    rendered: &web_sys::HtmlElement,
    selections: &[Selection],
) -> Vec<Box> {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return Vec::new();
    };
    let origin = input.get_bounding_client_rect();
    let mut result = Vec::new();
    let value = input.value();
    for &(start, end, _) in selections {
        let mut boxes = Vec::<Box>::new();
        let mut offset = 0_u32;
        for (index, text) in value.split('\n').enumerate() {
            let length = text.encode_utf16().count() as u32;
            let line_start = offset;
            offset += length + 1;
            let Some(line) = rendered.child_nodes().item(index as u32) else {
                continue;
            };
            if line.text_content().as_deref() != Some(text) {
                continue;
            }
            if end < line_start || start > line_start + length {
                continue;
            }
            if length == 0 && start == end && start == line_start {
                if let Some(line) = line.dyn_ref::<web_sys::Element>() {
                    let rect = line.get_bounding_client_rect();
                    boxes.push(Box {
                        left: rect.left() - origin.left(),
                        top: rect.top() - origin.top(),
                        width: 0.0,
                        height: 16.0,
                        caret: true,
                    });
                }
                continue;
            }
            let mut position = line_start;
            let children = line.child_nodes();
            for child in 0..children.length() {
                let Some(span) = children
                    .item(child)
                    .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                    .filter(|node| node.tag_name() == "SPAN")
                else {
                    continue;
                };
                let Some(node) = span.first_child() else {
                    continue;
                };
                let size = node
                    .text_content()
                    .unwrap_or_default()
                    .encode_utf16()
                    .count() as u32;
                let from = start.max(position);
                let to = end.min(position + size);
                if from <= to
                    && (start == end || from != to)
                    && let Ok(range) = document.create_range()
                    && range.set_start(&node, from - position).is_ok()
                    && range.set_end(&node, to - position).is_ok()
                    && let Some(rects) = range.get_client_rects()
                {
                    for i in 0..rects.length() {
                        let Some(rect) = rects.item(i) else { continue };
                        let left = rect.left() - origin.left();
                        let top = rect.top() - origin.top() - 1.0;
                        let width = rect.width();
                        if let Some(last) = boxes.last_mut()
                            && last.caret == (start == end)
                            && (last.top - top).abs() < 1.0
                            && left <= last.left + last.width
                            && left + width >= last.left
                        {
                            let right = (left + width).max(last.left + last.width);
                            last.left = left.min(last.left);
                            last.width = right - last.left;
                        } else {
                            boxes.push(Box {
                                left,
                                top,
                                width,
                                height: 16.0,
                                caret: start == end,
                            });
                        }
                    }
                    if start == end {
                        break;
                    }
                }
                position += size;
            }
        }
        if let (Some(first), Some(last)) = (boxes.first(), boxes.last()) {
            let first_top = first.top;
            let last_top = last.top;
            for rect in &mut boxes {
                if !rect.caret {
                    // Monaco reserves a pixel above each first selection row,
                    // and below its last row when the selection spans rows.
                    if (rect.top - first_top).abs() < 1.0 {
                        rect.top += 1.0;
                        rect.height -= 1.0;
                    } else if (rect.top - last_top).abs() < 1.0 {
                        rect.height -= 1.0;
                    }
                }
            }
        }
        result.extend(boxes);
    }
    result
}
