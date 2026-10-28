//! Cursor-only history, separate from native text Undo.
//!
//! Follows Monaco's cursorUndo controller (Microsoft, MIT); the full notice is
//! shipped in public/assets/translator-codicon-LICENSE.txt.

use super::multicursor::{selection, Selection};
use web_sys::HtmlTextAreaElement;

#[derive(Clone)]
struct Position {
    selections: Vec<Selection>,
    top: i32,
    left: i32,
}

impl Position {
    fn read(input: &HtmlTextAreaElement, ranges: &[Selection]) -> Self {
        let primary = selection(input);
        let mut selections = if ranges.first() == Some(&primary) {
            ranges.to_vec()
        } else {
            vec![primary]
        };
        for (start, end, direction) in &mut selections {
            *direction = if start != end && direction == "backward" {
                "backward"
            } else {
                "forward"
            }
            .into();
        }
        Self {
            selections,
            top: input.scroll_top(),
            left: input.scroll_left(),
        }
    }

    fn same_selection(&self, other: &Self) -> bool {
        self.selections == other.selections
    }
}

#[derive(Default)]
pub(super) struct CursorUndo {
    current: Option<Position>,
    entries: Vec<Position>,
}

impl CursorUndo {
    pub fn reset(&mut self, input: &HtmlTextAreaElement, ranges: &[Selection]) {
        self.entries.clear();
        self.current = Some(Position::read(input, ranges));
    }

    pub fn observe(&mut self, input: &HtmlTextAreaElement, ranges: &[Selection]) {
        let next = Position::read(input, ranges);
        if let Some(previous) = &self.current
            && !previous.same_selection(&next)
            && !self
                .entries
                .last()
                .is_some_and(|entry| entry.same_selection(previous))
        {
            self.entries.push(previous.clone());
            // The reference bounds cursor-only history to 50 selections.
            if self.entries.len() > 50 {
                self.entries.remove(0);
            }
        }
        self.current = Some(next);
    }

    pub fn undo(
        &mut self,
        input: &HtmlTextAreaElement,
        ranges: &[Selection],
    ) -> Option<Vec<Selection>> {
        self.observe(input, ranges);
        let previous = self.entries.pop()?;
        self.current = Some(previous.clone());
        let primary = &previous.selections[0];
        let _ = input.set_selection_range_with_direction(primary.0, primary.1, &primary.2);
        input.set_scroll_top(previous.top);
        input.set_scroll_left(previous.left);
        Some(previous.selections)
    }
}
