//! Generic vertical picker used as an overlay above the chat.
//!
//! Multiple use cases:
//!  - Slash command picker (triggered by typing `/` on an empty input)
//!  - Model picker (`/model`)
//!  - Reasoning effort picker (`/thinking` or `/effort`)
//!
//! Selection model: items are filtered by `query` (substring match on title)
//! and rendered with the active item highlighted. Arrow keys navigate; Enter
//! commits the selection.

use crate::tui::palette;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

mod items;
pub use items::{
    efforts, logout_targets, models, rewind_points, sessions, slash_commands, verbosities,
};

pub struct PickerItem {
    pub key: String,         // identifier returned on select
    pub title: String,       // primary line, e.g. "gpt-5.5"
    pub description: String, // secondary line (gray)
}

pub struct Picker {
    pub title: String,
    pub items: Vec<PickerItem>,
    pub selected: usize,
    pub query: String, // for slash picker — text after the leading "/"
}

impl Picker {
    pub fn new(title: impl Into<String>, items: Vec<PickerItem>) -> Self {
        Self {
            title: title.into(),
            items,
            selected: 0,
            query: String::new(),
        }
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        if q.is_empty() {
            return (0..self.items.len()).collect();
        }
        // Description matches too, so typing what a command DOES finds it
        // (`/eff` by name, but also "reasoning" finding the effort picker row).
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                it.title.to_lowercase().contains(&q)
                    || it.key.to_lowercase().contains(&q)
                    || it.description.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn move_up(&mut self) {
        let visible = self.filtered_indices();
        if visible.is_empty() {
            return;
        }
        let pos = visible
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        let new_pos = if pos == 0 { visible.len() - 1 } else { pos - 1 };
        self.selected = visible[new_pos];
    }

    pub fn move_down(&mut self) {
        let visible = self.filtered_indices();
        if visible.is_empty() {
            return;
        }
        let pos = visible
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        let new_pos = (pos + 1) % visible.len();
        self.selected = visible[new_pos];
    }

    pub fn ensure_visible_selected(&mut self) {
        let visible = self.filtered_indices();
        if visible.is_empty() {
            return;
        }
        if !visible.contains(&self.selected) {
            self.selected = visible[0];
        }
    }

    pub fn selected_key(&self) -> Option<String> {
        let visible = self.filtered_indices();
        if !visible.contains(&self.selected) {
            visible.first().map(|i| self.items[*i].key.clone())
        } else {
            Some(self.items[self.selected].key.clone())
        }
    }
}

/// Half-open range `[start, end)` of the visible item list to draw, scrolled so
/// position `sel_pos` stays on screen. Stays at the top until the selection
/// passes the last row, then scrolls one row at a time. Clamped to `len`.
fn scroll_window(sel_pos: usize, len: usize, max_rows: usize) -> (usize, usize) {
    let start = sel_pos.saturating_sub(max_rows.saturating_sub(1));
    let end = (start + max_rows).min(len);
    (start, end)
}

pub fn render(f: &mut Frame, anchor_area: Rect, picker: &Picker) {
    let visible = picker.filtered_indices();
    // Cap the popup at MAX_ROWS rows; a longer list scrolls (see the window
    // math below) so the selected row always stays on screen.
    const MAX_ROWS: usize = 10;
    let item_count = visible.len().max(1);
    let height = (item_count as u16).min(MAX_ROWS as u16) + 2; // borders
    let width = 60u16.min(anchor_area.width.saturating_sub(4));
    // Anchor above the input area: bottom-left of popup just above anchor_area.
    let x = anchor_area.x + 1;
    let bottom = anchor_area.y; // popup's bottom row sits one above this
    let y = bottom.saturating_sub(height);

    let popup = Rect {
        x,
        y,
        width,
        height,
    };

    // Clear the area under the popup so it floats over content.
    f.render_widget(Clear, popup);

    let dim = Style::default().fg(palette::TEXT_MUTED);
    let bg = Style::default().bg(palette::SURFACE);
    let sel_bg = Style::default()
        .bg(palette::ACCENT_DEEP)
        .fg(palette::TEXT_BRIGHT)
        .add_modifier(Modifier::BOLD);
    let title = format!(" {} ", picker.title);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette::ACCENT))
        .title(Span::styled(
            title,
            Style::default()
                .fg(palette::ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
        .style(bg);
    let inner_rect = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    if visible.is_empty() {
        lines.push(Line::from(Span::styled("  (no matches)", dim)));
    } else {
        // Scroll so the selected row is always on screen. render() is stateless,
        // so derive the window start from `selected` each frame: stay at the top
        // until the selection passes the last visible row, then scroll one row at
        // a time (selected pinned to the bottom row). Previously this always drew
        // the first MAX_ROWS items, so the highlight vanished below the fold when
        // scrolling down a list longer than MAX_ROWS.
        let sel_pos = visible
            .iter()
            .position(|&i| i == picker.selected)
            .unwrap_or(0);
        let (start, end) = scroll_window(sel_pos, visible.len(), MAX_ROWS);
        for &idx in &visible[start..end] {
            let it = &picker.items[idx];
            let is_sel = idx == picker.selected
                || (!visible.contains(&picker.selected) && Some(&idx) == visible.first());
            let marker = if is_sel { "▌ " } else { "  " };
            let title_style = if is_sel {
                sel_bg
            } else {
                Style::default().fg(palette::TEXT_BRIGHT)
            };
            let desc_style = if is_sel {
                Style::default()
                    .fg(palette::TEXT)
                    .bg(sel_bg.bg.unwrap_or(Color::Reset))
            } else {
                dim
            };
            // Budget the row to the popup's inner width: the title is never
            // cut below its own length, the description absorbs the squeeze
            // with an ellipsis (it used to clip dead at the border).
            let inner_w = inner_rect.width as usize;
            let marker_w = 2usize;
            let title = crate::tui::ui::truncate_to_width(
                &it.title,
                inner_w.saturating_sub(marker_w).max(1),
            );
            let title_w = unicode_width::UnicodeWidthStr::width(title.as_str());
            let mut spans = vec![
                Span::styled(marker.to_string(), title_style),
                Span::styled(title, title_style),
            ];
            if !it.description.is_empty() {
                let desc_budget = inner_w.saturating_sub(marker_w + title_w + 2);
                let desc = crate::tui::ui::truncate_to_width(&it.description, desc_budget);
                if !desc.is_empty() {
                    spans.push(Span::styled(format!("  {desc}"), desc_style));
                }
            }
            lines.push(Line::from(spans));
        }
    }

    let para = Paragraph::new(lines).style(bg);
    f.render_widget(para, inner_rect);
}

#[cfg(test)]
mod filter_tests {
    use super::{Picker, PickerItem};

    #[test]
    fn query_matches_description_too() {
        let picker = Picker {
            title: "t".into(),
            items: vec![
                PickerItem {
                    key: "effort".into(),
                    title: "effort".into(),
                    description: "pick the reasoning depth".into(),
                },
                PickerItem {
                    key: "model".into(),
                    title: "model".into(),
                    description: "switch the active model".into(),
                },
            ],
            selected: 0,
            query: "reasoning".into(),
        };
        assert_eq!(picker.filtered_indices(), vec![0]);
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::scroll_window;

    #[test]
    fn short_list_shows_everything() {
        assert_eq!(scroll_window(0, 5, 10), (0, 5));
        assert_eq!(scroll_window(4, 5, 10), (0, 5));
    }

    #[test]
    fn window_stays_at_top_until_selection_passes_last_row() {
        // 14 items, 10 rows: selecting within the first window keeps start at 0.
        assert_eq!(scroll_window(0, 14, 10), (0, 10));
        assert_eq!(scroll_window(9, 14, 10), (0, 10));
    }

    #[test]
    fn window_scrolls_to_follow_selection() {
        // Regression: scrolling past the last visible row must move the window so
        // the highlighted row stays on screen (it used to fall below the fold).
        assert_eq!(scroll_window(10, 14, 10), (1, 11));
        assert_eq!(scroll_window(13, 14, 10), (4, 14));
    }
}
