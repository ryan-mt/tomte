use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
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
use opencli_core::session::{SessionGoalSnapshot, SessionRecord};
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
    /// Stable string persisted to `config.default_permission_mode`. Matches the
    /// names Claude Code uses for `permissions.defaultMode`.
    pub fn config_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPerms => "bypassPermissions",
        }
    }
    /// Inverse of `config_str`. Unknown values fall back to `Default` so a
    /// hand-edited or stale config can never wedge startup.
    pub fn from_config_str(s: &str) -> Self {
        match s {
            "plan" => Self::Plan,
            "acceptEdits" => Self::AcceptEdits,
            "bypassPermissions" => Self::BypassPerms,
            _ => Self::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    SlashMenu,
    FilePicker,
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
    run_with(false, false).await
}

pub async fn run_plan_mode_required() -> Result<()> {
    run_with(false, true).await
}

/// Same as [`run`] but opens the resume-session picker on first frame so
/// `opencli resume` lands the user directly on the session list.
pub async fn run_resume() -> Result<()> {
    run_with(true, false).await
}

pub async fn run_resume_plan_mode_required() -> Result<()> {
    run_with(true, true).await
}

async fn run_with(start_with_resume_picker: bool, plan_mode_required: bool) -> Result<()> {
    // Install a panic hook that restores the terminal before unwinding, so a
    // panic inside main_loop (or any library it pulls in) doesn't leave the
    // user's shell stuck in raw mode + alternate screen.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    // SessionStart hook (best-effort, once per interactive session). Spawned so a
    // slow hook can't delay the first frame; its output/exit code is ignored.
    tokio::spawn(async { opencli_core::hooks::load().fire_session_start().await });
    let res = main_loop(&mut terminal, start_with_resume_picker, plan_mode_required).await;
    restore_terminal(&mut terminal)?;
    res
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // EnableBracketedPaste makes the terminal wrap pasted text in escape
    // markers and deliver it as one `Event::Paste(String)` instead of a stream
    // of individual key presses. Without it, a multi-line paste arrives as
    // KeyCode::Char + KeyCode::Enter events, and the first Enter submits the
    // (partial) message — the "long paste auto-sends" bug.
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(out);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(t: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        t.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
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

/// An in-progress `/buddy` hatch animation: a wobbling egg that cracks and
/// reveals the account's companion, after which the small corner buddy takes
/// over. Time-driven, like the spinner, so the frame follows wall-clock.
#[derive(Debug, Clone)]
pub struct HatchAnim {
    pub pet: usize,
    pub started: std::time::Instant,
}

/// One live sub-agent row in the fleet view, populated from `Subagent*` events
/// forwarded by `dispatch_agent`. Mirrors Claude Code's sub-agent list.
#[derive(Debug, Clone)]
pub struct SubagentView {
    pub id: String,
    pub kind: String,
    pub prompt: String,
    pub activity: String,
    pub steps: u64,
    pub started_at: std::time::Instant,
    /// None while running; `Some(ok)` once the sub-agent finishes.
    pub done: Option<bool>,
    /// Toggled by clicking the row — shows the full prompt + status detail.
    pub expanded: bool,
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
    /// Screen rect of the clickable "Jump to bottom" hint. `render` sets it each
    /// frame when the user has scrolled away from the tail (and clears it
    /// otherwise); the mouse handler tests left-clicks against it to re-enable
    /// auto-follow, mirroring Claude Code's click-to-jump affordance.
    pub jump_to_bottom_hint: Option<ratatui::layout::Rect>,
    /// Live sub-agents spawned by `dispatch_agent` during the current turn, in
    /// start order. Rendered as a fleet-view panel above the input and cleared
    /// when the turn ends.
    pub subagents: Vec<SubagentView>,
    /// Screen rect of each fleet row paired with its sub-agent id, set by
    /// `render` each frame so a left-click can toggle the row's detail.
    pub subagent_rows: Vec<(ratatui::layout::Rect, String)>,
    pub status_line: String,
    pub last_height: u16,
    pub last_width: u16,
    pub turn_started_at: Option<std::time::Instant>,
    pub spinner_word: String,
    pub tokens_used: u64,
    /// Cumulative input tokens for the session — used by `/cost` to estimate
    /// USD spend and break down the bill.
    pub input_tokens_total: u64,
    /// Cumulative output (reasoning + completion) tokens for the session.
    pub output_tokens_total: u64,
    /// Number of turns the user has run this session. Surfaced by `/cost`.
    pub turn_count: u64,
    /// Latest provider quota/rate-limit snapshot, captured from the most recent
    /// turn's response. Surfaced by `/usage`; `None` until the first turn.
    pub last_quota: Option<opencli_core::usage::QuotaSnapshot>,
    pub pending_images: Vec<std::path::PathBuf>,
    pub next_image_num: usize,
    /// Output of `!`-prefixed shell commands the user ran from the composer,
    /// waiting to be prepended to the next real message as context so the model
    /// can reason about it. Drained in `launch_turn`.
    pub pending_shell_context: Vec<String>,
    /// `/buddy` companion state. `hatch` is the in-progress reveal animation;
    /// `buddy_pet` is the adopted pet (locked to the account) shown small in the
    /// corner; `buddy_hidden` toggles that corner display via `/buddy off`.
    pub hatch: Option<HatchAnim>,
    pub buddy_pet: Option<usize>,
    pub buddy_hidden: bool,
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
    /// Completion timestamps keyed by stable todo text. Used only for display
    /// priority so newly completed items remain visible briefly in long lists.
    pub todo_completed_at: HashMap<String, std::time::Instant>,
    /// Whether the Claude-style live todo panel is expanded above the input.
    /// Defaults on so users see progress as soon as the model writes todos;
    /// Ctrl+T toggles it without touching the canonical todo state.
    pub show_todos: bool,
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
    /// Active `/goal` objective, if the host is automatically continuing turns
    /// until the model reports complete or blocked via `goal_update`.
    pub active_goal: Option<ActiveGoal>,
    /// A new `/goal <objective>` submitted while another goal is active. The
    /// user must confirm before the current goal is replaced.
    pub pending_goal_replacement: Option<PendingGoalReplacement>,
    /// Plan proposed by `exit_plan_mode`, waiting for the user to approve
    /// leaving plan mode or reject and continue planning.
    pub pending_plan_exit: Option<PendingPlanExit>,
    /// Set whenever host-owned session state (currently `/goal`) changes and
    /// should be merged into the persisted session record once the agent is idle.
    pub pending_session_save: bool,
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
    /// Highlighted menu option: 0 = allow once, 1 = allow this tool/command in
    /// this project (persisted to .opencli/permissions.json), 2 = deny. Driven
    /// by Up/Down; Enter commits it.
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct ActiveGoal {
    pub objective: String,
    pub turns_completed: u32,
    pub waiting_for_user: bool,
    pub last_summary: Option<String>,
    pub started_at: std::time::Instant,
    pub started_at_ms: u64,
}

impl ActiveGoal {
    pub fn new(objective: String) -> Self {
        Self {
            objective,
            turns_completed: 0,
            waiting_for_user: false,
            last_summary: None,
            started_at: std::time::Instant::now(),
            started_at_ms: opencli_core::session::now_ms(),
        }
    }

    pub fn elapsed_label(&self) -> String {
        format_goal_elapsed(self.started_at.elapsed())
    }

    pub fn to_session_snapshot(&self) -> SessionGoalSnapshot {
        SessionGoalSnapshot {
            objective: self.objective.clone(),
            turns_completed: self.turns_completed,
            waiting_for_user: self.waiting_for_user,
            last_summary: self.last_summary.clone(),
            started_at_ms: self.started_at_ms,
        }
    }

    pub fn from_session_snapshot(snapshot: SessionGoalSnapshot) -> Self {
        let elapsed = opencli_core::session::now_ms().saturating_sub(snapshot.started_at_ms);
        let started_at = std::time::Instant::now()
            .checked_sub(Duration::from_millis(elapsed))
            .unwrap_or_else(std::time::Instant::now);
        Self {
            objective: snapshot.objective,
            turns_completed: snapshot.turns_completed,
            waiting_for_user: snapshot.waiting_for_user,
            last_summary: snapshot.last_summary,
            started_at,
            started_at_ms: snapshot.started_at_ms,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingGoalReplacement {
    pub objective: String,
}

#[derive(Debug, Clone)]
pub struct PendingPlanExit {
    pub plan: String,
}

const GOAL_START_PREFIX: &str = "[opencli:/goal start]";
const GOAL_CONTINUATION_PREFIX: &str = "[opencli:/goal continuation]";
/// Max words allowed in a `/goal` objective. The objective is re-injected into
/// context on every autonomous continuation turn, so an overlong one crowds out
/// the actual work and degrades the model's reasoning ("mất thông minh"). ~100
/// words (≈150 tokens/turn) stays negligible over a long run while still fitting
/// a detailed, multi-step objective; longer detail belongs in a normal message.
const GOAL_MAX_WORDS: usize = 100;

/// Char ceiling that backstops [`GOAL_MAX_WORDS`] for scripts without spaces
/// (CJK, Thai, …), where word-counting collapses a whole objective to one
/// "word" and would let the limit be bypassed entirely. Sized so any objective
/// within the word cap (~100 words ≈ ~650 chars) stays under it, so it only
/// bites genuinely overlong non-whitespace-delimited input.
const GOAL_MAX_CHARS: usize = 800;

/// Word count used to bound a `/goal` objective (whitespace-separated).
fn goal_word_count(objective: &str) -> usize {
    objective.split_whitespace().count()
}

/// Whether a `/goal` objective is too long to re-inject every turn. Bounds by
/// word count (whitespace-delimited languages) AND raw char count, so a CJK/Thai
/// objective with no whitespace — which counts as a single word — is still
/// caught instead of bypassing the limit.
fn goal_exceeds_limit(objective: &str) -> bool {
    goal_word_count(objective) > GOAL_MAX_WORDS || objective.chars().count() > GOAL_MAX_CHARS
}
const PLAN_APPROVED_PREFIX: &str = "[opencli:/plan approved]";
const PLAN_REJECTED_PREFIX: &str = "[opencli:/plan rejected]";
pub const TODO_RECENT_COMPLETED_TTL: Duration = Duration::from_secs(30);
/// Cap on in-memory composer recall history so a multi-day session can't grow
/// it without bound. Oldest entries are dropped past this.
const MAX_INPUT_HISTORY: usize = 1000;

pub fn todo_completion_key(todo: &TodoItem) -> String {
    format!("{}\n{}", todo.content, todo.active_form)
}

pub fn format_goal_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs <= 60 {
        format!("{secs}s")
    } else {
        format!("{}m{}", secs / 60, secs % 60)
    }
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
    // More flavor — the spinner word is picked per turn, so a longer list means
    // less repetition across a session.
    "Conjuring",
    "Summoning",
    "Channeling",
    "Manifesting",
    "Tinkering",
    "Wrangling",
    "Untangling",
    "Noodling",
    "Percolating",
    "Marinating",
    "Simmering",
    "Brewing",
    "Distilling",
    "Fermenting",
    "Crystallizing",
    "Synthesizing",
    "Assembling",
    "Engineering",
    "Architecting",
    "Sculpting",
    "Chiseling",
    "Whittling",
    "Polishing",
    "Buffing",
    "Calibrating",
    "Tuning",
    "Orchestrating",
    "Choreographing",
    "Weaving",
    "Spinning",
    "Knitting",
    "Stitching",
    "Threading",
    "Plotting",
    "Scheming",
    "Devising",
    "Hatching",
    "Concocting",
    "Brainstorming",
    "Daydreaming",
    "Wondering",
    "Speculating",
    "Theorizing",
    "Hypothesizing",
    "Extrapolating",
    "Computing",
    "Crunching",
    "Number-crunching",
    "Processing",
    "Parsing",
    "Compiling",
    "Optimizing",
    "Refactoring",
    "Debugging",
    "Untangling spaghetti",
    "Herding bits",
    "Wrangling tokens",
    "Chasing pointers",
    "Greasing gears",
    "Stoking the furnace",
    "Charging flux",
    "Spooling up",
    "Warming up",
    "Limbering up",
    "Cranking",
    "Whirring",
    "Vibing",
    "Grooving",
    "Riffing",
    "Jamming",
    "Improvising",
    "Freestyling",
    "Doodling",
    "Sketching",
    "Drafting",
    "Outlining",
    "Storyboarding",
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
        let mut app = Self {
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
            jump_to_bottom_hint: None,
            subagents: Vec::new(),
            subagent_rows: Vec::new(),
            status_line: String::new(),
            last_height: 0,
            last_width: 0,
            turn_started_at: None,
            spinner_word: String::new(),
            tokens_used: 0,
            input_tokens_total: 0,
            output_tokens_total: 0,
            turn_count: 0,
            last_quota: None,
            pending_images: Vec::new(),
            next_image_num: 1,
            pending_shell_context: Vec::new(),
            hatch: None,
            buddy_pet: None,
            buddy_hidden: false,
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
            todo_completed_at: HashMap::new(),
            show_todos: true,
            chat_render_cache: None,
            pending_approval: None,
            approval_handle: None,
            input_history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            active_goal: None,
            pending_goal_replacement: None,
            pending_plan_exit: None,
            pending_session_save: false,
        };
        // Restore the last-persisted permission mode (Claude Code's
        // `defaultMode`). Overrides the literal defaults above for the three
        // approval fields via the canonical setter.
        app.set_permission_mode(PermissionMode::from_config_str(
            &app.config.default_permission_mode,
        ));
        app
    }

    /// Record a submitted prompt in the input history (skipping a consecutive
    /// duplicate) and reset the browse cursor.
    fn record_history(&mut self, text: &str) {
        if self.input_history.last().map(String::as_str) != Some(text) {
            self.input_history.push(text.to_string());
            // Drop the oldest entries once past the cap. Safe here because
            // `history_pos` is reset to `None` right below, so no live cursor
            // can be left pointing at a shifted index.
            if self.input_history.len() > MAX_INPUT_HISTORY {
                let overflow = self.input_history.len() - MAX_INPUT_HISTORY;
                self.input_history.drain(0..overflow);
            }
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

    /// Whether a deferred operation that locks the agent mutex (resume load,
    /// undo) may run on this iteration. It must NOT run while a turn is streaming
    /// (`busy`) or a compaction is in flight (`compacting`): both hold the agent
    /// mutex, so locking it from the main loop would stall the loop, and a turn
    /// that then fills the bounded `agent_rx` would block on `tx.send` forever —
    /// a hard deadlock. Any new agent-locking deferred op must gate on this.
    pub fn can_run_deferred_agent_op(&self) -> bool {
        !self.compacting && !self.busy
    }

    pub fn open_overlay(&mut self, kind: OverlayKind) {
        let picker = match kind {
            OverlayKind::SlashMenu => Picker::new("commands", picker::slash_commands()),
            OverlayKind::FilePicker => Picker::new("attach file (@)", file_candidates(&self.cwd)),
            OverlayKind::ModelPicker => {
                let mut p = Picker::new("select model", picker::models());
                // pre-select current
                if let Some(i) = p.items.iter().position(|it| it.key == self.config.model) {
                    p.selected = i;
                }
                p
            }
            OverlayKind::EffortPicker => {
                let mut p = Picker::new("reasoning effort", picker::efforts(&self.config.model));
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

fn apply_host_state_to_session_record(app: &App, record: &mut SessionRecord) {
    record.state.active_goal = app
        .active_goal
        .as_ref()
        .map(ActiveGoal::to_session_snapshot);
}

fn set_permission_mode_and_save(app: &mut App, mode: PermissionMode) {
    app.set_permission_mode(mode);
    app.config.default_permission_mode = mode.config_str().to_string();
    if let Err(e) = config::save(&app.config) {
        app.blocks
            .push(Block::System(format!("config save failed: {e}")));
    }
}

fn permission_mode_after_plan_approval(app: &App) -> PermissionMode {
    match PermissionMode::from_config_str(&app.config.default_permission_mode) {
        PermissionMode::Plan => PermissionMode::Default,
        mode => mode,
    }
}

fn apply_plan_mode_required(app: &mut App) {
    app.set_permission_mode(PermissionMode::Plan);
    app.pending_plan_exit = None;
    app.blocks.push(Block::System(
        "plan mode required → on (read-only until a plan is approved)".into(),
    ));
    app.auto_scroll = true;
}

async fn save_current_session_record(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
) {
    let mut record = {
        let guard = agent.lock().await;
        let Some(a) = guard.as_ref() else {
            app.pending_session_save = false;
            return;
        };
        a.to_session_record().await
    };
    apply_host_state_to_session_record(app, &mut record);
    if let Err(e) = opencli_core::session::save(&record) {
        tracing::debug!(error = %e, "session save with host state failed");
    }
    app.pending_session_save = false;
}

async fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    start_with_resume_picker: bool,
    plan_mode_required: bool,
) -> Result<()> {
    let mut app = App::new();
    if plan_mode_required {
        apply_plan_mode_required(&mut app);
    }
    if start_with_resume_picker && app.screen == Screen::Chat {
        app.start_with_resume_picker = true;
    }
    let mut events = EventStream::new();
    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(256);
    // Background `!`-command output flows back here so it lands on the UI thread
    // (the command itself runs off the event loop — see `handle_bang_shell`).
    let (bang_tx, mut bang_rx) = mpsc::channel::<BangResult>(8);
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
        // Deferred while compacting OR busy: a background compaction/turn holds
        // the agent mutex, so `apply_resume` locking it here would stall the main
        // loop — and a streaming turn that then fills the 256-cap agent_rx would
        // block forever on `tx.send`, a hard deadlock (the resume picker can be
        // opened mid-turn). `&&` short-circuits before `.take()`, so the id is
        // kept until the turn finishes.
        if app.can_run_deferred_agent_op() {
            if let Some(id) = app.pending_resume_id.take() {
                apply_resume(&mut app, &agent, &id).await;
            }
        }
        // `/undo` sets this so the agent Arc can stay out of handle_slash.
        // Deferred while compacting OR busy for the same agent-mutex reason as
        // the resume load above (left side of `&&` short-circuits, so the flag
        // survives until the turn/compaction finishes).
        if app.can_run_deferred_agent_op() && std::mem::take(&mut app.pending_undo) {
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
        if app.pending_session_save && !app.busy && !app.compacting && app.screen == Screen::Chat {
            save_current_session_record(&mut app, &agent).await;
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
        //
        // Also deferred while a Y/N decision is pending (plan approval or goal
        // replacement): a turn finishing with `exit_plan_mode` leaves `!busy`
        // with the approval prompt up, but type-ahead messages the user queued
        // while the agent was planning would otherwise flush here and relaunch a
        // turn — flipping `busy` back to true. With `pending_plan_exit` set AND
        // `busy`, the key router swallows the Y keystroke (see handle_key), so
        // the user can never approve the plan and the UI looks frozen. Hold the
        // queue until the decision is resolved; approving/rejecting clears the
        // pending state and queues its own follow-up, which then flushes.
        if !app.busy
            && !app.compacting
            && !app.message_queue.is_empty()
            && app.screen == Screen::Chat
            && !turn_launch_blocked_by_pending_decision(&app)
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
                // Composer-prefix items (`/`, `!`, `#`) are commands, not model
                // input — dispatch them in order instead of sending them to the
                // model verbatim. To preserve ordering, any pending normal text
                // is flushed as a turn first (then we requeue and stop, since
                // launching a turn must be the last action — see the deadlock
                // note above).
                let is_command =
                    item.starts_with('/') || item.starts_with('!') || item.starts_with('#');
                if is_command {
                    if normal.is_empty() {
                        if let Some(rest) = item.strip_prefix('/') {
                            handle_slash(&mut app, rest.trim()).await;
                        } else if let Some(cmd) = item.strip_prefix('!') {
                            handle_bang_shell(&mut app, &bang_tx, cmd.trim());
                        } else if let Some(note) = item.strip_prefix('#') {
                            handle_hash_memory(&mut app, &agent, note.trim()).await;
                        }
                    } else {
                        let combined = std::mem::take(&mut normal).join("\n\n");
                        let mut requeue = vec![item.clone()];
                        requeue.extend(iter.by_ref());
                        app.message_queue = requeue;
                        push_visible_user_block(&mut app, &combined);
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
                push_visible_user_block(&mut app, &combined);
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

        prune_expired_completed_todos(&mut app);

        // Finish the hatch animation once it has run its course, adopting the
        // companion (the loop keeps redrawing it via the idle tick below).
        if app.hatch.as_ref().is_some_and(|h| {
            h.started.elapsed() >= Duration::from_millis(crate::tui::buddy::HATCH_MS)
        }) {
            finish_hatch(&mut app);
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
                                if handle_key(&mut app, key, &agent, &agent_tx, &bang_tx).await? { break; }
                            }
                        }
                    }
                    Ok(Event::Resize(_, _)) => {}
                    // Bracketed paste: the whole clipboard arrives as one event,
                    // so multi-line text lands in the composer for editing
                    // instead of submitting on the first newline.
                    Ok(Event::Paste(text))
                        if app.screen == Screen::Chat && app.pending_approval.is_none() =>
                    {
                        for c in text.chars() {
                            if c == '\r' {
                                continue;
                            }
                            app.input.insert_char(c);
                        }
                        app.history_pos = None;
                    }
                    Ok(Event::Mouse(m)) => {
                        use crossterm::event::{MouseButton, MouseEventKind};
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                app.scroll = app.scroll.saturating_sub(3);
                                app.auto_scroll = false;
                            }
                            MouseEventKind::ScrollDown => {
                                app.scroll = app.scroll.saturating_add(3);
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                let (col, row) = (m.column, m.row);
                                if app
                                    .jump_to_bottom_hint
                                    .is_some_and(|r| point_in(r, col, row))
                                {
                                    // Click the jump-to-bottom bar → resume tail-following.
                                    app.auto_scroll = true;
                                } else if let Some(id) = app
                                    .subagent_rows
                                    .iter()
                                    .find(|(r, _)| point_in(*r, col, row))
                                    .map(|(_, id)| id.clone())
                                {
                                    // Click a fleet row → toggle its detail.
                                    if let Some(s) =
                                        app.subagents.iter_mut().find(|s| s.id == id)
                                    {
                                        s.expanded = !s.expanded;
                                    }
                                }
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
            Some(r) = bang_rx.recv() => {
                // A background `!`-command finished; show its output and stage it
                // as context for the next message.
                app.blocks.push(Block::System(r.display));
                app.pending_shell_context.push(r.context);
                app.auto_scroll = true;
            }
            _ = tokio::time::sleep(Duration::from_millis(if app.hatch.is_some() { 45 } else { 80 })) => {
                // Wake periodically so the spinner redraws while a turn runs and
                // no agent events arrive; the glyph itself advances on elapsed
                // wall-clock time in render_spinner, not on a counter.
            }
        }
    }
    Ok(())
}

/// Finish the hatch animation: adopt the pet (locked to the account) and let
/// the small corner companion take over.
fn finish_hatch(app: &mut App) {
    if let Some(h) = app.hatch.take() {
        app.buddy_pet = Some(h.pet);
        app.buddy_hidden = false;
    }
}

async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    tx: &mpsc::Sender<AgentEvent>,
    bang_tx: &mpsc::Sender<BangResult>,
) -> Result<bool> {
    // A keypress during the hatch animation skips straight to the reveal.
    if app.hatch.is_some() {
        finish_hatch(app);
        return Ok(false);
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if app.pending_approval.is_some() {
        // Three-option menu, navigable with Up/Down + Enter, with y/n/Esc kept
        // as shortcuts. 0 = allow once, 1 = allow in this project (persisted to
        // .opencli/permissions.json), 2 = deny.
        const N_OPTS: usize = 3;
        match key.code {
            KeyCode::Up => {
                if let Some(p) = app.pending_approval.as_mut() {
                    p.selected = (p.selected + N_OPTS - 1) % N_OPTS;
                }
                return Ok(false);
            }
            KeyCode::Down => {
                if let Some(p) = app.pending_approval.as_mut() {
                    p.selected = (p.selected + 1) % N_OPTS;
                }
                return Ok(false);
            }
            _ => {}
        }
        let sel = app.pending_approval.as_ref().map_or(0, |p| p.selected);
        let choice = match key.code {
            KeyCode::Enter => Some(sel),
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(0),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(2),
            _ => None,
        };
        if let Some(choice) = choice {
            let p = app
                .pending_approval
                .take()
                .expect("pending_approval present");
            let granted = choice != 2;
            // Option 1 = "allow in this project": persist a rule to
            // .opencli/permissions.json so this tool/command never prompts again
            // in this project (the agent re-reads the file on its next gate
            // check). Unlike a session-wide bypass, it is scoped and durable.
            if choice == 1 {
                let args_val: serde_json::Value =
                    serde_json::from_str(&p.args_json).unwrap_or(serde_json::Value::Null);
                match opencli_core::permissions::allow_in_project(&app.cwd, &p.tool_name, &args_val)
                {
                    Ok(rule) => app.blocks.push(Block::System(format!(
                        "✓ allowed in this project: {rule} — saved to .opencli/permissions.json"
                    ))),
                    Err(e) => app.blocks.push(Block::System(format!(
                        "could not save project permission: {e}"
                    ))),
                }
            } else {
                let label = if granted { "approved" } else { "denied" };
                app.blocks
                    .push(Block::System(format!("{label}: {}", p.tool_name)));
            }
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
        }
        return Ok(false);
    }

    if app.pending_goal_replacement.is_some() {
        if key.code == KeyCode::Char('c') && ctrl {
            return Ok(true);
        }
        handle_goal_replacement_key(app, key);
        return Ok(false);
    }

    if app.pending_plan_exit.is_some() {
        if key.code == KeyCode::Char('c') && ctrl {
            return Ok(true);
        }
        if app.busy {
            return Ok(false);
        }
        handle_plan_exit_key(app, key);
        return Ok(false);
    }

    if app.overlay.is_some() {
        return handle_overlay_key(app, key).await;
    }

    if matches!(key.code, KeyCode::BackTab) {
        let next = app.permission_mode().next();
        // Persist so the mode survives quit/relaunch (mirrors how /model and
        // /effort save on change). Errors are surfaced, not swallowed.
        set_permission_mode_and_save(app, next);
        // No chat notification: the status-bar footer already shows the active
        // mode (see render_status), matching Claude Code, which only updates the
        // indicator on Shift+Tab rather than printing a line.
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
        KeyCode::Char('a') if ctrl => app.input.move_to_start(),
        KeyCode::Char('k') if ctrl => {
            app.input.kill_to_line_end();
            app.history_pos = None;
        }
        KeyCode::Char('v') if ctrl => {
            handle_paste(app).await;
        }
        KeyCode::Char('o') if ctrl => {
            app.expanded_tools = !app.expanded_tools;
        }
        KeyCode::Char('t') if ctrl => {
            app.show_todos = !app.show_todos;
        }
        KeyCode::Char('/') if app.input.is_empty() => {
            // Trigger the slash menu overlay; also insert '/' into the input
            // so users can keep typing to filter.
            app.input.insert_char('/');
            app.open_overlay(OverlayKind::SlashMenu);
        }
        KeyCode::Char('@') if ctrl == alt => {
            // Insert '@' and open the file typeahead; characters typed after it
            // filter the list (handled in handle_overlay_key), matching the
            // `@file` reference flow in Claude Code / Codex.
            app.input.insert_char('@');
            app.history_pos = None;
            app.open_overlay(OverlayKind::FilePicker);
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
            if !app.busy && !app.compacting {
                if let Some(rest) = text.strip_prefix('/') {
                    handle_slash(app, rest.trim()).await;
                    return Ok(false);
                }
                if let Some(cmd) = text.strip_prefix('!') {
                    handle_bang_shell(app, bang_tx, cmd.trim());
                    return Ok(false);
                }
                if let Some(note) = text.strip_prefix('#') {
                    handle_hash_memory(app, agent, note.trim()).await;
                    return Ok(false);
                }
                app.blocks.push(Block::User(text.clone()));
                app.auto_scroll = true;
                launch_turn(app, agent, tx, text).await;
            } else {
                // Busy or compacting: queue the message. During compaction
                // launch_turn would block on the agent mutex the compaction task
                // holds, freezing the UI; the post-compaction queue flush picks
                // it up instead. Slash commands queue too, no special-casing, so
                // the user's intent is preserved.
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
        KeyCode::End if ctrl => app.auto_scroll = true,
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

/// True while the user owes a Y/N decision that must be resolved before any new
/// turn launches. Used to hold the message-queue flush so queued type-ahead
/// can't relaunch a turn under an open approval prompt (which would lock the
/// user out of pressing Y — see the flush gate in `main_loop`).
fn turn_launch_blocked_by_pending_decision(app: &App) -> bool {
    app.pending_plan_exit.is_some() || app.pending_goal_replacement.is_some()
}

fn handle_worktree_slash(app: &mut App, arg: &str) {
    let mut parts = arg.split_whitespace();
    let Some(cmd) = parts.next() else {
        app.blocks.push(Block::System(
            "Usage:\n  /worktree create [name]\n  /worktree exit keep\n  /worktree exit remove [--discard]"
                .into(),
        ));
        return;
    };

    match cmd {
        "create" | "enter" => {
            let name = parts.next().map(str::to_string);
            if parts.next().is_some() {
                app.blocks
                    .push(Block::System("Usage: /worktree create [name]".into()));
                return;
            }
            let prompt = match &name {
                Some(name) => format!(
                    "Create and enter an isolated git worktree for this session using the enter_worktree tool. Pass name={name:?}, then report the worktree path and branch."
                ),
                None => "Create and enter an isolated git worktree for this session using the enter_worktree tool with name=null, then report the worktree path and branch."
                    .to_string(),
            };
            app.message_queue.push(prompt);
            app.blocks.push(Block::System(
                "Queued worktree creation; the agent will create it with enter_worktree.".into(),
            ));
            app.auto_scroll = true;
        }
        "exit" | "leave" => {
            let Some(action) = parts.next() else {
                app.blocks.push(Block::System(
                    "Usage: /worktree exit keep|remove [--discard]".into(),
                ));
                return;
            };
            if !matches!(action, "keep" | "remove") {
                app.blocks.push(Block::System(
                    "Usage: /worktree exit keep|remove [--discard]".into(),
                ));
                return;
            }
            let mut discard = false;
            for part in parts {
                if part == "--discard" {
                    discard = true;
                } else {
                    app.blocks.push(Block::System(format!(
                        "Unknown worktree flag `{part}`. Usage: /worktree exit keep|remove [--discard]"
                    )));
                    return;
                }
            }
            let prompt = format!(
                "Exit the active session worktree using the exit_worktree tool. Pass action={action:?} and discard_changes={discard}. Report what happened."
            );
            app.message_queue.push(prompt);
            app.blocks.push(Block::System(format!(
                "Queued worktree exit ({action}); the agent will call exit_worktree."
            )));
            app.auto_scroll = true;
        }
        _ => app.blocks.push(Block::System(
            "Usage:\n  /worktree create [name]\n  /worktree exit keep\n  /worktree exit remove [--discard]"
                .into(),
        )),
    }
}

fn handle_goal_replacement_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(pending) = app.pending_goal_replacement.take() else {
                return;
            };
            start_goal(app, pending.objective, true);
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            let Some(pending) = app.pending_goal_replacement.take() else {
                return;
            };
            if let Some(goal) = &app.active_goal {
                app.blocks.push(Block::System(format!(
                    "Kept active goal: {}\nDiscarded new goal: {}",
                    goal.objective, pending.objective
                )));
            }
            queue_goal_continuation(app);
            app.auto_scroll = true;
        }
        _ => {}
    }
}

fn handle_plan_exit_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(pending) = app.pending_plan_exit.take() else {
                return;
            };
            if let Some(goal) = app.active_goal.as_mut() {
                goal.waiting_for_user = false;
                goal.last_summary = Some("plan approved".into());
                app.pending_session_save = true;
            }
            app.set_permission_mode(permission_mode_after_plan_approval(app));
            app.message_queue.push(format!(
                "{PLAN_APPROVED_PREFIX}\n\nThe user approved this plan and exited plan mode. Implement it now and verify the result.\n\nApproved plan:\n{}",
                pending.plan
            ));
            app.blocks.push(Block::System(
                "Plan approved — leaving plan mode and continuing.".into(),
            ));
            app.auto_scroll = true;
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            let Some(pending) = app.pending_plan_exit.take() else {
                return;
            };
            if let Some(goal) = app.active_goal.as_mut() {
                goal.waiting_for_user = false;
                goal.last_summary = Some("plan rejected; revising".into());
                app.pending_session_save = true;
            }
            app.set_permission_mode(PermissionMode::Plan);
            app.message_queue.push(format!(
                "{PLAN_REJECTED_PREFIX}\n\nThe user rejected this plan. Stay in plan mode, revise the plan, and call exit_plan_mode again only when the revised plan is ready.\n\nRejected plan:\n{}",
                pending.plan
            ));
            app.blocks.push(Block::System(
                "Plan rejected — staying in plan mode.".into(),
            ));
            app.auto_scroll = true;
        }
        _ => {}
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
        // FilePicker arms must precede the generic Esc/Enter below, which assume
        // a slash/model/effort selection.
        KeyCode::Esc if kind == OverlayKind::FilePicker => {
            // Dismiss the typeahead but keep whatever the user typed.
            app.overlay = None;
        }
        KeyCode::Enter | KeyCode::Tab if kind == OverlayKind::FilePicker => {
            if let Some(key_sel) = picker.selected_key() {
                app.input.complete_at_token(&key_sel);
            }
            app.overlay = None;
        }
        KeyCode::Backspace if kind == OverlayKind::FilePicker => {
            app.input.backspace();
            match app.input.active_at_token() {
                Some((_, q)) => {
                    if let Some((_, p)) = app.overlay.as_mut() {
                        p.query = q;
                        p.ensure_visible_selected();
                    }
                }
                None => app.overlay = None,
            }
        }
        KeyCode::Char(c) if kind == OverlayKind::FilePicker => {
            app.input.insert_char(c);
            // A space (or any whitespace) ends the `@`-token; close the overlay.
            match app.input.active_at_token() {
                Some((_, q)) => {
                    if let Some((_, p)) = app.overlay.as_mut() {
                        p.query = q;
                        p.ensure_visible_selected();
                    }
                }
                None => app.overlay = None,
            }
        }
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
        KeyCode::Tab if kind == OverlayKind::SlashMenu => {
            // Autocomplete the highlighted command into the input (like Claude
            // Code's Tab), then close the menu. The completed `/<cmd> ` lands in
            // the normal composer so the user can add arguments or press Enter
            // to run it via the standard slash path.
            if let Some(key_sel) = picker.selected_key() {
                app.input.set_text(format!("/{key_sel} "));
                app.overlay = None;
            }
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
        // FilePicker commits via its own Enter/Tab arm in handle_overlay_key
        // (it rewrites the `@`-token in place), so it never reaches here.
        OverlayKind::FilePicker => {}
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
    let restored_todos = record.state.todos.clone();
    let restored_goal = record.state.active_goal.clone();

    // Rebuild visible blocks BEFORE we hand the history off to the agent so
    // we still own the record's contents.
    let rebuilt = rebuild_blocks_from_history(&record.history);

    {
        let mut guard = agent.lock().await;
        if guard.is_none() {
            // Lazily construct an Agent so we can stash the restored state
            // on it. The client will be created on the next turn via
            // launch_turn, which rebuilds it from the active credential.
            let client = match LlmClient::for_config(&app.config).await {
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
    app.session_todos = restored_todos;
    app.todo_completed_at.clear();
    app.active_goal = restored_goal.map(ActiveGoal::from_session_snapshot);
    app.pending_goal_replacement = None;
    app.pending_plan_exit = None;
    if !app.session_todos.is_empty() {
        app.show_todos = true;
    }
    if let Some(goal) = &app.active_goal {
        if goal.waiting_for_user {
            app.status_line = "(goal paused for user input)".into();
        } else {
            queue_goal_continuation(app);
        }
    }
    app.auto_scroll = true;
}

/// Kick off real compaction in the BACKGROUND (mirrors `launch_turn`'s spawn):
/// a task locks the agent, summarizes the history and REPLACES it with the
/// summary, persists, then reports back via `AgentEvent::CompactDone`. Running
/// off the main loop is what keeps the UI responsive and the progress bar
/// animating instead of freezing on the model call. Returns immediately.
/// Match a provider's "input too large for the context window" rejection so the
/// raw API error becomes an actionable /compact hint. Delegates to the core
/// heuristic the agent's auto-recovery also uses, keeping one source of truth.
fn is_context_overflow(message: &str) -> bool {
    opencli_core::agent::is_context_overflow_message(message)
}

fn start_compaction(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    tx: &mpsc::Sender<AgentEvent>,
) {
    app.compacting = true;
    app.compact_started_at = Some(std::time::Instant::now());
    app.compact_done_at = None;
    app.compact_result_msg = None;
    let goal_snapshot = app
        .active_goal
        .as_ref()
        .map(ActiveGoal::to_session_snapshot);
    if goal_snapshot.is_some() {
        app.pending_session_save = true;
    }
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
                        let mut record = a.to_session_record().await;
                        record.state.active_goal = goal_snapshot.clone();
                        if let Err(e) = opencli_core::session::save(&record) {
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
        if let InputItem::FunctionCallOutput {
            call_id,
            output,
            error,
        } = item
        {
            outputs.insert(call_id.clone(), (output.clone(), *error));
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
                 /worktree create [name]  create and enter an isolated git worktree\n  \
                 /worktree exit keep|remove [--discard]\n  \
                 /goal <objective>   keep working until the objective is complete\n  \
                 /status             show auth status\n  \
                 /cost               show token usage and estimated cost\n  \
                 /usage              show the provider's real quota / rate-limit status\n  \
                 /context            show context-window usage + composition\n  \
                 /buddy [off|reset]  meet your account's pixel companion\n  \
                 /config             show current configuration\n  \
                 /hooks              list configured PreToolUse hooks\n  \
                 /mcp                list configured MCP servers\n  \
                 /init               create CLAUDE.md for this project\n  \
                 /memory             show CLAUDE.md\n  \
                 /diff               show `git diff` for the working tree\n  \
                 /review             ask the agent to review uncommitted changes\n  \
                 /commit             stage & commit with a generated message\n  \
                 /commit-push-pr     commit, push a branch, and open a PR\n  \
                 /export [path]      save conversation as markdown\n  \
                 /compact            ask the agent to compact the conversation\n  \
                 /todos              show the session todo list\n  \
                 /about              show opencli version + build info\n  \
                 /perms [on|off]     toggle the approval modal for writes/shell\n  \
                 /undo               revert the most recent file edit\n  \
                 /quit               exit\n\n\
                 Composer prefixes:\n  \
                 @<path>             reference a file (typeahead); its contents are attached\n  \
                 !<command>          run a shell command now (!! to force past the guard)\n  \
                 #<note>             save a note to this project's CLAUDE.md\n\n\
                 Keyboard shortcuts:\n  \
                 Esc                 cancel the running turn (while busy)\n  \
                 Ctrl+O              toggle tool-call detail view\n  \
                 Ctrl+T              show / hide the live todo panel\n  \
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
                auth::activate_openai_api_key(&mut record, arg.to_string());
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
            let coverage = auth::credential_coverage();
            msg.push_str(&format!(
                "\n\nCredential coverage:\n  OpenAI OAuth:       {}\n  OpenAI API key:     {}\n  Anthropic OAuth:    {}\n  Anthropic API key:  {}",
                coverage.openai_oauth.label(),
                coverage.openai_api_key.label(),
                coverage.anthropic_oauth.label(),
                coverage.anthropic_api_key.label(),
            ));
            app.blocks.push(Block::System(msg));
            let catalogs = auth::signed_in_model_catalogs();
            if !catalogs.is_empty() {
                let mut text = String::from("Available models:");
                for catalog in catalogs {
                    text.push_str(&format!(
                        "\n  {} ({}):",
                        catalog.provider.display_name(),
                        catalog.provider
                    ));
                    for m in catalog.models {
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
                let model = config::normalize_model_name(arg);
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
        "worktree" => {
            handle_worktree_slash(app, arg);
        }
        "goal" => {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                match &app.active_goal {
                    Some(goal) => {
                        let mut msg = format!(
                            "Active goal:\n  {}\n  turns completed: {}",
                            goal.objective, goal.turns_completed
                        );
                        if goal.waiting_for_user {
                            msg.push_str("\n  waiting for user input");
                        }
                        if let Some(summary) = &goal.last_summary {
                            msg.push_str(&format!("\n  last update: {summary}"));
                        }
                        app.blocks.push(Block::System(msg));
                    }
                    None => app.blocks.push(Block::System(
                        "Usage: /goal <objective>\nExample: /goal fix the failing tests and verify the build"
                            .into(),
                    )),
                }
            } else if matches!(trimmed, "stop" | "cancel") {
                if let Some(goal) = app.active_goal.take() {
                    app.pending_goal_replacement = None;
                    remove_pending_goal_continuations(&mut app.message_queue);
                    app.pending_session_save = true;
                    app.blocks
                        .push(Block::System(format!("Stopped goal: {}", goal.objective)));
                } else {
                    app.blocks
                        .push(Block::System("No active goal to stop.".into()));
                }
            } else if goal_exceeds_limit(trimmed) {
                app.blocks.push(Block::System(format!(
                    "Goal too long ({} words / {} chars; max {GOAL_MAX_WORDS} words or {GOAL_MAX_CHARS} chars). The objective is re-sent to the model every turn, so a long one crowds out the real work and dulls its reasoning. Tighten it to one clear objective and put the extra detail in a normal message before running a short /goal.",
                    goal_word_count(trimmed),
                    trimmed.chars().count()
                )));
                app.auto_scroll = true;
            } else {
                let objective = trimmed.to_string();
                if let Some(goal) = &app.active_goal {
                    if goal.objective == objective {
                        app.blocks.push(Block::System(format!(
                            "Goal already active: {}",
                            goal.objective
                        )));
                    } else {
                        app.pending_goal_replacement = Some(PendingGoalReplacement {
                            objective: objective.clone(),
                        });
                        app.blocks.push(Block::System(format!(
                            "A goal is already active:\n  current: {}\n  new: {}\n\nReplace the current goal? Press Y to replace, or N/Esc to keep the current goal.",
                            goal.objective, objective
                        )));
                    }
                    app.auto_scroll = true;
                } else {
                    start_goal(app, objective, false);
                }
            }
        }
        "plan" => {
            set_permission_mode_and_save(app, PermissionMode::Plan);
            app.blocks.push(Block::System(
                "plan mode → on (read-only tools only; write/edit/shell will be blocked)".into(),
            ));
        }
        "normal" => {
            set_permission_mode_and_save(app, PermissionMode::Default);
            app.pending_plan_exit = None;
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
            if app.active_goal.take().is_some() {
                app.pending_session_save = true;
            }
            app.pending_goal_replacement = None;
            app.pending_plan_exit = None;
            remove_pending_goal_continuations(&mut app.message_queue);
        }
        "resume" => {
            app.open_overlay(OverlayKind::ResumePicker);
        }
        "cost" => {
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
        "usage" => {
            app.blocks.push(Block::System(render_usage_report(app)));
        }
        "context" | "ctx" => {
            app.blocks.push(Block::System(render_context_report(app)));
        }
        "buddy" | "pet" => {
            let arg = arg.trim();
            if arg.eq_ignore_ascii_case("off") {
                app.buddy_hidden = true;
                app.blocks.push(Block::System(
                    "buddy hidden — run /buddy to bring your companion back".to_string(),
                ));
            } else if arg.eq_ignore_ascii_case("reset") || arg.eq_ignore_ascii_case("clear") {
                // Dev/testing: forget the adopted companion so the next /buddy
                // hatches again. This only replays the hatch — WHICH pet you get
                // is still derived from the account (or OPENCLI_BUDDY_SEED), so
                // it can't be tricked into a different companion.
                app.buddy_pet = None;
                app.buddy_hidden = false;
                app.hatch = None;
                app.blocks.push(Block::System(
                    "buddy reset — run /buddy to hatch again".to_string(),
                ));
            } else if app.hatch.is_some() {
                // Already hatching; ignore repeat presses.
            } else if let Some(pet) = app.buddy_pet {
                // Locked: the companion is already adopted for this account.
                app.buddy_hidden = false;
                app.blocks.push(Block::System(format!(
                    "{} is already your companion — locked to this account.",
                    crate::tui::buddy::pet_name(pet)
                )));
            } else {
                // First hatch: the pet is a pure function of the signed-in
                // account, so it persists for that account and re-rolls only on
                // account switch — nothing is stored to delete or tamper with.
                // `OPENCLI_BUDDY_SEED` lets a dev preview other pets by seeding
                // the roll directly instead of from the account.
                let identity = std::env::var("OPENCLI_BUDDY_SEED")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        auth::account_identity(&auth::load_auth().unwrap_or_default())
                    });
                let pet = crate::tui::buddy::roll(&identity);
                app.hatch = Some(HatchAnim {
                    pet,
                    started: std::time::Instant::now(),
                });
                app.auto_scroll = true;
            }
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
        "commit" => {
            app.message_queue.push(commit_prompt(arg));
            app.blocks.push(Block::System(
                "Queued: agent will stage and commit the changes.".into(),
            ));
        }
        "commit-push-pr" | "commitpushpr" | "pr" => {
            app.message_queue.push(commit_push_pr_prompt(arg));
            app.blocks.push(Block::System(
                "Queued: agent will commit, push a branch, and open a PR.".into(),
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
                let done = app
                    .session_todos
                    .iter()
                    .filter(|t| matches!(t.status, TodoStatus::Completed))
                    .count();
                let in_progress = app
                    .session_todos
                    .iter()
                    .filter(|t| matches!(t.status, TodoStatus::InProgress))
                    .count();
                let pending = app
                    .session_todos
                    .iter()
                    .filter(|t| matches!(t.status, TodoStatus::Pending))
                    .count();
                let mut msg = format!(
                    "{} tasks ({} done, {} in progress, {} open)\n",
                    app.session_todos.len(),
                    done,
                    in_progress,
                    pending
                );
                for t in &app.session_todos {
                    let marker = match t.status {
                        TodoStatus::Completed => "✓",
                        TodoStatus::InProgress => "▪",
                        TodoStatus::Pending => "□",
                    };
                    let label = match t.status {
                        TodoStatus::InProgress => &t.active_form,
                        _ => &t.content,
                    };
                    msg.push_str(&format!("  {marker} {label}\n"));
                }
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
                    "No subagents installed. Create one at {}/agents/<name>.md, or install Claude/Codex agents under ~/.claude/agents or ~/.codex/agents.",
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
                    "No skills installed. Create one at {}/skills/<name>/SKILL.md, or install Claude Code/Codex skills under ~/.claude/skills, ~/.codex/skills, or their plugin directories.",
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

/// `/context` report: the real provider-reported occupancy as the headline,
/// plus a chars/4 estimate of where the *visible conversation* is spending
/// context (tool I/O is usually the bulk — and the prime microcompaction
/// target). The system prompt and tool schemas live in the agent layer and are
/// not counted here, so the headline total is the source of truth.
fn render_context_report(app: &App) -> String {
    fn est(s: &str) -> u64 {
        (s.chars().count() as u64).div_ceil(4)
    }
    let limit = app.config.effective_context_limit();
    let used = app.tokens_used;
    let pct = used.saturating_mul(100).checked_div(limit).unwrap_or(0);

    let (mut user, mut assistant, mut reasoning, mut tool_io, mut system) = (0u64, 0, 0, 0, 0);
    for b in &app.blocks {
        match b {
            Block::User(t) => user += est(t),
            Block::Assistant {
                text, reasoning: r, ..
            } => {
                assistant += est(text);
                reasoning += est(r);
            }
            Block::Tool { args, output, .. } => {
                tool_io += est(args) + output.as_deref().map(est).unwrap_or(0);
            }
            Block::System(t) => system += est(t),
            Block::Welcome => {}
        }
    }
    let visible = user + assistant + reasoning + tool_io + system;

    let mut msg = String::new();
    msg.push_str(&format!("Context window — model: {}\n", app.config.model));
    msg.push_str(&format!(
        "  Used: {used} / {limit} tokens ({pct}%)  {}\n",
        ratio_bar(pct)
    ));
    msg.push_str("  Microcompaction sheds old tool output above 75%; auto-compact at 85%.\n");
    if used == 0 {
        msg.push_str(
            "  (No turn has run yet — the real occupancy appears after the first response.)\n",
        );
    }
    msg.push('\n');
    msg.push_str("Estimated composition of the visible conversation (chars/4):\n");
    let rows = [
        ("Tool calls + output", tool_io),
        ("Assistant text", assistant),
        ("Reasoning", reasoning),
        ("User messages", user),
        ("System notes", system),
    ];
    for (label, toks) in rows {
        let share = toks.saturating_mul(100).checked_div(visible).unwrap_or(0);
        msg.push_str(&format!("  {label:<22} {toks:>8} tok  ({share:>2}%)\n"));
    }
    msg.push_str(&format!("  {:-<22} {visible:>8} tok\n", ""));
    msg.push_str(
        "  Note: estimates exclude the system prompt + tool schemas; the headline total above is the real provider-reported occupancy.",
    );
    msg
}

/// Shared git safety protocol injected into the `/commit` family so the agent
/// can't freelance into a destructive operation. Mirrors Claude Code's commit
/// guardrails.
const GIT_SAFETY_PROTOCOL: &str = "Git safety protocol — follow exactly:\n\
- NEVER `git commit --amend`, `git rebase`, or `git reset --hard` unless the user explicitly asked.\n\
- NEVER pass `--no-verify` (do not skip hooks).\n\
- NEVER force-push, and never push to the `main`/`master` branch directly.\n\
- Do not commit unrelated changes; if the working tree mixes concerns, stage only the relevant files.\n\
- Do not add `git add -A` blindly — review `git status` first and stage deliberately.\n\
- Write the commit message yourself from the actual diff; do not invent changes you didn't make.";

fn commit_prompt(extra: &str) -> String {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("\n\nAdditional instructions from the user: {extra}")
    };
    format!(
        "Commit the current changes.\n\
Steps:\n\
1. Run `git status` and `git diff` (use the run_shell tool) to see exactly what changed.\n\
2. Stage the appropriate files.\n\
3. Commit with a concise Conventional-Commits message (e.g. `fix:`, `feat:`, `refactor:`) whose subject summarizes the change and whose body explains the why when non-obvious.\n\
4. Show the resulting `git log -1 --stat` so the user can confirm.\n\n\
{GIT_SAFETY_PROTOCOL}{extra}"
    )
}

fn commit_push_pr_prompt(extra: &str) -> String {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("\n\nAdditional instructions from the user: {extra}")
    };
    format!(
        "Commit the current changes, push a branch, and open a pull request.\n\
Steps:\n\
1. Run `git status` and `git diff` to see what changed; determine the current branch.\n\
2. If on `main`/`master`, create a new descriptively-named branch first.\n\
3. Stage the appropriate files and commit with a concise Conventional-Commits message.\n\
4. Push the branch with `git push -u origin <branch>`.\n\
5. Open a PR with `gh pr create` (fill in a clear title and body summarizing the change). If `gh` is not installed, stop after pushing and print the branch name plus the URL the user can use to open the PR manually.\n\n\
{GIT_SAFETY_PROTOCOL}{extra}"
    )
}

/// Fixed-width `[████░░░░]` bar for a 0–100 percentage.
fn ratio_bar(pct: u64) -> String {
    const WIDTH: u64 = 20;
    let filled = (pct.min(100) * WIDTH / 100) as usize;
    let empty = WIDTH as usize - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

fn goal_start_prompt(objective: &str) -> String {
    format!(
        "{GOAL_START_PREFIX}\n\n\
Active goal:\n{objective}\n\n\
You are now in /goal mode. Work autonomously until this objective is genuinely complete.\n\n\
Rules:\n\
- Read the relevant codebase/context before changing code.\n\
- Use tools to inspect, edit, and verify. Do not stop at planning or partial work.\n\
- If the goal has 3+ concrete steps, maintain a todo list with todo_write and update it after each meaningful step.\n\
- Before marking complete, run the most relevant tests/build/checks available, or state exactly why a check cannot run.\n\
- Call goal_update with status \"complete\" only when the full objective is done and verified.\n\
- Call goal_update with status \"blocked\" only when no meaningful progress is possible without user input or an external state change.\n\
- If work remains at the end of a turn, call goal_update with status \"in_progress\" and summarize the next concrete action.\n\
- Keep going; the host will continue automatically until complete or blocked."
    )
}

fn goal_continuation_prompt(goal: &ActiveGoal) -> String {
    let last = goal
        .last_summary
        .as_deref()
        .unwrap_or("no explicit progress update yet");
    format!(
        "{GOAL_CONTINUATION_PREFIX}\n\n\
Continue the active /goal until it is genuinely complete.\n\n\
Goal:\n{}\n\n\
Last goal_update summary:\n{last}\n\n\
Keep working from the current repository state and conversation context. Do not ask whether to continue. \
Use tools, make the remaining changes, and verify them. Call goal_update with status \"complete\" only after the goal is fully done and verified; call \"blocked\" only if you cannot make meaningful progress without user input or an external state change.",
        goal.objective
    )
}

fn remove_pending_goal_continuations(queue: &mut Vec<String>) {
    queue.retain(|item| {
        !item.starts_with(GOAL_START_PREFIX) && !item.starts_with(GOAL_CONTINUATION_PREFIX)
    });
}

fn start_goal(app: &mut App, objective: String, replaced: bool) {
    app.pending_goal_replacement = None;
    remove_pending_goal_continuations(&mut app.message_queue);
    app.active_goal = Some(ActiveGoal::new(objective.clone()));
    app.pending_session_save = true;
    app.message_queue.push(goal_start_prompt(&objective));
    let prefix = if replaced {
        "Replaced active goal"
    } else {
        "Started goal"
    };
    app.blocks
        .push(Block::System(format!("{prefix}: {objective}")));
    app.auto_scroll = true;
}

fn push_visible_user_block(app: &mut App, text: &str) {
    if text.starts_with(GOAL_START_PREFIX) {
        if let Some(goal) = &app.active_goal {
            app.blocks
                .push(Block::System(format!("Goal running: {}", goal.objective)));
        } else {
            app.blocks.push(Block::System("Goal running.".into()));
        }
    } else if text.starts_with(GOAL_CONTINUATION_PREFIX) {
        if let Some(goal) = &app.active_goal {
            app.blocks.push(Block::System(format!(
                "Continuing goal: {} (turn {})",
                goal.objective,
                goal.turns_completed.saturating_add(1)
            )));
        } else {
            app.blocks.push(Block::System("Continuing goal.".into()));
        }
    } else if text.starts_with(PLAN_APPROVED_PREFIX) {
        app.blocks
            .push(Block::System("Implementing approved plan.".into()));
    } else if text.starts_with(PLAN_REJECTED_PREFIX) {
        app.blocks
            .push(Block::System("Revising rejected plan.".into()));
    } else {
        app.blocks.push(Block::User(text.to_string()));
    }
}

fn queue_goal_continuation(app: &mut App) {
    let Some(goal) = app.active_goal.as_ref() else {
        return;
    };
    if goal.waiting_for_user {
        app.status_line = "(goal paused for user input)".into();
        return;
    }
    if !app.message_queue.is_empty() {
        return;
    }
    let prompt = goal_continuation_prompt(goal);
    app.message_queue.push(prompt);
    app.status_line = "(continuing active goal...)".into();
}

fn schedule_goal_continuation(app: &mut App) {
    let Some(goal) = app.active_goal.as_mut() else {
        return;
    };
    goal.turns_completed = goal.turns_completed.saturating_add(1);
    app.pending_session_save = true;
    queue_goal_continuation(app);
}

fn update_todo_completion_timestamps(app: &mut App, next_todos: &[TodoItem]) {
    let next_all_completed = all_todos_completed(next_todos);
    let previous_completed: HashSet<String> = app
        .session_todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::Completed))
        .map(todo_completion_key)
        .collect();
    let current_completed: HashSet<String> = next_todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::Completed))
        .map(todo_completion_key)
        .collect();
    app.todo_completed_at
        .retain(|key, _| current_completed.contains(key));
    let now = std::time::Instant::now();
    for key in current_completed {
        if !previous_completed.contains(&key)
            || (next_all_completed && !app.todo_completed_at.contains_key(&key))
        {
            app.todo_completed_at.insert(key, now);
        }
    }
}

fn all_todos_completed(todos: &[TodoItem]) -> bool {
    !todos.is_empty()
        && todos
            .iter()
            .all(|todo| matches!(todo.status, TodoStatus::Completed))
}

fn has_recent_completed_todo(app: &App) -> bool {
    let now = std::time::Instant::now();
    app.session_todos.iter().any(|todo| {
        app.todo_completed_at
            .get(&todo_completion_key(todo))
            .is_some_and(|completed_at| {
                now.duration_since(*completed_at) < TODO_RECENT_COMPLETED_TTL
            })
    })
}

fn should_keep_recent_completed_todos_for_empty_snapshot(app: &App) -> bool {
    all_todos_completed(&app.session_todos) && has_recent_completed_todo(app)
}

fn prune_expired_completed_todos(app: &mut App) {
    if !all_todos_completed(&app.session_todos) || has_recent_completed_todo(app) {
        return;
    }
    app.session_todos.clear();
    app.todo_completed_at.clear();
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

/// `/usage` report: the active provider's real quota/rate-limit status (5h +
/// weekly for subscriptions, token/request budgets for API keys), captured from
/// the last turn's response. Falls back to a hint when no turn has run yet.
fn render_usage_report(app: &App) -> String {
    let current = Provider::from_model(&app.config.model);
    match &app.last_quota {
        Some(snapshot) => {
            // Label the report with the snapshot's OWN provider, not the active
            // model's: after a mid-session `/model` switch the cached quota can
            // belong to a different provider, and labeling it "current" would
            // print one provider's name over another's windows.
            let snap_provider = snapshot.provider.unwrap_or(current);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let mut msg = format!(
                "Usage — provider: {}, model: {}\n",
                snap_provider.display_name(),
                app.config.model
            );
            if snap_provider != current {
                msg.push_str(&format!(
                    "  (last captured {} quota; the active model now uses {} — send a message to refresh)\n",
                    snap_provider.display_name(),
                    current.display_name()
                ));
            }
            msg.push_str(&snapshot.render(now));
            msg
        }
        None => format!(
            "Usage — provider: {}, model: {}\n  No live quota data yet — send a message, then run /usage.\n  \
             (quota is read from the provider's response to your turns)",
            current.display_name(),
            app.config.model
        ),
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

/// Hard ceiling on a composer `!`-command. The command runs on a background
/// task, so the UI stays responsive regardless; this bound exists so a
/// non-terminating command (a dev server, `tail -f`, a hung pipe) is killed
/// instead of leaking a process forever.
const BANG_TIMEOUT: Duration = Duration::from_secs(120);

/// Result of a background `!`-command, sent back to the main loop so the output
/// is appended on the UI thread (never touching `App` from the spawned task).
pub struct BangResult {
    /// What to show inline in the transcript.
    display: String,
    /// What to stage into `pending_shell_context` for the next model turn.
    context: String,
}

/// Run a `!`-prefixed shell command straight from the composer (no model turn),
/// mirroring `!bash` mode in Claude Code. A leading `!` (the user typed `!!cmd`)
/// forces past the destructive-command guard.
///
/// The command runs on a **background task** and reports back over `bang_tx`, so
/// a slow or non-terminating command never blocks the event loop (the previous
/// version `.await`ed `output()` inline, freezing the whole TUI until the
/// command exited — an unrecoverable hang for e.g. a dev server). The task is
/// also bounded by [`BANG_TIMEOUT`] with `kill_on_drop`, so the child is killed
/// on timeout rather than leaking.
fn handle_bang_shell(app: &mut App, bang_tx: &mpsc::Sender<BangResult>, raw: &str) {
    let (force, cmd) = match raw.strip_prefix('!') {
        Some(rest) => (true, rest.trim()),
        None => (false, raw),
    };
    if cmd.is_empty() {
        app.blocks.push(Block::System(
            "usage: !<shell command>  (!! to force)".into(),
        ));
        return;
    }
    if !force {
        if let Some(reason) = opencli_core::tools::shell::classify_danger(cmd) {
            app.blocks.push(Block::System(format!(
                "⚠ refused: {reason}. Re-run as `!!{cmd}` to force."
            )));
            return;
        }
    }
    app.blocks.push(Block::System(format!("! {cmd}")));
    app.auto_scroll = true;

    let cwd = app.cwd.clone();
    let cmd = cmd.to_string();
    let tx = bang_tx.clone();
    tokio::spawn(async move {
        let result = run_bang_command(&cmd, &cwd, BANG_TIMEOUT).await;
        // Receiver gone (app exiting) → nothing to do.
        let _ = tx.send(result).await;
    });
}

/// Execute one composer `!`-command with a hard `timeout`, killing the child on
/// timeout (`kill_on_drop`). Pure w.r.t. `App` so it is safe to run off-thread
/// and straightforward to unit-test. Split out from [`handle_bang_shell`] so the
/// timeout can be exercised in tests without waiting the full ceiling.
async fn run_bang_command(cmd: &str, cwd: &std::path::Path, timeout: Duration) -> BangResult {
    let ctx = |body: &str| format!("[The user ran a shell command in the terminal]\n$ {cmd}\n{body}");

    #[cfg(windows)]
    let mut command = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    command.current_dir(cwd).kill_on_drop(true);

    // On timeout the `output()` future is dropped; `kill_on_drop` then kills the
    // child so a hung command can't survive past the ceiling.
    let output = match tokio::time::timeout(timeout, command.output()).await {
        Ok(r) => r,
        Err(_) => {
            let msg = format!("⏱ timed out after {}s (process killed)", timeout.as_secs());
            return BangResult {
                display: msg.clone(),
                context: ctx(&msg),
            };
        }
    };

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let mut body = String::new();
            if !stdout.trim().is_empty() {
                body.push_str(stdout.trim_end());
            }
            if !stderr.trim().is_empty() {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(stderr.trim_end());
            }
            let code = o.status.code().unwrap_or(-1);
            let body = truncate_output(&body, 16 * 1024);
            let shown = if body.trim().is_empty() {
                if o.status.success() {
                    "(no output)".to_string()
                } else {
                    format!("(exit {code}, no output)")
                }
            } else {
                body
            };
            BangResult {
                context: ctx(&format!("Exit code: {code}\nOutput:\n{shown}")),
                display: shown,
            }
        }
        Err(e) => BangResult {
            display: format!("! failed to run: {e}"),
            context: ctx(&format!("failed to run: {e}")),
        },
    }
}

/// Build the text to append to a project `CLAUDE.md` for a `#`-note. Writes a
/// header for a brand-new file, and guarantees the note starts on its own line
/// even when the existing file doesn't end in a newline (otherwise the bullet
/// would be glued onto the last line, e.g. `...last line- note`).
fn claude_md_note_block(existed: bool, ends_with_newline: bool, note: &str) -> String {
    if !existed {
        format!("# CLAUDE.md\n\n- {note}\n")
    } else if ends_with_newline {
        format!("- {note}\n")
    } else {
        format!("\n- {note}\n")
    }
}

/// Append a `#`-prefixed note to the project `CLAUDE.md` and re-apply memory to
/// the live agent so it takes effect immediately (Claude Code's `#` quick-add).
async fn handle_hash_memory(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    note: &str,
) {
    if note.is_empty() {
        app.blocks
            .push(Block::System("usage: #<note to remember>".into()));
        return;
    }
    let path = app.cwd.join("CLAUDE.md");
    let existed = path.exists();
    let ends_with_newline = std::fs::read_to_string(&path)
        .map(|c| c.is_empty() || c.ends_with('\n'))
        .unwrap_or(true);
    let block = claude_md_note_block(existed, ends_with_newline, note);
    use std::io::Write;
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(block.as_bytes()));
    match res {
        Ok(()) => {
            app.blocks
                .push(Block::System(format!("📝 remembered → {}", path.display())));
            // Re-apply project memory so the note lands in this session's system
            // prompt. With no agent yet it's applied on the first turn instead.
            let mut guard = agent.lock().await;
            if let Some(a) = guard.as_mut() {
                a.apply_project_memory();
            }
        }
        Err(e) => app
            .blocks
            .push(Block::System(format!("memory write failed: {e}"))),
    }
    app.auto_scroll = true;
}

/// Build the `@`-file picker list: project files relative to `cwd`, gitignore-
/// aware via `rg --files`, falling back to a bounded manual walk when ripgrep is
/// absent. Capped so a huge tree can't stall the UI.
fn file_candidates(cwd: &std::path::Path) -> Vec<picker::PickerItem> {
    const MAX: usize = 5000;
    let mut paths: Vec<String> = Vec::new();
    // Stream `rg --files` and stop at MAX, killing rg early, so a giant monorepo
    // can't make the synchronous picker-open stall enumerating millions of files
    // (the old code buffered all of rg's stdout before capping).
    use std::io::BufRead;
    let rg = std::process::Command::new("rg")
        .arg("--files")
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    match rg {
        Ok(mut child) => {
            if let Some(out) = child.stdout.take() {
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    paths.push(line.replace('\\', "/"));
                    if paths.len() >= MAX {
                        break;
                    }
                }
            }
            // We have enough (or hit EOF) — stop rg and reap it.
            let _ = child.kill();
            let _ = child.wait();
            // rg ran but produced nothing (e.g. not a usable dir) → manual walk.
            if paths.is_empty() {
                walk_files(cwd, MAX, &mut paths);
            }
        }
        // rg not installed / failed to spawn.
        Err(_) => walk_files(cwd, MAX, &mut paths),
    }
    paths
        .into_iter()
        .map(|p| picker::PickerItem {
            key: p.clone(),
            title: p,
            description: String::new(),
        })
        .collect()
}

/// Bounded, gitignore-blind directory walk used when `rg` is unavailable. Skips
/// hidden entries and a few notoriously heavy directories.
fn walk_files(root: &std::path::Path, max: usize, out: &mut Vec<String>) {
    const SKIP: &[&str] = &[".git", "node_modules", "target", ".venv", "dist", "build"];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= max {
                return;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP.contains(&name.as_ref()) {
                    stack.push(entry.path());
                }
            } else if ft.is_file() {
                if let Ok(rel) = entry.path().strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
}

/// Scan `text` for `@<path>` references that resolve to real files/dirs under
/// `cwd` and return a context block with their contents (files) or listings
/// (dirs), to append to the prompt — mirroring Claude Code's `@file` expansion.
/// Each `@` must start the string or follow whitespace, so `a@b.com` is left
/// alone. Returns `None` when nothing resolves.
fn expand_at_mentions(text: &str, cwd: &std::path::Path) -> Option<String> {
    const MAX_FILE_BYTES: usize = 64 * 1024;
    const MAX_TOTAL_BYTES: usize = 256 * 1024;
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut sections = String::new();

    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'@' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
                end += 1;
            }
            if end > start {
                let token = text[start..end].trim_end_matches(['.', ',', ':', ';', ')']);
                if !token.is_empty() && seen.insert(token.to_string()) {
                    if let Some(section) = render_mention(token, cwd, MAX_FILE_BYTES) {
                        if sections.len() + section.len() <= MAX_TOTAL_BYTES {
                            sections.push_str(&section);
                        }
                    }
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    (!sections.is_empty())
        .then(|| format!("[Contents of files referenced with @ in the message above]\n{sections}"))
}

/// Render one resolved `@`-mention: a fenced file body or a directory listing.
///
/// The resolved path is confined to `cwd`: it is canonicalized (resolving `..`
/// and symlinks) and must stay under the canonical `cwd`, so `@/etc/passwd`,
/// `@../../secret`, or a symlink pointing outside the workspace resolve to
/// `None` rather than silently shipping out-of-tree file contents to the model.
fn render_mention(token: &str, cwd: &std::path::Path, max_bytes: usize) -> Option<String> {
    let p = std::path::Path::new(token);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    // canonicalize() also confirms the path exists; a missing path → None.
    let path = joined.canonicalize().ok()?;
    let base = cwd.canonicalize().ok()?;
    if !path.starts_with(&base) {
        return None;
    }
    let meta = std::fs::metadata(&path).ok()?;
    if meta.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&path)
            .ok()?
            .flatten()
            .map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() {
                    format!("{n}/")
                } else {
                    n
                }
            })
            .collect();
        names.sort();
        names.truncate(200);
        Some(format!("\n## @{token} (directory)\n{}\n", names.join("\n")))
    } else if meta.is_file() {
        // Skip binary: only attach valid UTF-8.
        let text = String::from_utf8(std::fs::read(&path).ok()?).ok()?;
        let body = truncate_output(&text, max_bytes);
        Some(format!("\n## @{token}\n```\n{body}\n```\n"))
    } else {
        None
    }
}

/// Truncate `s` to at most `max` bytes on a char boundary, appending a note when
/// it was cut. Shared by `!` output capture and `@file` expansion.
fn truncate_output(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (truncated, {} bytes total)", &s[..end], s.len())
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
    // Fold in any composer-prefix context the user staged before this prompt:
    // `!` shell output (prepended) and `@file` references (file contents appended).
    // The hook above and the visible Block::User both see only the raw prompt;
    // this enrichment is invisible plumbing sent to the model.
    //
    // `@`-expansion runs over the user's prompt ONLY — not the prepended shell
    // output — so a `@token` that merely appears in a command's stdout doesn't
    // get its file attached unexpectedly.
    let text = {
        let attached = expand_at_mentions(&text, &app.cwd);
        let mut t = text;
        if !app.pending_shell_context.is_empty() {
            let ctx = std::mem::take(&mut app.pending_shell_context).join("\n\n");
            t = format!("{ctx}\n\n{t}");
        }
        if let Some(attached) = attached {
            t = format!("{t}\n\n{attached}");
        }
        t
    };
    if let Some(goal) = app.active_goal.as_mut() {
        goal.waiting_for_user = false;
        app.pending_session_save = true;
    }
    let should_defer_session_save_to_host = app.active_goal.is_some();
    let client = match LlmClient::for_config(&app.config).await {
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
    if should_defer_session_save_to_host {
        // Persist the just-queued user/goal prompt before the long-running
        // model turn starts. The post-turn save is still deferred to the host
        // so `goal_update complete|blocked` cannot be overwritten by the
        // launch-time goal snapshot.
        save_current_session_record(app, agent).await;
    }
    app.busy = true;
    app.turn_started_at = Some(std::time::Instant::now());
    app.spinner_word = pick_spinner_word();
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
            //
            // When `/goal` is active, host-owned state may change DURING the
            // turn (for example `goal_update complete`). The TUI saves the
            // latest host state immediately after the turn finishes, so avoid
            // writing a stale launch-time goal snapshot here.
            if !should_defer_session_save_to_host {
                let record = a.to_session_record().await;
                if let Err(e) = opencli_core::session::save(&record) {
                    tracing::debug!(error = %e, "session save failed");
                }
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
    // The aborted turn won't emit TurnComplete, so collapse the fleet view here.
    app.subagents.clear();
    if let Some(goal) = app.active_goal.take() {
        app.pending_goal_replacement = None;
        remove_pending_goal_continuations(&mut app.message_queue);
        app.blocks
            .push(Block::System(format!("Stopped goal: {}", goal.objective)));
        app.pending_session_save = true;
    }
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

/// True when the (col, row) terminal cell falls inside `r`. Mouse hit-test for
/// the clickable jump-to-bottom bar and fleet-view rows.
fn point_in(r: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
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
            let mut did_ask_user = false;
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
                    did_ask_user = true;
                    ask_prompt = o
                        .as_deref()
                        .and_then(opencli_core::tools::ask::render_ask_envelope);
                }
            }
            if let Some(prompt) = ask_prompt {
                app.blocks.push(Block::System(prompt));
            }
            if did_ask_user {
                if let Some(goal) = app.active_goal.as_mut() {
                    goal.waiting_for_user = true;
                    goal.last_summary = Some("waiting for user input".into());
                    app.pending_session_save = true;
                }
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
        AgentEvent::CwdChanged { cwd } => {
            app.cwd = std::path::PathBuf::from(&cwd);
            app.blocks.push(Block::System(format!("cwd → {cwd}")));
            app.chat_render_cache = None;
            app.auto_scroll = true;
        }
        AgentEvent::TurnComplete => {
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            finish_open_assistant_block(&mut app.blocks);
            app.busy = false;
            app.turn_started_at = None;
            app.status_line.clear();
            app.current_turn = None;
            // The fleet view is per-turn: clear finished sub-agents now that the
            // turn is over so the panel collapses away.
            app.subagents.clear();
            schedule_goal_continuation(app);

            // Drain the message queue — send EVERYTHING as one combined prompt.
            if !app.message_queue.is_empty() {
                // Defer to handler in main_loop via a tick: rebuild a fake key event isn't ideal,
                // so we expose a helper to launch directly. Stash the merged text on app.
                app.status_line = "(flushing queued messages…)".into();
            }
        }
        AgentEvent::GoalStatusUpdated { status, summary } => {
            let status = status.trim().to_ascii_lowercase();
            match status.as_str() {
                "complete" => {
                    if let Some(goal) = app.active_goal.take() {
                        app.pending_goal_replacement = None;
                        remove_pending_goal_continuations(&mut app.message_queue);
                        app.pending_session_save = true;
                        app.blocks.push(Block::System(format!(
                            "Goal complete: {}\n{summary}",
                            goal.objective
                        )));
                    }
                }
                "blocked" => {
                    if let Some(goal) = app.active_goal.take() {
                        app.pending_goal_replacement = None;
                        remove_pending_goal_continuations(&mut app.message_queue);
                        app.pending_session_save = true;
                        app.blocks.push(Block::System(format!(
                            "Goal blocked: {}\n{summary}",
                            goal.objective
                        )));
                    }
                }
                "in_progress" => {
                    if let Some(goal) = app.active_goal.as_mut() {
                        goal.last_summary = Some(summary);
                        app.pending_session_save = true;
                    }
                }
                _ => {
                    if let Some(goal) = app.active_goal.as_mut() {
                        goal.last_summary = Some(format!("unknown goal status `{status}`"));
                        app.pending_session_save = true;
                    }
                }
            }
        }
        AgentEvent::PlanExitRequested { plan } => {
            if let Some(goal) = app.active_goal.as_mut() {
                goal.waiting_for_user = true;
                goal.last_summary = Some("waiting for plan approval".into());
                app.pending_session_save = true;
            }
            app.pending_plan_exit = Some(PendingPlanExit { plan: plan.clone() });
            app.blocks.push(Block::System(format!(
                "Plan ready for approval:\n{plan}\n\nPress Y to approve and leave plan mode, or N/Esc to keep planning."
            )));
            app.auto_scroll = true;
        }
        AgentEvent::PlanModeRequested => {
            app.set_permission_mode(PermissionMode::Plan);
            app.pending_plan_exit = None;
            if let Some(goal) = app.active_goal.as_mut() {
                goal.last_summary = Some("entered plan mode".into());
                app.pending_session_save = true;
            }
            app.blocks.push(Block::System(
                "plan mode → on (read-only tools only; write/edit/shell will be blocked)".into(),
            ));
            app.auto_scroll = true;
        }
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
            total_tokens: _,
        } => {
            // Current context occupancy, NOT a running total. Each agentic step
            // resends the whole history, so summing across steps balloons into
            // the millions and reads as runaway spend. Claude Code shows the
            // live context size instead; `input_tokens` already folds in cache
            // read/creation, so it is the true window usage of the last request.
            app.tokens_used = input_tokens;
            app.input_tokens_total = app.input_tokens_total.saturating_add(input_tokens);
            app.output_tokens_total = app.output_tokens_total.saturating_add(output_tokens);
        }
        AgentEvent::Quota { snapshot } => {
            // Latest real provider quota; rendered on demand by `/usage`.
            app.last_quota = Some(snapshot);
        }
        AgentEvent::TodosSnapshot { todos } => {
            if todos.is_empty() && should_keep_recent_completed_todos_for_empty_snapshot(app) {
                return;
            }
            let was_empty = app.session_todos.is_empty();
            update_todo_completion_timestamps(app, &todos);
            app.session_todos = todos;
            if was_empty && !app.session_todos.is_empty() {
                app.show_todos = true;
            }
        }
        AgentEvent::Error { message } => {
            collapse_reasoning_into_thought(app);
            finish_open_assistant_block(&mut app.blocks);
            if let Some(goal) = app.active_goal.as_mut() {
                goal.waiting_for_user = true;
                goal.last_summary = Some(format!("paused after turn error: {message}"));
                app.pending_session_save = true;
            }
            if is_context_overflow(&message) {
                // The provider rejected the request because the conversation
                // outgrew its real context window before compaction could free
                // space — usually because a custom provider's true window is
                // smaller than the catalog's guess-by-model-name. Surface an
                // actionable recovery path instead of a raw API error.
                let mut msg = String::from(
                    "⚠ context overflow — this conversation is larger than the model's context window.\n   \
                     Run /compact to summarize older history, then resend your message.",
                );
                if app.config.model.split_once('/').is_some() {
                    msg.push_str(&format!(
                        "\n   opencli assumed a {}-token window for this provider; set its real `context_limit` in config.json so it compacts in time.",
                        app.config.effective_context_limit()
                    ));
                }
                app.blocks.push(Block::System(msg));
            } else {
                app.blocks.push(Block::System(format!("error: {message}")));
            }
            app.busy = false;
            app.is_thinking = false;
            app.turn_started_at = None;
            app.status_line.clear();
            app.current_turn = None;
            app.subagents.clear();
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
                selected: 0,
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
        AgentEvent::SubagentStarted {
            id,
            subagent_type,
            prompt,
        } => {
            app.subagents.push(SubagentView {
                id,
                kind: subagent_type,
                prompt,
                activity: "starting".into(),
                steps: 0,
                started_at: std::time::Instant::now(),
                done: None,
                expanded: false,
            });
        }
        AgentEvent::SubagentActivity { id, summary } => {
            if let Some(s) = app.subagents.iter_mut().find(|s| s.id == id) {
                s.activity = summary;
                s.steps = s.steps.saturating_add(1);
            }
        }
        AgentEvent::SubagentDone { id, ok } => {
            if let Some(s) = app.subagents.iter_mut().find(|s| s.id == id) {
                s.done = Some(ok);
                s.activity = if ok { "done".into() } else { "failed".into() };
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

    #[test]
    fn expand_at_mentions_includes_existing_file_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "hello world").unwrap();

        // A real file is attached; an email-shaped token and a missing path are
        // left untouched (no section, returns the same None when nothing resolves).
        let out = expand_at_mentions("see @notes.txt and mail a@b.com", tmp.path()).unwrap();
        assert!(out.contains("## @notes.txt"));
        assert!(out.contains("hello world"));
        assert!(!out.contains("a@b.com"));

        assert!(expand_at_mentions("ping a@b.com only", tmp.path()).is_none());
        assert!(expand_at_mentions("read @does/not/exist", tmp.path()).is_none());
    }

    // Regression: @-mentions are confined to cwd — an absolute path or a `..`
    // escape that resolves outside the workspace must not be attached.
    #[test]
    fn expand_at_mentions_refuses_paths_outside_cwd() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        // A secret living next to (not under) the project dir.
        let secret = root.path().join("secret.txt");
        std::fs::write(&secret, "TOP SECRET").unwrap();

        // Absolute path outside cwd.
        let abs = format!("look @{}", secret.display());
        assert!(expand_at_mentions(&abs, &project).is_none());
        // Parent-dir escape.
        assert!(expand_at_mentions("look @../secret.txt", &project).is_none());

        // A file genuinely under cwd still resolves.
        std::fs::write(project.join("ok.txt"), "fine").unwrap();
        let out = expand_at_mentions("see @ok.txt", &project).unwrap();
        assert!(out.contains("fine"));
    }

    #[tokio::test]
    async fn run_bang_command_captures_output_and_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let r = run_bang_command("echo hi", tmp.path(), Duration::from_secs(10)).await;
        assert!(r.display.contains("hi"));
        assert!(r.context.contains("$ echo hi"));
        assert!(r.context.contains("Exit code: 0"));
    }

    // Regression: agent-locking deferred ops (resume/undo) must be blocked while
    // a turn is streaming, or apply_resume would deadlock against the turn task.
    #[test]
    fn deferred_agent_ops_blocked_while_busy_or_compacting() {
        let mut app = App::new();
        app.busy = false;
        app.compacting = false;
        assert!(app.can_run_deferred_agent_op());
        app.busy = true;
        assert!(!app.can_run_deferred_agent_op());
        app.busy = false;
        app.compacting = true;
        assert!(!app.can_run_deferred_agent_op());
    }

    #[test]
    fn claude_md_note_block_keeps_note_on_its_own_line() {
        // New file gets a header.
        assert_eq!(
            claude_md_note_block(false, true, "x"),
            "# CLAUDE.md\n\n- x\n"
        );
        // Existing file ending in newline: plain append.
        assert_eq!(claude_md_note_block(true, true, "x"), "- x\n");
        // Existing file WITHOUT a trailing newline: prepend one so the bullet
        // doesn't glue onto the last line.
        assert_eq!(claude_md_note_block(true, false, "x"), "\n- x\n");
    }

    // Regression: a non-terminating `!`-command must be killed at the deadline,
    // never freeze the UI (the old inline `.await` hung forever on such commands).
    #[cfg(not(windows))]
    #[tokio::test]
    async fn run_bang_command_times_out_instead_of_hanging() {
        let tmp = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let r = run_bang_command("sleep 30", tmp.path(), Duration::from_millis(200)).await;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "command must return at the deadline, not run to completion"
        );
        assert!(r.display.contains("timed out"), "got: {}", r.display);
    }

    #[test]
    fn goal_word_count_and_limit_boundary() {
        assert_eq!(goal_word_count("  fix   the   build  "), 3);
        assert_eq!(goal_word_count(""), 0);
        // A concise, detailed objective stays well under the cap.
        let ok = "Refactor the auth module to the new token format, update all call \
                  sites, keep every existing test passing, and verify the login flow.";
        assert!(
            goal_word_count(ok) <= GOAL_MAX_WORDS,
            "detailed goal must fit"
        );
        assert!(!goal_exceeds_limit(ok), "detailed goal must fit");
        // Exactly at the cap is accepted; one over is rejected.
        let at_cap = "w ".repeat(GOAL_MAX_WORDS);
        let over_cap = "w ".repeat(GOAL_MAX_WORDS + 1);
        assert!(goal_word_count(&at_cap) <= GOAL_MAX_WORDS);
        assert!(!goal_exceeds_limit(&at_cap));
        assert!(goal_word_count(&over_cap) > GOAL_MAX_WORDS);
        assert!(goal_exceeds_limit(&over_cap));
        // A space-free CJK objective counts as one "word" but must NOT bypass the
        // limit — the char ceiling catches it.
        let cjk_huge = "目".repeat(GOAL_MAX_CHARS + 1);
        assert_eq!(goal_word_count(&cjk_huge), 1, "no whitespace → one word");
        assert!(
            goal_exceeds_limit(&cjk_huge),
            "char ceiling must catch CJK abuse"
        );
        // A short CJK objective is still fine.
        assert!(!goal_exceeds_limit(&"目标".repeat(5)));
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("opencli-tui-app-{name}-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn todo(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_string(),
            status,
            active_form: format!("Doing {content}"),
            id: None,
            blocked_by: Vec::new(),
        }
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

    #[test]
    fn goal_elapsed_formats_seconds_then_minutes() {
        assert_eq!(format_goal_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(format_goal_elapsed(Duration::from_secs(60)), "60s");
        assert_eq!(format_goal_elapsed(Duration::from_secs(62)), "1m2");
        assert_eq!(format_goal_elapsed(Duration::from_secs(92)), "1m32");
    }

    #[test]
    fn active_goal_session_snapshot_roundtrips_state() {
        let mut goal = ActiveGoal::new("finish verification".to_string());
        goal.turns_completed = 7;
        goal.waiting_for_user = true;
        goal.last_summary = Some("waiting on approval".to_string());
        goal.started_at_ms = opencli_core::session::now_ms().saturating_sub(92_000);

        let restored = ActiveGoal::from_session_snapshot(goal.to_session_snapshot());

        assert_eq!(restored.objective, "finish verification");
        assert_eq!(restored.turns_completed, 7);
        assert!(restored.waiting_for_user);
        assert_eq!(
            restored.last_summary.as_deref(),
            Some("waiting on approval")
        );
        assert_eq!(restored.started_at_ms, goal.started_at_ms);
        assert!(restored.started_at.elapsed() >= Duration::from_secs(90));
    }

    #[test]
    fn host_state_session_record_includes_active_goal() {
        let mut app = App::new();
        app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));
        let mut record = opencli_core::session::SessionRecord {
            meta: opencli_core::session::SessionMeta {
                id: "test".to_string(),
                cwd: app.cwd.clone(),
                model: "gpt-5".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
                message_count: 0,
                preview: "test".to_string(),
            },
            state: opencli_core::session::SessionSnapshot::default(),
            history: Vec::new(),
        };

        apply_host_state_to_session_record(&app, &mut record);

        assert_eq!(
            record
                .state
                .active_goal
                .as_ref()
                .map(|g| g.objective.as_str()),
            Some("finish verification")
        );
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

    #[tokio::test]
    async fn goal_slash_starts_active_goal_and_queues_prompt() {
        let mut app = App::new();

        handle_slash(&mut app, "goal stabilize the release flow").await;

        let goal = app.active_goal.as_ref().expect("goal should be active");
        assert_eq!(goal.objective, "stabilize the release flow");
        assert_eq!(goal.turns_completed, 0);
        assert_eq!(app.message_queue.len(), 1);
        assert!(app.message_queue[0].contains("stabilize the release flow"));
        assert!(app.message_queue[0].contains("goal_update"));
        assert!(app.pending_session_save);
    }

    #[tokio::test]
    async fn context_slash_renders_window_report() {
        let mut app = App::new();
        handle_slash(&mut app, "context").await;
        let Some(Block::System(text)) = app.blocks.last() else {
            panic!("expected a system block from /context");
        };
        assert!(text.contains("Context window"));
        assert!(text.contains(&app.config.model));
        assert!(text.contains("composition"));
    }

    #[tokio::test]
    async fn buddy_starts_hatch_then_locks() {
        let mut app = App::new();
        handle_slash(&mut app, "buddy").await;
        assert!(
            app.hatch.is_some(),
            "/buddy should start the hatch animation"
        );

        finish_hatch(&mut app);
        assert!(
            app.buddy_pet.is_some(),
            "finishing the hatch adopts the pet"
        );
        assert!(app.hatch.is_none());

        // Locked: a second /buddy must NOT re-hatch or spawn another pet.
        handle_slash(&mut app, "buddy").await;
        assert!(app.hatch.is_none(), "must not re-hatch once adopted");
        assert!(
            matches!(app.blocks.last(), Some(Block::System(t)) if t.contains("already")),
            "expected the locked note"
        );
    }

    #[tokio::test]
    async fn buddy_reset_allows_rehatch() {
        let mut app = App::new();
        handle_slash(&mut app, "buddy").await;
        finish_hatch(&mut app);
        assert!(app.buddy_pet.is_some());

        handle_slash(&mut app, "buddy reset").await;
        assert!(app.buddy_pet.is_none(), "reset clears the adopted pet");

        handle_slash(&mut app, "buddy").await;
        assert!(app.hatch.is_some(), "can hatch again after reset");
    }

    #[tokio::test]
    async fn buddy_off_hides_the_corner_companion() {
        let mut app = App::new();
        handle_slash(&mut app, "buddy off").await;
        assert!(app.buddy_hidden, "/buddy off should hide the corner buddy");
        let Some(Block::System(text)) = app.blocks.last() else {
            panic!("expected a system block from /buddy off");
        };
        assert!(text.contains("hidden"), "{text}");
    }

    #[tokio::test]
    async fn usage_slash_without_quota_shows_hint() {
        let mut app = App::new();
        handle_slash(&mut app, "usage").await;
        let Some(Block::System(text)) = app.blocks.last() else {
            panic!("expected a system block from /usage");
        };
        assert!(text.contains("Usage — provider:"), "{text}");
        assert!(text.contains(&app.config.model), "{text}");
        assert!(text.contains("No live quota data yet"), "{text}");
    }

    #[tokio::test]
    async fn usage_slash_renders_captured_quota() {
        let mut app = App::new();
        app.last_quota = Some(opencli_core::usage::QuotaSnapshot {
            provider: Some(opencli_core::provider::Provider::OpenAi),
            plan: Some("pro".into()),
            windows: vec![opencli_core::usage::QuotaWindow {
                label: "5h".into(),
                used_percent: Some(12.5),
                remaining: None,
                limit: None,
                resets_at_epoch: None,
            }],
            captured_at_epoch: 0,
        });
        handle_slash(&mut app, "usage").await;
        let Some(Block::System(text)) = app.blocks.last() else {
            panic!("expected a system block from /usage");
        };
        assert!(text.contains("Plan: pro"), "{text}");
        assert!(text.contains("5-hour: 12.5% used"), "{text}");
    }

    #[test]
    fn record_history_is_bounded() {
        let mut app = App::new();
        for i in 0..(MAX_INPUT_HISTORY + 50) {
            app.record_history(&format!("msg {i}"));
        }
        assert_eq!(app.input_history.len(), MAX_INPUT_HISTORY);
        // Oldest dropped, newest kept.
        assert_eq!(app.input_history.first().unwrap(), "msg 50");
        assert_eq!(
            app.input_history.last().unwrap(),
            &format!("msg {}", MAX_INPUT_HISTORY + 49)
        );
    }

    #[tokio::test]
    async fn commit_slash_queues_prompt_with_git_safety_protocol() {
        let mut app = App::new();
        handle_slash(&mut app, "commit").await;
        assert_eq!(app.message_queue.len(), 1);
        let p = &app.message_queue[0];
        assert!(p.contains("Conventional-Commits"));
        assert!(p.contains("NEVER force-push"));
        assert!(p.contains("--no-verify"));
    }

    #[tokio::test]
    async fn commit_push_pr_slash_queues_prompt_with_pr_step_and_user_arg() {
        let mut app = App::new();
        handle_slash(&mut app, "commit-push-pr fixes #42").await;
        assert_eq!(app.message_queue.len(), 1);
        let p = &app.message_queue[0];
        assert!(p.contains("gh pr create"));
        assert!(p.contains("fixes #42"));
        assert!(p.contains("force-push"));
    }

    #[test]
    fn ratio_bar_fills_proportionally_and_clamps() {
        assert_eq!(ratio_bar(0), format!("[{}]", "░".repeat(20)));
        assert_eq!(ratio_bar(100), format!("[{}]", "█".repeat(20)));
        assert_eq!(ratio_bar(150), format!("[{}]", "█".repeat(20)));
        assert_eq!(
            ratio_bar(50),
            format!("[{}{}]", "█".repeat(10), "░".repeat(10))
        );
    }

    #[tokio::test]
    async fn goal_slash_asks_before_replacing_active_goal() {
        let mut app = App::new();
        app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));

        handle_slash(&mut app, "goal ship a different feature").await;

        assert_eq!(
            app.active_goal.as_ref().map(|g| g.objective.as_str()),
            Some("finish verification")
        );
        assert_eq!(
            app.pending_goal_replacement
                .as_ref()
                .map(|p| p.objective.as_str()),
            Some("ship a different feature")
        );
        assert!(app.message_queue.is_empty());
        match app.blocks.last() {
            Some(Block::System(text)) => {
                assert!(text.contains("Replace the current goal?"));
                assert!(text.contains("Press Y"));
                assert!(text.contains("N/Esc"));
            }
            other => panic!("expected replacement confirmation, got {other:?}"),
        }
    }

    #[test]
    fn confirming_goal_replacement_starts_new_goal() {
        let mut app = App::new();
        app.active_goal = Some(ActiveGoal::new("old goal".to_string()));
        app.pending_goal_replacement = Some(PendingGoalReplacement {
            objective: "new goal".to_string(),
        });

        handle_goal_replacement_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );

        assert_eq!(
            app.active_goal.as_ref().map(|g| g.objective.as_str()),
            Some("new goal")
        );
        assert!(app.pending_goal_replacement.is_none());
        assert_eq!(app.message_queue.len(), 1);
        assert!(app.message_queue[0].starts_with(GOAL_START_PREFIX));
        assert!(app.message_queue[0].contains("new goal"));
    }

    #[test]
    fn declining_goal_replacement_keeps_goal_and_continues() {
        let mut app = App::new();
        app.active_goal = Some(ActiveGoal::new("old goal".to_string()));
        app.pending_goal_replacement = Some(PendingGoalReplacement {
            objective: "new goal".to_string(),
        });

        handle_goal_replacement_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );

        assert_eq!(
            app.active_goal.as_ref().map(|g| g.objective.as_str()),
            Some("old goal")
        );
        assert!(app.pending_goal_replacement.is_none());
        assert_eq!(app.message_queue.len(), 1);
        assert!(app.message_queue[0].starts_with(GOAL_CONTINUATION_PREFIX));
        assert!(app.message_queue[0].contains("old goal"));
    }

    #[test]
    fn active_goal_queues_continuation_after_turn_complete() {
        let mut app = App::new();
        app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));

        apply_agent_event(&mut app, AgentEvent::TurnComplete);

        let goal = app.active_goal.as_ref().expect("goal should remain active");
        assert_eq!(goal.turns_completed, 1);
        assert_eq!(app.message_queue.len(), 1);
        assert!(app.message_queue[0].contains("Continue the active /goal"));
        assert!(app.message_queue[0].contains("finish verification"));
        assert!(app.pending_session_save);
    }

    #[test]
    fn active_goal_keeps_continuing_past_previous_turn_cap() {
        let mut app = App::new();
        let mut goal = ActiveGoal::new("finish a long migration".to_string());
        goal.turns_completed = 50;
        app.active_goal = Some(goal);

        apply_agent_event(&mut app, AgentEvent::TurnComplete);

        let goal = app.active_goal.as_ref().expect("goal should remain active");
        assert_eq!(goal.turns_completed, 51);
        assert_eq!(app.message_queue.len(), 1);
        assert!(app.message_queue[0].contains("finish a long migration"));
    }

    #[test]
    fn todo_snapshot_tracks_recent_completion_transitions() {
        let mut app = App::new();
        apply_agent_event(
            &mut app,
            AgentEvent::TodosSnapshot {
                todos: vec![todo("run tests", TodoStatus::Pending)],
            },
        );
        assert!(app.todo_completed_at.is_empty());

        apply_agent_event(
            &mut app,
            AgentEvent::TodosSnapshot {
                todos: vec![todo("run tests", TodoStatus::Completed)],
            },
        );
        assert!(app
            .todo_completed_at
            .contains_key(&todo_completion_key(&todo(
                "run tests",
                TodoStatus::Completed
            ))));

        apply_agent_event(
            &mut app,
            AgentEvent::TodosSnapshot {
                todos: vec![todo("new task", TodoStatus::Pending)],
            },
        );
        assert!(app.todo_completed_at.is_empty());
    }

    #[test]
    fn empty_todo_snapshot_keeps_recent_completed_todos_until_ttl() {
        let mut app = App::new();
        apply_agent_event(
            &mut app,
            AgentEvent::TodosSnapshot {
                todos: vec![todo("run tests", TodoStatus::Completed)],
            },
        );
        assert_eq!(app.session_todos.len(), 1);
        assert!(!app.todo_completed_at.is_empty());

        apply_agent_event(&mut app, AgentEvent::TodosSnapshot { todos: Vec::new() });

        assert_eq!(app.session_todos.len(), 1);
        assert!(matches!(app.session_todos[0].status, TodoStatus::Completed));

        let expired_at =
            std::time::Instant::now() - (TODO_RECENT_COMPLETED_TTL + Duration::from_secs(1));
        for completed_at in app.todo_completed_at.values_mut() {
            *completed_at = expired_at;
        }
        prune_expired_completed_todos(&mut app);

        assert!(app.session_todos.is_empty());
        assert!(app.todo_completed_at.is_empty());
    }

    #[test]
    fn completed_todos_without_recent_timestamp_are_pruned() {
        let mut app = App::new();
        app.session_todos = vec![todo("run tests", TodoStatus::Completed)];

        prune_expired_completed_todos(&mut app);

        assert!(app.session_todos.is_empty());
        assert!(app.todo_completed_at.is_empty());
    }

    #[test]
    fn empty_todo_snapshot_clears_non_completed_todos() {
        let mut app = App::new();
        apply_agent_event(
            &mut app,
            AgentEvent::TodosSnapshot {
                todos: vec![todo("run tests", TodoStatus::InProgress)],
            },
        );

        apply_agent_event(&mut app, AgentEvent::TodosSnapshot { todos: Vec::new() });

        assert!(app.session_todos.is_empty());
        assert!(app.todo_completed_at.is_empty());
    }

    #[test]
    fn goal_update_complete_clears_active_goal_before_continuation() {
        let mut app = App::new();
        app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));

        apply_agent_event(
            &mut app,
            AgentEvent::GoalStatusUpdated {
                status: "complete".to_string(),
                summary: "verified".to_string(),
            },
        );
        apply_agent_event(&mut app, AgentEvent::TurnComplete);

        assert!(app.active_goal.is_none());
        assert!(app.message_queue.is_empty());
        assert!(app.pending_session_save);
    }

    #[test]
    fn plan_mode_required_forces_session_plan_without_persisting_config_mode() {
        let mut app = App::new();
        let persisted_mode = app.config.default_permission_mode.clone();

        apply_plan_mode_required(&mut app);

        assert_eq!(app.permission_mode(), PermissionMode::Plan);
        assert_eq!(app.config.default_permission_mode, persisted_mode);
        match app.blocks.last() {
            Some(Block::System(text)) => assert!(text.contains("plan mode required")),
            other => panic!("expected plan mode required system block, got {other:?}"),
        }
    }

    #[test]
    fn model_enter_plan_mode_is_session_only_and_does_not_persist_config_mode() {
        let mut app = App::new();
        app.config.default_permission_mode = PermissionMode::Default.config_str().to_string();
        app.set_permission_mode(PermissionMode::Default);
        app.pending_plan_exit = Some(PendingPlanExit {
            plan: "old plan".to_string(),
        });

        apply_agent_event(&mut app, AgentEvent::PlanModeRequested);

        assert_eq!(app.permission_mode(), PermissionMode::Plan);
        assert_eq!(
            app.config.default_permission_mode,
            PermissionMode::Default.config_str()
        );
        assert!(app.pending_plan_exit.is_none());
        match app.blocks.last() {
            Some(Block::System(text)) => assert!(text.contains("plan mode")),
            other => panic!("expected plan mode system block, got {other:?}"),
        }
    }

    #[test]
    fn active_goal_pauses_when_agent_requests_user_input() {
        let mut app = App::new();
        app.active_goal = Some(ActiveGoal::new("choose a safe path".to_string()));
        apply_agent_event(
            &mut app,
            AgentEvent::ToolCallStarted {
                name: "ask_user_question".to_string(),
                call_id: "ask_1".to_string(),
            },
        );

        apply_agent_event(
            &mut app,
            AgentEvent::ToolResult {
                call_id: "ask_1".to_string(),
                output: "{}".to_string(),
                error: false,
            },
        );
        apply_agent_event(&mut app, AgentEvent::TurnComplete);

        let goal = app.active_goal.as_ref().expect("goal should remain active");
        assert!(goal.waiting_for_user);
        assert!(app.message_queue.is_empty());
        assert!(app.pending_session_save);
    }

    #[test]
    fn active_goal_pauses_after_agent_error() {
        let mut app = App::new();
        app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));
        app.busy = true;
        app.turn_started_at = Some(std::time::Instant::now());

        apply_agent_event(
            &mut app,
            AgentEvent::Error {
                message: "provider 400 invalid schema".to_string(),
            },
        );

        let goal = app.active_goal.as_ref().expect("goal should remain active");
        assert!(goal.waiting_for_user);
        assert_eq!(
            goal.last_summary.as_deref(),
            Some("paused after turn error: provider 400 invalid schema")
        );
        assert!(!app.busy);
        assert!(app.turn_started_at.is_none());
        assert!(app.message_queue.is_empty());
        assert!(app.pending_session_save);
    }

    #[test]
    fn plan_exit_request_waits_for_user_approval() {
        let mut app = App::new();
        app.set_permission_mode(PermissionMode::Plan);

        apply_agent_event(
            &mut app,
            AgentEvent::PlanExitRequested {
                plan: "1. Patch\n2. Test".to_string(),
            },
        );

        assert_eq!(
            app.pending_plan_exit.as_ref().map(|p| p.plan.as_str()),
            Some("1. Patch\n2. Test")
        );
        match app.blocks.last() {
            Some(Block::System(text)) => {
                assert!(text.contains("Plan ready for approval"));
                assert!(text.contains("Press Y"));
            }
            other => panic!("expected plan approval prompt, got {other:?}"),
        }
    }

    #[test]
    fn plan_exit_request_pauses_active_goal_continuation() {
        let mut app = App::new();
        app.set_permission_mode(PermissionMode::Plan);
        app.active_goal = Some(ActiveGoal::new("ship the feature".to_string()));

        apply_agent_event(
            &mut app,
            AgentEvent::PlanExitRequested {
                plan: "1. Patch\n2. Test".to_string(),
            },
        );
        apply_agent_event(&mut app, AgentEvent::TurnComplete);

        let goal = app.active_goal.as_ref().expect("goal should remain active");
        assert!(goal.waiting_for_user);
        assert_eq!(
            goal.last_summary.as_deref(),
            Some("waiting for plan approval")
        );
        assert!(app.message_queue.is_empty());
        assert!(app.pending_session_save);
    }

    #[test]
    fn approving_plan_exit_leaves_plan_mode_and_queues_implementation() {
        let mut app = App::new();
        app.config.default_permission_mode = PermissionMode::Default.config_str().to_string();
        app.set_permission_mode(PermissionMode::Plan);
        let mut goal = ActiveGoal::new("ship the feature".to_string());
        goal.waiting_for_user = true;
        goal.last_summary = Some("waiting for plan approval".to_string());
        app.active_goal = Some(goal);
        app.pending_plan_exit = Some(PendingPlanExit {
            plan: "1. Patch\n2. Test".to_string(),
        });

        handle_plan_exit_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );

        assert_eq!(app.permission_mode(), PermissionMode::Default);
        assert!(app.pending_plan_exit.is_none());
        assert_eq!(app.message_queue.len(), 1);
        assert!(app.message_queue[0].starts_with(PLAN_APPROVED_PREFIX));
        assert!(app.message_queue[0].contains("Approved plan"));
        let goal = app.active_goal.as_ref().expect("goal should stay active");
        assert!(!goal.waiting_for_user);
        assert_eq!(goal.last_summary.as_deref(), Some("plan approved"));
        assert!(app.pending_session_save);
    }

    #[test]
    fn pending_plan_exit_holds_queued_type_ahead_until_approved() {
        // Repro: user types a follow-up while the agent is planning (it queues),
        // then the turn ends with exit_plan_mode. The queued message must NOT
        // flush a new turn while the approval prompt is up — otherwise `busy`
        // flips true and the Y keystroke is swallowed, locking the user out.
        let mut app = App::new();
        app.set_permission_mode(PermissionMode::Plan);
        app.message_queue
            .push("also handle the edge case".to_string());

        apply_agent_event(
            &mut app,
            AgentEvent::PlanExitRequested {
                plan: "1. Patch\n2. Test".to_string(),
            },
        );
        apply_agent_event(&mut app, AgentEvent::TurnComplete);

        assert!(!app.busy);
        assert!(app.pending_plan_exit.is_some());
        // The flush gate must refuse to launch while the decision is pending.
        assert!(turn_launch_blocked_by_pending_decision(&app));
        assert_eq!(app.message_queue.len(), 1);

        // Approving clears the gate and queues the implementation prompt, so the
        // next flush is allowed (now with both the approval and the type-ahead).
        handle_plan_exit_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );

        assert!(!turn_launch_blocked_by_pending_decision(&app));
        assert!(app.pending_plan_exit.is_none());
        assert!(app
            .message_queue
            .iter()
            .any(|m| m.starts_with(PLAN_APPROVED_PREFIX)));
    }

    #[test]
    fn approving_plan_exit_restores_configured_non_plan_mode() {
        let mut app = App::new();
        app.config.default_permission_mode = PermissionMode::AcceptEdits.config_str().to_string();
        app.set_permission_mode(PermissionMode::Plan);
        app.pending_plan_exit = Some(PendingPlanExit {
            plan: "1. Patch\n2. Test".to_string(),
        });

        handle_plan_exit_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );

        assert_eq!(app.permission_mode(), PermissionMode::AcceptEdits);
        assert_eq!(
            app.config.default_permission_mode,
            PermissionMode::AcceptEdits.config_str()
        );
    }

    #[test]
    fn approving_plan_exit_never_keeps_session_in_plan_mode() {
        let mut app = App::new();
        app.config.default_permission_mode = PermissionMode::Plan.config_str().to_string();
        app.set_permission_mode(PermissionMode::Plan);
        app.pending_plan_exit = Some(PendingPlanExit {
            plan: "1. Patch\n2. Test".to_string(),
        });

        handle_plan_exit_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );

        assert_eq!(app.permission_mode(), PermissionMode::Default);
        assert_eq!(
            app.config.default_permission_mode,
            PermissionMode::Plan.config_str()
        );
    }

    #[test]
    fn rejecting_plan_exit_stays_in_plan_mode_and_queues_revision() {
        let mut app = App::new();
        app.set_permission_mode(PermissionMode::Plan);
        let mut goal = ActiveGoal::new("ship the feature".to_string());
        goal.waiting_for_user = true;
        goal.last_summary = Some("waiting for plan approval".to_string());
        app.active_goal = Some(goal);
        app.pending_plan_exit = Some(PendingPlanExit {
            plan: "1. Patch\n2. Test".to_string(),
        });

        handle_plan_exit_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );

        assert_eq!(app.permission_mode(), PermissionMode::Plan);
        assert!(app.pending_plan_exit.is_none());
        assert_eq!(app.message_queue.len(), 1);
        assert!(app.message_queue[0].starts_with(PLAN_REJECTED_PREFIX));
        assert!(app.message_queue[0].contains("Rejected plan"));
        let goal = app.active_goal.as_ref().expect("goal should stay active");
        assert!(!goal.waiting_for_user);
        assert_eq!(
            goal.last_summary.as_deref(),
            Some("plan rejected; revising")
        );
        assert!(app.pending_session_save);
    }

    #[test]
    fn internal_goal_prompt_renders_as_system_progress_not_full_user_message() {
        let mut app = App::new();
        app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));

        push_visible_user_block(&mut app, &goal_start_prompt("finish verification"));

        match app.blocks.last() {
            Some(Block::System(text)) => assert!(text.contains("Goal running")),
            other => panic!("expected system goal progress block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tab_autocompletes_highlighted_slash_command() {
        let mut app = App::new();
        app.input.set_text("/".to_string());
        app.open_overlay(OverlayKind::SlashMenu);
        // Filter to a single command so the highlight is deterministic.
        if let Some((_, p)) = app.overlay.as_mut() {
            p.query = "model".to_string();
            p.ensure_visible_selected();
        }

        handle_overlay_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();

        // The command is completed into the composer and the menu closes, so
        // the user can add arguments or press Enter to run it.
        assert!(app.overlay.is_none(), "menu should close after Tab");
        assert_eq!(app.input.buffer, "/model ");
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
            selected: 0,
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
