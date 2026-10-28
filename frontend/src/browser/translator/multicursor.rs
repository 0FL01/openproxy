//! Occurrence selections layered over the existing native text input/history.
//! Rules follow Monaco 0.55.1 (Copyright Microsoft Corporation, MIT).
//! Permission/warranty notice: public/assets/translator-codicon-LICENSE.txt.
use super::{
    block_comment_edits, character_top, find_ranges, grapheme, in_json_string,
    indentation_backspace, insert_text, json_bracket_newline, line_comment_edits, line_indentation,
    position_top, shifted_indentation, LineComment, NativeUndo,
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

fn word_at(text: &str, position: u32, json: bool) -> Option<(u32, u32)> {
    let text = js_sys::JsString::from(text);
    let before = text.slice(0, position).as_string()?;
    let line_start = before
        .rfind('\n')
        .map_or(0, |byte| before[..=byte].encode_utf16().count() as u32);
    let line = text.slice(line_start, text.length()).as_string()?;
    let line = line.split('\n').next().unwrap_or_default();
    let pattern = if json {
        r#"(-?\d*\.\d\w*)|([^\[\{\]\}\:\",\s]+)"#
    } else {
        r#"(-?\d*\.\d\w*)|([^`~!@#$%^&*()\-=+\[\{\]\}\\|;:'\",.<>/?\s]+)"#
    };
    let words = js_sys::RegExp::new(pattern, "g");
    while let Some(found) = words.exec(line) {
        let end = words.last_index() + line_start;
        let start = end - found.get(0).length();
        if start <= position && position <= end {
            return Some((start, end));
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Regular,
    Whitespace,
    Separator,
}

fn word_class(unit: u16) -> WordClass {
    if matches!(unit, 9 | 32) {
        WordClass::Whitespace
    } else if unit < 128 && b"`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?".contains(&(unit as u8)) {
        WordClass::Separator
    } else {
        WordClass::Regular
    }
}

// Configured Monaco defaults: WordStartFast with multiple cursors on the left,
// WordEnd on the right, with no locale-specific word segmenter.
fn word_position(units: &[u16], position: usize, left: bool) -> usize {
    let mut position = position.min(units.len());
    if left {
        if position > 0 && units[position - 1] == 10 {
            position -= 1;
        }
        let start = units[..position]
            .iter()
            .rposition(|unit| *unit == 10)
            .map_or(0, |index| index + 1);
        while position > start && word_class(units[position - 1]) == WordClass::Whitespace {
            position -= 1;
        }
        if position > start {
            let kind = word_class(units[position - 1]);
            while position > start && word_class(units[position - 1]) == kind {
                position -= 1;
            }
        }
    } else {
        if units.get(position) == Some(&10) {
            position += 1;
        }
        let start = units[..position]
            .iter()
            .rposition(|unit| *unit == 10)
            .map_or(0, |index| index + 1);
        let end = units[position..]
            .iter()
            .position(|unit| *unit == 10)
            .map_or(units.len(), |index| position + index);
        while position < end && word_class(units[position]) == WordClass::Whitespace {
            position += 1;
        }
        if position < end {
            let kind = word_class(units[position]);
            let mut word_start = position;
            while word_start > start && word_class(units[word_start - 1]) == kind {
                word_start -= 1;
            }
            while position < end && word_class(units[position]) == kind {
                position += 1;
            }
            // WordEnd skips a single separator followed immediately by a word,
            // but not the remainder of a longer separator run.
            if kind == WordClass::Separator && position - word_start == 1 {
                while position < end && word_class(units[position]) == WordClass::Regular {
                    position += 1;
                }
            }
        }
    }
    position
}

// DeleteWord uses whitespace heuristics and stays on the current logical line,
// except for joining at its boundary. It does not use movement's separator skip.
fn word_deletion(units: &[u16], position: usize, left: bool) -> (u32, u32) {
    let position = position.min(units.len());
    let line_start = units[..position]
        .iter()
        .rposition(|unit| *unit == 10)
        .map_or(0, |index| index + 1);
    let line_end = units[position..]
        .iter()
        .position(|unit| *unit == 10)
        .map_or(units.len(), |index| position + index);
    let mut target = position;
    if left {
        if position == line_start {
            return (position.saturating_sub(1) as u32, position as u32);
        }
        while target > line_start && word_class(units[target - 1]) == WordClass::Whitespace {
            target -= 1;
        }
        if position - target < 2 && target > line_start {
            let kind = word_class(units[target - 1]);
            while target > line_start && word_class(units[target - 1]) == kind {
                target -= 1;
            }
        }
        (target as u32, position as u32)
    } else {
        if position == line_end && position < units.len() {
            target += 1;
            while target < units.len() && word_class(units[target]) == WordClass::Whitespace {
                target += 1;
            }
        } else {
            while target < line_end && word_class(units[target]) == WordClass::Whitespace {
                target += 1;
            }
            if target - position < 2 && target < line_end {
                let kind = word_class(units[target]);
                while target < line_end && word_class(units[target]) == kind {
                    target += 1;
                }
            }
        }
        (position as u32, target as u32)
    }
}

// The source command executor rejects a whole later cursor command if any of
// its edits overlap. Empty insertions at the same position do not overlap.
fn conflicting_cursors(mut operations: Vec<(usize, u32, u32)>) -> HashSet<usize> {
    operations.sort_by_key(|&(_, start, end)| std::cmp::Reverse((end, start)));
    let mut losing = HashSet::new();
    let mut index = 1;
    while index < operations.len() {
        let previous = operations[index - 1];
        let current = operations[index];
        if previous.1 < current.2 {
            let loser = previous.0.max(current.0);
            losing.insert(loser);
            operations.retain(|operation| operation.0 != loser);
            index = index.saturating_sub(1).max(1);
        } else {
            index += 1;
        }
    }
    losing
}

fn normalize(ranges: &mut Vec<Selection>) -> Vec<usize> {
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
    let owners = merged.iter().map(|(owner, _, _)| *owner).collect();
    *ranges = merged.into_iter().map(|(_, range, _)| range).collect();
    owners
}

#[derive(Clone, Default)]
pub(super) struct Cursors {
    pub ranges: Vec<Selection>,
    pub typing: bool,
    pub starts_typing: bool,
    query: Option<(String, bool, bool)>,
    auto_closed: Vec<Vec<ClosedPair>>,
    vertical: Option<Vertical>,
    added: Option<Added>,
    line_edge: Option<LineEdge>,
}

#[derive(Clone)]
struct LineEdge {
    ranges: Vec<Selection>,
    width: i32,
    end_rows: Vec<Option<u32>>,
}

#[derive(Clone)]
struct Vertical {
    ranges: Vec<Selection>,
    width: i32,
    leftover: Vec<f64>,
}

#[derive(Clone)]
struct Added {
    ranges: Vec<Selection>,
    columns: Vec<(usize, usize)>,
}

struct ViewRow {
    start: u32,
    end: u32,
    line_start: u32,
    line: web_sys::HtmlElement,
}

impl ViewRow {
    fn left(&self, position: u32) -> Option<f64> {
        if self.start == self.end {
            return Some(self.line.get_bounding_client_rect().left());
        }
        let at_end = position == self.end;
        let mut offset = position - self.line_start - u32::from(at_end);
        let document = self.line.owner_document()?;
        let children = self.line.child_nodes();
        for index in 0..children.length() {
            let Some(span) = children
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                .filter(|element| element.tag_name() == "SPAN")
            else {
                continue;
            };
            let text = span.first_child()?;
            let length = js_sys::JsString::from(text.text_content()?).length();
            if offset < length {
                let range = document.create_range().ok()?;
                range.set_start(&text, offset).ok()?;
                range.set_end(&text, offset + 1).ok()?;
                let rect = range.get_bounding_client_rect();
                return Some(if at_end { rect.right() } else { rect.left() });
            }
            offset -= length;
        }
        None
    }
}

fn view_rows(text: &str, rendered: &web_sys::HtmlElement) -> Option<Vec<ViewRow>> {
    let mut rows = Vec::new();
    let mut line_start = 0;
    for (index, text) in text.split('\n').enumerate() {
        let line = rendered
            .child_nodes()
            .item(index as u32)?
            .dyn_into::<web_sys::HtmlElement>()
            .ok()?;
        if line.text_content().as_deref() != Some(text) {
            return None;
        }
        let length = text.encode_utf16().count();
        let mut start = 0;
        loop {
            let end = if line.offset_height() <= 16 || start == length {
                length
            } else {
                let top = character_top(&line, start)?;
                let (mut low, mut high) = (start + 1, length);
                while low < high {
                    let middle = low + (high - low) / 2;
                    if character_top(&line, middle)? > top {
                        high = middle;
                    } else {
                        low = middle + 1;
                    }
                }
                low
            };
            rows.push(ViewRow {
                start: line_start + start as u32,
                end: line_start + end as u32,
                line_start,
                line: line.clone(),
            });
            if end == length {
                break;
            }
            start = end;
        }
        line_start += length as u32 + 1;
    }
    Some(rows)
}

fn reveal(input: &web_sys::HtmlTextAreaElement, rendered: &web_sys::HtmlElement, position: u32) {
    if let Some(top) = position_top(input, rendered, position) {
        let padding = input
            .style()
            .get_property_value("padding-top")
            .ok()
            .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
            .unwrap_or_default();
        let shift = if top < padding {
            top - padding
        } else {
            (top + 16.0 - f64::from(input.client_height())).max(0.0)
        };
        input.set_scroll_top(input.scroll_top() + shift as i32);
    }
}

fn reveal_all(input: &web_sys::HtmlTextAreaElement, top: f64, bottom: f64) {
    let padding = input
        .style()
        .get_property_value("padding-top")
        .ok()
        .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
        .unwrap_or_default();
    // Leave the viewport alone when the complete selection box cannot fit.
    // Both bounds must be measured before this single scroll update.
    let height = f64::from(input.client_height());
    if bottom - top <= height - padding {
        let shift = if top < padding {
            top - padding
        } else {
            (bottom + 16.0 - height).max(0.0)
        };
        input.set_scroll_top(input.scroll_top() + shift as i32);
    }
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
            self.vertical = None;
            self.added = None;
            self.line_edge = None;
            self.validate_pairs(&units);
            let primary = &self.ranges[0];
            let _ = input.set_selection_range(primary.0, primary.1);
            true
        } else {
            false
        }
    }

    pub fn horizontal(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        left: bool,
        extend: bool,
        word: bool,
    ) -> bool {
        self.vertical = None;
        self.added = None;
        self.line_edge = None;
        let units: Vec<_> = input.value().encode_utf16().collect();
        let mut ranges: Vec<_> = self
            .ranges
            .iter()
            .map(|(start, end, direction)| {
                let (anchor, active) = if direction == "backward" {
                    (*end, *start)
                } else {
                    (*start, *end)
                };
                let next = if word {
                    word_position(&units, active as usize, left) as u32
                } else if !extend && start != end {
                    if left {
                        *start
                    } else {
                        *end
                    }
                } else if left {
                    grapheme::previous(&units, active as usize) as u32
                } else {
                    grapheme::next(&units, active as usize) as u32
                };
                if extend {
                    (
                        anchor.min(next),
                        anchor.max(next),
                        if next < anchor { "backward" } else { "forward" }.into(),
                    )
                } else {
                    (next, next, "forward".into())
                }
            })
            .collect();
        normalize(&mut ranges);
        if ranges == self.ranges {
            return false;
        }
        self.ranges = ranges;
        self.query = None;
        self.typing = false;
        self.starts_typing = false;
        self.validate_pairs(&units);
        let primary = &self.ranges[0];
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        true
    }

    pub fn document_edge(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        start: bool,
        extend: bool,
    ) {
        let units: Vec<_> = input.value().encode_utf16().collect();
        let next = if start { 0 } else { units.len() as u32 };
        for (start, end, direction) in &mut self.ranges {
            let anchor = if direction == "backward" {
                *end
            } else {
                *start
            };
            *start = if extend { anchor.min(next) } else { next };
            *end = if extend { anchor.max(next) } else { next };
            *direction = if extend && next < anchor {
                "backward"
            } else {
                "forward"
            }
            .into();
        }
        normalize(&mut self.ranges);
        self.vertical = None;
        self.added = None;
        self.line_edge = None;
        self.query = None;
        self.typing = false;
        self.starts_typing = false;
        self.validate_pairs(&units);
        let primary = &self.ranges[0];
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        input.set_scroll_top(if next == 0 { 0 } else { input.scroll_height() });
    }

    pub fn home_end(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        rendered: &web_sys::HtmlElement,
        home: bool,
        extend: bool,
    ) -> Option<bool> {
        let text = input.value();
        let units: Vec<_> = text.encode_utf16().collect();
        let rows = view_rows(&text, rendered)?;
        let previous = self
            .line_edge
            .as_ref()
            .filter(|state| state.ranges == self.ranges && state.width == input.client_width());
        let mut ranges = Vec::new();
        let mut end_rows = Vec::new();
        let (mut top, mut bottom) = (f64::MAX, f64::MIN);
        for (index, &(start, end, ref direction)) in self.ranges.iter().enumerate() {
            let (anchor, active) = if direction == "backward" {
                (end, start)
            } else {
                (start, end)
            };
            // A wrap boundary belongs to either row. Keep the row selected by
            // End so the next End visits the logical end instead of the next row.
            let row_start = previous.and_then(|state| state.end_rows[index]);
            let row = &rows[rows
                .partition_point(|row| row.start <= row_start.unwrap_or(active))
                .saturating_sub(1)];
            let logical_end = units[row.line_start as usize..]
                .iter()
                .position(|unit| *unit == 10)
                .map_or(units.len(), |length| row.line_start as usize + length)
                as u32;
            let first_nonblank = |start: u32, end: u32| {
                units[start as usize..end as usize]
                    .iter()
                    .position(|unit| !matches!(*unit, 9 | 32))
                    .map_or(start, |offset| start + offset as u32)
            };
            let next = if home {
                let first = first_nonblank(row.start, row.end);
                if row.start != row.line_start && active != first {
                    first
                } else {
                    let first = first_nonblank(row.line_start, logical_end);
                    if active == first {
                        row.line_start
                    } else {
                        first
                    }
                }
            } else if active == row.end || row.end == logical_end {
                logical_end
            } else {
                row.end
            };
            let end_row = (!home && next == row.end && next < logical_end).then_some(row.start);
            end_rows.push(end_row);
            let cursor_top = position_top(input, rendered, end_row.unwrap_or(next))?;
            let anchor_top = if extend {
                position_top(input, rendered, anchor)?
            } else {
                cursor_top
            };
            top = top.min(cursor_top.min(anchor_top));
            bottom = bottom.max(cursor_top.max(anchor_top) + 16.0);
            ranges.push(if extend {
                (
                    anchor.min(next),
                    anchor.max(next),
                    if next < anchor { "backward" } else { "forward" }.into(),
                )
            } else {
                (next, next, "forward".into())
            });
        }
        let generated = ranges.clone();
        let owners = normalize(&mut ranges);
        self.line_edge = Some(LineEdge {
            ranges: ranges.clone(),
            width: input.client_width(),
            end_rows: owners
                .into_iter()
                .enumerate()
                .map(|(index, owner)| {
                    (ranges[index] == generated[owner])
                        .then_some(end_rows[owner])
                        .flatten()
                })
                .collect(),
        });
        let changed = ranges != self.ranges;
        self.ranges = ranges;
        self.vertical = None;
        self.added = None;
        self.query = None;
        self.typing = false;
        self.starts_typing = false;
        self.validate_pairs(&units);
        let primary = &self.ranges[0];
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        reveal_all(input, top, bottom);
        Some(changed)
    }

    pub fn vertical(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        rendered: &web_sys::HtmlElement,
        up: bool,
        extend: bool,
        distance: usize,
    ) -> Option<bool> {
        let text = input.value();
        let units: Vec<_> = text.encode_utf16().collect();
        let rows = view_rows(&text, rendered)?;
        self.line_edge = None;
        let origin = rendered.get_bounding_client_rect().left();
        let previous = self
            .vertical
            .as_ref()
            .filter(|state| state.ranges == self.ranges && state.width == input.client_width());
        let mut ranges = Vec::new();
        let mut leftover = Vec::new();
        let (mut top, mut bottom) = (f64::MAX, f64::MIN);
        for (index, &(start, end, ref direction)) in self.ranges.iter().enumerate() {
            let (anchor, active) = if direction == "backward" {
                (end, start)
            } else {
                (start, end)
            };
            let position = if !extend && start != end {
                if up {
                    start
                } else {
                    end
                }
            } else {
                active
            };
            let row_index = rows
                .partition_point(|row| row.start <= position)
                .saturating_sub(1);
            let row = &rows[row_index];
            let preferred =
                row.left(position)? - origin + previous.map_or(0.0, |state| state.leftover[index]);
            let overshoots = if up {
                distance > row_index
            } else {
                distance > rows.len() - 1 - row_index
            };
            let target_index = if up {
                row_index.saturating_sub(distance)
            } else {
                row_index.saturating_add(distance).min(rows.len() - 1)
            };
            let target = &rows[target_index];
            let next = if overshoots {
                if up {
                    target.start
                } else {
                    target.end
                }
            } else {
                let mut nearest = target.start;
                let mut distance = (target.left(nearest)? - origin - preferred).abs();
                let mut offset = nearest as usize;
                while offset < target.end as usize {
                    offset = grapheme::next(&units[..target.end as usize], offset);
                    let next_distance = (target.left(offset as u32)? - origin - preferred).abs();
                    if next_distance < distance {
                        nearest = offset as u32;
                        distance = next_distance;
                    }
                }
                nearest
            };
            let cursor_top = position_top(input, rendered, target.start)?;
            let anchor_top = if extend {
                position_top(input, rendered, anchor)?
            } else {
                cursor_top
            };
            top = top.min(cursor_top.min(anchor_top));
            bottom = bottom.max(cursor_top.max(anchor_top) + 16.0);
            let at_edge = target_index == row_index && position == next;
            leftover.push(if at_edge {
                0.0
            } else {
                preferred - (target.left(next)? - origin)
            });
            ranges.push(if extend {
                (
                    anchor.min(next),
                    anchor.max(next),
                    if next < anchor { "backward" } else { "forward" }.into(),
                )
            } else {
                (next, next, "forward".into())
            });
        }
        let owners = normalize(&mut ranges);
        self.vertical = Some(Vertical {
            ranges: ranges.clone(),
            width: input.client_width(),
            leftover: owners.into_iter().map(|owner| leftover[owner]).collect(),
        });
        if ranges == self.ranges {
            return Some(false);
        }
        self.ranges = ranges;
        self.query = None;
        self.typing = false;
        self.starts_typing = false;
        self.validate_pairs(&units);
        let primary = &self.ranges[0];
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        self.added = None;
        reveal_all(input, top, bottom);
        Some(true)
    }

    pub fn add_vertical(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        rendered: Option<&web_sys::HtmlElement>,
        up: bool,
    ) -> bool {
        self.line_edge = None;
        let primary = selection(input);
        let original = if self.ranges.first() == Some(&primary) {
            self.ranges.clone()
        } else {
            vec![primary]
        };
        let units: Vec<_> = input.value().encode_utf16().collect();
        let mut starts = vec![0];
        starts.extend(
            units
                .iter()
                .enumerate()
                .filter_map(|(index, ch)| (*ch == 10).then_some(index + 1)),
        );
        let line_index = |position: u32| {
            starts
                .partition_point(|start| *start <= position as usize)
                .saturating_sub(1)
        };
        let line_end = |index: usize| starts.get(index + 1).map_or(units.len(), |start| start - 1);
        let column = |position: u32| {
            let line = line_index(position);
            grapheme::visible_column(
                &units[starts[line]..line_end(line)],
                position as usize - starts[line],
            )
        };
        let columns = |range: &Selection| {
            if range.2 == "backward" {
                (column(range.1), column(range.0))
            } else {
                (column(range.0), column(range.1))
            }
        };
        let previous = self.added.as_ref().filter(|state| state.ranges == original);
        let translated = |position: u32, preferred: usize| {
            let line = line_index(position);
            let target = if up {
                line.saturating_sub(1)
            } else {
                (line + 1).min(starts.len() - 1)
            };
            if target == line {
                position
            } else {
                (starts[target]
                    + grapheme::column_offset(&units[starts[target]..line_end(target)], preferred))
                    as u32
            }
        };
        let mut ranges = Vec::new();
        let mut preferred = Vec::new();
        for (index, range) in original.iter().enumerate() {
            let desired = previous.map_or_else(|| columns(range), |state| state.columns[index]);
            ranges.push(range.clone());
            preferred.push(desired);
            if ranges.len() == 10_000 {
                break;
            }
            let (anchor, active) = if range.2 == "backward" {
                (range.1, range.0)
            } else {
                (range.0, range.1)
            };
            let anchor = translated(anchor, desired.0);
            let active = translated(active, desired.1);
            ranges.push((
                anchor.min(active),
                anchor.max(active),
                if active < anchor {
                    "backward"
                } else {
                    "forward"
                }
                .into(),
            ));
            preferred.push(desired);
            if ranges.len() == 10_000 {
                break;
            }
        }
        let generated = ranges.clone();
        let owners = normalize(&mut ranges);
        self.added = Some(Added {
            ranges: ranges.clone(),
            columns: owners
                .into_iter()
                .zip(&ranges)
                .map(|(owner, range)| {
                    if *range == generated[owner] {
                        preferred[owner]
                    } else {
                        columns(range)
                    }
                })
                .collect(),
        });
        self.vertical = None;
        self.query = None;
        self.typing = false;
        self.starts_typing = false;
        let changed = ranges != original;
        self.ranges = ranges;
        self.validate_pairs(&units);
        let primary = &self.ranges[0];
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        if let Some(rendered) = rendered {
            let positions = self.ranges.iter().map(|range| {
                if range.2 == "backward" {
                    range.0
                } else {
                    range.1
                }
            });
            let target = if up { positions.min() } else { positions.max() };
            if let Some(target) = target {
                reveal(input, rendered, target);
            }
        }
        changed
    }

    pub fn added_message(
        &self,
        input: &web_sys::HtmlTextAreaElement,
        rendered: &web_sys::HtmlElement,
        previous: &[Selection],
    ) -> Option<String> {
        let rows = view_rows(&input.value(), rendered)?;
        let previous: HashSet<_> = previous.iter().collect();
        let mut positions = Vec::new();
        for range in &self.ranges {
            if previous.contains(range) {
                continue;
            }
            let active = if range.2 == "backward" {
                range.0
            } else {
                range.1
            };
            let index = rows
                .partition_point(|row| row.start <= active)
                .saturating_sub(1);
            let row = &rows[index];
            let indent = if row.start > row.line_start
                && row
                    .line
                    .style()
                    .get_property_value("padding-left")
                    .ok()?
                    .trim_end_matches("px")
                    .parse::<f64>()
                    .ok()?
                    > 0.0
            {
                let text = row.line.text_content()?;
                let prefix: Vec<_> = text
                    .chars()
                    .take_while(|ch| matches!(ch, ' ' | '\t'))
                    .collect::<String>()
                    .encode_utf16()
                    .collect();
                grapheme::visible_column(&prefix, prefix.len()) as u32
            } else {
                0
            };
            positions.push(format!(
                "line {} column {}",
                index + 1,
                active - row.start + 1 + indent
            ));
        }
        if positions.is_empty() {
            return None;
        }
        Some(format!(
            "{}: {}",
            if positions.len() == 1 {
                "Cursor added"
            } else {
                "Cursors added"
            },
            positions.join(", ")
        ))
    }

    pub fn choose(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        json: bool,
        case: bool,
        whole: bool,
        all: bool,
    ) {
        self.vertical = None;
        self.added = None;
        self.line_edge = None;
        let primary = selection(input);
        if self.ranges.first() != Some(&primary) {
            self.ranges.clear();
            self.query = None;
        }
        let text = input.value();
        let string = js_sys::JsString::from(text.as_str());
        if self.query.is_none() {
            let (start, end) = if primary.0 == primary.1 {
                let Some(word) = word_at(&text, primary.0, json) else {
                    return;
                };
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
                start = if let Some(count) = indentation_backspace(&units, start) {
                    start - count
                } else {
                    grapheme::delete_left(&units, start)
                };
            } else if start == end && kind == "deleteContentForward" && end < units.len() {
                end = grapheme::next(&units, end);
            }
            replacements.push((start as u32, end as u32, inserted.to_owned()));
        }
        let operation = self.replace(input, replacements, false)?;
        self.typing = kind == "insertText" && !data.contains(char::is_whitespace);
        Some(operation)
    }

    pub fn delete_words(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        backwards: bool,
    ) -> Option<NativeUndo> {
        if self.ranges.len() < 2 || self.ranges.first() != Some(&selection(input)) {
            return None;
        }
        let units: Vec<_> = input.value().encode_utf16().collect();
        self.validate_pairs(&units);
        let marked: HashSet<_> = self
            .auto_closed
            .iter()
            .flatten()
            .map(|pair| (pair.close, pair.character as u16))
            .collect();
        let replacements: Vec<_> = self
            .ranges
            .iter()
            .map(|&(start, end, _)| {
                let (start, end) = if start != end {
                    (start, end)
                } else if backwards
                    && start > 0
                    && units
                        .get(start as usize - 1..=start as usize)
                        .is_some_and(|pair| {
                            matches!(pair, [123, 125] | [91, 93] | [34, 34])
                                && marked.contains(&(start, pair[1]))
                        })
                {
                    (start - 1, end + 1)
                } else {
                    word_deletion(&units, start as usize, backwards)
                };
                (start, end, String::new())
            })
            .collect();
        self.typing = false;
        self.starts_typing = false;
        if replacements.iter().all(|(start, end, _)| start == end) {
            return None;
        }

        // Monaco rejects the later cursor's conflicting command, rather than
        // taking the union used by whole-line Cut. Keep original cursor priority.
        let losing = conflicting_cursors(
            replacements
                .iter()
                .enumerate()
                .map(|(index, &(start, end, _))| (index, start, end))
                .collect(),
        );
        let original = std::mem::take(&mut self.ranges);
        self.ranges = original
            .iter()
            .enumerate()
            .filter(|(index, _)| !losing.contains(index))
            .map(|(_, range)| range.clone())
            .collect();
        let replacements = replacements
            .into_iter()
            .enumerate()
            .filter(|(index, _)| !losing.contains(index))
            .map(|(_, replacement)| replacement)
            .collect();
        let operation = self.replace(input, replacements, false);
        if operation.is_none() {
            self.ranges = original;
        }
        operation
    }

    pub fn indent(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        backwards: bool,
        tab: bool,
    ) -> Option<NativeUndo> {
        let original = selection(input);
        if self.ranges.len() < 2 || self.ranges.first() != Some(&original) {
            return None;
        }
        let before = input.value();
        let units: Vec<_> = before.encode_utf16().collect();
        let mut offset = 0_u32;
        let lines: Vec<_> = before
            .split('\n')
            .map(|line| {
                let start = offset;
                offset += line.encode_utf16().count() as u32 + 1;
                (start, line)
            })
            .collect();
        let mut operations = Vec::new();
        let mut stays = vec![None; self.ranges.len()];
        let mut replaced = vec![false; self.ranges.len()];
        for (index, &(start, end, _)) in self.ranges.iter().enumerate() {
            let first = lines
                .partition_point(|(offset, _)| *offset <= start)
                .saturating_sub(1);
            let mut last = lines
                .partition_point(|(offset, _)| *offset <= end)
                .saturating_sub(1);
            let line_end = lines[first].0 + lines[first].1.encode_utf16().count() as u32;
            if tab
                && !backwards
                && (start == end || first == last && (start != lines[first].0 || end != line_end))
            {
                let prefix = &units[lines[first].0 as usize..start as usize];
                let column = grapheme::visible_column(prefix, prefix.len());
                operations.push((index, (start, end, " ".repeat(4 - column % 4))));
                replaced[index] = true;
                continue;
            }
            if last > first && end == lines[last].0 {
                last -= 1;
            }
            for (line_index, &(offset, line)) in lines.iter().enumerate().take(last + 1).skip(first)
            {
                let (old, indent) = shifted_indentation(line, backwards);
                if backwards && old == 0 || first != last && line.is_empty() {
                    continue;
                }
                if line_index == first && start != end && start <= offset + old as u32 {
                    stays[index] = Some((offset, start - offset));
                }
                operations.push((index, (offset, offset + old as u32, indent)));
            }
        }
        self.typing = false;
        self.starts_typing = false;
        if operations.is_empty() {
            return None;
        }
        let losing = conflicting_cursors(
            operations
                .iter()
                .map(|(index, (start, end, _))| (*index, *start, *end))
                .collect(),
        );
        operations.retain(|(index, _)| !losing.contains(index));
        operations.sort_by_key(|(_, (start, end, _))| (*start, *end));
        let mut output = Vec::new();
        let mut last = 0;
        let mut positions = Vec::with_capacity(operations.len());
        for (_, (start, end, text)) in operations {
            output.extend_from_slice(&units[last..start as usize]);
            output.extend(text.encode_utf16());
            positions.push((start, end, output.len() as u32));
            last = end as usize;
        }
        output.extend_from_slice(&units[last..]);
        let map = |position: u32| {
            let index = positions.partition_point(|(start, _, _)| *start <= position);
            if index == 0 {
                position
            } else {
                let (_, end, after) = positions[index - 1];
                after + position.saturating_sub(end)
            }
        };
        let mut ranges: Vec<_> = self
            .ranges
            .iter()
            .enumerate()
            .filter(|(index, _)| !losing.contains(index))
            .map(|(index, &(start, end, ref direction))| {
                let end = map(end);
                let mut start = if replaced[index] { end } else { map(start) };
                if let Some((line, column)) = stays[index] {
                    let previous = positions.partition_point(|(offset, _, _)| *offset < line);
                    let line = if previous == 0 {
                        line
                    } else {
                        let (_, end, after) = positions[previous - 1];
                        after + line - end
                    };
                    start = start.min(line + column);
                }
                (
                    start,
                    end.max(start),
                    if replaced[index] {
                        "forward".into()
                    } else {
                        direction.clone()
                    },
                )
            })
            .collect();
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
        self.vertical = None;
        self.added = None;
        self.line_edge = None;
        self.query = None;
        self.map_auto_closed(&positions, &output);
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

    pub fn line_comments(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        mode: LineComment,
    ) -> Option<NativeUndo> {
        let original = selection(input);
        if self.ranges.first() != Some(&original) {
            return None;
        }
        let before = input.value();
        let units: Vec<_> = before.encode_utf16().collect();
        let mut ordered: Vec<_> = self.ranges.iter().enumerate().collect();
        ordered.sort_by_key(|(_, range)| (range.0, range.1));
        let mut ignore_first = vec![false; self.ranges.len()];
        let mut previous = ordered[0];
        for current in ordered.iter().skip(1).copied() {
            if !units[previous.1 .1 as usize..current.1 .0 as usize].contains(&10) {
                if previous.0 < current.0 {
                    ignore_first[current.0] = true;
                } else {
                    ignore_first[previous.0] = true;
                    previous = current;
                }
            }
        }
        let mut operations = Vec::new();
        for (index, &(start, end, _)) in self.ranges.iter().enumerate() {
            operations.extend(
                line_comment_edits(
                    &units,
                    start as usize,
                    end as usize,
                    mode,
                    ignore_first[index],
                )
                .into_iter()
                .map(|edit| (index, edit)),
            );
        }
        self.typing = false;
        self.starts_typing = false;
        if operations.is_empty() {
            return None;
        }
        let losing = conflicting_cursors(
            operations
                .iter()
                .map(|(index, (start, end, _))| (*index, *start, *end))
                .collect(),
        );
        operations.retain(|(index, _)| !losing.contains(index));
        operations.sort_by_key(|(_, (start, end, _))| (*start, *end));
        let mut output = Vec::new();
        let mut last = 0;
        let mut positions = Vec::with_capacity(operations.len());
        for (_, (start, end, text)) in operations {
            output.extend_from_slice(&units[last..start as usize]);
            output.extend(text.encode_utf16());
            positions.push((start, end, output.len() as u32));
            last = end as usize;
        }
        output.extend_from_slice(&units[last..]);
        let map = |position: u32| {
            let index = positions.partition_point(|(start, _, _)| *start <= position);
            if index == 0 {
                position
            } else {
                let (_, end, after) = positions[index - 1];
                after + position.saturating_sub(end)
            }
        };
        let mut ranges: Vec<_> = self
            .ranges
            .iter()
            .enumerate()
            .filter(|(index, _)| !losing.contains(index))
            .map(|(_, (start, end, direction))| (map(*start), map(*end), direction.clone()))
            .collect();
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
        self.vertical = None;
        self.added = None;
        self.line_edge = None;
        self.query = None;
        self.map_auto_closed(&positions, &output);
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

    pub fn block_comments(&mut self, input: &web_sys::HtmlTextAreaElement) -> Option<NativeUndo> {
        let original = selection(input);
        if self.ranges.first() != Some(&original) {
            return None;
        }
        let before = input.value();
        let units: Vec<_> = before.encode_utf16().collect();
        let commands: Vec<_> = self
            .ranges
            .iter()
            .map(|(start, end, _)| block_comment_edits(&units, *start as usize, *end as usize))
            .collect();
        let losing = conflicting_cursors(
            commands
                .iter()
                .enumerate()
                .flat_map(|(index, command)| {
                    command
                        .edits
                        .iter()
                        .map(move |(start, end, _)| (index, *start, *end))
                })
                .collect(),
        );
        let mut operations: Vec<_> = commands
            .iter()
            .enumerate()
            .filter(|(index, _)| !losing.contains(index))
            .flat_map(|(_, command)| command.edits.iter())
            .collect();
        operations.sort_by_key(|(start, end, _)| (*start, *end));
        let mut output = Vec::new();
        let mut last = 0;
        let mut positions = Vec::with_capacity(operations.len());
        for (start, end, text) in operations {
            output.extend_from_slice(&units[last..*start as usize]);
            output.extend(text.encode_utf16());
            positions.push((*start, *end, output.len() as u32));
            last = *end as usize;
        }
        output.extend_from_slice(&units[last..]);
        let map = |position: u32, after_insertion: bool| {
            let index = positions.partition_point(|(start, _, _)| {
                *start < position || after_insertion && *start == position
            });
            if index == 0 {
                position
            } else {
                let (_, end, after) = positions[index - 1];
                after + position.saturating_sub(end)
            }
        };
        let mut ranges: Vec<_> = commands
            .iter()
            .enumerate()
            .filter(|(index, _)| !losing.contains(index))
            .map(|(_, command)| {
                let (start, end) = if let Some(offset) = command.empty_caret {
                    let position = map(command.start, false) + offset;
                    (position, position)
                } else {
                    (map(command.start, true), map(command.end, false))
                };
                (start, end, "forward".to_owned())
            })
            .collect();
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
        self.vertical = None;
        self.added = None;
        self.line_edge = None;
        self.query = None;
        self.typing = false;
        self.starts_typing = false;
        self.map_auto_closed(&positions, &output);
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

    pub fn run_command(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        command: super::commands::Command,
        json: bool,
    ) -> Option<NativeUndo> {
        if command.is_line_action() {
            return self.transform_lines(input, command);
        }
        let before = input.value();
        if self.ranges.first() != Some(&selection(input)) {
            return None;
        }
        let text = js_sys::JsString::from(before.as_str());
        let mut changed = false;
        let replacements: Vec<_> = self
            .ranges
            .iter()
            .map(|range| {
                let (start, end) = if range.0 == range.1 {
                    word_at(&before, range.0, json).unwrap_or((range.0, range.1))
                } else {
                    (range.0, range.1)
                };
                let original = text.slice(start, end).as_string().unwrap_or_default();
                let replacement = command.transform(&original);
                changed |= replacement != original;
                (start, end, replacement)
            })
            .collect();
        if !changed {
            return None;
        }
        let mut order: Vec<_> = replacements.iter().enumerate().collect();
        order.sort_by_key(|(_, (start, end, _))| (*start, *end));
        let mut ranges = self.ranges.clone();
        let mut delta = 0_i64;
        let mut previous_end = 0_u32;
        for (index, (start, end, replacement)) in order {
            if *start < previous_end {
                return None;
            }
            let length = replacement.encode_utf16().count() as u32;
            let new_start = (*start as i64 + delta) as u32;
            let range = &mut ranges[index];
            if range.0 == range.1 {
                let offset = if range.0 == *end {
                    length
                } else {
                    (range.0 - start).min(length)
                };
                range.0 = new_start + offset;
                range.1 = range.0;
            } else {
                range.0 = new_start;
                range.1 = new_start + length;
            }
            delta += i64::from(length) - i64::from(end - start);
            previous_end = *end;
        }
        let mut operation = self.replace(input, replacements, false)?;
        normalize(&mut ranges);
        self.ranges = ranges;
        let primary = self.ranges[0].clone();
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        operation.after_selection = Some(primary);
        Some(operation)
    }

    fn transform_lines(
        &mut self,
        input: &web_sys::HtmlTextAreaElement,
        command: super::commands::Command,
    ) -> Option<NativeUndo> {
        use super::commands::Command;
        let before = input.value();
        if self.ranges.first() != Some(&selection(input)) || before.is_empty() {
            return None;
        }
        let lines: Vec<_> = before.split('\n').collect();
        let lengths: Vec<_> = lines
            .iter()
            .map(|line| line.encode_utf16().count() as u32)
            .collect();
        let mut starts = vec![0_u32];
        for length in &lengths[..lengths.len() - 1] {
            starts.push(starts.last()? + length + 1);
        }
        let line_at = |position| {
            starts
                .partition_point(|start| *start <= position)
                .saturating_sub(1)
        };
        let whole = self.ranges.len() == 1 && self.ranges[0].0 == self.ranges[0].1;
        let sorting = matches!(command, Command::SortAscending | Command::SortDescending);
        let original = self.ranges.clone();
        let mut scopes = Vec::with_capacity(original.len());
        let mut replacements = Vec::with_capacity(original.len());
        let mut unique_lengths = Vec::with_capacity(original.len());
        for range in &original {
            let (start, end) = if whole {
                (0, before.encode_utf16().count() as u32)
            } else {
                (range.0, range.1)
            };
            let first = line_at(start);
            let mut last = line_at(end);
            if command != Command::Deduplicate && end == starts[last] && last > first {
                last -= 1;
            }
            if command == Command::Reverse && last == lines.len() - 1 && lines[last].is_empty() {
                if last <= first {
                    return None;
                }
                last -= 1;
            }
            if sorting && first >= last {
                return None;
            }
            let selected = lines[first..=last].join("\n");
            let replacement = command.transform(&selected);
            // Monaco refuses the entire sort if any selected range cannot change.
            if sorting && replacement == selected {
                return None;
            }
            unique_lengths.push((
                replacement.split('\n').count(),
                replacement.rsplit('\n').next()?.encode_utf16().count() as u32,
            ));
            replacements.push((starts[first], starts[last] + lengths[last], replacement));
            scopes.push((first, last));
        }
        let losing = if sorting {
            conflicting_cursors(
                replacements
                    .iter()
                    .enumerate()
                    .map(|(owner, (start, end, _))| (owner, *start, *end))
                    .collect(),
            )
        } else {
            HashSet::new()
        };
        let owners: Vec<_> = (0..original.len())
            .filter(|index| !losing.contains(index))
            .collect();
        let mut ranges = original.clone();
        if sorting {
            let mut order = owners.clone();
            order.sort_by_key(|index| (replacements[*index].0, replacements[*index].1));
            let mut delta = 0_i64;
            for index in order {
                let (start, end, text) = &replacements[index];
                let length = text.encode_utf16().count() as u32;
                let map = |position: u32, at_start: bool| {
                    if position <= *start {
                        (i64::from(position) + delta) as u32
                    } else if position < *end {
                        (i64::from(*start) + delta) as u32 + if at_start { 0 } else { length }
                    } else {
                        (i64::from(position) + delta + i64::from(length) - i64::from(end - start))
                            as u32
                    }
                };
                let range = if whole {
                    (
                        0,
                        before.encode_utf16().count() as u32,
                        "forward".to_owned(),
                    )
                } else {
                    original[index].clone()
                };
                ranges[index] = (map(range.0, true), map(range.1, false), range.2);
                delta += i64::from(length) - i64::from(end - start);
            }
        }
        self.ranges = owners
            .iter()
            .map(|index| original[*index].clone())
            .collect();
        let replacements = replacements
            .into_iter()
            .enumerate()
            .filter_map(|(index, edit)| (!losing.contains(&index)).then_some(edit))
            .collect();
        let Some(mut operation) = self.replace(input, replacements, false) else {
            self.ranges = original;
            return None;
        };
        let after = input.value();
        let after_lengths: Vec<_> = after
            .split('\n')
            .map(|line| line.encode_utf16().count() as u32)
            .collect();
        let mut after_starts = vec![0_u32];
        for length in &after_lengths[..after_lengths.len() - 1] {
            after_starts.push(after_starts.last()? + length + 1);
        }
        let position = |line: usize, column: u32| {
            let line = line.min(after_lengths.len() - 1);
            after_starts[line] + column.min(after_lengths[line])
        };
        let mut deleted = 0_usize;
        for &index in &owners {
            let (first, last) = scopes[index];
            let range = &original[index];
            if command == Command::Reverse {
                let map = |offset| {
                    let line = line_at(offset);
                    let target = if line <= last {
                        last - line + first
                    } else {
                        line
                    };
                    position(target, offset - starts[line])
                };
                let (a, b) = (map(range.0), map(range.1));
                let backward = (a > b) != (range.2 == "backward");
                ranges[index] = (
                    a.min(b),
                    a.max(b),
                    if backward { "backward" } else { "forward" }.into(),
                );
            } else if command == Command::Deduplicate {
                if whole {
                    let line = line_at(range.0);
                    let cursor = position(line, range.0 - starts[line]);
                    ranges[index] = (cursor, cursor, "forward".into());
                } else {
                    let (count, last_length) = unique_lengths[index];
                    let first = first.saturating_sub(deleted);
                    ranges[index] = (
                        position(first, 0),
                        position(first + count - 1, last_length.saturating_sub(1)),
                        "forward".into(),
                    );
                }
                deleted += last - first + 1 - unique_lengths[index].0;
            }
        }
        self.ranges = owners
            .into_iter()
            .map(|index| ranges[index].clone())
            .collect();
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
        self.vertical = None;
        self.added = None;
        self.line_edge = None;
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
        self.map_auto_closed(&positions, &output);
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

    fn map_auto_closed(&mut self, positions: &[(u32, u32, u32)], output: &[u16]) {
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
        self.validate_pairs(output);
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
                        // A collapsed range may report adjacent font-run rects;
                        // they still represent one caret, not two cursors.
                        if start == end {
                            break;
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
