//! Tool runtime types: todo items, background-shell handles, undo entries,
//! worktree state, and the shared `SessionState`. Split out of `tools`;
//! logic unchanged.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex};

/// Status of a single todo item.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn parse(s: &str) -> Option<Self> {
        let normalized = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match normalized.as_str() {
            "pending" | "todo" | "open" | "not_started" => Some(Self::Pending),
            "in_progress" | "inprogress" | "active" | "doing" | "started" => Some(Self::InProgress),
            "completed" | "complete" | "done" | "finished" => Some(Self::Completed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod todo_status_tests {
    use super::TodoStatus;

    #[test]
    fn parse_accepts_model_facing_aliases_and_rejects_unknown() {
        // A model phrases a todo status many ways; these all funnel to one of the
        // three canonical states (lock so a dropped alias can't silently mis-parse
        // a status). Trim + case + `-`/space → `_` normalization is exercised too.
        for s in [
            "pending",
            "todo",
            "open",
            "not_started",
            "NOT STARTED",
            " Pending ",
        ] {
            assert_eq!(TodoStatus::parse(s), Some(TodoStatus::Pending), "{s:?}");
        }
        for s in [
            "in_progress",
            "inprogress",
            "in progress",
            "active",
            "doing",
            "started",
        ] {
            assert_eq!(TodoStatus::parse(s), Some(TodoStatus::InProgress), "{s:?}");
        }
        for s in ["completed", "complete", "done", "finished"] {
            assert_eq!(TodoStatus::parse(s), Some(TodoStatus::Completed), "{s:?}");
        }
        // An unrecognized value is rejected (so the caller keeps the raw value
        // rather than guessing a wrong state).
        assert_eq!(TodoStatus::parse("bogus"), None);
        assert_eq!(TodoStatus::parse(""), None);
    }
}

/// One entry in the session todo list. The shape stays close to the common
/// `TodoWrite` convention so existing prompts and skills transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub active_form: String,
    /// Optional stable id used to express dependencies between items. `None`
    /// for a plain flat list. Skipped on the wire when absent so existing
    /// (idless) session records round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Ids of items that must reach `completed` before this one can start.
    /// Empty for an unconstrained item; lets the model plan a DAG instead of a
    /// flat list. Skipped on the wire when empty for round-trip parity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

/// Status of a background shell command. Returned as part of every
/// `bash_output` poll so the model can tell when a job has finished.
#[derive(Debug, Clone)]
pub enum BgStatus {
    Running,
    Exited(i32),
    Killed,
    Error(String),
}

impl BgStatus {
    pub fn label(&self) -> String {
        match self {
            BgStatus::Running => "running".into(),
            BgStatus::Exited(c) => format!("exited({c})"),
            BgStatus::Killed => "killed".into(),
            BgStatus::Error(e) => format!("error({e})"),
        }
    }
    pub fn is_terminal(&self) -> bool {
        !matches!(self, BgStatus::Running)
    }
}

/// Handle to a background shell command spawned by `run_shell {run_in_background: true}`.
/// Lives inside `SessionState.background_shells` so the model can later poll
/// output via `bash_output` or terminate via `kill_shell`.
#[derive(Debug)]
pub struct BackgroundShellState {
    pub command: String,
    pub started_at_ms: u64,
    pub stdout: Mutex<Vec<u8>>,
    pub stderr: Mutex<Vec<u8>>,
    pub status: Mutex<BgStatus>,
    /// Read cursors so successive `bash_output` calls only return new bytes.
    pub stdout_cursor: Mutex<usize>,
    pub stderr_cursor: Mutex<usize>,
    /// `Some` while the child is alive; `None` after termination or kill.
    pub kill_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Process-group leader pid, so the group can be killed synchronously (e.g.
    /// from `SessionState`'s `Drop`) without going through the async kill_tx.
    pub pid: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub path: std::path::PathBuf,
    pub original_content: Option<Vec<u8>>,
    /// Mtime snapshot captured immediately after the tool wrote the file.
    /// Compared against the current mtime at undo time — if the file has
    /// been touched in between (user edited it externally, another tool,
    /// an editor save) we refuse to restore so the user's manual changes
    /// are not silently overwritten. `None` disables the check.
    pub post_edit_mtime: Option<std::time::SystemTime>,
    /// File size snapshot captured alongside `post_edit_mtime`. Compared too
    /// at undo time so a same-second external edit (which a coarse 1s-resolution
    /// mtime can't distinguish) is still caught whenever it changes the length.
    pub post_edit_size: Option<u64>,
}

/// A rewind point recorded at a user-turn boundary, so `/rewind` can restore the
/// session to just before that turn: truncate the conversation back to
/// `history_index`, and revert every file edit made since (those pushed onto the
/// undo stack after `edits_before`). Runtime-only — it indexes into the undo
/// stack, which is itself deliberately not persisted across `/resume`.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    /// `history.len()` just before this turn's user message — the truncation point.
    pub history_index: usize,
    /// [`SessionState::undo_pushed`] at the moment this turn started. The edits to
    /// revert are those pushed after it; a monotonic counter (not the stack
    /// length) so it stays correct even after the capped stack evicts old entries.
    pub edits_before: u64,
    /// One-line label for the picker (the user prompt, trimmed).
    pub label: String,
    /// Epoch ms the turn started, for an "ago" hint in the picker.
    pub created_at_ms: u64,
}

/// A picker-ready view of one rewind point, including its blast radius (how many
/// files reverting to it would touch) — surfaced BEFORE the user commits, so the
/// choice is legible (Pillar 1). Built by [`crate::agent::Agent::rewind_preview`]
/// under the session lock; the ordinal is the entry's index in the returned list.
#[derive(Debug, Clone)]
pub struct RewindPointView {
    pub label: String,
    pub created_at_ms: u64,
    /// Distinct files reverting to this point would restore.
    pub files_to_revert: usize,
}

/// What a `/rewind` actually did, for the calm end-of-rewind summary.
#[derive(Debug, Clone, Default)]
pub struct RewindOutcome {
    /// The label of the turn we rewound to.
    pub label: String,
    /// How many later user turns were dropped from the conversation.
    pub turns_dropped: usize,
    /// Files restored to their pre-turn content (or deleted, if newly created).
    pub files_reverted: usize,
    /// Files left untouched because they changed outside tomte (or a write failed)
    /// — reported, never clobbered.
    pub files_skipped: Vec<std::path::PathBuf>,
    /// `run_shell` side effects since the checkpoint, which cannot be undone.
    pub shell_effects: usize,
}

#[derive(Debug, Clone)]
pub struct WorktreeState {
    pub original_cwd: std::path::PathBuf,
    pub repo_root: std::path::PathBuf,
    pub worktree_path: std::path::PathBuf,
    pub branch: String,
    pub base_head: String,
}

#[derive(Debug, Default)]
pub struct SessionState {
    pub todos: Vec<TodoItem>,
    pub background_shells: HashMap<String, Arc<BackgroundShellState>>,
    pub undo_stack: std::collections::VecDeque<UndoEntry>,
    /// Sequence number of the most recent *live* edit on
    /// [`undo_stack`](Self::undo_stack): incremented on every push and decremented
    /// on every `/undo`-style pop ([`pop_undo_entry`](Self::pop_undo_entry)), but
    /// NOT on eviction — so it keeps climbing as the capped stack drops its oldest
    /// entry, yet falls back when an edit is undone. A [`Checkpoint`] records this
    /// value so `/rewind` can tell exactly how many edits are still live since it:
    /// eviction is absorbed by the stack-length clamp, and an intervening `/undo`
    /// by this decrement. Without the decrement, `/rewind` to an earlier point
    /// would over-count and revert edits from before the checkpoint.
    pub undo_pushed: u64,
    /// Canonical paths read this session, keyed exactly as `fs::resolve`
    /// produces them. Powers the read-before-write safety:
    /// `write_file` refuses to overwrite, and `edit_file`/`multi_edit` refuse
    /// to touch, a file that was never read — so the model can't clobber
    /// content it has not seen. A successful write/edit also records the path.
    pub read_files: std::collections::HashSet<std::path::PathBuf>,
    /// Subset of [`read_files`](Self::read_files) whose ENTIRE current content
    /// the model has seen this session — a full (offset 0, untruncated) read, or
    /// a file it just wrote/authored in full. `write_file` overwrites only files
    /// in this set, so a partial (`offset`/`limit`) read can't let it discard
    /// unseen content. A partial read of a file drops it back out.
    pub fully_read_files: std::collections::HashSet<std::path::PathBuf>,
    /// `(mtime, size)` captured for each file when it was last read or written
    /// this session. Lets `edit_file`/`multi_edit`/`write_file` force a re-read
    /// when the file changed on disk since the model last saw it (the user's
    /// editor, another tool, or another process touched it) — editing a stale
    /// view would either fail to match or apply against bytes the model never
    /// saw. Refreshed after each successful write/edit so back-to-back edits to
    /// a file the model itself just changed don't spuriously demand a re-read.
    /// Runtime only (not part of `SessionRecord`); a resumed session starts
    /// empty and falls back to the plain read-once check until the file is read
    /// again.
    pub read_file_meta:
        std::collections::HashMap<std::path::PathBuf, (Option<std::time::SystemTime>, Option<u64>)>,
    /// Worktree created by this session via `enter_worktree`. Exit/remove tools
    /// are scoped to this state so tomte never cleans up a user-created worktree.
    pub worktree: Option<WorktreeState>,
}

impl Drop for SessionState {
    /// Kill any still-running background shells when the session ends so their
    /// processes (and descendants) don't outlive the CLI as orphans. Nothing
    /// else fires their kill switch on shutdown.
    fn drop(&mut self) {
        for shell in self.background_shells.values() {
            shell.kill_now();
        }
    }
}

impl SessionState {
    pub fn push_undo_entry(&mut self, entry: UndoEntry) {
        const MAX_UNDO: usize = 32;
        if self.undo_stack.len() >= MAX_UNDO {
            self.undo_stack.pop_front();
        }
        self.undo_stack.push_back(entry);
        self.undo_pushed = self.undo_pushed.saturating_add(1);
    }

    /// Pop the most recent undo entry (an `/undo` / `undo_last_edit`), keeping
    /// [`undo_pushed`](Self::undo_pushed) in step. `/undo` removes the TOP of the
    /// stack, so the live edits-pushed count must drop too — otherwise `/rewind`'s
    /// "edits since this checkpoint" math (which trusts `undo_pushed`) would
    /// over-count and revert edits from BEFORE the checkpoint. Eviction
    /// (`push_undo_entry`'s `pop_front`) is deliberately different: it drops the
    /// OLDEST entry and does NOT decrement, so the count still survives eviction.
    /// Returns the popped entry, if any.
    pub fn pop_undo_entry(&mut self) -> Option<UndoEntry> {
        let entry = self.undo_stack.pop_back();
        if entry.is_some() {
            self.undo_pushed = self.undo_pushed.saturating_sub(1);
        }
        entry
    }

    /// After an `/undo` restores `path` to a previous edit's post-state, the file
    /// carries a fresh mtime. If the NEXT undo entry targets that same `path`, its
    /// recorded post-snapshot is now stale — the staleness guard would read our
    /// own restore as an external edit and refuse the next `/undo`. Refresh it to
    /// the file's current `(mtime, size)` so stacked edits to one file unwind all
    /// the way; a genuine external edit afterwards still moves the mtime and trips
    /// the guard, so the protection is intact. Call only after a content restore
    /// (not a new-file deletion) and after the just-undone entry is popped.
    pub fn refresh_top_snapshot_for(&mut self, path: &std::path::Path) {
        if let Some(top) = self.undo_stack.back_mut() {
            if top.path == path {
                let meta = std::fs::metadata(path);
                top.post_edit_mtime = meta.as_ref().ok().and_then(|m| m.modified().ok());
                top.post_edit_size = meta.as_ref().ok().map(|m| m.len());
            }
        }
    }
}
