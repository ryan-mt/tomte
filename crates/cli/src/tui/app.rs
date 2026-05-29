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
use opencli_core::client::LlmClient;
use opencli_core::config::{self, Config};
use opencli_core::provider::Provider;
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
    LogoutPicker,
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
    // SessionStart hook (best-effort, once per interactive session). Spawned so a
    // slow hook can't delay the first frame; its output/exit code is ignored.
    tokio::spawn(async { opencli_core::hooks::load().fire_session_start().await });
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
    /// Set true by `/compact` or the auto-compact trigger so main_loop can call
    /// `Agent::compact_history()` on the next tick (slash/event handlers don't
    /// have the agent Arc).
    pub pending_compact: bool,
    /// Guards against repeated auto-compaction within one over-threshold
    /// window: set when the 85% trigger fires, cleared after a successful
    /// compaction so future windows can auto-compact again.
    pub auto_compact_done_this_window: bool,
    /// True while a background compaction is running (and during the brief
    /// 100%-hold after it finishes). Drives the progress bar and gates other
    /// agent-locking work so the UI never blocks on the compaction's mutex.
    pub compacting: bool,
    /// When the running compaction started — drives the time-based progress
    /// ease so the bar moves smoothly without a real percentage from the model.
    pub compact_started_at: Option<std::time::Instant>,
    /// Set when the compaction task reports success: the bar snaps to 100% and
    /// holds for a moment before `compact_result_msg` replaces it.
    pub compact_done_at: Option<std::time::Instant>,
    /// Result line queued by a finished compaction, pushed after the 100%-hold.
    pub compact_result_msg: Option<String>,
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
    /// Previously submitted prompts, oldest first. Up/Down in the composer
    /// recall these (shell / Claude Code style). In-memory for this session.
    pub input_history: Vec<String>,
    /// Cursor into `input_history` while browsing with Up/Down. `None` means the
    /// user is editing a fresh draft rather than navigating history.
    pub history_pos: Option<usize>,
    /// Draft text stashed when history browsing begins, restored when the user
    /// presses Down past the newest entry.
    pub history_draft: String,
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
    /// Wrapped lines for every block EXCEPT the last one, when the last block
    /// renders as its own standalone stanza (the common streaming case: a
    /// growing `Assistant` block). `None` when the last block has no clean
    /// split point (e.g. it was merged into a grouped read_file stanza). Lets
    /// a streaming frame re-wrap only the final block instead of the whole
    /// transcript — the difference between O(transcript) and O(last block) per
    /// token, which is what kept long chats from streaming smoothly.
    pub prefix_lines: Option<Vec<ratatui::text::Line<'static>>>,
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
        let auth_mode = auth::load_auth()
            .map(|a| auth::effective_mode_with_env(&a))
            .unwrap_or_else(|_| auth_mode_from_env().unwrap_or(AuthMode::None));
        let cwd = std::env::current_dir().unwrap_or_default();
        let blocks = vec![Block::Welcome];
        let screen = initial_screen(auth_mode, has_supported_env_key());
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
            pending_compact: false,
            auto_compact_done_this_window: false,
            compacting: false,
            compact_started_at: None,
            compact_done_at: None,
            compact_result_msg: None,
            start_with_resume_picker: false,
            session_todos: Vec::new(),
            chat_render_cache: None,
            pending_approval: None,
            approval_handle: None,
            input_history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
        }
    }

    /// Record a submitted prompt in the input history (skipping a consecutive
    /// duplicate) and reset the browse cursor.
    fn record_history(&mut self, text: &str) {
        if self.input_history.last().map(String::as_str) != Some(text) {
            self.input_history.push(text.to_string());
        }
        self.history_pos = None;
    }

    /// Recall the previous (older) history entry into the composer.
    fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let target = match self.history_pos {
            None => {
                // Starting to browse — stash the in-progress draft.
                self.history_draft = self.input.buffer.clone();
                self.input_history.len() - 1
            }
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_pos = Some(target);
        self.input.set_text(self.input_history[target].clone());
    }

    /// Move toward newer history; past the newest entry restores the draft.
    fn history_next(&mut self) {
        let Some(p) = self.history_pos else {
            return;
        };
        if p + 1 < self.input_history.len() {
            self.history_pos = Some(p + 1);
            self.input.set_text(self.input_history[p + 1].clone());
        } else {
            self.history_pos = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.input.set_text(draft);
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
            OverlayKind::LogoutPicker => {
                Picker::new("log out — pick a credential", picker::logout_targets())
            }
        };
        self.overlay = Some((kind, picker));
    }
}

fn has_supported_env_key() -> bool {
    auth_mode_from_env().is_some()
}

fn auth_mode_from_env() -> Option<AuthMode> {
    ["OPENAI_API_KEY", "ANTHROPIC_API_KEY"]
        .iter()
        .find_map(|name| match *name {
            "OPENAI_API_KEY" if std::env::var(name).is_ok_and(|v| !v.is_empty()) => {
                Some(AuthMode::OpenaiApiKey)
            }
            "ANTHROPIC_API_KEY" if std::env::var(name).is_ok_and(|v| !v.is_empty()) => {
                Some(AuthMode::AnthropicApiKey)
            }
            _ => None,
        })
}

fn initial_screen(auth_mode: AuthMode, has_env_key: bool) -> Screen {
    if auth_mode == AuthMode::None && !has_env_key {
        Screen::Login
    } else {
        Screen::Chat
    }
}

fn resolve_cwd_arg(current: &std::path::Path, arg: &str) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(arg);
    let candidate = if path.is_absolute() {
        path
    } else {
        current.join(path)
    };
    if !candidate.is_dir() {
        return None;
    }
    Some(candidate.canonicalize().unwrap_or(candidate))
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

    // Cap redraws to a frame budget while a turn is streaming. The agent emits
    // many token deltas per frame interval and each draw re-wraps the whole
    // transcript; without the cap that runs hundreds of times/sec and shows up
    // as visible jank. When idle (`!busy`) we draw immediately so keystrokes
    // still echo without latency.
    let frame_budget = Duration::from_millis(16);
    let mut last_draw = std::time::Instant::now()
        .checked_sub(frame_budget)
        .unwrap_or_else(std::time::Instant::now);

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
        // Deferred while compacting: a background compaction holds the agent
        // mutex, so locking here would block the whole UI (and both replace
        // history). `&&` short-circuits before `.take()`, so the id is kept.
        if !app.compacting {
            if let Some(id) = app.pending_resume_id.take() {
                apply_resume(&mut app, &agent, &id).await;
            }
        }
        // `/undo` sets this so the agent Arc can stay out of handle_slash.
        // Deferred while compacting for the same reason (left side of `&&`
        // short-circuits, so the flag survives until compaction finishes).
        if !app.compacting && std::mem::take(&mut app.pending_undo) {
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
        // `/compact` and the auto-compact trigger set this so the agent Arc can
        // stay out of the slash/event handlers. Gated on `!busy` so it never
        // runs mid-turn (it locks the same mutex `run_turn` holds). Don't use
        // `mem::take` here: the auto trigger fires while `busy` is still true
        // (AutoCompactSuggested precedes TurnComplete), and consuming the flag
        // before the `!busy` check would silently drop that compaction. Clear
        // it only when we actually start. Runs in the BACKGROUND (a spawned
        // task) so the main loop keeps ticking and animates the progress bar.
        if app.pending_compact && !app.busy && !app.compacting && app.screen == Screen::Chat {
            app.pending_compact = false;
            start_compaction(&mut app, &agent, &agent_tx);
        }
        // After a successful compaction the bar holds at 100% briefly, then we
        // swap in the result line and tear the bar down. The 80ms select tick
        // keeps redrawing during the hold.
        if let Some(done_at) = app.compact_done_at {
            if done_at.elapsed() >= Duration::from_millis(450) {
                app.compacting = false;
                app.compact_started_at = None;
                app.compact_done_at = None;
                if let Some(msg) = app.compact_result_msg.take() {
                    app.blocks.push(Block::System(msg));
                }
                app.auto_scroll = true;
            }
        }
        // Safety net: a compaction task that died WITHOUT reporting (e.g. an
        // unexpected panic) would otherwise pin `compacting` true forever and
        // wedge the queue/undo/resume gates. A panic unwinds and drops the
        // agent MutexGuard, so the lock is free again — force-clearing here is
        // safe. A live compaction is bounded by STREAM_IDLE_TIMEOUT (120s) and
        // a short summary, so it never reaches this 150s backstop.
        if app.compacting && app.compact_done_at.is_none() {
            if let Some(started) = app.compact_started_at {
                if started.elapsed() >= Duration::from_secs(150) {
                    app.compacting = false;
                    app.compact_started_at = None;
                    app.blocks
                        .push(Block::System("compact timed out — try again".into()));
                    app.auto_scroll = true;
                }
            }
        }
        // Flush the message queue after a turn completes. Deferred while
        // compacting: launch_turn would block on the agent mutex the
        // compaction task holds, re-freezing the UI.
        if !app.busy
            && !app.compacting
            && !app.message_queue.is_empty()
            && app.screen == Screen::Chat
        {
            let queued = std::mem::take(&mut app.message_queue);
            app.status_line.clear();
            // Process items individually: dispatch slash commands in order and
            // accumulate normal messages for a single launch_turn. Flushing
            // pending normal messages before each slash command preserves order
            // and avoids sending a raw `/command` string to the model.
            //
            // Crucially, AT MOST ONE turn may be launched per flush: launch_turn
            // sets `busy` and spawns a task that holds the agent mutex for the
            // whole turn. A second launch_turn would block on that mutex while
            // the main loop is stalled here (so `select!` never drains the
            // 256-cap agent_rx), and the running turn would in turn block on a
            // full channel — a hard deadlock. So when we must launch a turn with
            // items still pending, re-queue the remainder and stop; the next
            // flush (after the turn completes) picks them up.
            let mut normal: Vec<String> = Vec::new();
            let mut iter = queued.into_iter();
            let mut launched = false;
            while let Some(item) = iter.next() {
                if let Some(rest) = item.strip_prefix('/') {
                    if normal.is_empty() {
                        handle_slash(&mut app, rest.trim()).await;
                    } else {
                        let combined = std::mem::take(&mut normal).join("\n\n");
                        let mut requeue = vec![item.clone()];
                        requeue.extend(iter.by_ref());
                        app.message_queue = requeue;
                        app.blocks.push(Block::User(combined.clone()));
                        app.auto_scroll = true;
                        launch_turn(&mut app, &agent, &agent_tx, combined).await;
                        launched = true;
                        break;
                    }
                } else {
                    normal.push(item);
                }
            }
            if !launched && !normal.is_empty() {
                let combined = normal.join("\n\n");
                app.blocks.push(Block::User(combined.clone()));
                app.auto_scroll = true;
                launch_turn(&mut app, &agent, &agent_tx, combined).await;
            }
        }

        // Poll login completion (the spawned OAuth flow writes auth.json).
        // Snapshot stage + error exactly once per frame so the transition
        // check and render see a consistent view (the OAuth task can mutate
        // the shared mutex between two reads otherwise).
        let mut login_render: Option<(LoginStage, Option<String>)> = None;
        if app.screen == Screen::Login {
            let stage = app.login.stage().await;
            let login_err = app.login.error_text().await;
            match &stage {
                LoginStage::Success(mode) => {
                    app.auth_mode = *mode;
                    app.screen = Screen::Chat;
                    app.login = LoginScreen::new();
                }
                LoginStage::Cancelled => break,
                _ => {}
            }
            // Only render login if still on login screen (transition may have
            // just fired) — avoids passing a Success snapshot to render.
            if app.screen == Screen::Login {
                login_render = Some((stage, login_err));
            }
        }

        if !app.busy || last_draw.elapsed() >= frame_budget {
            terminal.draw(|f| {
                app.last_width = f.area().width;
                app.last_height = f.area().height;
                match app.screen {
                    Screen::Login => {
                        if let Some((stage, login_err)) = login_render.as_ref() {
                            login::render(f, f.area(), &app.login, stage, login_err.as_deref());
                        }
                    }
                    Screen::Chat => ui::render(f, &mut app),
                }
            })?;
            last_draw = std::time::Instant::now();
        }

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
                // Drain the rest of the burst so a fast token stream produces
                // one redraw per frame, not one redraw per token.
                while let Ok(ev) = agent_rx.try_recv() {
                    apply_agent_event(&mut app, ev);
                }
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
        // Don't insert the mode block between assistant deltas while a turn
        // is streaming — it pushes the open Assistant block up and reads as
        // "reply disappeared". The footer label reflects the new mode either
        // way, so visual feedback is preserved.
        if !app.busy {
            app.blocks
                .push(Block::System(format!("mode → {}", next.label())));
        }
        return Ok(false);
    }
    match key.code {
        KeyCode::Char('c') if ctrl => return Ok(true),
        KeyCode::Char('d') if ctrl && app.input.is_empty() => return Ok(true),
        KeyCode::Char('l') if ctrl => {
            app.blocks.clear();
        }
        KeyCode::Char('u') if ctrl => {
            app.input.clear();
            app.history_pos = None;
        }
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
        // Insert only for a plain key or AltGr (Ctrl+Alt, used for `@{}[]` etc.
        // on international layouts — there ctrl==alt). A lone Ctrl or lone Alt is
        // a command/no-op, not text; otherwise unhandled combos like Ctrl+A typed
        // a literal 'a' into the composer.
        KeyCode::Char(ch) if ctrl == alt => {
            app.input.insert_char(ch);
            app.history_pos = None;
        }
        KeyCode::Enter if shift || alt => app.input.insert_newline(),
        KeyCode::Enter => {
            if app.input.is_empty() {
                return Ok(false);
            }
            let text = app.input.take();
            app.record_history(&text);
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
        KeyCode::Backspace => {
            app.input.backspace();
            app.history_pos = None;
        }
        KeyCode::Left => app.input.move_left(),
        KeyCode::Right => app.input.move_right(),
        KeyCode::Up => {
            // On the first line, recall older history; otherwise move the
            // cursor within the (multi-line) composer.
            if app.input.cursor_pos().0 == 0 {
                app.history_prev();
            } else {
                app.input.move_up();
            }
        }
        KeyCode::Down => {
            // On the last line, walk toward newer history (and the draft);
            // otherwise move the cursor down within the composer.
            if app.input.cursor_pos().0 + 1 >= app.input.line_count() {
                app.history_next();
            } else {
                app.input.move_down();
            }
        }
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
                cancel_current_turn(app).await;
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
            if let Err(e) = config::save(&app.config) {
                app.blocks
                    .push(Block::System(format!("config save failed: {e}")));
            }
            app.blocks.push(Block::System(format!("model → {key_sel}")));
            if app.chain_to_effort {
                app.chain_to_effort = false;
                app.open_overlay(OverlayKind::EffortPicker);
            }
        }
        OverlayKind::EffortPicker => {
            app.config.reasoning_effort = key_sel.to_string();
            if let Err(e) = config::save(&app.config) {
                app.blocks
                    .push(Block::System(format!("config save failed: {e}")));
            }
            app.blocks
                .push(Block::System(format!("effort → {key_sel}")));
        }
        OverlayKind::VerbosityPicker => {
            app.config.verbosity = key_sel.to_string();
            if let Err(e) = config::save(&app.config) {
                app.blocks
                    .push(Block::System(format!("config save failed: {e}")));
            }
            app.blocks
                .push(Block::System(format!("verbosity → {key_sel}")));
        }
        OverlayKind::ResumePicker => {
            if !key_sel.is_empty() {
                app.pending_resume_id = Some(key_sel.to_string());
            }
        }
        OverlayKind::LogoutPicker => {
            if let Some(target) = opencli_core::auth::LogoutTarget::from_key(key_sel) {
                let mut record = auth::load_auth().unwrap_or_default();
                opencli_core::auth::clear_credential(&mut record, target);
                match auth::save_auth(&record) {
                    Ok(_) => {
                        app.auth_mode = auth::effective_mode_with_env(&record);
                        app.blocks.push(Block::System("✅ Logged out.".into()));
                    }
                    Err(e) => app
                        .blocks
                        .push(Block::System(format!("logout failed: {e}"))),
                }
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
            let provider = Provider::from_model(&app.config.model);
            let credential = match opencli_core::auth::resolve_credential(provider).await {
                Ok(c) => c,
                Err(e) => {
                    app.blocks
                        .push(Block::System(format!("resume auth error: {e}")));
                    return;
                }
            };
            let client = match LlmClient::new(credential) {
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
            a.apply_skill_manifest();
            a.load_mcp().await.ok();
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

/// Kick off real compaction in the BACKGROUND (mirrors `launch_turn`'s spawn):
/// a task locks the agent, summarizes the history and REPLACES it with the
/// summary, persists, then reports back via `AgentEvent::CompactDone`. Running
/// off the main loop is what keeps the UI responsive and the progress bar
/// animating instead of freezing on the model call. Returns immediately.
fn start_compaction(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    tx: &mpsc::Sender<AgentEvent>,
) {
    app.compacting = true;
    app.compact_started_at = Some(std::time::Instant::now());
    app.compact_done_at = None;
    app.compact_result_msg = None;
    let agent = agent.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = {
            let mut guard = agent.lock().await;
            match guard.as_mut() {
                Some(a) => {
                    let r = a.compact_history().await;
                    // Persist the compacted history so /resume picks up the
                    // smaller baseline (compaction runs outside the per-turn
                    // save path).
                    if r.is_ok() {
                        if let Err(e) = opencli_core::session::save(&a.to_session_record()) {
                            tracing::debug!(error = %e, "session save after compact failed");
                        }
                    }
                    r
                }
                None => Err(anyhow::anyhow!("no agent yet — nothing to compact")),
            }
        };
        let ev = match result {
            Ok(original_len) => AgentEvent::CompactDone {
                original_len: original_len as u64,
                error: None,
            },
            Err(e) => AgentEvent::CompactDone {
                original_len: 0,
                error: Some(e.to_string()),
            },
        };
        let _ = tx.send(ev).await;
    });
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
    let (head, arg) = split_slash_command(cmd);
    match head {
        "help" | "?" => {
            app.blocks.push(Block::System(
                "Commands:\n  \
                 /login              sign in (OpenAI / Anthropic, OAuth or API key)\n  \
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
            app.login = LoginScreen::new();
            app.screen = Screen::Login;
            app.status_line.clear();
        }
        "apikey" => {
            if arg.is_empty() {
                app.blocks
                    .push(Block::System("Usage: /apikey sk-…".to_string()));
            } else {
                let mut record = auth::load_auth().unwrap_or_default();
                record.mode = AuthMode::OpenaiApiKey;
                record.api_key = Some(arg.to_string());
                record.tokens = None;
                match auth::save_auth(&record) {
                    Ok(_) => {
                        app.auth_mode = AuthMode::OpenaiApiKey;
                        app.blocks.push(Block::System("✅ API key saved.".into()));
                        let models = Provider::OpenAi
                            .available_models()
                            .iter()
                            .map(|m| {
                                let win = opencli_core::agent::context_window_label(m);
                                format!("  · {m:<20} ({win} context)")
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        app.blocks
                            .push(Block::System(format!("Available models:\n{models}")));
                    }
                    Err(e) => app.blocks.push(Block::System(format!("Error: {e}"))),
                }
            }
        }
        "logout" => {
            // Open a picker so the user chooses WHICH stored credential to
            // remove (a session can hold several at once) instead of nuking all
            // of auth.json. Env-var credentials aren't listed — they can't be
            // cleared by logging out.
            if picker::logout_targets().is_empty() {
                app.blocks.push(Block::System(
                    "Nothing to log out — no stored credentials.".into(),
                ));
            } else {
                app.open_overlay(OverlayKind::LogoutPicker);
            }
        }
        "status" => {
            let record = auth::load_auth().unwrap_or_default();
            let active_mode = auth::effective_mode_with_env(&record);
            let mut msg = match active_mode {
                AuthMode::None => "Not signed in.".to_string(),
                AuthMode::OpenaiApiKey => "Signed in with OpenAI API key.".to_string(),
                AuthMode::OpenaiOauth => {
                    let acc = record
                        .tokens
                        .as_ref()
                        .and_then(|t| t.account_id.clone())
                        .unwrap_or_default();
                    format!("Signed in with ChatGPT. account_id={acc}")
                }
                AuthMode::AnthropicApiKey => "Signed in with Anthropic API key.".to_string(),
                AuthMode::AnthropicOauth => "Signed in with Claude Pro/Max OAuth.".to_string(),
            };
            let mut extra = Vec::new();
            if auth::has_openai_oauth(&record) && !matches!(active_mode, AuthMode::OpenaiOauth) {
                extra.push("OpenAI OAuth token is also stored");
            }
            if auth::has_openai_api_key(&record) && !matches!(active_mode, AuthMode::OpenaiApiKey) {
                extra.push("OpenAI API key is also stored");
            }
            if auth::has_anthropic_oauth(&record)
                && !matches!(active_mode, AuthMode::AnthropicOauth)
            {
                extra.push("Anthropic OAuth token is also stored");
            }
            if auth::has_anthropic_api_key(&record)
                && !matches!(active_mode, AuthMode::AnthropicApiKey)
            {
                extra.push("Anthropic API key is also stored");
            }
            for note in extra {
                msg.push_str(&format!("\n  ({note})"));
            }
            app.blocks.push(Block::System(msg));
            let providers = auth::signed_in_providers();
            if !providers.is_empty() {
                let mut text = String::from("Available models:");
                for p in providers {
                    text.push_str(&format!("\n  {} ({}):", p.display_name(), p));
                    for m in p.available_models() {
                        let win = opencli_core::agent::context_window_label(m);
                        text.push_str(&format!("\n    · {m:<20} ({win} context)"));
                    }
                }
                app.blocks.push(Block::System(text));
            }
        }
        "model" => {
            if arg.is_empty() {
                app.chain_to_effort = true;
                app.open_overlay(OverlayKind::ModelPicker);
            } else {
                // Accept an explicit `provider/model` spec; store the bare wire
                // id used everywhere downstream.
                let model = Provider::parse_model(arg).1;
                app.config.model = model.clone();
                if let Err(e) = config::save(&app.config) {
                    app.blocks
                        .push(Block::System(format!("config save failed: {e}")));
                }
                app.blocks.push(Block::System(format!("model → {model}")));
            }
        }
        "effort" | "thinking" => {
            if arg.is_empty() {
                app.open_overlay(OverlayKind::EffortPicker);
            } else if let Some(effort) = config::normalize_reasoning_effort(arg) {
                app.config.reasoning_effort = effort.clone();
                if let Err(e) = config::save(&app.config) {
                    app.blocks
                        .push(Block::System(format!("config save failed: {e}")));
                }
                app.blocks.push(Block::System(format!("effort → {effort}")));
            } else {
                app.blocks.push(Block::System(format!(
                    "Invalid effort `{arg}`. Expected one of: {}",
                    config::VALID_REASONING_EFFORTS.join(", ")
                )));
            }
        }
        "verbosity" => {
            if arg.is_empty() {
                app.open_overlay(OverlayKind::VerbosityPicker);
            } else if let Some(verbosity) = config::normalize_verbosity(arg) {
                app.config.verbosity = verbosity.clone();
                if let Err(e) = config::save(&app.config) {
                    app.blocks
                        .push(Block::System(format!("config save failed: {e}")));
                }
                app.blocks
                    .push(Block::System(format!("verbosity → {verbosity}")));
            } else {
                app.blocks.push(Block::System(format!(
                    "Invalid verbosity `{arg}`. Expected one of: {}",
                    config::VALID_VERBOSITIES.join(", ")
                )));
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
            } else if let Some(path) = resolve_cwd_arg(&app.cwd, arg) {
                app.cwd = path;
                app.blocks
                    .push(Block::System(format!("cwd → {}", app.cwd.display())));
            } else {
                app.blocks.push(Block::System("Invalid path.".into()));
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
                AuthMode::OpenaiApiKey => "api_key",
                AuthMode::OpenaiOauth => "chatgpt",
                AuthMode::AnthropicApiKey => "anthropic_api_key",
                AuthMode::AnthropicOauth => "claude_oauth",
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
                     Add some in ~/.config/opencli/settings.json under mcp_servers"
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
            // Real compaction: main_loop calls Agent::compact_history() on the
            // next tick, which summarizes the history and REPLACES it with the
            // summary — actually reclaiming context, unlike the old behavior
            // that just appended a summary and left the full history in place.
            if app.busy {
                app.blocks.push(Block::System(
                    "Can't compact mid-turn — wait for the current turn to finish.".into(),
                ));
            } else if app.compacting {
                app.blocks.push(Block::System("Already compacting…".into()));
            } else {
                app.pending_compact = true;
            }
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
        "agents" => {
            let defs = opencli_core::subagent::load_all(&app.cwd);
            if defs.is_empty() {
                app.blocks.push(Block::System(format!(
                    "No subagents installed. Create one at {}/agents/<name>.md",
                    opencli_core::config::config_dir().display()
                )));
            } else {
                let mut out = String::from(
                    "Installed subagents:
",
                );
                for d in &defs {
                    let tools = if d.tools.is_empty() {
                        "all".to_string()
                    } else {
                        d.tools.join(", ")
                    };
                    out.push_str(&format!(
                        "  {:<20} {} [tools: {}]
",
                        d.name, d.description, tools
                    ));
                }
                out.push_str(
                    "
Invoke from the model via dispatch_agent with subagent_type set to the name.",
                );
                app.blocks.push(Block::System(out));
            }
        }
        "skills" => {
            let skills = opencli_core::skill::discover(&app.cwd);
            if skills.is_empty() {
                app.blocks.push(Block::System(format!(
                    "No skills installed. Create one at {}/skills/<name>/SKILL.md, or install Claude Code skills under ~/.claude/skills/.",
                    opencli_core::config::config_dir().display()
                )));
            } else {
                let mut out = format!("Available skills ({}):\n", skills.len());
                for s in &skills {
                    out.push_str(&format!("  {:<28} {}\n", s.name, s.description));
                }
                out.push_str(
                    "\nThe model loads a skill's full instructions on demand via the `skill` tool.",
                );
                app.blocks.push(Block::System(out));
            }
        }
        "commands" => {
            let cmds = opencli_core::command::load_all(&app.cwd);
            if cmds.is_empty() {
                app.blocks.push(Block::System(format!(
                    "No custom commands installed. Create one at {}/commands/<name>.md or {}/.opencli/commands/<name>.md",
                    opencli_core::config::config_dir().display(),
                    app.cwd.display()
                )));
            } else {
                let mut out = String::from(
                    "Custom commands:
",
                );
                for c in &cmds {
                    let hint = if c.argument_hint.is_empty() {
                        "".to_string()
                    } else {
                        format!(" {}", c.argument_hint)
                    };
                    out.push_str(&format!(
                        "  /{:<20} {}
      ↳ {}{}
",
                        c.name, c.description, c.name, hint
                    ));
                }
                out.push_str(
                    "
Type /<name> [args] to expand and send.",
                );
                app.blocks.push(Block::System(out));
            }
        }
        other => {
            // Check if it matches a custom command before reporting unknown.
            let cmds = opencli_core::command::load_all(&app.cwd);
            if let Some(cmd) = cmds.iter().find(|c| c.name == other) {
                let expanded = opencli_core::command::expand(&cmd.body, &cmd.name, arg);
                app.input.buffer = expanded;
                app.input.cursor = app.input.buffer.len();
                app.blocks.push(Block::System(format!(
                    "Expanded /{} into input — press Enter to send.",
                    cmd.name
                )));
            } else {
                app.blocks.push(Block::System(format!(
                    "Unknown command /{other}. Try /help, /commands, /agents, or /skills."
                )));
            }
        }
    }
}

fn split_slash_command(cmd: &str) -> (&str, &str) {
    let trimmed = cmd.trim();
    let Some((idx, ch)) = trimmed.char_indices().find(|(_, c)| c.is_whitespace()) else {
        return (trimmed, "");
    };
    let head = &trimmed[..idx];
    let arg = trimmed[idx + ch.len_utf8()..].trim();
    (head, arg)
}

/// Rough OpenAI Responses pricing per model — used by `/cost` for a local
/// estimate. Returns `(input_$_per_million, output_$_per_million)`.
/// Update when official pricing changes; an inexact estimate beats none.
fn pricing_for(model: &str) -> (f64, f64) {
    match model {
        "gpt-5.5" => (5.00, 30.0),
        "gpt-5.4" => (2.50, 15.0),
        "gpt-5.3" | "gpt-5.3-chat-latest" | "gpt-5.3-codex" => (1.75, 14.0),
        "gpt-5" => (1.25, 10.0),
        "gpt-5.5-pro" | "gpt-5.4-pro" => (30.00, 180.0),
        "gpt-5-pro" => (15.00, 120.0),
        "gpt-5.4-mini" => (0.75, 4.50),
        "gpt-5-mini" => (0.25, 2.00),
        "gpt-5.4-nano" => (0.20, 1.25),
        "gpt-5-nano" => (0.05, 0.40),
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
    // UserPromptSubmit hook: may BLOCK the prompt (exit 2). Load hooks fresh
    // (cheap) so it works even on the first turn before the agent exists.
    if let opencli_core::hooks::HookDecision::Block(reason) = opencli_core::hooks::load()
        .fire_user_prompt_submit(&text)
        .await
    {
        app.blocks
            .push(Block::System(format!("⛔ prompt blocked: {reason}")));
        return;
    }
    let provider = Provider::from_model(&app.config.model);
    let credential = match opencli_core::auth::resolve_credential(provider).await {
        Ok(c) => c,
        Err(e) => {
            app.blocks.push(Block::System(format!("Auth error: {e}")));
            return;
        }
    };
    let client = match LlmClient::new(credential) {
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
            a.apply_skill_manifest();
            a.load_mcp().await.ok();
            *guard = Some(a);
        } else {
            // Update mutable config every turn so /model, /effort, /plan take effect.
            if let Some(a) = guard.as_mut() {
                let cwd_changed = a.cwd != app.cwd;
                a.client = client;
                a.config = app.config.clone();
                a.cwd = app.cwd.clone();
                a.approval = app.approval;
                a.require_approval = app.require_approval;
                a.auto_approve_edits = app.auto_approve_edits;
                if cwd_changed {
                    a.refresh_system_context();
                }
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
                tracing::debug!(error = %e, "agent turn failed");
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
async fn cancel_current_turn(app: &mut App) {
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
    if let Some(pending) = app.pending_approval.take() {
        if let Some(handle) = app.approval_handle.clone() {
            let sender = {
                let mut map = handle.lock().await;
                map.remove(&pending.call_id)
            };
            if let Some(sender) = sender {
                let _ = sender.send(false);
            }
        }
    }
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
                if !arguments.is_empty() {
                    *args = arguments;
                }
            }
        }
        AgentEvent::ToolResult {
            call_id,
            output,
            error,
        } => {
            let mut ask_prompt = None;
            if let Some(Block::Tool {
                name,
                output: o,
                error: e,
                ..
            }) = find_tool_mut(&mut app.blocks, &call_id)
            {
                *o = Some(output);
                *e = error;
                if name == "ask_user_question" && !error {
                    ask_prompt = o
                        .as_deref()
                        .and_then(opencli_core::tools::ask::render_ask_envelope);
                }
            }
            if let Some(prompt) = ask_prompt {
                app.blocks.push(Block::System(prompt));
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
            // Invalidate the render cache: this event mutated a non-last block
            // (the tool's output flipped None->Some) while `rotate_assistant_block`
            // removed the empty open assistant and pushed a fresh one, leaving
            // `blocks.len()` unchanged. The cache is keyed on (blocks_len, +
            // last-block fingerprint), so without this the next frame would
            // replay the stale lines where the tool still looked pending.
            app.chat_render_cache = None;
        }
        AgentEvent::TurnComplete => {
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            finish_open_assistant_block(&mut app.blocks);
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
            collapse_reasoning_into_thought(app);
            finish_open_assistant_block(&mut app.blocks);
            app.blocks.push(Block::System(format!("error: {message}")));
            app.busy = false;
            app.is_thinking = false;
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
        AgentEvent::AutoCompactSuggested { used, limit } => {
            let pct = (used as f64 / limit.max(1) as f64 * 100.0) as u64;
            // Stronger signal than ContextWarning — at 85% we are one or two
            // turns away from a hard 1xx context-window failure on the next
            // request. Block::System makes the message persistent in the
            // scrollback so the user can't miss it while scrolling.
            // With auto_compact on, also schedule a real compaction (once per
            // over-threshold window — the guard clears after it succeeds) so a
            // long session never hits a hard context overflow unattended.
            if app.config.auto_compact && !app.auto_compact_done_this_window {
                app.pending_compact = true;
                app.auto_compact_done_this_window = true;
                app.blocks.push(Block::System(format!(
                    "⚠ context {used}/{limit} tokens ({pct}%) — auto-compacting to free space…"
                )));
            } else {
                app.blocks.push(Block::System(format!(
                    "⚠ context {used}/{limit} tokens ({pct}%) — run /compact now to avoid a context overflow on the next turn"
                )));
            }
        }
        AgentEvent::CompactDone {
            original_len,
            error,
        } => match error {
            // Success: snap the bar to 100% and let main_loop hold it briefly
            // before swapping in the result line. Reclaiming context lets auto-
            // compaction fire again in a future over-threshold window.
            None => {
                app.compact_done_at = Some(std::time::Instant::now());
                app.compact_result_msg = Some(format!(
                    "✓ compacted: {original_len} items → 1 summary. Earlier history is now summarized."
                ));
                app.auto_compact_done_this_window = false;
            }
            // Failure / no-op: tear the bar down immediately (no celebratory
            // 100%) and report why.
            Some(e) => {
                app.compacting = false;
                app.compact_started_at = None;
                app.compact_done_at = None;
                app.compact_result_msg = None;
                app.blocks
                    .push(Block::System(format!("compact skipped: {e}")));
                app.auto_scroll = true;
            }
        },
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

fn finish_open_assistant_block(blocks: &mut Vec<Block>) {
    let Some(i) = blocks
        .iter()
        .rposition(|b| matches!(b, Block::Assistant { done: false, .. }))
    else {
        return;
    };
    let should_remove = matches!(
        &blocks[i],
        Block::Assistant {
            text,
            reasoning,
            thought_for_secs,
            ..
        } if text.is_empty() && reasoning.is_empty() && thought_for_secs.is_none()
    );
    if should_remove {
        blocks.remove(i);
    } else if let Block::Assistant { done, .. } = &mut blocks[i] {
        *done = true;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("opencli-tui-app-{name}-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn initial_screen_allows_any_supported_env_key() {
        assert_eq!(initial_screen(AuthMode::None, false), Screen::Login);
        assert_eq!(initial_screen(AuthMode::None, true), Screen::Chat);
        assert_eq!(
            initial_screen(AuthMode::AnthropicApiKey, false),
            Screen::Chat
        );
    }

    #[test]
    fn cwd_arg_resolves_relative_to_current_app_cwd() {
        let root = temp_dir("cwd-root");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let resolved = resolve_cwd_arg(&root, "nested").unwrap();
        assert_eq!(resolved, nested.canonicalize().unwrap());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn slash_command_splits_on_any_whitespace() {
        assert_eq!(split_slash_command("cwd\t./nested"), ("cwd", "./nested"));
        assert_eq!(
            split_slash_command("review   --focus security"),
            ("review", "--focus security")
        );
        assert_eq!(split_slash_command("status"), ("status", ""));
    }

    #[tokio::test]
    async fn login_slash_opens_login_screen_instead_of_detached_task() {
        let mut app = App::new();
        app.screen = Screen::Chat;
        app.status_line = "stale".to_string();

        handle_slash(&mut app, "login").await;

        assert_eq!(app.screen, Screen::Login);
        assert!(matches!(app.login.stage().await, LoginStage::PickMode));
        assert!(app.status_line.is_empty());
    }

    #[test]
    fn empty_args_done_does_not_clear_streamed_tool_args() {
        let mut app = App::new();
        apply_agent_event(
            &mut app,
            AgentEvent::ToolCallStarted {
                name: "read_file".to_string(),
                call_id: "call_1".to_string(),
            },
        );
        apply_agent_event(
            &mut app,
            AgentEvent::ToolCallArgsDelta {
                call_id: "call_1".to_string(),
                delta: "{\"path\":\"src/lib.rs\"}".to_string(),
            },
        );
        apply_agent_event(
            &mut app,
            AgentEvent::ToolCallArgsDone {
                call_id: "call_1".to_string(),
                arguments: String::new(),
            },
        );

        match find_tool_mut(&mut app.blocks, "call_1") {
            Some(Block::Tool { args, .. }) => assert_eq!(args, "{\"path\":\"src/lib.rs\"}"),
            other => panic!("expected tool block, got {other:?}"),
        }
    }

    #[test]
    fn finishing_open_assistant_drops_empty_block() {
        let mut blocks = vec![Block::Assistant {
            text: String::new(),
            reasoning: String::new(),
            done: false,
            thought_for_secs: None,
            reasoning_started_at: None,
        }];

        finish_open_assistant_block(&mut blocks);
        assert!(blocks.is_empty());
    }

    #[test]
    fn finishing_open_assistant_marks_non_empty_block_done() {
        let mut blocks = vec![Block::Assistant {
            text: "hello".to_string(),
            reasoning: String::new(),
            done: false,
            thought_for_secs: None,
            reasoning_started_at: None,
        }];

        finish_open_assistant_block(&mut blocks);
        match &blocks[0] {
            Block::Assistant { done, .. } => assert!(*done),
            other => panic!("expected assistant block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_current_turn_clears_pending_approval_sender() {
        let mut app = App::new();
        app.busy = true;
        app.pending_approval = Some(PendingApproval {
            call_id: "call_1".to_string(),
            tool_name: "run_shell".to_string(),
            args_json: "{}".to_string(),
            diff_preview: None,
        });

        let approvals =
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let (tx, rx) = tokio::sync::oneshot::channel();
        approvals.lock().await.insert("call_1".to_string(), tx);
        app.approval_handle = Some(approvals.clone());

        cancel_current_turn(&mut app).await;

        assert!(!app.busy);
        assert!(app.pending_approval.is_none());
        assert!(app.approval_handle.is_none());
        assert!(!approvals.lock().await.contains_key("call_1"));
        assert_eq!(rx.await, Ok(false));
    }

    #[test]
    fn pricing_for_current_openai_models_matches_api_docs() {
        assert_eq!(pricing_for("gpt-5.5"), (5.00, 30.0));
        assert_eq!(pricing_for("gpt-5.4"), (2.50, 15.0));
        assert_eq!(pricing_for("gpt-5.3"), (1.75, 14.0));
        assert_eq!(pricing_for("gpt-5-pro"), (15.00, 120.0));
        assert_eq!(pricing_for("gpt-5.5-pro"), (30.00, 180.0));
        assert_eq!(pricing_for("gpt-5.4-mini"), (0.75, 4.50));
        assert_eq!(pricing_for("gpt-5.4-nano"), (0.20, 1.25));
        assert_eq!(pricing_for("gpt-5-mini"), (0.25, 2.00));
        assert_eq!(pricing_for("gpt-5-nano"), (0.05, 0.40));
    }

    #[test]
    fn tool_result_invalidates_render_cache() {
        // A completed tool result mutates a non-last block (output None->Some)
        // while rotate_assistant_block keeps blocks.len() constant, which the
        // (blocks_len, last-block fingerprint) cache key alone misses. The
        // handler must drop the cache so the finished tool re-renders instead
        // of replaying the stale "pending" lines.
        let mut app = App::new();
        apply_agent_event(
            &mut app,
            AgentEvent::ToolCallStarted {
                name: "read_file".to_string(),
                call_id: "c1".to_string(),
            },
        );
        apply_agent_event(
            &mut app,
            AgentEvent::ToolCallArgsDone {
                call_id: "c1".to_string(),
                arguments: "{\"path\":\"x\"}".to_string(),
            },
        );
        // Stand in for a frame the renderer already cached.
        app.chat_render_cache = Some(ChatRenderCache {
            blocks_len: app.blocks.len(),
            inner_width: 80,
            expanded_tools: false,
            last_block_size: 0,
            lines: Vec::new(),
            prefix_lines: None,
        });
        apply_agent_event(
            &mut app,
            AgentEvent::ToolResult {
                call_id: "c1".to_string(),
                output: "done".to_string(),
                error: false,
            },
        );
        assert!(
            app.chat_render_cache.is_none(),
            "tool result must invalidate the stale render cache"
        );
    }
}
