use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use opencli_core::agent::{Agent, AgentEvent};
use opencli_core::auth::{self, AuthMode};
use opencli_core::config::{self, Config};
use opencli_core::openai::OpenAiClient;
use opencli_core::tools::ApprovalMode;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use super::input::TextInput;
use super::login::{self, LoginScreen, Stage as LoginStage};
use super::picker::{self, Picker};
use super::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    SlashMenu,
    ModelPicker,
    EffortPicker,
    VerbosityPicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Chat,
}

pub async fn run() -> Result<()> {
    // Install a panic hook that restores the terminal before unwinding, so a
    // panic inside main_loop (or any library it pulls in) doesn't leave the
    // user's shell stuck in raw mode + alternate screen.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let res = main_loop(&mut terminal).await;
    restore_terminal(&mut terminal)?;
    res
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(t: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(t.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    t.show_cursor()?;
    Ok(())
}

#[derive(Debug, Clone)]
pub enum Block {
    Welcome,
    User(String),
    Assistant {
        text: String,
        reasoning: String,
        /// Whether the assistant's text/reasoning is done (i.e. moved to history).
        done: bool,
        /// Set when reasoning has been collapsed: the value is the elapsed seconds.
        thought_for_secs: Option<u64>,
        /// Marks the moment we received the first reasoning event for this block.
        reasoning_started_at: Option<std::time::Instant>,
    },
    Tool {
        call_id: String,
        name: String,
        args: String,
        output: Option<String>,
        error: bool,
    },
    System(String),
}

pub struct App {
    pub screen: Screen,
    pub login: LoginScreen,
    pub blocks: Vec<Block>,
    pub input: TextInput,
    pub busy: bool,
    pub cwd: std::path::PathBuf,
    pub config: Config,
    pub auth_mode: AuthMode,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub status_line: String,
    pub last_height: u16,
    pub last_width: u16,
    pub turn_started_at: Option<std::time::Instant>,
    pub spinner_word: String,
    pub spinner_frame: usize,
    pub tokens_used: u64,
    pub pending_images: Vec<std::path::PathBuf>,
    pub next_image_num: usize,
    pub overlay: Option<(OverlayKind, Picker)>,
    /// When set, after choosing a model the next overlay shown is the effort picker.
    pub chain_to_effort: bool,
    /// Messages typed while a turn is in flight. Flushed as a single combined
    /// user message after `TurnComplete`.
    pub message_queue: Vec<String>,
    /// True while the agent is in the reasoning phase (no assistant text yet).
    pub is_thinking: bool,
    /// When true, tool call blocks render with more detail (toggled via Ctrl+O).
    pub expanded_tools: bool,
    /// Handle to the currently running turn task. Held so the user can cancel
    /// (Esc) a stuck turn — aborting the task drops the agent mutex guard and
    /// the in-flight HTTP request, freeing things up for the next turn.
    pub current_turn: Option<tokio::task::JoinHandle<()>>,
    /// Current permission/approval mode. `/plan` flips to `ApprovalMode::Plan`
    /// (read-only); `/normal` flips back to `ApprovalMode::OnRequest`. The
    /// value is copied onto the active `Agent` each turn in `launch_turn`.
    pub approval: ApprovalMode,
    /// Set true by /quit and /exit so the main loop can break cleanly,
    /// letting `run()` call restore_terminal before returning. Previously
    /// these commands called std::process::exit(0) which skipped the
    /// terminal-restore path and left the shell in raw mode.
    pub should_exit: bool,
}

pub const SPINNER_WORDS: &[&str] = &[
    // Cognitive
    "Thinking", "Pondering", "Cogitating", "Mulling", "Reasoning",
    "Reflecting", "Deliberating", "Ruminating", "Contemplating", "Musing",
    "Considering", "Reckoning", "Surmising", "Inferring", "Speculating",
    // Creative
    "Composing", "Crafting", "Forging", "Brewing", "Hatching",
    "Cooking", "Conjuring", "Sketching", "Imagining", "Drafting",
    "Painting", "Concocting", "Designing", "Improvising", "Inventing",
    // Mechanical / process
    "Computing", "Processing", "Whirring", "Calibrating", "Tuning",
    "Spinning", "Threading", "Weaving", "Hammering", "Tinkering",
    "Welding", "Wiring", "Polishing", "Refining", "Sharpening",
    "Aligning", "Recalibrating", "Hashing", "Crunching", "Buffing",
    // Search / discovery
    "Probing", "Investigating", "Surveying", "Mapping", "Scanning",
    "Exploring", "Hunting", "Sleuthing", "Decoding", "Unraveling",
    "Untangling", "Decrypting", "Foraging", "Excavating", "Quarrying",
    "Sifting", "Tracing", "Reading", "Parsing", "Combing",
    // Action / strategy
    "Plotting", "Scheming", "Charting", "Synthesizing", "Distilling",
    "Brainstorming", "Wrangling", "Marshaling", "Orchestrating", "Solving",
    "Sculpting", "Carving", "Molding", "Shaping", "Architecting",
    "Engineering", "Bootstrapping", "Stitching", "Coaxing", "Steering",
    // Whimsical
    "Noodling", "Doodling", "Stargazing", "Daydreaming", "Percolating",
    "Bubbling", "Fermenting", "Marinating", "Stewing", "Simmering",
    "Frothing", "Steeping", "Buzzing", "Humming", "Rumbling",
    "Whisking", "Kneading", "Folding", "Layering", "Garnishing",
];

pub const SPINNER_FRAMES: &[&str] = &["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];

pub fn pick_spinner_word() -> String {
    use rand::seq::SliceRandom;
    let mut rng = rand::thread_rng();
    SPINNER_WORDS
        .choose(&mut rng)
        .copied()
        .unwrap_or("Thinking")
        .to_string()
}

impl App {
    fn new() -> Self {
        let config = config::load();
        let auth_mode = auth::load_auth().map(|a| a.mode).unwrap_or(AuthMode::None);
        let cwd = std::env::current_dir().unwrap_or_default();
        let mut blocks = Vec::new();
        blocks.push(Block::Welcome);
        let has_env_key = std::env::var("OPENAI_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        let screen = if auth_mode == AuthMode::None && !has_env_key {
            Screen::Login
        } else {
            Screen::Chat
        };
        Self {
            screen,
            login: LoginScreen::new(),
            blocks,
            input: TextInput::default(),
            busy: false,
            cwd,
            config,
            auth_mode,
            scroll: 0,
            auto_scroll: true,
            status_line: String::new(),
            last_height: 0,
            last_width: 0,
            turn_started_at: None,
            spinner_word: String::new(),
            spinner_frame: 0,
            tokens_used: 0,
            pending_images: Vec::new(),
            next_image_num: 1,
            overlay: None,
            chain_to_effort: false,
            message_queue: Vec::new(),
            is_thinking: false,
            expanded_tools: false,
            current_turn: None,
            approval: ApprovalMode::OnRequest,
            should_exit: false,
        }
    }

    pub fn open_overlay(&mut self, kind: OverlayKind) {
        let picker = match kind {
            OverlayKind::SlashMenu => Picker::new("commands", picker::slash_commands()),
            OverlayKind::ModelPicker => {
                let mut p = Picker::new("select model", picker::models());
                // pre-select current
                if let Some(i) = p.items.iter().position(|it| it.key == self.config.model) {
                    p.selected = i;
                }
                p
            }
            OverlayKind::EffortPicker => {
                let mut p = Picker::new("reasoning effort", picker::efforts());
                if let Some(i) = p
                    .items
                    .iter()
                    .position(|it| it.key == self.config.reasoning_effort)
                {
                    p.selected = i;
                }
                p
            }
            OverlayKind::VerbosityPicker => {
                let mut p = Picker::new("verbosity", picker::verbosities());
                if let Some(i) = p
                    .items
                    .iter()
                    .position(|it| it.key == self.config.verbosity)
                {
                    p.selected = i;
                }
                p
            }
        };
        self.overlay = Some((kind, picker));
    }
}

async fn main_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::new();
    let mut events = EventStream::new();
    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(256);
    // Persistent agent kept across turns to preserve history.
    let agent: std::sync::Arc<tokio::sync::Mutex<Option<Agent>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(None));

    loop {
        if app.should_exit {
            break;
        }
        // Flush the message queue after a turn completes.
        if !app.busy && !app.message_queue.is_empty() && app.screen == Screen::Chat {
            let combined: String = std::mem::take(&mut app.message_queue).join("\n\n");
            app.status_line.clear();
            // Strip a leading slash so queued slash commands don't bypass intent.
            if let Some(rest) = combined.strip_prefix('/') {
                if !rest.contains('\n') {
                    handle_slash(&mut app, rest.trim()).await;
                } else {
                    app.blocks.push(Block::User(combined.clone()));
                    app.auto_scroll = true;
                    launch_turn(&mut app, &agent, &agent_tx, combined).await;
                }
            } else {
                app.blocks.push(Block::User(combined.clone()));
                app.auto_scroll = true;
                launch_turn(&mut app, &agent, &agent_tx, combined).await;
            }
        }

        // Poll login completion (the spawned OAuth flow writes auth.json).
        if app.screen == Screen::Login {
            let stage = app.login.stage().await;
            match stage {
                LoginStage::Success(mode) => {
                    app.auth_mode = mode;
                    app.screen = Screen::Chat;
                    app.login = LoginScreen::new();
                }
                LoginStage::Cancelled => break,
                _ => {}
            }
        }

        let login_stage = if app.screen == Screen::Login {
            Some(app.login.stage().await)
        } else {
            None
        };
        let login_err = if app.screen == Screen::Login {
            app.login.error_text().await
        } else {
            None
        };

        terminal.draw(|f| {
            app.last_width = f.area().width;
            app.last_height = f.area().height;
            match app.screen {
                Screen::Login => {
                    if let Some(stage) = login_stage.as_ref() {
                        login::render(f, f.area(), &app.login, stage, login_err.as_deref());
                    }
                }
                Screen::Chat => ui::render(f, &mut app),
            }
        })?;

        tokio::select! {
            biased;
            maybe_ev = events.next() => {
                let Some(ev) = maybe_ev else { break; };
                match ev {
                    Ok(Event::Key(key)) => {
                        if key.kind != KeyEventKind::Press { continue; }
                        match app.screen {
                            Screen::Login => {
                                if app.login.handle_key(key).await? {
                                    let stage = app.login.stage().await;
                                    if let LoginStage::Success(mode) = stage {
                                        app.auth_mode = mode;
                                        app.screen = Screen::Chat;
                                        app.login = LoginScreen::new();
                                    } else if matches!(stage, LoginStage::Cancelled) {
                                        break;
                                    }
                                }
                            }
                            Screen::Chat => {
                                if handle_key(&mut app, key, &agent, &agent_tx).await? { break; }
                            }
                        }
                    }
                    Ok(Event::Resize(_, _)) => {}
                    Ok(Event::Mouse(m)) => {
                        use crossterm::event::MouseEventKind;
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                app.scroll = app.scroll.saturating_sub(3);
                                app.auto_scroll = false;
                            }
                            MouseEventKind::ScrollDown => {
                                app.scroll = app.scroll.saturating_add(3);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            Some(ev) = agent_rx.recv() => {
                apply_agent_event(&mut app, ev);
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                if app.busy {
                    app.spinner_frame = app.spinner_frame.wrapping_add(1);
                }
            }
        }
    }
    Ok(())
}

async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    tx: &mpsc::Sender<AgentEvent>,
) -> Result<bool> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // Overlay key handling takes precedence.
    if app.overlay.is_some() {
        return handle_overlay_key(app, key).await;
    }

    match key.code {
        KeyCode::Char('c') if ctrl => return Ok(true),
        KeyCode::Char('d') if ctrl && app.input.is_empty() => return Ok(true),
        KeyCode::Char('l') if ctrl => {
            app.blocks.clear();
        }
        KeyCode::Char('u') if ctrl => app.input.clear(),
        KeyCode::Char('w') if ctrl => app.input.delete_word_left(),
        KeyCode::Char('v') if ctrl => {
            handle_paste(app).await;
        }
        KeyCode::Char('o') if ctrl => {
            app.expanded_tools = !app.expanded_tools;
        }
        KeyCode::Char('/') if app.input.is_empty() => {
            // Trigger the slash menu overlay; also insert '/' into the input
            // so users can keep typing to filter.
            app.input.insert_char('/');
            app.open_overlay(OverlayKind::SlashMenu);
        }
        KeyCode::Char(ch) => app.input.insert_char(ch),
        KeyCode::Enter if shift || alt => app.input.insert_newline(),
        KeyCode::Enter => {
            if app.input.is_empty() {
                return Ok(false);
            }
            let text = app.input.take();
            if !app.busy {
                if let Some(rest) = text.strip_prefix('/') {
                    handle_slash(app, rest.trim()).await;
                    return Ok(false);
                }
                app.blocks.push(Block::User(text.clone()));
                app.auto_scroll = true;
                launch_turn(app, agent, tx, text).await;
            } else {
                // Busy: queue the message. Slash commands also queue, no special-casing,
                // so the user's intent is preserved.
                app.message_queue.push(text);
            }
        }
        KeyCode::Backspace => app.input.backspace(),
        KeyCode::Left => app.input.move_left(),
        KeyCode::Right => app.input.move_right(),
        KeyCode::Up => app.input.move_up(),
        KeyCode::Down => app.input.move_down(),
        KeyCode::Home => app.input.move_home(),
        KeyCode::End => app.input.move_end(),
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_sub(10);
            app.auto_scroll = false;
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_add(10);
        }
        KeyCode::Esc => {
            if app.busy {
                cancel_current_turn(app);
            } else {
                app.input.clear();
            }
        }
        _ => {}
    }
    Ok(false)
}

async fn handle_paste(app: &mut App) {
    use super::clipboard::{try_paste, PasteResult};
    match try_paste() {
        Ok(PasteResult::Image(path)) => {
            let n = app.next_image_num;
            app.pending_images.push(path);
            app.next_image_num += 1;
            let marker = format!("[Image #{n}] ");
            for c in marker.chars() {
                app.input.insert_char(c);
            }
        }
        Ok(PasteResult::Text(t)) => {
            for c in t.chars() {
                if c == '\r' {
                    continue;
                }
                app.input.insert_char(c);
            }
        }
        Ok(PasteResult::Empty) => {}
        Err(e) => {
            app.blocks
                .push(Block::System(format!("paste failed: {e}")));
        }
    }
}

async fn handle_overlay_key(app: &mut App, key: KeyEvent) -> Result<bool> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Char('c') && ctrl {
        return Ok(true);
    }
    let Some((kind, picker)) = app.overlay.as_mut() else {
        return Ok(false);
    };
    let kind = *kind;
    match key.code {
        KeyCode::Up => picker.move_up(),
        KeyCode::Down => picker.move_down(),
        KeyCode::Esc => {
            app.overlay = None;
            if kind == OverlayKind::SlashMenu {
                app.input.clear();
            }
            app.chain_to_effort = false;
        }
        KeyCode::Enter => {
            let key_sel = picker.selected_key().unwrap_or_default();
            app.overlay = None;
            handle_overlay_select(app, kind, &key_sel).await;
        }
        KeyCode::Backspace => {
            if kind == OverlayKind::SlashMenu {
                app.input.backspace();
                let buf = app.input.buffer.clone();
                if let Some(rest) = buf.strip_prefix('/') {
                    let q = rest.to_string();
                    if let Some((_, p)) = app.overlay.as_mut() {
                        p.query = q;
                        p.ensure_visible_selected();
                    }
                } else {
                    app.overlay = None;
                    app.input.clear();
                }
            }
        }
        KeyCode::Char(c) => {
            if kind == OverlayKind::SlashMenu {
                app.input.insert_char(c);
                let buf = app.input.buffer.clone();
                if let Some(rest) = buf.strip_prefix('/') {
                    let q = rest.to_string();
                    if let Some((_, p)) = app.overlay.as_mut() {
                        p.query = q;
                        p.ensure_visible_selected();
                    }
                }
            }
        }
        _ => {}
    }
    Ok(false)
}

async fn handle_overlay_select(app: &mut App, kind: OverlayKind, key_sel: &str) {
    match kind {
        OverlayKind::SlashMenu => {
            // The user picked a slash command. Some commands open another picker;
            // others run the slash handler directly.
            app.input.clear();
            match key_sel {
                "model" => {
                    app.chain_to_effort = true;
                    app.open_overlay(OverlayKind::ModelPicker);
                }
                "thinking" | "effort" => {
                    app.open_overlay(OverlayKind::EffortPicker);
                }
                "verbosity" => {
                    app.open_overlay(OverlayKind::VerbosityPicker);
                }
                other => {
                    handle_slash(app, other).await;
                }
            }
        }
        OverlayKind::ModelPicker => {
            app.config.model = key_sel.to_string();
            let _ = config::save(&app.config);
            app.blocks
                .push(Block::System(format!("model → {key_sel}")));
            if app.chain_to_effort {
                app.chain_to_effort = false;
                app.open_overlay(OverlayKind::EffortPicker);
            }
        }
        OverlayKind::EffortPicker => {
            app.config.reasoning_effort = key_sel.to_string();
            let _ = config::save(&app.config);
            app.blocks
                .push(Block::System(format!("effort → {key_sel}")));
        }
        OverlayKind::VerbosityPicker => {
            app.config.verbosity = key_sel.to_string();
            let _ = config::save(&app.config);
            app.blocks
                .push(Block::System(format!("verbosity → {key_sel}")));
        }
    }
}

async fn handle_slash(app: &mut App, cmd: &str) {
    let mut parts = cmd.splitn(2, ' ');
    let head = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    match head {
        "help" | "?" => {
            app.blocks.push(Block::System(
                "Commands:\n  \
                 /login              sign in with ChatGPT (OAuth)\n  \
                 /apikey <sk-…>      save an API key\n  \
                 /logout             clear credentials\n  \
                 /model              pick model (arrow keys), then reasoning\n  \
                 /thinking           pick reasoning (none|low|medium|high|xhigh)\n  \
                 /verbosity          pick verbosity (low|medium|high)\n  \
                 /img <path>         attach a file as an image (or use Ctrl+V)\n  \
                 /clear              clear conversation\n  \
                 /cwd [path]         show / set working directory\n  \
                 /status             show auth status\n  \
                 /quit               exit\n\n\
                 Keyboard shortcuts:\n  \
                 Esc                 cancel the running turn (while busy)\n  \
                 Ctrl+O              toggle tool-call detail view\n  \
                 Ctrl+L              clear the screen\n  \
                 Ctrl+V              paste text or image\n  \
                 Ctrl+C              quit"
                    .to_string(),
            ));
        }
        "login" => {
            app.status_line = "Opening OAuth in browser…".to_string();
            tokio::spawn(async {
                let _ = auth::login_with_browser(true).await;
            });
        }
        "apikey" => {
            if arg.is_empty() {
                app.blocks
                    .push(Block::System("Usage: /apikey sk-…".to_string()));
            } else {
                let record = auth::AuthRecord {
                    mode: AuthMode::ApiKey,
                    api_key: Some(arg.to_string()),
                    tokens: None,
                    last_refresh: None,
                };
                match auth::save_auth(&record) {
                    Ok(_) => {
                        app.auth_mode = AuthMode::ApiKey;
                        app.blocks.push(Block::System("✅ API key saved.".into()));
                    }
                    Err(e) => app.blocks.push(Block::System(format!("Error: {e}"))),
                }
            }
        }
        "logout" => {
            let _ = std::fs::remove_file(config::config_dir().join("auth.json"));
            app.auth_mode = AuthMode::None;
            app.blocks.push(Block::System("Signed out.".into()));
        }
        "status" => {
            let record = auth::load_auth().unwrap_or_default();
            let msg = match record.mode {
                AuthMode::None => "Not signed in.".to_string(),
                AuthMode::ApiKey => "Signed in with API key.".to_string(),
                AuthMode::ChatGPT => {
                    let acc = record
                        .tokens
                        .as_ref()
                        .and_then(|t| t.account_id.clone())
                        .unwrap_or_default();
                    format!("Signed in with ChatGPT. account_id={acc}")
                }
            };
            app.blocks.push(Block::System(msg));
        }
        "model" => {
            if arg.is_empty() {
                app.chain_to_effort = true;
                app.open_overlay(OverlayKind::ModelPicker);
            } else {
                app.config.model = arg.to_string();
                let _ = config::save(&app.config);
                app.blocks.push(Block::System(format!("model → {arg}")));
            }
        }
        "effort" | "thinking" => {
            if arg.is_empty() {
                app.open_overlay(OverlayKind::EffortPicker);
            } else {
                app.config.reasoning_effort = arg.to_string();
                let _ = config::save(&app.config);
                app.blocks
                    .push(Block::System(format!("effort → {arg}")));
            }
        }
        "verbosity" => {
            if arg.is_empty() {
                app.open_overlay(OverlayKind::VerbosityPicker);
            } else {
                app.config.verbosity = arg.to_string();
                let _ = config::save(&app.config);
                app.blocks.push(Block::System(format!("verbosity → {arg}")));
            }
        }
        "img" | "image" => {
            if arg.is_empty() {
                app.blocks.push(Block::System(
                    "Usage: /img <path>  — attach image to next message".into(),
                ));
            } else {
                let p = std::path::PathBuf::from(arg);
                let path = if p.is_absolute() { p } else { app.cwd.join(&p) };
                if !path.is_file() {
                    app.blocks.push(Block::System(format!(
                        "File not found: {}",
                        path.display()
                    )));
                } else {
                    let n = app.next_image_num;
                    app.pending_images.push(path.clone());
                    app.next_image_num += 1;
                    let marker = format!("[Image #{n}] ");
                    for c in marker.chars() {
                        app.input.insert_char(c);
                    }
                    app.blocks.push(Block::System(format!(
                        "attached [Image #{n}]: {}",
                        path.display()
                    )));
                }
            }
        }
        "cwd" => {
            if arg.is_empty() {
                app.blocks
                    .push(Block::System(format!("cwd: {}", app.cwd.display())));
            } else {
                let p = std::path::PathBuf::from(arg);
                if p.is_dir() {
                    app.cwd = p;
                    app.blocks.push(Block::System(format!("cwd → {}", app.cwd.display())));
                } else {
                    app.blocks.push(Block::System("Invalid path.".into()));
                }
            }
        }
        "plan" => {
            app.approval = ApprovalMode::Plan;
            app.blocks.push(Block::System(
                "plan mode → on (read-only tools only; write/edit/shell will be blocked)".into(),
            ));
        }
        "normal" => {
            app.approval = ApprovalMode::OnRequest;
            app.blocks
                .push(Block::System("plan mode → off".into()));
        }
        "clear" => {
            app.blocks.clear();
        }
        "quit" | "exit" => app.should_exit = true,
        other => app
            .blocks
            .push(Block::System(format!("Unknown command /{other}. Try /help."))),
    }
}


async fn launch_turn(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    tx: &mpsc::Sender<AgentEvent>,
    text: String,
) {
    let credential = match opencli_core::auth::resolve_credential().await {
        Ok(c) => c,
        Err(e) => {
            app.blocks
                .push(Block::System(format!("Auth error: {e}")));
            return;
        }
    };
    let client = match OpenAiClient::new(credential) {
        Ok(c) => c,
        Err(e) => {
            app.blocks.push(Block::System(format!("Client error: {e}")));
            return;
        }
    };
    {
        let mut guard = agent.lock().await;
        if guard.is_none() {
            let mut a = Agent::new(client, app.config.clone());
            a.cwd = app.cwd.clone();
            a.approval = app.approval;
            *guard = Some(a);
        } else {
            // Update mutable config every turn so /model, /effort, /plan take effect.
            if let Some(a) = guard.as_mut() {
                a.config = app.config.clone();
                a.cwd = app.cwd.clone();
                a.approval = app.approval;
            }
        }
        if let Some(a) = guard.as_mut() {
            if app.pending_images.is_empty() {
                a.push_user_message(text);
            } else {
                let imgs = std::mem::take(&mut app.pending_images);
                a.push_user_message_with_images(text, &imgs);
            }
        }
    }
    app.busy = true;
    app.turn_started_at = Some(std::time::Instant::now());
    app.spinner_word = pick_spinner_word();
    app.spinner_frame = 0;
    app.status_line.clear();
    app.blocks.push(Block::Assistant {
        text: String::new(),
        reasoning: String::new(),
        done: false,
        thought_for_secs: None,
        reasoning_started_at: None,
    });
    let agent_clone = agent.clone();
    let tx_clone = tx.clone();
    let handle = tokio::spawn(async move {
        let mut guard = agent_clone.lock().await;
        if let Some(a) = guard.as_mut() {
            if let Err(e) = a.run_turn(tx_clone.clone()).await {
                let _ = tx_clone
                    .send(AgentEvent::Error {
                        message: e.to_string(),
                    })
                    .await;
            }
        }
    });
    // Remember the task so Esc can abort it while it's still running.
    if let Some(prev) = app.current_turn.replace(handle) {
        prev.abort();
    }
}

/// Abort the in-flight turn (if any) and reset transient UI state. Used by the
/// Esc handler when a turn appears stuck (e.g. SSE stalled, model thinking
/// forever) so the user can recover without killing the app.
fn cancel_current_turn(app: &mut App) {
    if let Some(handle) = app.current_turn.take() {
        handle.abort();
    }
    app.busy = false;
    app.is_thinking = false;
    app.turn_started_at = None;
    app.status_line.clear();
    // Close the open assistant block, then surface a small note so the user
    // can see the cancel happened.
    if let Some(Block::Assistant { done, .. }) = last_assistant_mut_open(&mut app.blocks) {
        *done = true;
    }
    app.blocks
        .push(Block::System("(cancelled — Esc)".to_string()));
}

fn apply_agent_event(app: &mut App, ev: AgentEvent) {
    // Note: we deliberately do NOT force auto_scroll=true on every event — that
    // caused the chat to snap back to the bottom whenever a new delta arrived,
    // making manual scrolling impossible while the agent was streaming. The
    // scroll behaviour is: stay where the user put it; resume auto-follow only
    // when the user scrolls back to the bottom (handled in `render_chat`), or
    // when the user sends a new message (handled in `handle_key`/the queue
    // flush in `main_loop`).
    match ev {
        AgentEvent::AssistantTextDelta { text } => {
            // First text deltas terminate the thinking phase.
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            if let Some(Block::Assistant { text: buf, .. }) =
                last_assistant_mut_open(&mut app.blocks)
            {
                buf.push_str(&text);
            }
        }
        AgentEvent::AssistantTextDone { text } => {
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            if let Some(Block::Assistant { text: buf, .. }) =
                last_assistant_mut_open(&mut app.blocks)
            {
                *buf = text;
            }
        }
        AgentEvent::ReasoningDelta { text } => {
            app.is_thinking = true;
            if let Some(Block::Assistant {
                reasoning,
                reasoning_started_at,
                ..
            }) = last_assistant_mut_open(&mut app.blocks)
            {
                if reasoning_started_at.is_none() {
                    *reasoning_started_at = Some(std::time::Instant::now());
                }
                reasoning.push_str(&text);
            }
        }
        AgentEvent::ReasoningDone { text } => {
            if let Some(Block::Assistant {
                reasoning,
                reasoning_started_at,
                ..
            }) = last_assistant_mut_open(&mut app.blocks)
            {
                if reasoning_started_at.is_none() {
                    *reasoning_started_at = Some(std::time::Instant::now());
                }
                *reasoning = text;
            }
        }
        AgentEvent::ToolCallStarted { name, call_id } => {
            app.blocks.push(Block::Tool {
                call_id,
                name,
                args: String::new(),
                output: None,
                error: false,
            });
        }
        AgentEvent::ToolCallArgsDelta { call_id, delta } => {
            if let Some(Block::Tool { args, .. }) = find_tool_mut(&mut app.blocks, &call_id) {
                args.push_str(&delta);
            }
        }
        AgentEvent::ToolCallArgsDone { call_id, arguments } => {
            if let Some(Block::Tool { args, .. }) = find_tool_mut(&mut app.blocks, &call_id) {
                *args = arguments;
            }
        }
        AgentEvent::ToolResult { call_id, output, error } => {
            if let Some(Block::Tool { output: o, error: e, .. }) =
                find_tool_mut(&mut app.blocks, &call_id)
            {
                *o = Some(output);
                *e = error;
            }
            // Tool result terminates the reasoning phase too.
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            // Rotate the open assistant block: close (or remove if empty) any
            // currently-open assistant block, then push a fresh one so that
            // subsequent reasoning/text renders BELOW this tool result. Without
            // this, empty Assistant blocks accumulated between back-to-back
            // tool calls.
            rotate_assistant_block(&mut app.blocks);
        }
        AgentEvent::TurnComplete => {
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            if let Some(Block::Assistant { done, text, reasoning, thought_for_secs, .. }) =
                last_assistant_mut_open(&mut app.blocks)
            {
                *done = true;
                // Drop trailing assistant blocks that produced neither text nor reasoning.
                if text.is_empty() && reasoning.is_empty() && thought_for_secs.is_none() {
                    app.blocks.pop();
                }
            }
            app.busy = false;
            app.turn_started_at = None;
            app.status_line.clear();
            app.current_turn = None;

            // Drain the message queue — send EVERYTHING as one combined prompt.
            if !app.message_queue.is_empty() {
                // Defer to handler in main_loop via a tick: rebuild a fake key event isn't ideal,
                // so we expose a helper to launch directly. Stash the merged text on app.
                app.status_line = "(flushing queued messages…)".into();
            }
        }
        AgentEvent::Usage { total_tokens, .. } => {
            app.tokens_used = app.tokens_used.saturating_add(total_tokens);
        }
        AgentEvent::Error { message } => {
            app.blocks.push(Block::System(format!("error: {message}")));
            app.busy = false;
            app.turn_started_at = None;
            app.status_line.clear();
            app.current_turn = None;
        }
    }
}

fn collapse_reasoning_into_thought(app: &mut App) {
    if let Some(Block::Assistant {
        reasoning,
        reasoning_started_at,
        thought_for_secs,
        ..
    }) = last_assistant_mut_open(&mut app.blocks)
    {
        if thought_for_secs.is_none() && !reasoning.is_empty() {
            let secs = reasoning_started_at
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            *thought_for_secs = Some(secs);
            reasoning.clear();
        }
    }
}

fn last_assistant_mut_open(blocks: &mut [Block]) -> Option<&mut Block> {
    blocks.iter_mut().rev().find(|b| matches!(b, Block::Assistant { done: false, .. }))
}

fn find_tool_mut<'a>(blocks: &'a mut [Block], call_id: &str) -> Option<&'a mut Block> {
    blocks.iter_mut().rev().find(|b| matches!(b, Block::Tool { call_id: c, .. } if c == call_id))
}

/// Maintain the invariant "at most one open assistant block" by closing any
/// still-open blocks (and dropping empty ones), then pushing a fresh open
/// block. Used after a tool result so subsequent reasoning/text appears below
/// the tool, without leaving stale empties behind.
fn rotate_assistant_block(blocks: &mut Vec<Block>) {
    let mut i = 0;
    while i < blocks.len() {
        if let Block::Assistant { done, text, reasoning, thought_for_secs, .. } = &mut blocks[i] {
            if !*done && text.is_empty() && reasoning.is_empty() && thought_for_secs.is_none() {
                blocks.remove(i);
                continue;
            }
            *done = true;
        }
        i += 1;
    }
    blocks.push(Block::Assistant {
        text: String::new(),
        reasoning: String::new(),
        done: false,
        thought_for_secs: None,
        reasoning_started_at: None,
    });
}
