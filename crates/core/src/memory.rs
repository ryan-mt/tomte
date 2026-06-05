//! Inherited instruction files (`AGENTS.md`, `CLAUDE.md`, …) for the system prompt.
//!
//! Discovery follows Codex-style rules where practical:
//! - At most one instruction file per directory (`AGENTS.override.md` > `AGENTS.md` > `CLAUDE.md`).
//! - Project scope is limited to the git repository root through `cwd` (not the whole filesystem).
//! - Combined body text is capped at 32 KiB by default.
//! - Re-applying memory replaces the previous block (idempotent).

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// Default combined memory body budget (matches Codex `project_doc_max_bytes`).
pub const PROJECT_DOC_MAX_BYTES: usize = 32 * 1024;

pub const MEMORY_BLOCK_BEGIN: &str = "\n\n<!-- tomte-inherited-memory:start -->\n";
pub const MEMORY_BLOCK_END: &str = "\n<!-- tomte-inherited-memory:end -->\n";

const MEMORY_CANDIDATES: &[&str] = &["AGENTS.override.md", "AGENTS.md", "CLAUDE.md"];

/// Strip a previously applied inherited-memory block, if present.
pub fn strip_from_system_prompt(prompt: &mut String) {
    if let Some(start) = prompt.find(MEMORY_BLOCK_BEGIN) {
        prompt.truncate(start);
    }
}

/// Collect inherited memory for `cwd` and append it to `system_prompt` inside a
/// replaceable marker block. Safe to call multiple times.
pub fn apply_to_system_prompt(system_prompt: &mut String, cwd: &Path) {
    apply_to_system_prompt_with_globals(system_prompt, cwd, global_memory_dirs());
}

/// Like [`apply_to_system_prompt`] but with an explicit global directory list.
/// Exposed for integration tests so they do not depend on the developer's home
/// directory contents.
#[doc(hidden)]
pub fn apply_to_system_prompt_with_globals(
    system_prompt: &mut String,
    cwd: &Path,
    global_dirs: Vec<PathBuf>,
) {
    strip_from_system_prompt(system_prompt);
    let block = format_memory_block(&collect_entries_with_globals(cwd, global_dirs));
    if block.is_empty() {
        return;
    }
    system_prompt.push_str(MEMORY_BLOCK_BEGIN);
    system_prompt.push_str(&block);
    system_prompt.push_str(MEMORY_BLOCK_END);
}

/// Paths of every memory file currently applied to the system prompt for `cwd`,
/// in prompt display order (global first, then project files walking up to the
/// git root). Backs the `/context` report's "Memory files" section. Uses the
/// same discovery as [`apply_to_system_prompt`], so the count stays in sync with
/// what is actually injected.
pub fn applied_files(cwd: &Path) -> Vec<PathBuf> {
    collect_entries_with_globals(cwd, global_memory_dirs())
        .into_iter()
        .map(|entry| entry.path)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryEntry {
    path: PathBuf,
    text: String,
    global: bool,
}

fn collect_entries_with_globals(cwd: &Path, global_dirs: Vec<PathBuf>) -> Vec<MemoryEntry> {
    // Candidate directories in display order: least-specific (global) first,
    // most-specific (cwd) last, so the most task-relevant memory appears last in
    // the prompt.
    let mut candidates: Vec<(PathBuf, bool)> = Vec::new();
    for dir in global_dirs {
        candidates.push((dir, true));
    }
    for dir in project_memory_dirs(cwd) {
        candidates.push((dir, false));
    }

    // Resolve to entries, deduping by canonical path, preserving display order.
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut entries: Vec<MemoryEntry> = Vec::new();
    for (dir, global) in candidates {
        let Some((path, text)) = pick_memory_file(&dir) else {
            continue;
        };
        if !seen.insert(canonical_memory_path(&path)) {
            continue;
        }
        entries.push(MemoryEntry { path, text, global });
    }

    // Enforce the combined byte budget, but prioritize the most-specific
    // entries: walk from cwd-level (end) back toward global (start) and keep an
    // entry only while it fits. This drops the least-specific overflow first,
    // so a large global file can never starve the project's own memory.
    let mut used = 0usize;
    let mut keep = vec![false; entries.len()];
    for (i, entry) in entries.iter().enumerate().rev() {
        let next = used.saturating_add(entry.text.len());
        if next <= PROJECT_DOC_MAX_BYTES {
            used = next;
            keep[i] = true;
        }
    }
    entries
        .into_iter()
        .zip(keep)
        .filter_map(|(entry, keep)| keep.then_some(entry))
        .collect()
}

fn format_memory_block(entries: &[MemoryEntry]) -> String {
    let mut additions = String::new();
    for entry in entries {
        let label = if entry.global {
            "Global memory"
        } else {
            "Project memory"
        };
        additions.push_str(&format!("\n\n# {label} ({})\n\n", entry.path.display()));
        additions.push_str(&entry.text);
    }
    additions
}

fn pick_memory_file(dir: &Path) -> Option<(PathBuf, String)> {
    for name in MEMORY_CANDIDATES {
        let path = dir.join(name);
        let Ok(text) = crate::config::read_text_file_capped(&path, PROJECT_DOC_MAX_BYTES as u64)
        else {
            continue;
        };
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some((path, trimmed.to_string()));
        }
    }
    None
}

fn canonical_memory_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn global_memory_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(codex_home) = env_path("CODEX_HOME") {
        dirs.push(codex_home);
    } else if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".codex"));
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".claude"));
    }
    dirs.push(crate::config::config_dir());
    dirs
}

fn env_path(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    (!path.as_os_str().is_empty()).then_some(path)
}

/// Directories from the repository root through `cwd` (inclusive), in
/// ancestor-first order. When no git root is found, only `cwd` is checked.
pub fn project_memory_dirs(cwd: &Path) -> Vec<PathBuf> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let Some(git_root) = git_root_from(&cwd) else {
        return vec![cwd];
    };
    let git_root = git_root.canonicalize().unwrap_or(git_root);
    if !cwd.starts_with(&git_root) {
        return vec![cwd];
    }
    let mut dirs = vec![git_root.clone()];
    let rel = match cwd.strip_prefix(&git_root) {
        Ok(rel) => rel,
        Err(_) => return vec![cwd],
    };
    let mut acc = git_root;
    for comp in rel.components() {
        if let Component::Normal(name) = comp {
            acc = acc.join(name);
            dirs.push(acc.clone());
        }
    }
    dirs
}

/// Resolve the git repository root for `cwd`, or `None` when not inside a repo.
pub fn git_root_from(cwd: &Path) -> Option<PathBuf> {
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
    {
        if output.status.success() {
            let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !root.is_empty() {
                return std::fs::canonicalize(&root).ok();
            }
        }
    }
    let mut cur = cwd.canonicalize().ok()?;
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        match cur.parent() {
            Some(parent) if parent != cur => cur = parent.to_path_buf(),
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_git_repo(root: &Path) {
        let out = std::process::Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init");
        assert!(
            out.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn pick_memory_file_falls_through_when_override_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("AGENTS.md"), "AGENTS").unwrap();
        let (_, text) = pick_memory_file(dir).unwrap();
        assert_eq!(text, "AGENTS");
    }

    #[test]
    fn pick_memory_file_prefers_override_then_agents_then_claude() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("CLAUDE.md"), "CLAUDE").unwrap();
        std::fs::write(dir.join("AGENTS.md"), "AGENTS").unwrap();
        std::fs::write(dir.join("AGENTS.override.md"), "OVERRIDE").unwrap();

        let (_, text) = pick_memory_file(dir).unwrap();
        assert_eq!(text, "OVERRIDE");

        std::fs::remove_file(dir.join("AGENTS.override.md")).unwrap();
        let (_, text) = pick_memory_file(dir).unwrap();
        assert_eq!(text, "AGENTS");

        std::fs::remove_file(dir.join("AGENTS.md")).unwrap();
        let (_, text) = pick_memory_file(dir).unwrap();
        assert_eq!(text, "CLAUDE");
    }

    #[cfg(unix)]
    #[test]
    fn pick_memory_file_rejects_symlinked_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "SECRET").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), repo.join("AGENTS.md")).unwrap();

        assert!(
            pick_memory_file(&repo).is_none(),
            "symlinked instruction files must not be injected into the prompt"
        );
    }

    #[test]
    fn project_memory_dirs_stop_at_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path();
        let repo = parent.join("repo");
        let nested = repo.join("packages").join("app");
        std::fs::create_dir_all(&nested).unwrap();
        init_git_repo(&repo);

        let dirs = project_memory_dirs(&nested);
        assert_eq!(
            dirs,
            vec![
                repo.canonicalize().unwrap(),
                repo.join("packages").canonicalize().unwrap(),
                nested.canonicalize().unwrap(),
            ]
        );
    }

    #[test]
    fn project_memory_dirs_without_git_only_checks_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&dir).unwrap();
        let dirs = project_memory_dirs(&dir);
        assert_eq!(dirs, vec![dir.canonicalize().unwrap()]);
    }

    #[test]
    fn byte_cap_drops_least_specific_first_keeping_cwd_memory() {
        // A huge ancestor (git-root) file must not starve the most-specific
        // (cwd-level) memory: the cap drops the least-specific overflow first.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_git_repo(repo);
        let big = "x".repeat(PROJECT_DOC_MAX_BYTES);
        std::fs::write(repo.join("AGENTS.md"), &big).unwrap();
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("AGENTS.md"), "SMALL").unwrap();

        let entries = collect_entries_with_globals(&sub, vec![]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "SMALL", "cwd memory must survive the cap");
    }

    #[test]
    fn large_global_memory_does_not_starve_project_memory() {
        // A ≥32 KiB global file must not crowd out the project's own memory.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_git_repo(repo);
        std::fs::write(repo.join("AGENTS.md"), "PROJECT RULES").unwrap();

        let global = tmp.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(global.join("AGENTS.md"), "g".repeat(PROJECT_DOC_MAX_BYTES)).unwrap();

        let entries = collect_entries_with_globals(repo, vec![global]);
        assert!(
            entries.iter().any(|e| e.text == "PROJECT RULES"),
            "project memory must be retained even with a huge global file"
        );
    }

    #[test]
    fn apply_to_system_prompt_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        std::fs::write(tmp.path().join("AGENTS.md"), "RULES").unwrap();

        let mut prompt = "base".to_string();
        apply_to_system_prompt_with_globals(&mut prompt, tmp.path(), vec![]);
        let len = prompt.len();
        apply_to_system_prompt_with_globals(&mut prompt, tmp.path(), vec![]);
        assert_eq!(prompt.len(), len);
        assert_eq!(prompt.matches(MEMORY_BLOCK_BEGIN).count(), 1);
    }
}
