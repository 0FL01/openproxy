//! Cursor-only history, separate from native text Undo.
//!
//! Follows Monaco's cursorUndo controller (Microsoft, MIT); the full notice is
//! shipped in public/assets/translator-codicon-LICENSE.txt.

use web_sys::HtmlTextAreaElement;

#[derive(Clone)]
struct Position {
    start: u32,
    end: u32,
    backward: bool,
    top: i32,
    left: i32,
}

impl Position {
    fn read(input: &HtmlTextAreaElement) -> Option<Self> {
        Some(Self {
            start: input.selection_start().ok().flatten()?,
            end: input.selection_end().ok().flatten()?,
            backward: input.selection_direction().ok().flatten().as_deref() == Some("backward"),
            top: input.scroll_top(),
            left: input.scroll_left(),
        })
    }

    fn same_selection(&self, other: &Self) -> bool {
        (self.start, self.end, self.backward) == (other.start, other.end, other.backward)
    }
}

#[derive(Default)]
pub(super) struct CursorUndo {
    current: Option<Position>,
    entries: Vec<Position>,
}

impl CursorUndo {
    pub fn reset(&mut self, input: &HtmlTextAreaElement) {
        self.entries.clear();
        self.current = Position::read(input);
    }

    pub fn observe(&mut self, input: &HtmlTextAreaElement) {
        let Some(next) = Position::read(input) else {
            return;
        };
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

    pub fn undo(&mut self, input: &HtmlTextAreaElement) {
        self.observe(input);
        let Some(previous) = self.entries.pop() else {
            return;
        };
        self.current = Some(previous.clone());
        let direction = if previous.backward {
            "backward"
        } else {
            "forward"
        };
        let _ = input.set_selection_range_with_direction(previous.start, previous.end, direction);
        input.set_scroll_top(previous.top);
        input.set_scroll_left(previous.left);
    }
}
