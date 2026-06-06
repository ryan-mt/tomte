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

/// One entry in the session todo list. Mirrors Claude Code's TodoWrite shape
/// closely so existing prompts and skills transfer.
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
    /// Canonical paths read this session, keyed exactly as `fs::resolve`
    /// produces them. Powers Claude Code's read-before-write safety:
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
    }
}
