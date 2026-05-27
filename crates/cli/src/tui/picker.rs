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

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

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
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                it.title.to_lowercase().contains(&q)
                    || it.key.to_lowercase().contains(&q)
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

pub fn render(f: &mut Frame, anchor_area: Rect, picker: &Picker) {
    let visible = picker.filtered_indices();
    let item_count = visible.len().max(1);
    let height = (item_count as u16).min(10) + 2; // borders
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

    let dim = Style::default().fg(Color::Rgb(170, 170, 170));
    let bg = Style::default().bg(Color::Rgb(20, 20, 22));
    let sel_bg = Style::default()
        .bg(Color::Rgb(30, 60, 50))
        .fg(Color::Rgb(255, 255, 255))
        .add_modifier(Modifier::BOLD);
    let title = format!(" {} ", picker.title);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(80, 200, 160)))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Rgb(25, 195, 154))
                .add_modifier(Modifier::BOLD),
        ))
        .style(bg);
    let inner_rect = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    if visible.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches)",
            dim,
        )));
    } else {
        for &idx in visible.iter().take(10) {
            let it = &picker.items[idx];
            let is_sel = idx == picker.selected
                || (!visible.contains(&picker.selected) && Some(&idx) == visible.first());
            let marker = if is_sel { "▌ " } else { "  " };
            let title_style = if is_sel {
                sel_bg
            } else {
                Style::default().fg(Color::Rgb(235, 235, 235))
            };
            let desc_style = if is_sel {
                Style::default()
                    .fg(Color::Rgb(190, 230, 215))
                    .bg(sel_bg.bg.unwrap_or(Color::Reset))
            } else {
                dim
            };
            let mut spans = vec![
                Span::styled(marker.to_string(), title_style),
                Span::styled(it.title.clone(), title_style),
            ];
            if !it.description.is_empty() {
                spans.push(Span::styled(format!("  {}", it.description), desc_style));
            }
            lines.push(Line::from(spans));
        }
    }

    let para = Paragraph::new(lines).style(bg);
    f.render_widget(para, inner_rect);
}

// ============ predefined item builders ============

pub fn slash_commands() -> Vec<PickerItem> {
    vec![
        PickerItem {
            key: "model".into(),
            title: "/model".into(),
            description: "change the model".into(),
        },
        PickerItem {
            key: "thinking".into(),
            title: "/thinking".into(),
            description: "change reasoning effort".into(),
        },
        PickerItem {
            key: "effort".into(),
            title: "/effort".into(),
            description: "alias for /thinking".into(),
        },
        PickerItem {
            key: "verbosity".into(),
            title: "/verbosity".into(),
            description: "change output verbosity".into(),
        },
        PickerItem {
            key: "login".into(),
            title: "/login".into(),
            description: "sign in with ChatGPT".into(),
        },
        PickerItem {
            key: "apikey".into(),
            title: "/apikey".into(),
            description: "save an OpenAI API key".into(),
        },
        PickerItem {
            key: "logout".into(),
            title: "/logout".into(),
            description: "clear credentials".into(),
        },
        PickerItem {
            key: "status".into(),
            title: "/status".into(),
            description: "show auth status".into(),
        },
        PickerItem {
            key: "img".into(),
            title: "/img".into(),
            description: "attach an image to next message".into(),
        },
        PickerItem {
            key: "cwd".into(),
            title: "/cwd".into(),
            description: "show / set working directory".into(),
        },
        PickerItem {
            key: "clear".into(),
            title: "/clear".into(),
            description: "clear the conversation".into(),
        },
        PickerItem {
            key: "help".into(),
            title: "/help".into(),
            description: "list all commands".into(),
        },
        PickerItem {
            key: "plan".into(),
            title: "/plan".into(),
            description: "enter plan mode (read-only tools)".into(),
        },
        PickerItem {
            key: "normal".into(),
            title: "/normal".into(),
            description: "leave plan mode".into(),
        },
        PickerItem {
            key: "quit".into(),
            title: "/quit".into(),
            description: "exit opencli".into(),
        },
    ]
}

pub fn models() -> Vec<PickerItem> {
    vec![
        PickerItem {
            key: "gpt-5.5".into(),
            title: "gpt-5.5".into(),
            description: "frontier · 1M context · default".into(),
        },
        PickerItem {
            key: "gpt-5.5-pro".into(),
            title: "gpt-5.5-pro".into(),
            description: "more compute for hard problems".into(),
        },
        PickerItem {
            key: "gpt-5.4".into(),
            title: "gpt-5.4".into(),
            description: "frontier · tool search".into(),
        },
        PickerItem {
            key: "gpt-5.4-pro".into(),
            title: "gpt-5.4-pro".into(),
            description: "5.4 with more compute".into(),
        },
        PickerItem {
            key: "gpt-5.4-mini".into(),
            title: "gpt-5.4-mini".into(),
            description: "fast · cheaper".into(),
        },
        PickerItem {
            key: "gpt-5.4-nano".into(),
            title: "gpt-5.4-nano".into(),
            description: "latency-sensitive".into(),
        },
    ]
}

pub fn efforts() -> Vec<PickerItem> {
    vec![
        PickerItem {
            key: "none".into(),
            title: "none".into(),
            description: "no extra reasoning, fastest".into(),
        },
        PickerItem {
            key: "low".into(),
            title: "low".into(),
            description: "light reasoning · latency-sensitive".into(),
        },
        PickerItem {
            key: "medium".into(),
            title: "medium".into(),
            description: "balanced · default".into(),
        },
        PickerItem {
            key: "high".into(),
            title: "high".into(),
            description: "deep reasoning for hard tasks".into(),
        },
        PickerItem {
            key: "xhigh".into(),
            title: "xhigh".into(),
            description: "hardest async / eval workloads".into(),
        },
    ]
}

pub fn verbosities() -> Vec<PickerItem> {
    vec![
        PickerItem {
            key: "low".into(),
            title: "low".into(),
            description: "concise output".into(),
        },
        PickerItem {
            key: "medium".into(),
            title: "medium".into(),
            description: "default".into(),
        },
        PickerItem {
            key: "high".into(),
            title: "high".into(),
            description: "verbose output".into(),
        },
    ]
}
