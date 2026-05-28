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
                it.title.to_lowercase().contains(&q) || it.key.to_lowercase().contains(&q)
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
        lines.push(Line::from(Span::styled("  (no matches)", dim)));
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
    fn item(key: &str, title: &str, desc: &str) -> PickerItem {
        PickerItem {
            key: key.into(),
            title: title.into(),
            description: desc.into(),
        }
    }
    vec![
        item("help", "/help", "list all commands"),
        item("model", "/model", "change the model"),
        item("thinking", "/thinking", "change reasoning effort"),
        item("effort", "/effort", "alias for /thinking"),
        item("verbosity", "/verbosity", "change output verbosity"),
        item("cost", "/cost", "show token usage and estimated cost"),
        item("config", "/config", "show current configuration"),
        item("hooks", "/hooks", "list configured PreToolUse hooks"),
        item("mcp", "/mcp", "list configured MCP servers"),
        item("init", "/init", "create CLAUDE.md for this project"),
        item("memory", "/memory", "show CLAUDE.md"),
        item("diff", "/diff", "show `git diff` for the working tree"),
        item(
            "review",
            "/review",
            "ask the agent to review uncommitted changes",
        ),
        item("export", "/export", "save conversation as markdown"),
        item(
            "compact",
            "/compact",
            "ask the agent to compact the conversation",
        ),
        item("todos", "/todos", "show the session todo list"),
        item("about", "/about", "show opencli version + build info"),
        item("login", "/login", "sign in with ChatGPT"),
        item("apikey", "/apikey", "save an OpenAI API key"),
        item("logout", "/logout", "clear credentials"),
        item("status", "/status", "show auth status"),
        item("img", "/img", "attach an image to next message"),
        item("cwd", "/cwd", "show / set working directory"),
        item("clear", "/clear", "clear the conversation"),
        item("resume", "/resume", "pick a previous session to continue"),
        item("plan", "/plan", "enter plan mode (read-only tools)"),
        item("normal", "/normal", "leave plan mode"),
        item(
            "perms",
            "/perms",
            "toggle the approval modal for writes/shell",
        ),
        item("undo", "/undo", "revert the most recent file edit"),
        item("quit", "/quit", "exit opencli"),
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

/// Build picker items from a snapshot of stored sessions for the current cwd.
/// Newest first, with a single-line preview shaped like the slash command rows.
pub fn sessions(metas: &[opencli_core::session::SessionMeta]) -> Vec<PickerItem> {
    metas
        .iter()
        .map(|m| PickerItem {
            key: m.id.clone(),
            title: m.preview.clone(),
            description: format!(
                "{}  ·  {} msgs  ·  {}",
                ago(m.updated_at_ms),
                m.message_count,
                m.model
            ),
        })
        .collect()
}

fn ago(ms: u64) -> String {
    let now = opencli_core::session::now_ms();
    let diff = now.saturating_sub(ms);
    let secs = diff / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
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
