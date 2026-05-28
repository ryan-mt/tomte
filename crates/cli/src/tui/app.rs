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
use opencli_core::tools::{ApprovalMode, TodoItem, TodoStatus};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use super::input::TextInput;
use super::login::{self, LoginScreen, Stage as LoginStage};
use super::picker::{self, Picker};
use super::ui;

/// Shared handle into the Agent's in-flight approval map. Cloned from
/// `Agent.pending_approvals` BEFORE the long-lived `run_turn` lock is taken so
/// the TUI can deliver Y/N decisions without blocking on the outer agent mutex.
pub type ApprovalHandle = std::sync::Arc<
    tokio::sync::Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Plan,
    Default,
    AcceptEdits,
    BypassPerms,
}

impl PermissionMode {
    pub fn next(self) -> Self {
        match self {
            Self::Plan => Self::Default,
            Self::Default => Self::AcceptEdits,
            Self::AcceptEdits => Self::BypassPerms,
            Self::BypassPerms => Self::Plan,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "⏸ plan mode",
            Self::Default => "▸ default (ask)",
            Self::AcceptEdits => "⏵⏵ accept edits",
            Self::BypassPerms => "⚠ bypass permissions",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    SlashMenu,
    ModelPicker,
    EffortPicker,
    VerbosityPicker,
    ResumePicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Chat,
}

pub async fn run() -> Result<()> {
    run_with(false).await
}

/// Same as [`run`] but opens the resume-session picker on first frame so
/// `opencli resume` lands the user directly on the session list.
pub async fn run_resume() -> Result<()> {
    run_with(true).await
}

async fn run_with(start_with_resume_picker: bool) -> Result<()> {
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
    let res = main_loop(&mut terminal, start_with_resume_picker).await;
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
    /// Cumulative input tokens for the session — used by `/cost` to estimate
    /// USD spend and break down the bill.
    pub input_tokens_total: u64,
    /// Cumulative output (reasoning + completion) tokens for the session.
    pub output_tokens_total: u64,
    /// Number of turns the user has run this session. Surfaced by `/cost`.
    pub turn_count: u64,
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
    /// Whether destructive tool calls require a TUI approval modal. Toggled
    /// by `/perms`. Defaults to true so the modal stays on unless the user
    /// explicitly opts out.
    pub require_approval: bool,
    /// Mirror of `Agent.auto_approve_edits`. Driven by Shift+Tab cycling
    /// through the four `PermissionMode` states.
    pub auto_approve_edits: bool,
    /// Set true by /quit and /exit so the main loop can break cleanly,
    /// letting `run()` call restore_terminal before returning. Previously
    /// these commands called std::process::exit(0) which skipped the
    /// terminal-restore path and left the shell in raw mode.
    pub should_exit: bool,
    /// Picker writes the chosen session id here; main_loop performs the
    /// actual restore (locking the agent and rebuilding blocks) on the
    /// next tick. Going through a flag keeps `handle_overlay_select` free
    /// of the agent Arc and avoids re-plumbing its signature.
    pub pending_resume_id: Option<String>,
    /// Set true by `/undo` so main_loop can invoke `Agent::undo_last_edit()`
    /// on the next tick (slash handlers don't have the agent Arc).
    pub pending_undo: bool,
    /// True until the first frame has opened the resume picker. Used by
    /// `opencli resume` to bypass needing to type `/resume` after launch.
    pub start_with_resume_picker: bool,
    /// Snapshot of the agent's session todo list, refreshed after every
    /// tool batch via `AgentEvent::TodosSnapshot`. Read by `/todos`.
    pub session_todos: Vec<TodoItem>,
    /// Memoised wrapped-line output from `render_chat`. Re-using the previous
    /// frame's lines when nothing relevant has changed (chat length, terminal
    /// width, last block size, expanded-tools toggle) keeps long sessions out
    /// of an O(n*frames) textwrap loop. Invalidated implicitly by signature
    /// comparison inside `render_chat`.
    pub chat_render_cache: Option<ChatRenderCache>,
    pub pending_approval: Option<PendingApproval>,
    /// Clone of the Agent's `pending_approvals` Arc, captured BEFORE the
    /// long-lived turn lock so Y/N keystrokes can deliver a decision without
    /// blocking on the outer agent mutex (run_turn holds it for the whole
    /// turn and is itself waiting on this approval).
    pub approval_handle: Option<ApprovalHandle>,
}

/// Snapshot of the last `render_chat` output along with the inputs that
/// produced it. If `signature_matches(...)` returns true on the next frame
/// the cached `lines` are returned verbatim, skipping the textwrap pass
/// over every block. The cache is invalidated implicitly: a content change
/// or width resize produces a new signature, never matches the old one.
#[derive(Clone)]
pub struct ChatRenderCache {
    pub blocks_len: usize,
    pub inner_width: usize,
    pub expanded_tools: bool,
    /// Cheap fingerprint of the most recent block. During streaming the
    /// last block's text grows; comparing its size detects deltas without
    /// hashing every block. For non-streaming idle frames this stays
    /// constant and the cache is hit cleanly.
    pub last_block_size: usize,
    pub lines: Vec<ratatui::text::Line<'static>>,
}

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub call_id: String,
    pub tool_name: String,
    /// Pretty-printed JSON arguments shown inside the modal.
    pub args_json: String,
    /// Optional diff/preview rendered in a second pane (e.g. write_file).
    pub diff_preview: Option<String>,
}

pub const SPINNER_WORDS: &[&str] = &[
    // Cognitive
    "Thinking",
    "Pondering",
    "Cogitating",
    "Mulling",
    "Reasoning",
    "Reflecting",
    "Deliberating",
    "Ruminating",
    "Contemplating",
    "Musing",
    "Considering",
    "Reckoning",
    "Surmising",
    "Inferring",
    "Speculating",
    // Creative
    "Composing",
    "Crafting",
    "Forging",
    "Brewing",
    "Hatching",
    "Cooking",
    "Conjuring",
    "Sketching",
    "Imagining",
    "Drafting",
    "Painting",
    "Concocting",
    "Designing",
    "Improvising",
    "Inventing",
    // Mechanical / process
    "Computing",
    "Processing",
    "Whirring",
    "Calibrating",
    "Tuning",
    "Spinning",
    "Threading",
    "Weaving",
    "Hammering",
    "Tinkering",
    "Welding",
    "Wiring",
    "Polishing",
    "Refining",
    "Sharpening",
    "Aligning",
    "Recalibrating",
    "Hashing",
    "Crunching",
    "Buffing",
    // Search / discovery
    "Probing",
    "Investigating",
    "Surveying",
    "Mapping",
    "Scanning",
    "Exploring",
    "Hunting",
    "Sleuthing",
    "Decoding",
    "Unraveling",
    "Untangling",
    "Decrypting",
    "Foraging",
    "Excavating",
    "Quarrying",
    "Sifting",
    "Tracing",
    "Reading",
    "Parsing",
    "Combing",
    // Action / strategy
    "Plotting",
    "Scheming",
    "Charting",
    "Synthesizing",
    "Distilling",
    "Brainstorming",
    "Wrangling",
    "Marshaling",
    "Orchestrating",
    "Solving",
    "Sculpting",
    "Carving",
    "Molding",
    "Shaping",
    "Architecting",
    "Engineering",
    "Bootstrapping",
    "Stitching",
    "Coaxing",
    "Steering",
    // Whimsical
    "Noodling",
    "Doodling",
    "Stargazing",
    "Daydreaming",
    "Percolating",
    "Bubbling",
    "Fermenting",
    "Marinating",
    "Stewing",
    "Simmering",
    "Frothing",
    "Steeping",
    "Buzzing",
    "Humming",
    "Rumbling",
    "Whisking",
    "Kneading",
    "Folding",
    "Layering",
    "Garnishing",
];

pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
        let blocks = vec![Block::Welcome];
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
            input_tokens_total: 0,
            output_tokens_total: 0,
            turn_count: 0,
            pending_images: Vec::new(),
            next_image_num: 1,
            overlay: None,
            chain_to_effort: false,
            message_queue: Vec::new(),
            is_thinking: false,
            expanded_tools: false,
            current_turn: None,
            approval: ApprovalMode::OnRequest,
            require_approval: true,
            auto_approve_edits: false,
            should_exit: false,
            pending_resume_id: None,
            pending_undo: false,
            start_with_resume_picker: false,
            session_todos: Vec::new(),
            chat_render_cache: None,
            pending_approval: None,
            approval_handle: None,
        }
    }

    pub fn permission_mode(&self) -> PermissionMode {
        if self.approval == ApprovalMode::Plan {
            PermissionMode::Plan
        } else if !self.require_approval {
            PermissionMode::BypassPerms
        } else if self.auto_approve_edits {
            PermissionMode::AcceptEdits
        } else {
            PermissionMode::Default
        }
    }

    pub fn set_permission_mode(&mut self, m: PermissionMode) {
        match m {
            PermissionMode::Plan => {
                self.approval = ApprovalMode::Plan;
                self.require_approval = true;
                self.auto_approve_edits = false;
            }
            PermissionMode::Default => {
                self.approval = ApprovalMode::OnRequest;
                self.require_approval = true;
                self.auto_approve_edits = false;
            }
            PermissionMode::AcceptEdits => {
                self.approval = ApprovalMode::OnRequest;
                self.require_approval = true;
                self.auto_approve_edits = true;
            }
            PermissionMode::BypassPerms => {
                self.approval = ApprovalMode::OnRequest;
                self.require_approval = false;
                self.auto_approve_edits = false;
            }
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
            OverlayKind::ResumePicker => {
                let metas = opencli_core::session::list(&self.cwd);
                Picker::new("resume session", picker::sessions(&metas))
            }
        };
        self.overlay = Some((kind, picker));
    }
}

async fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    start_with_resume_picker: bool,
) -> Result<()> {
    let mut app = App::new();
    if start_with_resume_picker && app.screen == Screen::Chat {
        app.start_with_resume_picker = true;
    }
    let mut events = EventStream::new();
    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(256);
    // Persistent agent kept across turns to preserve history.
    let agent: std::sync::Arc<tokio::sync::Mutex<Option<Agent>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(None));

    loop {
        if app.should_exit {
            break;
        }
        // Open the resume picker once on first frame when launched via
        // `opencli resume`. Guarded so re-entry (eg. after Esc) doesn't pop
        // the picker back open unexpectedly.
        if app.start_with_resume_picker && app.screen == Screen::Chat && app.overlay.is_none() {
            app.start_with_resume_picker = false;
            app.open_overlay(OverlayKind::ResumePicker);
        }
        // Resume picker leaves the chosen session id here; perform the load
        // out-of-band so handle_overlay_select doesn't need the agent Arc.
        if let Some(id) = app.pending_resume_id.take() {
            apply_resume(&mut app, &agent, &id).await;
        }
        // `/undo` sets this so the agent Arc can stay out of handle_slash.
        if std::mem::take(&mut app.pending_undo) {
            let result = {
                let mut g = agent.lock().await;
                match g.as_mut() {
                    Some(a) => a.undo_last_edit().await,
                    None => Err(anyhow::anyhow!("no agent yet — nothing to undo")),
                }
            };
            let msg = match result {
                Ok(s) => s,
                Err(e) => format!("undo: {e}"),
            };
            app.blocks.push(Block::System(msg));
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

    if let Some(p) = app.pending_approval.clone() {
        let decision = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(false),
            _ => None,
        };
        if let Some(granted) = decision {
            app.pending_approval = None;
            let label = if granted { "approved" } else { "denied" };
            app.blocks
                .push(Block::System(format!("{label}: {}", p.tool_name)));
            // CRITICAL: do NOT lock the outer agent mutex here — run_turn
            // holds it for the entire turn and is itself awaiting this
            // approval. Use the handle Arc captured at turn start instead.
            if let Some(handle) = app.approval_handle.clone() {
                let call_id = p.call_id.clone();
                tokio::spawn(async move {
                    let sender = {
                        let mut map = handle.lock().await;
                        map.remove(&call_id)
                    };
                    if let Some(s) = sender {
                        let _ = s.send(granted);
                    }
                });
            }
            return Ok(false);
        }
        return Ok(false);
    }

    if app.overlay.is_some() {
        return handle_overlay_key(app, key).await;
    }

    if matches!(key.code, KeyCode::BackTab) {
        let next = app.permission_mode().next();
        app.set_permission_mode(next);
        app.blocks
            .push(Block::System(format!("mode → {}", next.label())));
        return Ok(false);
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
            app.blocks.push(Block::System(format!("paste failed: {e}")));
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
        KeyCode::Backspace if kind == OverlayKind::SlashMenu => {
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
        KeyCode::Char(c) if kind == OverlayKind::SlashMenu => {
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
            app.blocks.push(Block::System(format!("model → {key_sel}")));
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
        OverlayKind::ResumePicker => {
            if !key_sel.is_empty() {
                app.pending_resume_id = Some(key_sel.to_string());
            }
        }
    }
}

/// Carry out a resume after the picker has set `pending_resume_id`. Loads the
/// session from disk, rebuilds visible blocks from the persisted history, and
/// replaces the agent's in-memory state in place.
async fn apply_resume(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    id: &str,
) {
    let record = match opencli_core::session::load(&app.cwd, id) {
        Ok(r) => r,
        Err(e) => {
            app.blocks
                .push(Block::System(format!("resume failed: {e}")));
            return;
        }
    };
    let preview = record.meta.preview.clone();
    let msg_count = record.meta.message_count;

    // Rebuild visible blocks BEFORE we hand the history off to the agent so
    // we still own the record's contents.
    let rebuilt = rebuild_blocks_from_history(&record.history);

    {
        let mut guard = agent.lock().await;
        if guard.is_none() {
            // Lazily construct an Agent so we can stash the restored state
            // on it. The client will be created on the next turn via
            // launch_turn, which rebuilds it from the active credential.
            let credential = match opencli_core::auth::resolve_credential().await {
                Ok(c) => c,
                Err(e) => {
                    app.blocks
                        .push(Block::System(format!("resume auth error: {e}")));
                    return;
                }
            };
            let client = match OpenAiClient::new(credential) {
                Ok(c) => c,
                Err(e) => {
                    app.blocks
                        .push(Block::System(format!("resume client error: {e}")));
                    return;
                }
            };
            let mut a = Agent::new(client, app.config.clone());
            a.require_approval = app.require_approval;
            a.auto_approve_edits = app.auto_approve_edits;
            a.cwd = app.cwd.clone();
            a.approval = app.approval;
            a.apply_project_memory();
            a.restore_from(record);
            *guard = Some(a);
        } else if let Some(a) = guard.as_mut() {
            a.restore_from(record);
        }
    }

    app.blocks.clear();
    app.blocks.push(Block::Welcome);
    app.blocks.extend(rebuilt);
    app.blocks.push(Block::System(format!(
        "↻ resumed: {preview} ({msg_count} messages)"
    )));
    app.auto_scroll = true;
}

/// Reconstruct chat-visible blocks from a persisted history. The Responses
/// API history is a flat list of `InputItem`s; we group function_call +
/// function_call_output by call_id and drop the reasoning items (they were
/// only ever streamed deltas).
fn rebuild_blocks_from_history(history: &[opencli_core::openai::InputItem]) -> Vec<Block> {
    use opencli_core::openai::{InputItem, MessageContent};
    use std::collections::HashMap;

    let mut outputs: HashMap<String, (String, bool)> = HashMap::new();
    for item in history {
        if let InputItem::FunctionCallOutput { call_id, output } = item {
            let is_err = output.starts_with("Error:");
            outputs.insert(call_id.clone(), (output.clone(), is_err));
        }
    }

    let mut blocks: Vec<Block> = Vec::new();
    for item in history {
        match item {
            InputItem::Message { role, content } => {
                let mut text = String::new();
                for c in content {
                    match c {
                        MessageContent::InputText { text: t } => text.push_str(t),
                        MessageContent::OutputText { text: t } => text.push_str(t),
                        MessageContent::InputImage { .. } => text.push_str("[image]"),
                    }
                }
                if role == "user" {
                    blocks.push(Block::User(text));
                } else if role == "assistant" {
                    blocks.push(Block::Assistant {
                        text,
                        reasoning: String::new(),
                        done: true,
                        thought_for_secs: None,
                        reasoning_started_at: None,
                    });
                }
            }
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                let (output, error) = outputs
                    .get(call_id)
                    .cloned()
                    .map(|(o, e)| (Some(o), e))
                    .unwrap_or((None, false));
                blocks.push(Block::Tool {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    args: arguments.clone(),
                    output,
                    error,
                });
            }
            InputItem::FunctionCallOutput { .. } => { /* attached above */ }
            InputItem::Reasoning { .. } => { /* not persisted visually */ }
        }
    }
    blocks
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
                 /resume             pick a previous session to continue\n  \
                 /cwd [path]         show / set working directory\n  \
                 /status             show auth status\n  \
                 /cost               show token usage and estimated cost\n  \
                 /config             show current configuration\n  \
                 /hooks              list configured PreToolUse hooks\n  \
                 /mcp                list configured MCP servers\n  \
                 /init               create CLAUDE.md for this project\n  \
                 /memory             show CLAUDE.md\n  \
                 /diff               show `git diff` for the working tree\n  \
                 /review             ask the agent to review uncommitted changes\n  \
                 /export [path]      save conversation as markdown\n  \
                 /compact            ask the agent to compact the conversation\n  \
                 /todos              show the session todo list\n  \
                 /about              show opencli version + build info\n  \
                 /perms [on|off]     toggle the approval modal for writes/shell\n  \
                 /undo               revert the most recent file edit\n  \
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
                app.blocks.push(Block::System(format!("effort → {arg}")));
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
                    app.blocks
                        .push(Block::System(format!("File not found: {}", path.display())));
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
                    app.blocks
                        .push(Block::System(format!("cwd → {}", app.cwd.display())));
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
            app.blocks.push(Block::System("plan mode → off".into()));
        }
        "perms" | "approvals" => {
            let new_state = match arg {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                "" => !app.require_approval,
                other => {
                    app.blocks.push(Block::System(format!(
                        "Usage: /perms [on|off]  (current: {}). Got: {other}",
                        if app.require_approval { "on" } else { "off" }
                    )));
                    return;
                }
            };
            app.require_approval = new_state;
            app.blocks.push(Block::System(format!(
                "approval modal → {}",
                if new_state {
                    "on"
                } else {
                    "off (writes/shell auto-approved)"
                }
            )));
        }
        "undo" => {
            // main_loop drains this flag so the agent Arc stays out of
            // handle_slash (same pattern as `pending_resume_id`).
            app.pending_undo = true;
        }
        "clear" => {
            app.blocks.clear();
        }
        "resume" => {
            app.open_overlay(OverlayKind::ResumePicker);
        }
        "cost" | "usage" => {
            let input = app.input_tokens_total;
            let output = app.output_tokens_total;
            let total = input.saturating_add(output);
            let (in_per_m, out_per_m) = pricing_for(&app.config.model);
            let in_cost = (input as f64) * in_per_m / 1_000_000.0;
            let out_cost = (output as f64) * out_per_m / 1_000_000.0;
            let total_cost = in_cost + out_cost;
            let mut msg = String::new();
            msg.push_str(&format!("Session usage — model: {}\n", app.config.model));
            msg.push_str(&format!("  Turns:           {}\n", app.turn_count));
            msg.push_str(&format!(
                "  Input tokens:    {input:>12}  ·  ${in_cost:.4}\n",
                in_cost = in_cost
            ));
            msg.push_str(&format!(
                "  Output tokens:   {output:>12}  ·  ${out_cost:.4}\n",
                out_cost = out_cost
            ));
            msg.push_str(&format!(
                "  Total tokens:    {total:>12}\n  Estimated cost:  ${total_cost:.4}\n"
            ));
            msg.push_str(&format!(
                "  Pricing assumed: ${in_per_m:.2}/M input, ${out_per_m:.2}/M output"
            ));
            app.blocks.push(Block::System(msg));
        }
        "config" => {
            let auth = match app.auth_mode {
                AuthMode::None => "none",
                AuthMode::ApiKey => "api_key",
                AuthMode::ChatGPT => "chatgpt",
            };
            let mcp_count = opencli_core::mcp::load_servers_config().len();
            let hooks = opencli_core::hooks::load();
            let hook_count = hooks.config.pre_tool_use.len();
            let approval = match app.approval {
                ApprovalMode::Auto => "auto",
                ApprovalMode::OnRequest => "on_request",
                ApprovalMode::Manual => "manual",
                ApprovalMode::Plan => "plan",
            };
            app.blocks.push(Block::System(format!(
                "Current configuration:\n  \
                 model:            {}\n  \
                 reasoning_effort: {}\n  \
                 verbosity:        {}\n  \
                 cwd:              {}\n  \
                 approval:         {}\n  \
                 auth_mode:        {}\n  \
                 mcp_servers:      {}\n  \
                 hooks (PreToolUse): {}",
                app.config.model,
                app.config.reasoning_effort,
                app.config.verbosity,
                app.cwd.display(),
                approval,
                auth,
                mcp_count,
                hook_count,
            )));
        }
        "hooks" => {
            let hooks = opencli_core::hooks::load();
            let entries = &hooks.config.pre_tool_use;
            if entries.is_empty() {
                app.blocks.push(Block::System(
                    "No PreToolUse hooks configured.\n\
                     Add some in ~/.config/opencli/settings.json under .hooks.PreToolUse"
                        .into(),
                ));
            } else {
                let mut msg = String::from("PreToolUse hooks:\n");
                for (i, h) in entries.iter().enumerate() {
                    msg.push_str(&format!(
                        "  {}. matcher={:<14}  command={}\n",
                        i + 1,
                        h.matcher,
                        h.command
                    ));
                }
                app.blocks.push(Block::System(msg));
            }
        }
        "mcp" => {
            let servers = opencli_core::mcp::load_servers_config();
            if servers.is_empty() {
                app.blocks.push(Block::System(
                    "No MCP servers configured.\n\
                     Add some in ~/.config/opencli/settings.json under .mcp_servers"
                        .into(),
                ));
            } else {
                let mut msg = String::from("MCP servers (from settings.json):\n");
                let mut names: Vec<&String> = servers.keys().collect();
                names.sort();
                for n in names {
                    let cfg = &servers[n];
                    msg.push_str(&format!(
                        "  · {}  ·  {} {}\n",
                        n,
                        cfg.command,
                        cfg.args.join(" ")
                    ));
                }
                msg.push_str("\nServers are spawned on first turn; tools register under mcp__<server>__<tool>.");
                app.blocks.push(Block::System(msg));
            }
        }
        "init" => {
            let claude_md = app.cwd.join("CLAUDE.md");
            if claude_md.exists() {
                app.blocks.push(Block::System(format!(
                    "CLAUDE.md already exists at {}. Use /memory to view it.",
                    claude_md.display()
                )));
            } else {
                // Queue a prompt asking the agent to analyse the repo and
                // write a CLAUDE.md. The Enter handler will run it on the
                // next tick of main_loop.
                let prompt = "Analyze this repository and create a CLAUDE.md file at the repo root. \
                              The file should describe: project purpose, tech stack, key architecture / \
                              module layout, build + test commands, and any non-obvious conventions a new \
                              contributor must know. Keep it concise (under 80 lines) and write it as \
                              terse engineer-to-engineer notes, not a tutorial.";
                app.message_queue.push(prompt.to_string());
                app.blocks.push(Block::System(
                    "Queued: agent will analyse the repo and create CLAUDE.md.".into(),
                ));
            }
        }
        "memory" => {
            let claude_md = app.cwd.join("CLAUDE.md");
            match std::fs::read_to_string(&claude_md) {
                Ok(text) => app.blocks.push(Block::System(format!(
                    "CLAUDE.md ({}):\n{}",
                    claude_md.display(),
                    text
                ))),
                Err(_) => app.blocks.push(Block::System(format!(
                    "No CLAUDE.md at {}. Run /init to create one.",
                    claude_md.display()
                ))),
            }
        }
        "diff" => {
            // Pipe `git diff` from the cwd and surface its output. Empty
            // output means a clean tree; non-zero exit (no git, not a repo)
            // surfaces stderr so the user knows why.
            let cwd = app.cwd.clone();
            let out = tokio::process::Command::new("git")
                .args(["diff", "--no-color"])
                .current_dir(&cwd)
                .output()
                .await;
            match out {
                Ok(o) if o.status.success() => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    if stdout.trim().is_empty() {
                        app.blocks
                            .push(Block::System("(no uncommitted changes)".into()));
                    } else {
                        app.blocks
                            .push(Block::System(format!("$ git diff\n{stdout}")));
                    }
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    app.blocks
                        .push(Block::System(format!("git diff failed:\n{stderr}")));
                }
                Err(e) => app.blocks.push(Block::System(format!("git diff: {e}"))),
            }
        }
        "review" => {
            let prompt = "Review the uncommitted changes in this repository. Run `git diff` (or \
                          the run_shell tool) to see them, then assess for correctness, security \
                          risks, and obvious bugs. Cite locations as `path:line`. Surface only \
                          findings that are CRITICAL/HIGH/MEDIUM — skip stylistic nits.";
            app.message_queue.push(prompt.to_string());
            app.blocks.push(Block::System(
                "Queued: agent will review the uncommitted changes.".into(),
            ));
        }
        "export" => {
            let default_name = format!(
                "opencli-export-{}.md",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            );
            let path = if arg.is_empty() {
                app.cwd.join(default_name)
            } else {
                let p = std::path::PathBuf::from(arg);
                if p.is_absolute() {
                    p
                } else {
                    app.cwd.join(p)
                }
            };
            let md = render_blocks_as_markdown(&app.blocks);
            match std::fs::write(&path, md) {
                Ok(_) => app.blocks.push(Block::System(format!(
                    "Exported conversation → {}",
                    path.display()
                ))),
                Err(e) => app
                    .blocks
                    .push(Block::System(format!("export failed: {e}"))),
            }
        }
        "compact" => {
            // Ask the agent itself to produce a tight summary. The model
            // sees the full history; its next assistant message becomes the
            // new compact baseline that the user can keep iterating on.
            let prompt = "Summarize the conversation so far into a single self-contained block: \
                          what we worked on, what files / decisions matter going forward, and \
                          where we left off. Keep it under 30 lines. After this turn, treat the \
                          summary as the canonical context — earlier messages can be ignored.";
            app.message_queue.push(prompt.to_string());
            app.blocks.push(Block::System(
                "Queued: agent will compact the conversation.".into(),
            ));
        }
        "todos" | "todo" => {
            if app.session_todos.is_empty() {
                app.blocks.push(Block::System(
                    "No session todos. The model writes them with `todo_write` \
                     when it plans a multi-step task."
                        .into(),
                ));
            } else {
                let mut msg = String::from("Session todos:\n");
                for (i, t) in app.session_todos.iter().enumerate() {
                    let marker = match t.status {
                        TodoStatus::Completed => "[x]",
                        TodoStatus::InProgress => "[~]",
                        TodoStatus::Pending => "[ ]",
                    };
                    let label = match t.status {
                        TodoStatus::InProgress => &t.active_form,
                        _ => &t.content,
                    };
                    msg.push_str(&format!("  {} {}. {label}\n", marker, i + 1));
                }
                let done = app
                    .session_todos
                    .iter()
                    .filter(|t| matches!(t.status, TodoStatus::Completed))
                    .count();
                msg.push_str(&format!(
                    "\n  {} / {} completed",
                    done,
                    app.session_todos.len()
                ));
                app.blocks.push(Block::System(msg));
            }
        }
        "about" => {
            app.blocks.push(Block::System(format!(
                "opencli v{}\n\
                 model:  {}\n\
                 effort: {}\n\
                 build:  {}",
                env!("CARGO_PKG_VERSION"),
                app.config.model,
                app.config.reasoning_effort,
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                },
            )));
        }
        "quit" | "exit" => app.should_exit = true,
        other => app.blocks.push(Block::System(format!(
            "Unknown command /{other}. Try /help."
        ))),
    }
}

/// Rough OpenAI Responses pricing per model — used by `/cost` for a local
/// estimate. Returns `(input_$_per_million, output_$_per_million)`.
/// Update when official pricing changes; an inexact estimate beats none.
fn pricing_for(model: &str) -> (f64, f64) {
    match model {
        "gpt-5" | "gpt-5.5" | "gpt-5.4" => (1.25, 10.0),
        "gpt-5-pro" | "gpt-5.5-pro" | "gpt-5.4-pro" => (5.00, 20.0),
        "gpt-5-codex" => (1.25, 10.0),
        "gpt-5-mini" | "gpt-5.4-mini" => (0.25, 2.00),
        "gpt-5-nano" | "gpt-5.4-nano" => (0.05, 0.40),
        _ => (1.25, 10.0),
    }
}

/// Render the visible chat blocks as a portable Markdown transcript for
/// `/export`. Reasoning bodies and tool args/outputs are included so the
/// export captures the same shape the user saw on screen.
fn render_blocks_as_markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    out.push_str("# opencli conversation\n\n");
    for b in blocks {
        match b {
            Block::Welcome => {}
            Block::User(text) => {
                out.push_str("## 🧑 user\n\n");
                out.push_str(text);
                out.push_str("\n\n");
            }
            Block::Assistant {
                text,
                reasoning,
                thought_for_secs,
                ..
            } => {
                out.push_str("## 🤖 assistant\n\n");
                if let Some(secs) = thought_for_secs {
                    out.push_str(&format!("_thought for {secs}s_\n\n"));
                }
                if !reasoning.is_empty() {
                    out.push_str("<details><summary>reasoning</summary>\n\n```\n");
                    out.push_str(reasoning);
                    out.push_str("\n```\n\n</details>\n\n");
                }
                if !text.is_empty() {
                    out.push_str(text);
                    out.push_str("\n\n");
                }
            }
            Block::Tool {
                name,
                args,
                output,
                error,
                ..
            } => {
                let marker = if *error { "❌" } else { "🔧" };
                out.push_str(&format!("### {marker} tool: `{name}`\n\n"));
                if !args.is_empty() {
                    out.push_str("**args:**\n\n```json\n");
                    out.push_str(args);
                    out.push_str("\n```\n\n");
                }
                if let Some(o) = output {
                    out.push_str("**output:**\n\n```\n");
                    out.push_str(o);
                    out.push_str("\n```\n\n");
                }
            }
            Block::System(s) => {
                out.push_str("> ");
                out.push_str(&s.replace('\n', "\n> "));
                out.push_str("\n\n");
            }
        }
    }
    out
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
            app.blocks.push(Block::System(format!("Auth error: {e}")));
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
            a.require_approval = app.require_approval;
            a.auto_approve_edits = app.auto_approve_edits;
            a.cwd = app.cwd.clone();
            a.approval = app.approval;
            a.apply_project_memory();
            *guard = Some(a);
        } else {
            // Update mutable config every turn so /model, /effort, /plan take effect.
            if let Some(a) = guard.as_mut() {
                a.config = app.config.clone();
                a.cwd = app.cwd.clone();
                a.approval = app.approval;
                a.require_approval = app.require_approval;
                a.auto_approve_edits = app.auto_approve_edits;
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
    {
        // Snapshot the agent's pending_approvals Arc so Y/N keystrokes don't
        // need to grab the outer mutex (which run_turn holds for the duration).
        let guard = agent.lock().await;
        if let Some(a) = guard.as_ref() {
            app.approval_handle = Some(a.pending_approvals.clone());
        }
    }
    app.busy = true;
    app.turn_started_at = Some(std::time::Instant::now());
    app.spinner_word = pick_spinner_word();
    app.spinner_frame = 0;
    app.turn_count = app.turn_count.saturating_add(1);
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
            // Persist the conversation after every turn so /resume can pick
            // it up later. Failure to save is logged at debug level only —
            // we don't want to spam the chat with disk-error popups, and the
            // session is still safe in memory for the remainder of the run.
            let record = a.to_session_record();
            if let Err(e) = opencli_core::session::save(&record) {
                tracing::debug!(error = %e, "session save failed");
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
    // Drop any in-flight approval modal: leaving pending_approval=Some after
    // a cancel makes handle_key intercept every keystroke, locking the user
    // out of the input box with no way to recover short of restarting.
    app.pending_approval = None;
    app.approval_handle = None;
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
        AgentEvent::ToolResult {
            call_id,
            output,
            error,
        } => {
            if let Some(Block::Tool {
                output: o,
                error: e,
                ..
            }) = find_tool_mut(&mut app.blocks, &call_id)
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
            if let Some(Block::Assistant {
                done,
                text,
                reasoning,
                thought_for_secs,
                ..
            }) = last_assistant_mut_open(&mut app.blocks)
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
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
            total_tokens,
        } => {
            app.tokens_used = app.tokens_used.saturating_add(total_tokens);
            app.input_tokens_total = app.input_tokens_total.saturating_add(input_tokens);
            app.output_tokens_total = app.output_tokens_total.saturating_add(output_tokens);
        }
        AgentEvent::TodosSnapshot { todos } => {
            app.session_todos = todos;
        }
        AgentEvent::Error { message } => {
            app.blocks.push(Block::System(format!("error: {message}")));
            app.busy = false;
            app.turn_started_at = None;
            app.status_line.clear();
            app.current_turn = None;
        }
        AgentEvent::ContextWarning { used, limit } => {
            let pct = (used as f64 / limit.max(1) as f64 * 100.0) as u64;
            app.blocks.push(Block::System(format!(
                "context {used}/{limit} tokens ({pct}%) - consider /compact"
            )));
        }
        AgentEvent::ApprovalRequest {
            call_id,
            tool_name,
            args_json,
            diff_preview,
        } => {
            app.pending_approval = Some(PendingApproval {
                call_id,
                tool_name,
                args_json,
                diff_preview,
            });
        }
        AgentEvent::ApprovalGranted { call_id } => {
            if app
                .pending_approval
                .as_ref()
                .is_some_and(|p| p.call_id == call_id)
            {
                app.pending_approval = None;
            }
        }
        AgentEvent::ApprovalDenied { call_id } => {
            if app
                .pending_approval
                .as_ref()
                .is_some_and(|p| p.call_id == call_id)
            {
                app.pending_approval = None;
            }
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
    blocks
        .iter_mut()
        .rev()
        .find(|b| matches!(b, Block::Assistant { done: false, .. }))
}

fn find_tool_mut<'a>(blocks: &'a mut [Block], call_id: &str) -> Option<&'a mut Block> {
    blocks
        .iter_mut()
        .rev()
        .find(|b| matches!(b, Block::Tool { call_id: c, .. } if c == call_id))
}

/// Maintain the invariant "at most one open assistant block" by closing any
/// still-open blocks (and dropping empty ones), then pushing a fresh open
/// block. Used after a tool result so subsequent reasoning/text appears below
/// the tool, without leaving stale empties behind.
fn rotate_assistant_block(blocks: &mut Vec<Block>) {
    let mut i = 0;
    while i < blocks.len() {
        if let Block::Assistant {
            done,
            text,
            reasoning,
            thought_for_secs,
            ..
        } = &mut blocks[i]
        {
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
