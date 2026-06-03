//! System-prompt injection for the memory store: re-applies the `MEMORY.md`
//! index (or a listing of saved notes) into the prompt each session, inside a
//! replaceable marker block. Split out of `memory`; logic unchanged.

use std::path::Path;

use super::{md_files, store_dir};

/// Index file always re-injected into the system prompt.
pub(super) const INDEX_FILE: &str = "MEMORY.md";
/// Cap applied to the index when injecting it into the prompt.
pub(super) const INDEX_MAX_LINES: usize = 200;
pub(super) const INDEX_MAX_BYTES: usize = 25 * 1024;

pub(super) const STORE_BLOCK_BEGIN: &str = "\n\n<!-- opencli-memory-store:start -->\n";
const STORE_BLOCK_END: &str = "\n<!-- opencli-memory-store:end -->\n";

/// Re-inject the project memory index (`MEMORY.md`, capped) into `prompt`
/// inside a replaceable marker block. When `MEMORY.md` is absent but notes
/// exist, inject a short listing instead so a session that forgot to keep an
/// index still discovers its notes. Idempotent — strips any prior block first.
pub fn apply_store_to_prompt(prompt: &mut String, cwd: &Path) {
    apply_store_at(prompt, &store_dir(cwd));
}

/// Inner form of [`apply_store_to_prompt`] taking an explicit store root, so
/// tests can exercise injection against a tempdir instead of the real config
/// directory.
pub(super) fn apply_store_at(prompt: &mut String, root: &Path) {
    strip_store_block(prompt);
    let Some(block) = index_block(root) else {
        return;
    };
    prompt.push_str(STORE_BLOCK_BEGIN);
    prompt.push_str(&block);
    prompt.push_str(STORE_BLOCK_END);
}

fn strip_store_block(prompt: &mut String) {
    if let Some(start) = prompt.find(STORE_BLOCK_BEGIN) {
        prompt.truncate(start);
    }
}

pub(super) fn index_block(root: &Path) -> Option<String> {
    let index_path = root.join(INDEX_FILE);
    if let Ok(text) = std::fs::read_to_string(&index_path) {
        let capped = cap_index(&text);
        if !capped.trim().is_empty() {
            return Some(format!(
                "# Project memory\n\nYour saved notes for this project (from {INDEX_FILE}). Use the `memory` tool to `view` a note in full or to update these.\n\n{capped}"
            ));
        }
    }
    let files = md_files(root);
    if files.is_empty() {
        return None;
    }
    let mut s = String::from(
        "# Project memory\n\nThese saved notes exist for this project. Use the `memory` tool with `command: \"view\"` to read one (and keep a MEMORY.md index):\n",
    );
    for (name, size) in files {
        s.push_str(&format!("- {name} ({size} bytes)\n"));
    }
    Some(s)
}

const TRUNCATION_MARKER: &str =
    "\n(… memory index truncated; `view MEMORY.md` for the full list …)";

pub(super) fn cap_index(text: &str) -> String {
    // Reserve room for the marker so the result stays within INDEX_MAX_BYTES
    // even after it is appended.
    let budget = INDEX_MAX_BYTES.saturating_sub(TRUNCATION_MARKER.len());
    let mut out = String::new();
    let mut truncated = false;
    for (i, line) in text.lines().enumerate() {
        if i >= INDEX_MAX_LINES || out.len() + line.len() + 1 > budget {
            truncated = true;
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if truncated {
        out.push_str(TRUNCATION_MARKER);
    }
    out
}
