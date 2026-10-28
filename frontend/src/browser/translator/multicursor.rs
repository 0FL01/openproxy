//! Occurrence selections layered over the existing native text input/history.
//! Rules follow Monaco 0.55.1 (Copyright Microsoft Corporation, MIT).
//! Permission/warranty notice: public/assets/translator-codicon-LICENSE.txt.
use super::{find_ranges, insert_text, NativeUndo};
use dioxus::prelude::Callback;
use std::rc::Rc;
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
    query: Option<(String, bool, bool)>,
}

impl Cursors {
    pub fn restore(&mut self, ranges: Vec<Selection>) {
        *self = Self {
            ranges,
            ..Self::default()
        };
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
            self.ranges = vec![(start, end, primary.2.clone())];
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
        let mut edits: Vec<_> = self
            .ranges
            .iter()
            .enumerate()
            .map(|(index, range)| (range.0 as usize, range.1 as usize, index))
            .collect();
        edits.sort_unstable();
        let inserted: Vec<u16> = match kind {
            "insertText" => data.encode_utf16().collect(),
            "insertLineBreak" | "insertParagraph" => vec![10],
            "deleteContentBackward" | "deleteContentForward" => Vec::new(),
            _ => return None,
        };
        let mut output = Vec::new();
        let mut last = 0;
        let mut ranges = self.ranges.clone();
        for (mut start, mut end, index) in edits {
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
            if start < last {
                return None;
            }
            output.extend_from_slice(&units[last..start]);
            output.extend_from_slice(&inserted);
            let position = output.len() as u32;
            ranges[index] = (position, position, "forward".into());
            last = end;
        }
        output.extend_from_slice(&units[last..]);
        let after = String::from_utf16(&output).ok()?;
        let scroll = input.scroll_top();
        if !insert_text(input, 0, units.len() as u32, &after) {
            return None;
        }
        normalize(&mut ranges);
        let primary = ranges[0].clone();
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        input.set_scroll_top(scroll);
        self.ranges = ranges;
        self.query = None;
        self.typing = kind == "insertText" && !data.contains(char::is_whitespace);
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
