//! The `memory` tool: agent-writable, persistent project memory (a "memdir").
//!
//! Mirrors Anthropic's memory tool / Claude Code's memdir: a single builtin
//! with a `command` discriminator (`view`/`create`/`str_replace`/`insert`/
//! `delete`/`rename`) over a flat directory of Markdown notes the model owns.
//! Storage is project-scoped at `<config_dir>/projects/<key>/memory/`, and the
//! `MEMORY.md` index is re-injected into the system prompt each session (see
//! [`apply_store_to_prompt`]) so notes survive across runs.
//!
//! Safety:
//! - Files live in a flat namespace; names are validated to `[A-Za-z0-9._-]+.md`
//!   with no `/`, `\`, `..`, or leading dot, so there is no path to traverse out
//!   of the store. A defense-in-depth `parent == root` check backs that up.
//! - Writes reuse the same atomic temp-then-rename helper as the file tools.
//! - Writes are refused in non-interactive (headless) runs: because memory is
//!   replayed into later sessions, an unattended prompt-injected write would be
//!   a durable injection vector. `view` stays available.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

/// Per-file write ceiling — keeps one paste from bloating the store.
const FILE_MAX_BYTES: usize = 256 * 1024;

mod inject;
pub use inject::apply_store_to_prompt;

pub struct Memory;

#[derive(Deserialize)]
struct MemoryArgs {
    command: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default, alias = "oldPath")]
    old_path: Option<String>,
    #[serde(default, alias = "newPath")]
    new_path: Option<String>,
    #[serde(default, alias = "fileText", alias = "text", alias = "content")]
    file_text: Option<String>,
    #[serde(default, alias = "oldText", alias = "old_str", alias = "oldStr")]
    old_text: Option<String>,
    #[serde(
        default,
        alias = "newText",
        alias = "new_str",
        alias = "newStr",
        alias = "insert_text",
        alias = "insertText"
    )]
    new_text: Option<String>,
    #[serde(default, alias = "insertLine")]
    insert_line: Option<i64>,
    #[serde(default, alias = "viewRange")]
    view_range: Option<[i64; 2]>,
}

#[async_trait]
impl BuiltinTool for Memory {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn is_read_only(&self) -> bool {
        // It writes; keep it side-effecting so plan mode blocks it. Auto-approval
        // comes from the ALWAYS_AUTO_TOOLS allowlist, not from a read-only claim.
        false
    }

    fn description(&self) -> &'static str {
        "Persistent, project-scoped memory you own across sessions. Store durable facts so a future session starts informed: the user's preferences and goals, hard-won architecture notes, decisions, and the state of ongoing work.\n\
\n\
How it works:\n\
- Files are flat Markdown notes in a private per-project store (NOT in the repo). Keep a `MEMORY.md` index listing each note and its purpose — that index is the only thing auto-loaded into your context each session; individual notes are read on demand with `view`.\n\
- Paths are bare filenames (e.g. `decisions.md`), no directories, no leading slash.\n\
\n\
When to use:\n\
- At the end of meaningful work, record what a future session would need to know (and update `MEMORY.md`).\n\
- At the start of a task, `view MEMORY.md` to recall context, then `view` the relevant note.\n\
\n\
When NOT to use:\n\
- For things already in the repo (code, git history, CLAUDE.md/AGENTS.md) — don't duplicate them.\n\
- For scratch state that only matters this turn.\n\
\n\
Commands (set `command`):\n\
- `view` {path?, view_range?} — read a note, or list the store when `path` is omitted. `view_range` is [start,end], 0-based inclusive.\n\
- `create` {path, file_text} — create a new note. Fails if it already exists (use `str_replace`).\n\
- `str_replace` {path, old_text, new_text} — replace one unique occurrence.\n\
- `insert` {path, insert_line, new_text} — insert a line at a 0-based position.\n\
- `delete` {path} — remove a note.\n\
- `rename` {old_path, new_path} — rename/move a note.\n\
\n\
Writes are disabled in unattended headless runs (pass `--dangerously-skip-permissions` to allow them); `view` always works."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["view", "create", "str_replace", "insert", "delete", "rename"],
                    "description": "Operation to perform."
                },
                "path": {
                    "type": ["string", "null"],
                    "description": "Bare note filename (e.g. \"notes.md\"). Used by view/create/str_replace/insert/delete."
                },
                "old_path": {"type": ["string", "null"], "description": "Source filename for rename."},
                "new_path": {"type": ["string", "null"], "description": "Destination filename for rename."},
                "file_text": {"type": ["string", "null"], "description": "Full contents for create."},
                "old_text": {"type": ["string", "null"], "description": "Text to find for str_replace (must occur exactly once)."},
                "new_text": {"type": ["string", "null"], "description": "Replacement text for str_replace, or the line to add for insert."},
                "insert_line": {"type": ["integer", "null"], "description": "0-based line index to insert before (clamped to file length)."},
                "view_range": {
                    "type": ["array", "null"],
                    "items": {"type": "integer"},
                    "minItems": 2,
                    "maxItems": 2,
                    "description": "Optional [start, end] line range for view (0-based, inclusive)."
                }
            },
            "required": ["command", "path", "old_path", "new_path", "file_text", "old_text", "new_text", "insert_line", "view_range"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: MemoryArgs = super::parse_args("memory", args)?;
        let command = a.command.trim().to_ascii_lowercase();

        // Memory is replayed into later sessions, so an unattended prompt-injected
        // write is a durable injection vector. Block writes in an unattended
        // headless run (the same `non_interactive && require_approval` gate the
        // other side-effecting tools use); `view` is always allowed, and an
        // operator can still opt in with `--dangerously-skip-permissions`.
        if command != "view" && ctx.non_interactive && ctx.require_approval {
            bail!(
                "memory writes are disabled in unattended headless runs because memory is replayed into later sessions. Only `view` is available; pass --dangerously-skip-permissions to allow writes, or run opencli interactively."
            );
        }

        let root = store_dir(&ctx.cwd);
        match command.as_str() {
            "view" => cmd_view(&root, a.path.as_deref(), a.view_range),
            "create" => cmd_create(&root, a.path.as_deref(), a.file_text.as_deref()).await,
            "str_replace" => {
                cmd_str_replace(&root, a.path.as_deref(), a.old_text.as_deref(), a.new_text.as_deref())
                    .await
            }
            "insert" => cmd_insert(&root, a.path.as_deref(), a.insert_line, a.new_text.as_deref()).await,
            "delete" => cmd_delete(&root, a.path.as_deref()).await,
            "rename" => cmd_rename(&root, a.old_path.as_deref(), a.new_path.as_deref()).await,
            other => bail!("unknown memory command {other:?}. Use view, create, str_replace, insert, delete, or rename."),
        }
    }
}

// ---- storage location -------------------------------------------------------

/// The per-project memory directory: `<config_dir>/projects/<key>/memory/`.
pub fn store_dir(cwd: &Path) -> PathBuf {
    crate::config::config_dir()
        .join("projects")
        .join(project_key(cwd))
        .join("memory")
}

/// Stable, filesystem-safe key for a project: the git root (or `cwd` when not a
/// repo) with every non-`[A-Za-z0-9._-]` byte replaced by `-`. Mirrors the
/// `-home-user-proj` convention Claude Code uses for its per-project dirs.
fn project_key(cwd: &Path) -> String {
    let base = crate::memory::git_root_from(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut key: String = base
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Guard against a pathologically long path exceeding filesystem name limits;
    // keep the tail (most specific segments).
    if key.chars().count() > 180 {
        key = key
            .chars()
            .rev()
            .take(180)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
    }
    if key.is_empty() {
        key.push_str("default");
    }
    key
}

// ---- name validation & path resolution -------------------------------------

/// Validate a model-supplied memory filename and resolve it under `root`.
/// Flat namespace only: rejects directories, `..`, absolute paths, and a
/// leading dot; auto-appends `.md` when no extension is given. Tolerates the
/// `/memories/` root prefix that memory-tool-trained models sometimes send.
fn resolve_file(root: &Path, raw: &str) -> Result<PathBuf> {
    let name = normalize_name(raw)?;
    // Canonicalize the store root when it exists so a symlinked ancestor can't
    // redirect the store; before the first write the dir may not exist yet, so
    // fall back to the raw root (nothing is there to escape to).
    let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let path = base.join(&name);
    // Defense in depth: a valid flat name lands directly under the root.
    if path.parent() != Some(base.as_path()) {
        bail!("invalid memory path: {raw:?}");
    }
    // Refuse to read or write through a symlinked note: std::fs follows
    // symlinks, so a `notes.md -> /etc/passwd` planted in the store would
    // otherwise escape the sandbox. A regular file or a missing path is fine.
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() {
            bail!("memory note {raw:?} is a symlink; refusing to follow it out of the store.");
        }
    }
    Ok(path)
}

fn normalize_name(raw: &str) -> Result<String> {
    let mut name = raw.trim();
    // A model trained on Anthropic's memory tool may prefix its conceptual root.
    name = name.trim_start_matches('/');
    name = name.strip_prefix("memories/").unwrap_or(name);
    name = name.trim();

    if name.is_empty() {
        bail!("memory path is empty — pass a filename like \"notes.md\".");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("memory is a flat store: no directories. Use a bare filename like \"notes.md\", not {raw:?}.");
    }
    if name.contains("..") {
        bail!("memory filename must not contain \"..\": {raw:?}");
    }
    if name.starts_with('.') {
        bail!("memory filename must not start with '.': {raw:?}");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("memory filename may only contain letters, digits, '.', '_', '-': {raw:?}");
    }
    let name = if name.contains('.') {
        if !name.ends_with(".md") {
            bail!("memory files must be Markdown (.md): {raw:?}");
        }
        name.to_string()
    } else {
        format!("{name}.md")
    };
    Ok(name)
}

// ---- commands ---------------------------------------------------------------

fn cmd_view(root: &Path, path: Option<&str>, view_range: Option<[i64; 2]>) -> Result<String> {
    let listing_requested = path
        .map(|p| p.trim())
        .is_none_or(|p| p.is_empty() || p == ".");
    if listing_requested {
        return Ok(list_store(root));
    }
    let path = path.unwrap();
    let file = resolve_file(root, path)?;
    let text = std::fs::read_to_string(&file)
        .map_err(|_| anyhow!("memory note {path:?} not found. Use `view` with no path to list the store, or `create` to add it."))?;

    let lines: Vec<&str> = text.lines().collect();
    let (body, header) = if let Some([start, end]) = view_range {
        let start = start.max(0) as usize;
        let end = (end.max(0) as usize).min(lines.len().saturating_sub(1));
        if start >= lines.len() {
            bail!(
                "view_range start {start} is past the end of {path:?} ({} lines)",
                lines.len()
            );
        }
        (
            lines[start..=end].join("\n"),
            format!("(memory: {path}, lines {start}-{end} of {})", lines.len()),
        )
    } else {
        (
            text.clone(),
            format!("(memory: {path}, {} lines)", lines.len()),
        )
    };
    Ok(format!("{header}\n\n{body}"))
}

async fn cmd_create(root: &Path, path: Option<&str>, file_text: Option<&str>) -> Result<String> {
    let path = path.ok_or_else(|| anyhow!("create requires `path`"))?;
    let body = file_text.ok_or_else(|| anyhow!("create requires `file_text`"))?;
    check_size(body)?;
    let file = resolve_file(root, path)?;
    if file.exists() {
        bail!("memory note {path:?} already exists. Use `str_replace` to edit it or `delete` then `create`.");
    }
    write_atomic(&file, body).await?;
    Ok(format!(
        "Created memory note {} ({} bytes).",
        file.file_name().unwrap().to_string_lossy(),
        body.len()
    ))
}

async fn cmd_str_replace(
    root: &Path,
    path: Option<&str>,
    old_text: Option<&str>,
    new_text: Option<&str>,
) -> Result<String> {
    let path = path.ok_or_else(|| anyhow!("str_replace requires `path`"))?;
    let old = old_text
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("str_replace requires a non-empty `old_text`"))?;
    let new = new_text.unwrap_or("");
    let file = resolve_file(root, path)?;
    let original = std::fs::read_to_string(&file)
        .map_err(|_| anyhow!("memory note {path:?} not found — `view` it or `create` it first."))?;
    let count = original.matches(old).count();
    if count == 0 {
        bail!("old_text not found in {path:?}. `view` the note to see its current contents.");
    }
    if count > 1 {
        bail!("old_text occurs {count} times in {path:?}; include more surrounding context so it matches exactly once.");
    }
    let updated = original.replacen(old, new, 1);
    check_size(&updated)?;
    write_atomic(&file, &updated).await?;
    Ok(format!("Updated memory note {path}."))
}

async fn cmd_insert(
    root: &Path,
    path: Option<&str>,
    insert_line: Option<i64>,
    new_text: Option<&str>,
) -> Result<String> {
    let path = path.ok_or_else(|| anyhow!("insert requires `path`"))?;
    let line = insert_line.ok_or_else(|| anyhow!("insert requires `insert_line`"))?;
    let text = new_text.ok_or_else(|| anyhow!("insert requires `new_text`"))?;
    let file = resolve_file(root, path)?;
    let original = std::fs::read_to_string(&file)
        .map_err(|_| anyhow!("memory note {path:?} not found — `create` it first."))?;
    let mut lines: Vec<String> = original.lines().map(|s| s.to_string()).collect();
    let idx = (line.max(0) as usize).min(lines.len());
    for (i, piece) in text.split('\n').enumerate() {
        lines.insert(idx + i, piece.to_string());
    }
    let mut updated = lines.join("\n");
    if original.ends_with('\n') {
        updated.push('\n');
    }
    check_size(&updated)?;
    write_atomic(&file, &updated).await?;
    Ok(format!("Inserted into memory note {path} at line {idx}."))
}

async fn cmd_delete(root: &Path, path: Option<&str>) -> Result<String> {
    let path = path.ok_or_else(|| anyhow!("delete requires `path`"))?;
    let file = resolve_file(root, path)?;
    std::fs::remove_file(&file).map_err(|_| anyhow!("memory note {path:?} not found."))?;
    Ok(format!("Deleted memory note {path}."))
}

async fn cmd_rename(root: &Path, old_path: Option<&str>, new_path: Option<&str>) -> Result<String> {
    let old = old_path.ok_or_else(|| anyhow!("rename requires `old_path`"))?;
    let new = new_path.ok_or_else(|| anyhow!("rename requires `new_path`"))?;
    let from = resolve_file(root, old)?;
    let to = resolve_file(root, new)?;
    if !from.exists() {
        bail!("memory note {old:?} not found.");
    }
    if to.exists() {
        bail!("cannot rename to {new:?}: that note already exists.");
    }
    std::fs::rename(&from, &to).with_context(|| format!("rename memory {old} -> {new}"))?;
    Ok(format!("Renamed memory note {old} -> {new}."))
}

// ---- helpers ----------------------------------------------------------------

fn check_size(body: &str) -> Result<()> {
    if body.len() > FILE_MAX_BYTES {
        bail!(
            "memory note is {} bytes; the per-note limit is {} KiB. Keep notes concise or split them.",
            body.len(),
            FILE_MAX_BYTES / 1024
        );
    }
    Ok(())
}

async fn write_atomic(file: &Path, body: &str) -> Result<()> {
    if let Some(parent) = file.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create memory dir {}", parent.display()))?;
    }
    let tmp = file.with_extension(format!("mem-{}.tmp", tmp_suffix()));
    crate::tools::fs::atomic_write_preserving_permissions(file, &tmp, body.as_bytes()).await
}

fn tmp_suffix() -> String {
    use rand::RngCore;
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Human-readable listing of the store for `view` with no path.
fn list_store(root: &Path) -> String {
    let files = md_files(root);
    if files.is_empty() {
        return "Memory store is empty. Use `create` to add a note (and keep a `MEMORY.md` index)."
            .to_string();
    }
    let mut s = format!("Memory store ({} files):\n", files.len());
    for (name, size) in files {
        s.push_str(&format!("- {name} ({size} bytes)\n"));
    }
    s
}

/// `(name, size)` for every `.md` file directly in the store, sorted by name.
pub(super) fn md_files(root: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push((name, size));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests;
