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
        item("agents", "/agents", "list installed subagents"),
        item("skills", "/skills", "list installed skills"),
        item(
            "commands",
            "/commands",
            "list installed custom slash commands",
        ),
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

/// Build the model picker dynamically from the providers the user is
/// currently signed in to. When only OpenAI creds exist the user sees the
/// GPT catalogue; after `opencli login --provider anthropic` the Claude
/// models appear alongside (or instead of) the GPT ones. After `logout`
/// nothing is signed in and the picker shows a single offline placeholder.
pub fn models() -> Vec<PickerItem> {
    use opencli_core::auth::signed_in_providers;
    use opencli_core::provider::Provider;

    let mut items = Vec::new();
    for p in signed_in_providers() {
        match p {
            Provider::OpenAi => {
                items.extend([
                    PickerItem {
                        key: "gpt-5.5".into(),
                        title: "gpt-5.5".into(),
                        description: "frontier · default".into(),
                    },
                    PickerItem {
                        key: "gpt-5.4".into(),
                        title: "gpt-5.4".into(),
                        description: "previous frontier · stable".into(),
                    },
                    PickerItem {
                        key: "gpt-5.3".into(),
                        title: "gpt-5.3".into(),
                        description: "older frontier".into(),
                    },
                    PickerItem {
                        key: "gpt-5-pro".into(),
                        title: "gpt-5-pro".into(),
                        description: "more compute for hard problems".into(),
                    },
                    PickerItem {
                        key: "gpt-5-codex".into(),
                        title: "gpt-5-codex".into(),
                        description: "code-specialised · used via ChatGPT Codex".into(),
                    },
                    PickerItem {
                        key: "gpt-5-mini".into(),
                        title: "gpt-5-mini".into(),
                        description: "fast · cheaper".into(),
                    },
                    PickerItem {
                        key: "gpt-5-nano".into(),
                        title: "gpt-5-nano".into(),
                        description: "latency-sensitive".into(),
                    },
                ]);
            }
            Provider::Anthropic => {
                items.extend([
                    PickerItem {
                        key: "claude-opus-4-8".into(),
                        title: "claude-opus-4-8".into(),
                        description: "frontier · most capable".into(),
                    },
                    PickerItem {
                        key: "claude-opus-4-7".into(),
                        title: "claude-opus-4-7".into(),
                        description: "previous frontier · long-running agents".into(),
                    },
                    PickerItem {
                        key: "claude-opus-4-6".into(),
                        title: "claude-opus-4-6".into(),
                        description: "frontier · previous opus".into(),
                    },
                    PickerItem {
                        key: "claude-opus-4-5".into(),
                        title: "claude-opus-4-5".into(),
                        description: "max intelligence · practical".into(),
                    },
                    PickerItem {
                        key: "claude-sonnet-4-6".into(),
                        title: "claude-sonnet-4-6".into(),
                        description: "best speed/intelligence balance".into(),
                    },
                    PickerItem {
                        key: "claude-sonnet-4-5".into(),
                        title: "claude-sonnet-4-5".into(),
                        description: "high-perf agents · coding".into(),
                    },
                    PickerItem {
                        key: "claude-haiku-4-5".into(),
                        title: "claude-haiku-4-5".into(),
                        description: "fastest · near-frontier".into(),
                    },
                ]);
            }
        }
    }
    // Tag every model with its context window so 1M vs 200K is visible at a
    // glance in the picker (mirrors the textual catalogue). Done before the
    // not-signed-in placeholder below so that placeholder stays untagged.
    for it in &mut items {
        let win = opencli_core::agent::context_window_label(&it.key);
        it.description = format!("{win} ctx · {}", it.description);
    }
    if items.is_empty() {
        items.push(PickerItem {
            key: "gpt-5.5".into(),
            title: "(not signed in)".into(),
            description: "run `/login` to choose a provider".into(),
        });
    }
    items
}

/// Build the logout picker from the credentials actually stored in auth.json.
/// Env-var credentials are intentionally omitted — they aren't stored here and
/// can't be cleared by logging out. An "all" entry appears only when more than
/// one credential is stored.
pub fn logout_targets() -> Vec<PickerItem> {
    use opencli_core::auth::{load_auth, LogoutTarget};
    let r = load_auth().unwrap_or_default();
    let mut items = Vec::new();
    let item = |t: LogoutTarget, title: &str, desc: &str| PickerItem {
        key: t.key().into(),
        title: title.into(),
        description: desc.into(),
    };
    if r.tokens
        .as_ref()
        .is_some_and(|t| !t.access_token.is_empty())
    {
        items.push(item(
            LogoutTarget::OpenAiOauth,
            "OpenAI — ChatGPT OAuth",
            "sign out of the ChatGPT subscription token",
        ));
    }
    if r.api_key.as_ref().is_some_and(|k| !k.is_empty()) {
        items.push(item(
            LogoutTarget::OpenAiApiKey,
            "OpenAI — API key",
            "remove the stored OpenAI API key",
        ));
    }
    if r.anthropic_tokens
        .as_ref()
        .is_some_and(|t| !t.access_token.is_empty())
    {
        items.push(item(
            LogoutTarget::AnthropicOauth,
            "Anthropic — Claude Pro/Max OAuth",
            "sign out of the Claude subscription token",
        ));
    }
    if r.anthropic_api_key.as_ref().is_some_and(|k| !k.is_empty()) {
        items.push(item(
            LogoutTarget::AnthropicApiKey,
            "Anthropic — API key",
            "remove the stored Anthropic API key",
        ));
    }
    if items.len() > 1 {
        items.push(item(
            LogoutTarget::All,
            "All credentials",
            "clear every stored credential",
        ));
    }
    items
}

pub fn efforts() -> Vec<PickerItem> {
    vec![
        PickerItem {
            key: "none".into(),
            title: "none".into(),
            description: "no extra reasoning, fastest".into(),
        },
        PickerItem {
            key: "minimal".into(),
            title: "minimal".into(),
            description: "GPT-5 minimal · Claude: same as none".into(),
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
            description: "GPT-5 xhigh · Claude Opus 4.7 xhigh".into(),
        },
        PickerItem {
            key: "max".into(),
            title: "max".into(),
            description: "Claude adaptive max — top thinking tier".into(),
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
