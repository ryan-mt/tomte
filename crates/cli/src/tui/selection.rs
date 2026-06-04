//! Left-drag text selection over the rendered screen.
//!
//! The TUI captures the mouse (for scroll + click targets), which suppresses
//! the terminal's own drag-to-select — so without this the only way to copy
//! text was to hold Shift to bypass the capture. This re-implements selection
//! in-app: a plain left-drag highlights a linear (terminal-style) span of the
//! rendered cells and copies it to the clipboard on release, no Shift needed.
//!
//! Geometry only — the App owns the state and the event wiring; `ui` draws the
//! highlight; `clipboard` does the copy.
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use unicode_width::UnicodeWidthStr;

/// Selection highlight background — a muted blue, consistent with the truecolor
/// palette the TUI already assumes elsewhere.
const SELECTION_BG: Color = crate::tui::palette::SELECTION_BG;

/// A left-drag selection in terminal cell coordinates. `anchor` is where the
/// drag started, `cursor` where it currently is (or ended).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
}

impl Selection {
    /// Begin a selection at a single cell (anchor == cursor).
    pub fn new(col: u16, row: u16) -> Self {
        Self {
            anchor: (col, row),
            cursor: (col, row),
        }
    }

    /// Whether the cursor has moved off the anchor — i.e. a real drag rather
    /// than a plain click (which the caller treats as a click target instead).
    pub fn is_dragged(&self) -> bool {
        self.anchor != self.cursor
    }

    /// The two endpoints ordered top-to-bottom, then left-to-right, so a drag
    /// in any direction normalizes to (start, end).
    fn ends(&self) -> ((u16, u16), (u16, u16)) {
        // Compare by (row, col) so rows dominate, matching reading order.
        let key = |(c, r): (u16, u16)| (r, c);
        if key(self.anchor) <= key(self.cursor) {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }
}

/// Column range `[from, to]` (inclusive, clamped to `area`) selected on `row`,
/// or `None` if the row is outside the selection or the area.
fn row_span(sel: &Selection, row: u16, area: Rect) -> Option<(u16, u16)> {
    let ((sc, sr), (ec, er)) = sel.ends();
    if row < sr || row > er || row < area.top() || row >= area.bottom() {
        return None;
    }
    let last_col = area.right().saturating_sub(1);
    let from = if row == sr { sc } else { area.left() }.max(area.left());
    let to = if row == er { ec } else { last_col }.min(last_col);
    (from <= to).then_some((from, to))
}

/// Read the selected text out of a rendered buffer, terminal-style: trailing
/// whitespace trimmed per line, rows joined with `\n`.
pub fn extract_text(buf: &Buffer, sel: &Selection) -> String {
    let area = buf.area;
    let ((_, first_row), (_, er)) = sel.ends();
    let last_row = er.min(area.bottom().saturating_sub(1));
    let mut lines: Vec<String> = Vec::new();
    for row in first_row..=last_row {
        let Some((from, to)) = row_span(sel, row, area) else {
            continue;
        };
        let mut line = String::new();
        let mut col = from;
        while col <= to {
            let Some(cell) = buf.cell(Position::new(col, row)) else {
                col = col.saturating_add(1);
                continue;
            };
            let sym = cell.symbol();
            line.push_str(sym);
            // A wide grapheme occupies its cell plus continuation cells, which
            // ratatui fills with a space; skip them so the copied text doesn't
            // gain a spurious space after each wide char.
            col = col.saturating_add(UnicodeWidthStr::width(sym).max(1) as u16);
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

/// Paint the selection highlight over the already-rendered cells within `area`.
pub fn highlight(buf: &mut Buffer, sel: &Selection, area: Rect) {
    let (_, (_, er)) = sel.ends();
    let bottom = er.min(area.bottom().saturating_sub(1));
    for row in sel.ends().0 .1..=bottom {
        let Some((from, to)) = row_span(sel, row, area) else {
            continue;
        };
        for col in from..=to {
            if let Some(cell) = buf.cell_mut(Position::new(col, row)) {
                cell.set_bg(SELECTION_BG);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf() -> Buffer {
        Buffer::with_lines(vec!["hello world", "second line"])
    }

    #[test]
    fn extract_single_row() {
        let sel = Selection {
            anchor: (0, 0),
            cursor: (4, 0),
        };
        assert_eq!(extract_text(&buf(), &sel), "hello");
    }

    #[test]
    fn extract_multi_row_is_linear() {
        // "world" (from col 6 of row 0) through "second" (to col 5 of row 1).
        let sel = Selection {
            anchor: (6, 0),
            cursor: (5, 1),
        };
        assert_eq!(extract_text(&buf(), &sel), "world\nsecond");
    }

    #[test]
    fn extract_normalizes_a_right_to_left_drag() {
        let b = Buffer::with_lines(vec!["abcdef"]);
        let sel = Selection {
            anchor: (4, 0),
            cursor: (1, 0),
        };
        assert_eq!(extract_text(&b, &sel), "bcde");
    }

    #[test]
    fn extract_trims_trailing_padding() {
        // Selecting past the end of a short line drops the padding spaces.
        let sel = Selection {
            anchor: (0, 0),
            cursor: (10, 0),
        };
        assert_eq!(extract_text(&buf(), &sel), "hello world");
    }

    #[test]
    fn row_span_marks_the_linear_span() {
        let area = Rect::new(0, 0, 80, 5);
        let sel = Selection {
            anchor: (2, 0),
            cursor: (1, 2),
        };
        // Start row: from the anchor column to the row's end.
        assert_eq!(row_span(&sel, 0, area), Some((2, 79)));
        // Middle row: the whole row.
        assert_eq!(row_span(&sel, 1, area), Some((0, 79)));
        // End row: from the row's start up to the cursor column.
        assert_eq!(row_span(&sel, 2, area), Some((0, 1)));
        // Outside the selection's rows.
        assert_eq!(row_span(&sel, 3, area), None);
    }

    #[test]
    fn extract_skips_wide_char_continuation_cells() {
        // ratatui stores a 2-cell-wide grapheme in the first cell and fills the
        // continuation cell with a space; reading every column verbatim would
        // inject a spurious space after each wide char.
        let b = Buffer::with_lines(vec!["a🦀b"]);
        let sel = Selection {
            anchor: (0, 0),
            cursor: (3, 0),
        };
        assert_eq!(extract_text(&b, &sel), "a🦀b");
    }

    #[test]
    fn is_dragged_distinguishes_click_from_drag() {
        assert!(!Selection::new(3, 4).is_dragged());
        assert!(Selection {
            anchor: (3, 4),
            cursor: (5, 4)
        }
        .is_dragged());
    }
}
