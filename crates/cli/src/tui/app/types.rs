//! Core TUI types: permission/screen modes, blocks, App state, and goals.
//! Split out of `app`; logic unchanged.

use super::*;

mod block;
mod modes;
mod pending;

pub use block::*;
pub use modes::*;
pub use pending::*;

pub struct App {
    pub screen: Screen,
    /// Alternate-screen vs inline viewport (Pillar 4). Fixed at startup from
    /// the environment; see [`RenderMode`].
    pub render_mode: RenderMode,
    pub login: LoginScreen,
    pub blocks: Vec<Block>,
    /// Inline mode only: how many leading `blocks` have already been pushed to
    /// the terminal's native scrollback via `insert_before`. The live viewport
    /// renders only `blocks[committed_blocks..]`. Unused in alt-screen mode.
    pub committed_blocks: usize,
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
    /// auto-follow (click-to-jump).
    pub jump_to_bottom_hint: Option<ratatui::layout::Rect>,
    /// Live sub-agents spawned by `dispatch_agent` during the current turn, in
    /// start order. Rendered as a fleet-view panel above the input and cleared
    /// when the turn ends.
    pub subagents: Vec<SubagentView>,
    /// Screen rect of each fleet row paired with its sub-agent id, set by
    /// `render` each frame so a left-click can toggle the row's detail.
    pub subagent_rows: Vec<(ratatui::layout::Rect, String)>,
    /// Screen rect of each visible "Thought for Xs" line paired with its block
    /// index, set by `render_chat` each frame so a left-click can toggle the
    /// block's reasoning open/closed (click-to-expand the collapsed thought).
    pub thought_rows: Vec<(ratatui::layout::Rect, usize)>,
    pub status_line: String,
    /// In-progress / completed left-drag text selection over the screen, in
    /// cell coordinates. Drawn as a highlight and copied to the clipboard on
    /// release; `None` when nothing is selected. See `tui::selection`.
    pub selection: Option<crate::tui::selection::Selection>,
    /// Clone of the last rendered frame, captured only while a selection is
    /// active, so the selected cells' text can be read back on mouse-release.
    pub last_buffer: Option<ratatui::buffer::Buffer>,
    /// Brief "copied N chars" confirmation shown after a selection is copied;
    /// cleared on the next key / scroll / new click.
    pub copy_notice: Option<String>,
    pub last_height: u16,
    pub last_width: u16,
    pub turn_started_at: Option<std::time::Instant>,
    /// Per-turn seed for the drifting spinner word (see `spinner_word_index`).
    /// Chosen once per turn; the displayed word is a pure function of this seed
    /// and the turn's elapsed time, so it never flickers between draws.
    pub spinner_seed: u32,
    /// The effective spinner word pool, resolved once at startup from the
    /// built-in pool plus any `spinner_verbs` config override (see
    /// `resolve_spinner_words`). Held as owned strings so user words live here.
    pub spinner_words: Vec<String>,
    pub tokens_used: u64,
    /// Cumulative per-model billed token tally, mirrored from the agent's
    /// `AgentEvent::CostUpdate` and rendered by `/cost`. The agent owns the
    /// authoritative copy and persists it in the session record.
    pub usage_by_model: Vec<tomte_core::session::ModelUsage>,
    /// Number of turns the user has run this session. Surfaced by `/cost`.
    pub turn_count: u64,
    /// Latest provider quota/rate-limit snapshot, captured from the most recent
    /// turn's response. Surfaced by `/usage`; `None` until the first turn.
    pub last_quota: Option<tomte_core::usage::QuotaSnapshot>,
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
    /// The companion shown in the welcome card. Rolled once at startup from the
    /// signed-in account (same deterministic roll as `/buddy`), so the greeting
    /// pet matches the pet the account will later hatch. A glimpse, not the
    /// hatch reveal — the welcome shows the sprite but never names it.
    pub welcome_pet: usize,
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
    /// Set by Ctrl+O at rest in inline mode; the main loop opens the modal
    /// expanded-transcript pager on its next pass (the pager pumps the shared
    /// event stream, which the key handler doesn't hold).
    pub open_transcript_pager: bool,
    /// Set by `/memory edit` at rest; the main loop suspends the TUI and opens
    /// the project CLAUDE.md in `$VISUAL`/`$EDITOR` on its next pass (the
    /// suspend/restore needs the terminal handle, which the slash handler
    /// doesn't hold — same pattern as the pager flag above).
    pub open_memory_editor: bool,
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
    /// Set true by `/rewind` so main_loop (which has the agent Arc) can read the
    /// agent's checkpoints and open the rewind picker on the next tick.
    pub pending_rewind_open: bool,
    /// Set by the rewind picker to the chosen checkpoint ordinal so main_loop can
    /// run `Agent::rewind_to()` and rebuild the transcript on the next tick —
    /// the same deferred pattern as `pending_resume_id`.
    pub pending_rewind_ordinal: Option<usize>,
    /// Picker-ready rewind points (label, age, blast radius), copied here by
    /// main_loop just before it opens the rewind picker — so `open_overlay` (which
    /// has no agent Arc) can build the rows from it.
    pub rewind_points: Vec<tomte_core::tools::RewindPointView>,
    /// Set true by `/clear` so main_loop can invoke `Agent::clear_history()` on
    /// the next tick (slash handlers don't have the agent Arc). The transcript
    /// UI is cleared immediately in the handler; this resets the model's context.
    pub pending_clear: bool,
    /// Set true by `/prove` so main_loop spawns the background proof collection
    /// (git + the project's test/typecheck/lint/build scripts) on the next tick.
    /// The capsule it gathers is the CLI's own evidence — the model never sees a
    /// chance to forge it.
    pub pending_prove: bool,
    /// True while a background `/prove` collection is running — guards against a
    /// second `/prove` spawning a duplicate run before the first reports back.
    pub proving: bool,
    /// Set by `/prove explain`: when the background collection reports back,
    /// also queue a prompt asking the agent to explain the capsule (the CLI
    /// keeps the numbers; the model only interprets them).
    pub prove_explain: bool,
    /// Set true by `/compact` or the auto-compact trigger so main_loop can call
    /// `Agent::compact_history()` on the next tick (slash/event handlers don't
    /// have the agent Arc).
    pub pending_compact: bool,
    /// Optional user steer from `/compact <focus>`, consumed by
    /// `start_compaction` and passed to `Agent::compact_history`. `None` for an
    /// auto-compact or a bare `/compact`.
    pub compact_focus: Option<String>,
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
    /// `tomte resume` to bypass needing to type `/resume` after launch.
    pub start_with_resume_picker: bool,
    /// Snapshot of the agent's session todo list, refreshed after every
    /// tool batch via `AgentEvent::TodosSnapshot`. Read by `/todos`.
    pub session_todos: Vec<TodoItem>,
    /// Completion timestamps keyed by stable todo text. Used only for display
    /// priority so newly completed items remain visible briefly in long lists.
    pub todo_completed_at: HashMap<String, std::time::Instant>,
    /// Whether the live todo panel is expanded above the input.
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
    /// Pillar 5 (A2) — the active conscience-conflict card, if any. Sibling of
    /// `pending_approval`; resolved via `conscience_handle`.
    pub pending_conscience: Option<PendingConscience>,
    /// Clone of the Agent's `pending_conscience` Arc, captured at turn start so
    /// the card's choice is delivered without blocking on the agent mutex.
    pub conscience_handle: Option<ConscienceHandle>,
    /// Previously submitted prompts, oldest first. Up/Down in the composer
    /// recall these (shell-style history). In-memory for this session.
    pub input_history: Vec<String>,
    /// Cursor into `input_history` while browsing with Up/Down. `None` means the
    /// user is editing a fresh draft rather than navigating history.
    pub history_pos: Option<usize>,
    /// Draft text stashed when history browsing begins, restored when the user
    /// presses Down past the newest entry.
    pub history_draft: String,
    /// When the last Ctrl+C landed, arming the quit guard: a second Ctrl+C
    /// within [`CTRL_C_QUIT_WINDOW`] exits, any other key disarms. One reflexive
    /// Ctrl+C (terminal copy/cancel habit) must never kill a live session.
    pub ctrl_c_armed_at: Option<std::time::Instant>,
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
    /// Whether the OS terminal window/tab title has been set for the current
    /// segment. The title is named after the first prompt of the session
    /// (`tomte — <task>`); `/clear` re-baselines it to `tomte` and flips this
    /// back to `false` so the next prompt re-titles the window. See
    /// [`super::window_title_from_prompt`].
    pub window_titled: bool,
}
