//! Onboarding / login screen — first screen when the user is not authenticated.
//!
//! UX mirrors Codex CLI:
//!   ▸ welcome blurb
//!   ▸ pick mode: Sign in with ChatGPT / Provide your own API key
//!   ▸ on ChatGPT: shows the auth URL + "Finish signing in via your browser…"
//!   ▸ on API key: a single-line text input
//!   ▸ on success: closes login and hands control back to the chat screen
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use opencli_core::auth::{self, AuthMode};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::Mutex;

use super::input::TextInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Option_ {
    ChatGpt,
    ApiKey,
}

#[derive(Debug, Clone)]
pub enum Stage {
    PickMode,
    WaitingForBrowser { url: String },
    ApiKeyEntry,
    Success(AuthMode),
    Cancelled,
}

pub struct LoginScreen {
    pub stage: Arc<Mutex<Stage>>,
    pub selected: Option_,
    pub api_input: TextInput,
    pub error: Arc<Mutex<Option<String>>>,
}

impl LoginScreen {
    pub fn new() -> Self {
        Self {
            stage: Arc::new(Mutex::new(Stage::PickMode)),
            selected: Option_::ChatGpt,
            api_input: TextInput::default(),
            error: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn stage(&self) -> Stage {
        self.stage.lock().await.clone()
    }

    pub async fn error_text(&self) -> Option<String> {
        self.error.lock().await.clone()
    }

    /// Handle a key event. Returns Ok(true) when the screen is finished.
    pub async fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            *self.stage.lock().await = Stage::Cancelled;
            return Ok(true);
        }
        let stage = self.stage().await;
        match stage {
            Stage::PickMode => self.handle_pick(key).await,
            Stage::WaitingForBrowser { .. } => {
                if key.code == KeyCode::Esc {
                    *self.stage.lock().await = Stage::PickMode;
                }
                Ok(false)
            }
            Stage::ApiKeyEntry => self.handle_api_key(key).await,
            Stage::Success(_) | Stage::Cancelled => Ok(true),
        }
    }

    async fn handle_pick(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = Option_::ChatGpt;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = Option_::ApiKey;
            }
            KeyCode::Char('1') => self.selected = Option_::ChatGpt,
            KeyCode::Char('2') => self.selected = Option_::ApiKey,
            KeyCode::Enter => match self.selected {
                Option_::ChatGpt => self.start_chatgpt().await,
                Option_::ApiKey => {
                    *self.stage.lock().await = Stage::ApiKeyEntry;
                }
            },
            _ => {}
        }
        Ok(false)
    }

    async fn handle_api_key(&mut self, key: KeyEvent) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                *self.stage.lock().await = Stage::PickMode;
            }
            KeyCode::Enter => {
                let key_str = self.api_input.buffer.trim().to_string();
                if key_str.is_empty() {
                    *self.error.lock().await = Some("API key cannot be empty".into());
                    return Ok(false);
                }
                let mut record = auth::load_auth().unwrap_or_default();
                record.mode = AuthMode::OpenaiApiKey;
                record.api_key = Some(key_str);
                record.tokens = None;
                match auth::save_auth(&record) {
                    Ok(_) => {
                        *self.stage.lock().await = Stage::Success(AuthMode::OpenaiApiKey);
                        return Ok(true);
                    }
                    Err(e) => {
                        *self.error.lock().await = Some(format!("Failed to save: {e}"));
                    }
                }
            }
            KeyCode::Backspace => self.api_input.backspace(),
            KeyCode::Char('u') if ctrl => self.api_input.clear(),
            KeyCode::Char('w') if ctrl => self.api_input.delete_word_left(),
            KeyCode::Char(c) if !ctrl => self.api_input.insert_char(c),
            KeyCode::Left => self.api_input.move_left(),
            KeyCode::Right => self.api_input.move_right(),
            KeyCode::Home => self.api_input.move_home(),
            KeyCode::End => self.api_input.move_end(),
            _ => {}
        }
        Ok(false)
    }

    async fn start_chatgpt(&mut self) {
        let stage = self.stage.clone();
        let err = self.error.clone();
        // Start the OAuth flow. This returns immediately with the auth URL.
        match auth::start_browser_login(true).await {
            Ok(pending) => {
                *stage.lock().await = Stage::WaitingForBrowser {
                    url: pending.auth_url.clone(),
                };
                let stage2 = stage.clone();
                let err2 = err.clone();
                tokio::spawn(async move {
                    match pending.completion.await {
                        Ok(Ok(_)) => {
                            *stage2.lock().await = Stage::Success(AuthMode::OpenaiOauth);
                        }
                        Ok(Err(e)) => {
                            *err2.lock().await = Some(e.to_string());
                            *stage2.lock().await = Stage::PickMode;
                        }
                        Err(e) => {
                            *err2.lock().await = Some(format!("login task crashed: {e}"));
                            *stage2.lock().await = Stage::PickMode;
                        }
                    }
                });
            }
            Err(e) => {
                *err.lock().await = Some(e.to_string());
                *stage.lock().await = Stage::PickMode;
            }
        }
    }
}

const ASCII_LOGO: &str = "
  ██████╗ ██████╗ ███████╗███╗   ██╗ ██████╗██╗     ██╗
 ██╔═══██╗██╔══██╗██╔════╝████╗  ██║██╔════╝██║     ██║
 ██║   ██║██████╔╝█████╗  ██╔██╗ ██║██║     ██║     ██║
 ██║   ██║██╔═══╝ ██╔══╝  ██║╚██╗██║██║     ██║     ██║
 ╚██████╔╝██║     ███████╗██║ ╚████║╚██████╗███████╗██║
  ╚═════╝ ╚═╝     ╚══════╝╚═╝  ╚═══╝ ╚═════╝╚══════╝╚═╝
";

pub fn render(f: &mut Frame, area: Rect, screen: &LoginScreen, stage: &Stage, err: Option<&str>) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8), // logo
            Constraint::Length(3), // tagline
            Constraint::Min(8),    // body
            Constraint::Length(1), // footer
        ])
        .split(centered(area));

    render_logo(f, layout[0]);
    render_tagline(f, layout[1]);
    match stage {
        Stage::PickMode => render_pick(f, layout[2], screen.selected, err),
        Stage::WaitingForBrowser { url } => render_browser(f, layout[2], url, err),
        Stage::ApiKeyEntry => render_api_key(f, layout[2], &screen.api_input, err),
        Stage::Success(_) => {}
        Stage::Cancelled => {}
    }
    render_footer(f, layout[3]);
}

fn centered(area: Rect) -> Rect {
    let max_w = 76u16;
    if area.width <= max_w {
        return area;
    }
    let x = area.x + (area.width - max_w) / 2;
    Rect {
        x,
        y: area.y,
        width: max_w,
        height: area.height,
    }
}

fn render_logo(f: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for raw in ASCII_LOGO.lines() {
        lines.push(Line::from(Span::styled(
            raw.to_string(),
            Style::default()
                .fg(Color::Rgb(16, 163, 127))
                .add_modifier(Modifier::BOLD),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_tagline(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            " opencli — a coding agent for your terminal",
            Style::default()
                .fg(Color::Rgb(240, 240, 240))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            " powered by OpenAI Responses API",
            Style::default().fg(Color::Rgb(180, 180, 180)),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn render_pick(f: &mut Frame, area: Rect, selected: Option_, err: Option<&str>) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Sign in with ChatGPT to use your paid plan",
        Style::default().fg(Color::Rgb(240, 240, 240)),
    )));
    lines.push(Line::from(Span::styled(
        " or use an OpenAI API key for usage-based billing",
        Style::default().fg(Color::Rgb(190, 190, 190)),
    )));
    lines.push(Line::raw(""));

    let item = |idx: usize, opt: Option_, title: &str, desc: &str| -> Vec<Line<'static>> {
        let is_sel = selected == opt;
        let caret = if is_sel { ">" } else { " " };
        let title_style = if is_sel {
            Style::default()
                .fg(Color::Rgb(25, 195, 154))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(230, 230, 230))
        };
        let head = Line::from(vec![
            Span::styled(
                format!(" {caret} {idx}. "),
                Style::default().fg(if is_sel {
                    Color::Rgb(25, 195, 154)
                } else {
                    Color::Rgb(180, 180, 180)
                }),
            ),
            Span::styled(title.to_string(), title_style),
        ]);
        let sub = Line::from(Span::styled(
            format!("     {desc}"),
            Style::default().fg(Color::Rgb(180, 180, 180)),
        ));
        vec![head, sub]
    };
    lines.extend(item(
        1,
        Option_::ChatGpt,
        "Sign in with ChatGPT",
        "Included with Plus, Pro, Business, and Enterprise plans",
    ));
    lines.push(Line::raw(""));
    lines.extend(item(
        2,
        Option_::ApiKey,
        "Use an OpenAI API key",
        "Pay for what you use",
    ));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " ↑↓ to select · Enter to continue · Ctrl+C to exit",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    if let Some(e) = err {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(" {e}"),
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_browser(f: &mut Frame, area: Rect, url: &str, err: Option<&str>) {
    use ratatui::widgets::Wrap;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Finish signing in via your browser…",
        Style::default()
            .fg(Color::Rgb(25, 195, 154))
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " If the page didn't open automatically, copy this URL:",
        Style::default().fg(Color::Rgb(190, 190, 190)),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        format!(" {url}"),
        Style::default()
            .fg(Color::Rgb(120, 200, 255))
            .add_modifier(Modifier::UNDERLINED),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " Press Esc to cancel and pick a different sign-in method.",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    if let Some(e) = err {
        lines.push(Line::raw(""));
        for chunk in textwrap::wrap(e, area.width.saturating_sub(2) as usize) {
            lines.push(Line::from(Span::styled(
                format!(" {chunk}"),
                Style::default().fg(Color::Rgb(255, 120, 120)),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_api_key(f: &mut Frame, area: Rect, input: &TextInput, err: Option<&str>) {
    let masked: String = "•".repeat(input.buffer.chars().count());
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Paste your OpenAI API key",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        " starts with sk-…",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    lines.push(Line::raw(""));
    let cursor = "█";
    let body = if input.is_empty() {
        Span::styled("sk-…", Style::default().fg(Color::Rgb(170, 170, 170)))
    } else {
        Span::styled(masked, Style::default().fg(Color::White))
    };
    lines.push(Line::from(vec![
        Span::styled(
            " > ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        body,
        Span::styled(cursor, Style::default().fg(Color::Gray)),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " Enter to save · Esc to go back · Ctrl+U to clear",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    if let Some(e) = err {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(" {e}"),
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_footer(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " opencli · Rust + OpenAI · MIT",
            Style::default().fg(Color::Rgb(170, 170, 170)),
        ))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex as AsyncMutex;

    static ENV_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.as_ref() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn api_key_entry_does_not_prefill_env_secret() {
        let _lock = ENV_LOCK.lock().await;
        let _env = EnvGuard::set("OPENAI_API_KEY", "sk-secret-should-not-render");
        let mut login = LoginScreen::new();
        login.selected = Option_::ApiKey;

        let finished = login
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(!finished);
        assert!(matches!(login.stage().await, Stage::ApiKeyEntry));
        assert!(login.api_input.is_empty());
    }
}
