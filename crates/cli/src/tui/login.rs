//! Onboarding / login screen — first screen when the user is not authenticated.
//!
//! Offers the same four sign-in paths as `opencli login`:
//!   ▸ OpenAI — ChatGPT account (OAuth via a local browser callback)
//!   ▸ OpenAI — API key
//!   ▸ Anthropic — Claude Pro/Max (OAuth, manual code paste, may violate ToS)
//!   ▸ Anthropic — Console API key
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use opencli_core::auth::{self, anthropic as anth, AuthMode};
use opencli_core::provider::Provider;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::Mutex;

use super::input::TextInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Option_ {
    OpenAiChatGpt,
    OpenAiApiKey,
    AnthropicOauth,
    AnthropicApiKey,
}

impl Option_ {
    const ALL: [Option_; 4] = [
        Option_::OpenAiChatGpt,
        Option_::OpenAiApiKey,
        Option_::AnthropicOauth,
        Option_::AnthropicApiKey,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|o| *o == self).unwrap_or(0)
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn prev(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone)]
pub enum Stage {
    PickMode,
    /// OpenAI OAuth: waiting on the local browser callback.
    WaitingForBrowser {
        url: String,
    },
    /// API-key entry, shared by both providers.
    ApiKeyEntry {
        provider: Provider,
    },
    /// Anthropic OAuth: ToS gate shown before the flow starts.
    AnthropicTos,
    /// Anthropic OAuth: URL shown, waiting for the user to paste the code.
    AnthropicPaste {
        url: String,
    },
    Success(AuthMode),
    Cancelled,
}

pub struct LoginScreen {
    pub stage: Arc<Mutex<Stage>>,
    pub selected: Option_,
    pub api_input: TextInput,
    pub paste_input: TextInput,
    pub error: Arc<Mutex<Option<String>>>,
    /// Held between [`Stage::AnthropicPaste`] start and code submission. The
    /// Claude OAuth client has no loopback redirect, so there is no callback —
    /// the user pastes the code and we finish the exchange here.
    anth_pending: Option<anth::ManualLogin>,
    /// Bumped whenever a ChatGPT OAuth flow is abandoned (Esc / Ctrl+C) or a new
    /// one is started. The spawned completion task applies its result only if
    /// this still equals the value it captured at spawn, so a callback that
    /// fires after the user moved on can't clobber the screen (e.g. flip it to
    /// an unexpected `Success` or overwrite a fresh flow with a stale error).
    flow_generation: Arc<AtomicU64>,
}

impl LoginScreen {
    pub fn new() -> Self {
        Self {
            stage: Arc::new(Mutex::new(Stage::PickMode)),
            selected: Option_::OpenAiChatGpt,
            api_input: TextInput::default(),
            paste_input: TextInput::default(),
            error: Arc::new(Mutex::new(None)),
            anth_pending: None,
            flow_generation: Arc::new(AtomicU64::new(0)),
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
            // Invalidate any in-flight OAuth completion task before leaving.
            self.flow_generation.fetch_add(1, Ordering::SeqCst);
            *self.stage.lock().await = Stage::Cancelled;
            return Ok(true);
        }
        let stage = self.stage().await;
        match stage {
            Stage::PickMode => self.handle_pick(key).await,
            Stage::WaitingForBrowser { .. } => {
                if key.code == KeyCode::Esc {
                    // Abandon the flow: invalidate the pending completion task so
                    // a late browser callback can't reopen or "succeed" the screen.
                    self.flow_generation.fetch_add(1, Ordering::SeqCst);
                    *self.stage.lock().await = Stage::PickMode;
                }
                Ok(false)
            }
            Stage::ApiKeyEntry { provider } => self.handle_api_key(key, provider).await,
            Stage::AnthropicTos => self.handle_tos(key).await,
            Stage::AnthropicPaste { .. } => self.handle_paste(key).await,
            Stage::Success(_) | Stage::Cancelled => Ok(true),
        }
    }

    async fn handle_pick(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.prev(),
            KeyCode::Down | KeyCode::Char('j') => self.selected = self.selected.next(),
            KeyCode::Char('1') => self.selected = Option_::OpenAiChatGpt,
            KeyCode::Char('2') => self.selected = Option_::OpenAiApiKey,
            KeyCode::Char('3') => self.selected = Option_::AnthropicOauth,
            KeyCode::Char('4') => self.selected = Option_::AnthropicApiKey,
            KeyCode::Enter => match self.selected {
                Option_::OpenAiChatGpt => self.start_chatgpt().await,
                Option_::OpenAiApiKey => {
                    self.api_input.clear();
                    *self.error.lock().await = None;
                    *self.stage.lock().await = Stage::ApiKeyEntry {
                        provider: Provider::OpenAi,
                    };
                }
                Option_::AnthropicOauth => {
                    *self.error.lock().await = None;
                    *self.stage.lock().await = Stage::AnthropicTos;
                }
                Option_::AnthropicApiKey => {
                    self.api_input.clear();
                    *self.error.lock().await = None;
                    *self.stage.lock().await = Stage::ApiKeyEntry {
                        provider: Provider::Anthropic,
                    };
                }
            },
            _ => {}
        }
        Ok(false)
    }

    async fn handle_api_key(&mut self, key: KeyEvent, provider: Provider) -> Result<bool> {
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
                let mode = match provider {
                    Provider::OpenAi => {
                        auth::activate_openai_api_key(&mut record, key_str);
                        AuthMode::OpenaiApiKey
                    }
                    Provider::Anthropic => {
                        auth::activate_anthropic_api_key(&mut record, key_str);
                        AuthMode::AnthropicApiKey
                    }
                };
                match auth::save_auth(&record) {
                    Ok(_) => {
                        *self.stage.lock().await = Stage::Success(mode);
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

    async fn handle_tos(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                *self.stage.lock().await = Stage::PickMode;
            }
            KeyCode::Enter => {
                // Build the authorize URL and open the browser. Claude's OAuth
                // client registers no loopback redirect, so the user copies the
                // code shown by claude.ai back into the paste field.
                let login = anth::begin_manual_login(true);
                let url = login.auth_url.clone();
                self.anth_pending = Some(login);
                self.paste_input.clear();
                *self.error.lock().await = None;
                *self.stage.lock().await = Stage::AnthropicPaste { url };
            }
            _ => {}
        }
        Ok(false)
    }

    async fn handle_paste(&mut self, key: KeyEvent) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.anth_pending = None;
                *self.stage.lock().await = Stage::PickMode;
            }
            KeyCode::Enter => {
                let code = self.paste_input.buffer.trim().to_string();
                if code.is_empty() {
                    *self.error.lock().await = Some("Paste the code from the browser".into());
                    return Ok(false);
                }
                // Borrow ends when `result` is produced, freeing self for the
                // mutation/lock calls below.
                let result = match self.anth_pending.as_ref() {
                    Some(login) => Some(anth::complete_manual_login(login, &code).await),
                    None => None,
                };
                match result {
                    None => {
                        *self.error.lock().await = Some("Login expired; start again".into());
                        *self.stage.lock().await = Stage::PickMode;
                    }
                    Some(Ok(_)) => {
                        self.anth_pending = None;
                        *self.stage.lock().await = Stage::Success(AuthMode::AnthropicOauth);
                        return Ok(true);
                    }
                    Some(Err(e)) => {
                        *self.error.lock().await = Some(e.to_string());
                    }
                }
            }
            KeyCode::Backspace => self.paste_input.backspace(),
            KeyCode::Char('u') if ctrl => self.paste_input.clear(),
            KeyCode::Char('w') if ctrl => self.paste_input.delete_word_left(),
            KeyCode::Char(c) if !ctrl => self.paste_input.insert_char(c),
            KeyCode::Left => self.paste_input.move_left(),
            KeyCode::Right => self.paste_input.move_right(),
            KeyCode::Home => self.paste_input.move_home(),
            KeyCode::End => self.paste_input.move_end(),
            _ => {}
        }
        Ok(false)
    }

    async fn start_chatgpt(&mut self) {
        // Claim a fresh generation so this flow's completion task can tell
        // whether it's still current when it finishes (also supersedes any
        // previous flow's task on re-entry).
        let my_gen = self
            .flow_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
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
                let gen2 = self.flow_generation.clone();
                tokio::spawn(async move {
                    let outcome = match pending.completion.await {
                        Ok(Ok(_)) => Ok(()),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(e) => Err(format!("login task crashed: {e}")),
                    };
                    Self::finish_chatgpt(&stage2, &err2, &gen2, my_gen, outcome).await;
                });
            }
            Err(e) => {
                *err.lock().await = Some(e.to_string());
                *stage.lock().await = Stage::PickMode;
            }
        }
    }

    /// Apply a ChatGPT OAuth completion, but only if its flow is still current:
    /// if the user pressed Esc/Ctrl+C or started another flow, `flow_generation`
    /// has moved past `my_gen` and the (now stale) result is dropped instead of
    /// clobbering the screen. The generation is read under the `stage` lock so it
    /// stays consistent with the state being written.
    async fn finish_chatgpt(
        stage: &Arc<Mutex<Stage>>,
        err: &Arc<Mutex<Option<String>>>,
        generation: &Arc<AtomicU64>,
        my_gen: u64,
        outcome: Result<(), String>,
    ) {
        let mut s = stage.lock().await;
        if generation.load(Ordering::SeqCst) != my_gen {
            return;
        }
        match outcome {
            Ok(()) => *s = Stage::Success(AuthMode::OpenaiOauth),
            Err(msg) => {
                *err.lock().await = Some(msg);
                *s = Stage::PickMode;
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
        Stage::ApiKeyEntry { provider } => {
            render_api_key(f, layout[2], *provider, &screen.api_input, err)
        }
        Stage::AnthropicTos => render_tos(f, layout[2], err),
        Stage::AnthropicPaste { url } => render_paste(f, layout[2], url, &screen.paste_input, err),
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
            " powered by OpenAI & Anthropic models",
            Style::default().fg(Color::Rgb(180, 180, 180)),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn render_pick(f: &mut Frame, area: Rect, selected: Option_, err: Option<&str>) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Choose how to sign in to opencli",
        Style::default().fg(Color::Rgb(240, 240, 240)),
    )));
    lines.push(Line::raw(""));

    let item = |opt: Option_, idx: usize, title: &str, desc: &str| -> Vec<Line<'static>> {
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
        Option_::OpenAiChatGpt,
        1,
        "OpenAI — Sign in with ChatGPT",
        "Included with Plus, Pro, Business, and Enterprise plans",
    ));
    lines.extend(item(
        Option_::OpenAiApiKey,
        2,
        "OpenAI — API key",
        "Pay-as-you-go with an sk-… key",
    ));
    lines.extend(item(
        Option_::AnthropicOauth,
        3,
        "Anthropic — Claude Pro/Max (OAuth)",
        "Uses your claude.ai subscription — MAY violate Anthropic ToS",
    ));
    lines.extend(item(
        Option_::AnthropicApiKey,
        4,
        "Anthropic — Console API key",
        "Pay-as-you-go with an sk-ant-… key",
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

fn render_tos(f: &mut Frame, area: Rect, err: Option<&str>) {
    use ratatui::widgets::Wrap;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Claude Pro/Max sign-in — read this first",
        Style::default()
            .fg(Color::Rgb(255, 196, 0))
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));
    for raw in anth::TOS_WARNING.lines() {
        lines.push(Line::from(Span::styled(
            format!(" {raw}"),
            Style::default().fg(Color::Rgb(220, 200, 160)),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " Press Enter to accept and continue · Esc to cancel",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    if let Some(e) = err {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(" {e}"),
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_paste(f: &mut Frame, area: Rect, url: &str, input: &TextInput, err: Option<&str>) {
    use ratatui::widgets::Wrap;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Sign in with Claude in your browser…",
        Style::default()
            .fg(Color::Rgb(25, 195, 154))
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " If the page didn't open automatically, copy this URL:",
        Style::default().fg(Color::Rgb(190, 190, 190)),
    )));
    lines.push(Line::from(Span::styled(
        format!(" {url}"),
        Style::default()
            .fg(Color::Rgb(120, 200, 255))
            .add_modifier(Modifier::UNDERLINED),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " After approving, paste the authorization code shown by claude.ai:",
        Style::default().fg(Color::Rgb(230, 230, 230)),
    )));
    lines.push(Line::raw(""));
    let body = if input.is_empty() {
        Span::styled(
            "paste code here…",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        )
    } else {
        Span::styled(input.buffer.clone(), Style::default().fg(Color::White))
    };
    lines.push(Line::from(vec![
        Span::styled(
            " > ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        body,
        Span::styled("█", Style::default().fg(Color::Gray)),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " Enter to finish · Esc to cancel · Ctrl+U to clear",
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

fn render_api_key(
    f: &mut Frame,
    area: Rect,
    provider: Provider,
    input: &TextInput,
    err: Option<&str>,
) {
    let (label, hint, placeholder) = match provider {
        Provider::OpenAi => (" Paste your OpenAI API key", " starts with sk-…", "sk-…"),
        Provider::Anthropic => (
            " Paste your Anthropic API key",
            " starts with sk-ant-…",
            "sk-ant-…",
        ),
    };
    let masked: String = "•".repeat(input.buffer.chars().count());
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        label,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    lines.push(Line::raw(""));
    let body = if input.is_empty() {
        Span::styled(placeholder, Style::default().fg(Color::Rgb(170, 170, 170)))
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
        Span::styled("█", Style::default().fg(Color::Gray)),
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
            " opencli · Rust · MIT",
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
        login.selected = Option_::OpenAiApiKey;

        let finished = login
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(!finished);
        assert!(matches!(
            login.stage().await,
            Stage::ApiKeyEntry {
                provider: Provider::OpenAi
            }
        ));
        assert!(login.api_input.is_empty());
    }

    #[tokio::test]
    async fn anthropic_oauth_routes_through_tos_gate() {
        let mut login = LoginScreen::new();
        login.selected = Option_::AnthropicOauth;

        let finished = login
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(!finished);
        assert!(matches!(login.stage().await, Stage::AnthropicTos));
    }

    #[tokio::test]
    async fn esc_in_waiting_for_browser_bumps_generation() {
        let mut login = LoginScreen::new();
        *login.stage.lock().await = Stage::WaitingForBrowser { url: "x".into() };
        let before = login.flow_generation.load(Ordering::SeqCst);

        let finished = login
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(!finished);
        assert!(matches!(login.stage().await, Stage::PickMode));
        assert_ne!(login.flow_generation.load(Ordering::SeqCst), before);
    }

    #[tokio::test]
    async fn stale_chatgpt_result_is_dropped() {
        // A completion task from an abandoned flow must not clobber the screen.
        let login = LoginScreen::new();
        *login.stage.lock().await = Stage::WaitingForBrowser { url: "x".into() };
        let my_gen = login
            .flow_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        // User abandons the flow (Esc/Ctrl+C bumps the generation).
        login.flow_generation.fetch_add(1, Ordering::SeqCst);
        *login.stage.lock().await = Stage::PickMode;

        // The late OAuth success arrives — it must be ignored.
        LoginScreen::finish_chatgpt(
            &login.stage,
            &login.error,
            &login.flow_generation,
            my_gen,
            Ok(()),
        )
        .await;

        assert!(matches!(login.stage().await, Stage::PickMode));
        assert!(login.error_text().await.is_none());
    }

    #[tokio::test]
    async fn current_chatgpt_result_is_applied() {
        let login = LoginScreen::new();
        *login.stage.lock().await = Stage::WaitingForBrowser { url: "x".into() };
        let my_gen = login
            .flow_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);

        LoginScreen::finish_chatgpt(
            &login.stage,
            &login.error,
            &login.flow_generation,
            my_gen,
            Ok(()),
        )
        .await;

        assert!(matches!(
            login.stage().await,
            Stage::Success(AuthMode::OpenaiOauth)
        ));
    }
}
