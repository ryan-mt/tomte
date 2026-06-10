//! The Context Manifest — prove the context before the edit lands.
//!
//! Context-stuffing agents shovel files into the window and hope; tomte's Repo
//! Twin already answers *which files belong and why* (`why_context`). This
//! module turns that answer into an automatic pre-edit step: the first time a
//! session edits a file, the pre-flight card shows the manifest —
//!
//! - **pulling** — the files a maintainer would have in context for this edit,
//!   each with the real index edge it came from (import / symbol / test / git)
//!   and whether the agent has actually read it this session (`✓ read` vs
//!   `not read`) — the proof part: claimed context is checked against the
//!   session's own read log, not asserted;
//! - **leaving out** — the nearby files deliberately excluded, each with the
//!   reason it's unreachable from the seed.
//!
//! Cache-only by design: the manifest reads the twin's cached index and NEVER
//! builds one inline (a full scan mid-edit would stall the turn). No cache →
//! no manifest, and a cache the tree has outgrown is labeled stale. Shown once
//! per file per session, and only when the twin actually connects something —
//! an isolated file stays silent (Pillar 4: a tidy house is quiet).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::repo_twin::select::Selection;

/// Connected files shown on the card.
const MAX_PULLED: usize = 5;
/// Deliberately-excluded neighbors shown.
const MAX_LEFT_OUT: usize = 3;

/// Build the manifest lines from a twin selection. Pure: `was_read` answers
/// "has this session read that file?" so the rule is unit-testable without a
/// session. Empty when the selection connects nothing — no card for a file the
/// twin has no edges for.
pub fn manifest_lines(
    sel: &Selection,
    fresh: bool,
    was_read: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    if sel.selected.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for f in sel.selected.iter().take(MAX_PULLED) {
        let because = f
            .reasons
            .first()
            .map(|r| r.detail.clone())
            .unwrap_or_else(|| "connected to the seed".to_string());
        let seen = if was_read(&f.path) {
            "✓ read this session"
        } else {
            "not read yet"
        };
        out.push(format!("pulling {} — {} · {}", f.path, because, seen));
    }
    if sel.selected.len() > MAX_PULLED {
        out.push(format!(
            "… and {} more connected file(s): tomte why-context {}",
            sel.selected.len() - MAX_PULLED,
            sel.seed
        ));
    }
    for f in sel.ignored.iter().take(MAX_LEFT_OUT) {
        out.push(format!("leaving out {} — {}", f.path, f.reason));
    }
    if !fresh {
        out.push("(from the twin cache — the tree has changed since it was built)".to_string());
    }
    out
}

/// The manifest for an edit about to land on `path` (relative or absolute).
/// Loads the cached twin only (never builds), runs the X-ray selection, and
/// checks each pulled file against the session's read log. Empty when there is
/// no cache or the twin connects nothing to this file.
pub fn for_edit(cwd: &Path, path: &str, read_files: &HashSet<PathBuf>) -> Vec<String> {
    let Some((twin, fresh)) = crate::repo_twin::load_cached(cwd) else {
        return Vec::new();
    };
    let root = PathBuf::from(&twin.root);
    let sel = crate::repo_twin::select::why_context(&twin, cwd, path);
    let was_read = |rel: &str| {
        let abs = root.join(rel);
        let canon = std::fs::canonicalize(&abs).unwrap_or(abs);
        read_files.contains(&canon)
    };
    manifest_lines(&sel, fresh, &was_read)
}

#[cfg(test)]
mod tests;
