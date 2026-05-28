use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

fn rand_suffix() -> String {
    use rand::RngCore;
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub struct ReadFile;
pub struct WriteFile;
pub struct EditFile;
pub struct ListDir;

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl BuiltinTool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a text file from the working directory. Returns the file contents with line numbers in the format `<lineno>\\t<content>` per line. Line numbers start at 1 and are right-padded so columns stay aligned.\n\
\n\
When to use:\n\
- ALWAYS call this before `edit_file` or `multi_edit` — those tools need the exact existing bytes, and guessing wastes a turn.\n\
- When you need to understand what a file does, cite a specific line, or verify the result of an edit.\n\
- Prefer reading the whole file when feasible; reach for `offset` + `limit` only on truly large files.\n\
\n\
When NOT to use:\n\
- Don't read a directory — use `list_dir` or `glob`.\n\
- Don't read to search across many files — use `grep`.\n\
- Don't shell out to `cat` instead of this tool; this tool returns structured output with line numbers.\n\
\n\
Common mistakes:\n\
- Skipping the read and going straight to `edit_file` — your `old_string` will not match.\n\
- Re-reading a file you just read this turn — the contents are already in context.\n\
\n\
Parameters:\n\
- `path`: Relative path inside the working directory. Absolute paths and `..` traversal are rejected.\n\
- `offset`: Zero-indexed line to start reading from, or `null` to start at the top.\n\
- `limit`: Maximum number of lines to return, or `null` to use the default cap.\n\
\n\
Output rules:\n\
- Default cap is 2000 lines per call when `limit` is null; the response includes a truncation notice telling you how to read the next slice with `offset` + `limit`.\n\
- Lines longer than 2000 characters are truncated and marked `… [line truncated]` so a minified file can't blow out your context window.\n\
- An empty file returns a `<system-reminder>` warning instead of a blank string, so you don't assume the read failed.\n\
\n\
Constraints: files larger than 5 MB must be read with an explicit `limit`. Binary files are not supported by this tool — use `grep` or `run_shell` (e.g. `file`, `hexdump`) for non-text artefacts."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative path inside the working directory."},
                "offset": {"type": ["integer", "null"], "description": "Zero-indexed starting line; null starts at the top."},
                "limit": {"type": ["integer", "null"], "description": "Maximum number of lines to return; null reads to the end."}
            },
            "required": ["path", "offset", "limit"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: ReadArgs = super::parse_args("read_file", args)?;
        let path = resolve(&ctx.cwd, &a.path)?;
        // Bound the read so the LLM can't request /dev/zero or a multi-GB log
        // and OOM the process.
        const MAX_BYTES: u64 = 5_000_000;
        // Default lines-per-call when caller does not pass `limit`. Matches
        // Claude Code's Read tool — keeps a single read from flooding the
        // context window with a large file.
        const DEFAULT_LINE_LIMIT: usize = 2000;
        // Per-line truncation so a minified bundle (one giant line) can't
        // blow out the context. Mirrors Claude Code's 2000-char-per-line cap.
        const MAX_LINE_CHARS: usize = 2000;

        let meta = tokio::fs::metadata(&path)
            .await
            .with_context(|| format!("stat {}", path.display()))?;
        if meta.len() > MAX_BYTES && a.limit.is_none() {
            return Err(anyhow!(
                "file is too large ({} bytes > {} byte cap); pass `limit` to read a slice",
                meta.len(),
                MAX_BYTES
            ));
        }
        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        if text.is_empty() {
            return Ok(format!(
                "<system-reminder>The file `{}` exists but is empty.</system-reminder>\n",
                a.path
            ));
        }
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let start = a.offset.unwrap_or(0).min(total);
        // Caller's explicit limit wins; otherwise apply the default cap.
        let effective_limit = a.limit.unwrap_or(DEFAULT_LINE_LIMIT);
        let end = (start + effective_limit).min(total);
        let mut out = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            // Truncate by characters (not bytes) so we don't slice mid-codepoint.
            let printed: String = if line.chars().count() > MAX_LINE_CHARS {
                let head: String = line.chars().take(MAX_LINE_CHARS).collect();
                format!("{head}… [line truncated]")
            } else {
                (*line).to_string()
            };
            out.push_str(&format!("{:>6}\t{}\n", start + i + 1, printed));
        }
        // Tell the model how to grab the next slice when we hit the cap.
        if end < total {
            let remaining = total - end;
            out.push_str(&format!(
                "<system-reminder>Showing lines {}-{} of {}. {} more line(s) remain — call read_file again with offset={} and an explicit limit to continue.</system-reminder>\n",
                start + 1,
                end,
                total,
                remaining,
                end
            ));
        }
        Ok(out)
    }
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[async_trait]
impl BuiltinTool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Create a new file, or completely overwrite an existing one, with the supplied content. Parent directories are created automatically.\n\
\n\
When to use:\n\
- Creating brand-new files.\n\
- Truly replacing the entire contents of a file (e.g. regenerating an autogenerated artifact).\n\
\n\
When NOT to use:\n\
- DO NOT use this to modify an existing file. Use `edit_file` (for one change) or `multi_edit` (for several) so you don't silently destroy unrelated code.\n\
- Don't use this to append; read + edit_file, or read + write_file with the combined content.\n\
\n\
Common mistakes:\n\
- Treating this as a faster alternative to `edit_file` — it isn't; it's a complete overwrite.\n\
- Leaving out trailing newlines or shebangs that the original file had.\n\
\n\
Parameters:\n\
- `path`: Relative path inside the working directory.\n\
- `content`: Exact bytes to write."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative path inside the working directory."},
                "content": {"type": "string", "description": "Exact bytes to write."}
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }
    async fn compute_preview(&self, args: &Value, ctx: &ToolContext) -> Option<String> {
        let a: WriteArgs = serde_json::from_value(args.clone()).ok()?;
        let path = resolve(&ctx.cwd, &a.path).ok()?;
        let existing = tokio::fs::metadata(&path).await.ok().map(|m| m.len());
        Some(match existing {
            Some(n) => format!(
                "Overwrite {} ({n} bytes -> {} bytes)",
                a.path,
                a.content.len()
            ),
            None => format!("Create new file {} ({} bytes)", a.path, a.content.len()),
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: WriteArgs = super::parse_args("write_file", args)?;
        let path = resolve(&ctx.cwd, &a.path)?;
        let original = tokio::fs::read_to_string(&path).await.ok();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(&path, a.content.as_bytes())
            .await
            .with_context(|| format!("write {}", path.display()))?;
        let post_edit_mtime = snapshot_mtime(&path);
        ctx.session.lock().await.push_undo_entry(super::UndoEntry {
            path: path.clone(),
            original_content: original,
            post_edit_mtime,
        });
        Ok(format!(
            "Wrote {} bytes to {}",
            a.content.len(),
            path.display()
        ))
    }
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl BuiltinTool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }
    fn description(&self) -> &'static str {
        "Replace `old_string` with `new_string` in an existing file. By default `old_string` must appear exactly once; pass `replace_all: true` to substitute every occurrence. The write is atomic (temp file + rename).\n\
\n\
When to use:\n\
- Surgical changes to existing files: bug fixes, renames within a small scope, replacing a block of code with another.\n\
- One change per call. For multiple changes to the same file, prefer `multi_edit` so a partial failure rolls everything back.\n\
\n\
When NOT to use:\n\
- Creating new files — use `write_file`.\n\
- Wholesale rewrites — use `write_file`.\n\
- Project-wide renames touching many files — use `grep` to find them, then `edit_file`/`multi_edit` per file.\n\
\n\
How to make `old_string` unique:\n\
- Include the whole line(s) you're changing, not just a substring.\n\
- Include 1–3 neighboring lines (above or below) for additional context.\n\
- If the snippet truly repeats (e.g. multiple identical imports), set `replace_all: true`.\n\
\n\
Common mistakes:\n\
- Calling this without first calling `read_file` — your `old_string` will be a guess and won't match.\n\
- `old_string` that's a substring inside a longer line — whitespace, trailing punctuation, or tabs vs spaces will silently mismatch.\n\
- Trying to match across many lines without preserving exact indentation/newlines.\n\
\n\
Parameters:\n\
- `path`: Relative path inside the working directory; file must exist.\n\
- `old_string`: Exact text to find. Whitespace matters.\n\
- `new_string`: Text to substitute in.\n\
- `replace_all`: When true, replace every occurrence; when false, fail if `old_string` is not unique."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative path inside the working directory; file must exist."},
                "old_string": {"type": "string", "description": "Exact text to find. Whitespace matters."},
                "new_string": {"type": "string", "description": "Text to substitute in."},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence when true; require uniqueness when false."}
            },
            "required": ["path", "old_string", "new_string", "replace_all"],
            "additionalProperties": false
        })
    }
    async fn compute_preview(&self, args: &Value, _ctx: &ToolContext) -> Option<String> {
        let a: EditArgs = serde_json::from_value(args.clone()).ok()?;
        let trunc = |s: &str| -> String {
            let one = s.replace('\n', "/");
            if one.chars().count() > 60 {
                format!("{}...", one.chars().take(60).collect::<String>())
            } else {
                one
            }
        };
        Some(format!(
            "Edit {} - `{}` -> `{}`{}",
            a.path,
            trunc(&a.old_string),
            trunc(&a.new_string),
            if a.replace_all { " (all)" } else { "" }
        ))
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: EditArgs = super::parse_args("edit_file", args)?;
        let path = resolve(&ctx.cwd, &a.path)?;
        let original = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let count = original.matches(&a.old_string).count();
        if count == 0 {
            return Err(anyhow!("old_string not found in {}", path.display()));
        }
        if count > 1 && !a.replace_all {
            return Err(anyhow!(
                "old_string occurs {count} times; set replace_all=true or supply more surrounding context"
            ));
        }
        let new_content = if a.replace_all {
            original.replace(&a.old_string, &a.new_string)
        } else {
            original.replacen(&a.old_string, &a.new_string, 1)
        };
        // Atomic write: stage in a sibling tempfile, then rename. Prevents the
        // "killed mid-write -> file is now empty or half-written" failure mode.
        let tmp = path.with_extension(format!("edit-{}.tmp", rand_suffix()));
        tokio::fs::write(&tmp, new_content.as_bytes())
            .await
            .with_context(|| format!("write temp {}", tmp.display()))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        let post_edit_mtime = snapshot_mtime(&path);
        ctx.session.lock().await.push_undo_entry(super::UndoEntry {
            path: path.clone(),
            original_content: Some(original),
            post_edit_mtime,
        });
        Ok(format!(
            "Replaced {} occurrence(s) in {}",
            count,
            path.display()
        ))
    }
}

#[derive(Deserialize)]
struct ListArgs {
    path: String,
}

#[async_trait]
impl BuiltinTool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }
    fn description(&self) -> &'static str {
        "List the immediate entries of a directory. Directories are suffixed with `/` so you can distinguish them from files at a glance. Output is sorted lexicographically.\n\
\n\
When to use:\n\
- A snapshot of one directory's direct children — \"what's at the repo root?\", \"what's in src/?\".\n\
\n\
When NOT to use:\n\
- Recursive discovery — use `glob` with `**/*` patterns; it's faster and respects `.gitignore`.\n\
- Searching for files matching a pattern — use `glob` directly.\n\
- Searching file contents — use `grep`.\n\
\n\
Parameters:\n\
- `path`: Relative directory path inside the working directory."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative directory path inside the working directory."}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: ListArgs = super::parse_args("list_dir", args)?;
        let path = resolve(&ctx.cwd, &a.path)?;
        let mut entries = tokio::fs::read_dir(&path).await?;
        let mut items: Vec<String> = Vec::new();
        while let Some(e) = entries.next_entry().await? {
            let ft = e.file_type().await?;
            let name = e.file_name().to_string_lossy().to_string();
            items.push(if ft.is_dir() {
                format!("{name}/")
            } else {
                name
            });
        }
        items.sort();
        Ok(items.join("\n"))
    }
}

pub struct MultiEdit;

#[derive(Deserialize)]
struct MultiEditArgs {
    path: String,
    edits: Vec<EditOp>,
}

#[derive(Deserialize)]
struct EditOp {
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl BuiltinTool for MultiEdit {
    fn name(&self) -> &'static str {
        "multi_edit"
    }
    fn description(&self) -> &'static str {
        "Apply a sequence of replacements to a single file atomically. Each edit is `{old_string, new_string, replace_all}` and runs in order against the output of the previous edit, so edit #2 sees the file as if edit #1 already happened. The file is read once, transformed entirely in memory, and written back via a temp-file + rename — a crash mid-write leaves the original intact, and a failure on edit #N reverts every prior edit in the same call.\n\
\n\
When to use:\n\
- Several related changes to the same file in one shot — e.g. rename a symbol at its declaration and every call site in the file, or restructure a function and update its docs.\n\
- A refactor where the edits depend on each other (the second `old_string` only exists after the first edit applied).\n\
- Any time you'd otherwise issue 2+ `edit_file` calls against the same path — `multi_edit` is one tool call, one atomic write, one rollback boundary.\n\
\n\
When NOT to use:\n\
- One change to a file — `edit_file` is simpler.\n\
- Changes across multiple files — call `multi_edit` once per file.\n\
- Creating a new file — use `write_file`.\n\
- Wholesale rewrites — use `write_file`.\n\
\n\
How edits chain:\n\
- Edits apply in array order; each one sees the cumulative result of the previous edits, not the original file.\n\
- If edit #2's `old_string` overlaps text edit #1 just rewrote, write edit #2 to match the rewritten text.\n\
- If an edit fails (no match, ambiguous match) the whole call aborts and the file on disk is unchanged.\n\
\n\
How to make each `old_string` unique:\n\
- Include the whole line(s) you're changing, not just a substring.\n\
- Include 1–3 neighboring lines (above or below) for additional context.\n\
- For repeated tokens (imports, identical method calls), set `replace_all: true`.\n\
\n\
Common mistakes:\n\
- Skipping `read_file` first — `old_string` will be a guess and edit #1 won't match.\n\
- Writing edit #2's `old_string` against the original file when edit #1 already changed those lines.\n\
- Putting independent files into one call — `path` is a single string, this tool does not span files.\n\
- Reordering edits without re-reading: changing the order changes the result when edits touch overlapping regions.\n\
\n\
Parameters:\n\
- `path`: Relative path inside the working directory; file must exist.\n\
- `edits`: Ordered list of replacements; at least one entry. Each entry must include `old_string`, `new_string`, and `replace_all` (boolean)."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative path inside the working directory; file must exist."},
                "edits": {
                    "type": "array",
                    "description": "Ordered list of replacements applied in sequence.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": {"type": "string", "description": "Exact text to find."},
                            "new_string": {"type": "string", "description": "Text to substitute in."},
                            "replace_all": {"type": "boolean", "description": "Replace every occurrence when true; require uniqueness when false."}
                        },
                        "required": ["old_string", "new_string", "replace_all"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["path", "edits"],
            "additionalProperties": false
        })
    }
    async fn compute_preview(&self, args: &Value, _ctx: &ToolContext) -> Option<String> {
        let a: MultiEditArgs = serde_json::from_value(args.clone()).ok()?;
        Some(format!("Multi-edit {} ({} edit(s))", a.path, a.edits.len()))
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: MultiEditArgs = super::parse_args("multi_edit", args)?;
        if a.edits.is_empty() {
            return Err(anyhow!("`edits` must contain at least one entry"));
        }
        let path = resolve(&ctx.cwd, &a.path)?;
        let mut content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let original_for_undo = content.clone();
        let mut total_replacements = 0usize;
        for (i, edit) in a.edits.iter().enumerate() {
            let count = content.matches(&edit.old_string).count();
            if count == 0 {
                return Err(anyhow!(
                    "edit #{}: old_string not found in {}",
                    i + 1,
                    path.display()
                ));
            }
            if count > 1 && !edit.replace_all {
                return Err(anyhow!(
                    "edit #{}: old_string occurs {count} times; set replace_all=true or supply more surrounding context",
                    i + 1
                ));
            }
            content = if edit.replace_all {
                total_replacements += count;
                content.replace(&edit.old_string, &edit.new_string)
            } else {
                total_replacements += 1;
                content.replacen(&edit.old_string, &edit.new_string, 1)
            };
        }
        let tmp = path.with_extension(format!("medit-{}.tmp", rand_suffix()));
        tokio::fs::write(&tmp, content.as_bytes())
            .await
            .with_context(|| format!("write temp {}", tmp.display()))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        let post_edit_mtime = snapshot_mtime(&path);
        ctx.session.lock().await.push_undo_entry(super::UndoEntry {
            path: path.clone(),
            original_content: Some(original_for_undo),
            post_edit_mtime,
        });
        Ok(format!(
            "Applied {} edit(s) ({} total replacement(s)) to {}",
            a.edits.len(),
            total_replacements,
            path.display()
        ))
    }
}

pub struct UndoLastEdit;

#[async_trait]
impl BuiltinTool for UndoLastEdit {
    fn name(&self) -> &'static str {
        "undo_last_edit"
    }
    fn description(&self) -> &'static str {
        "Roll back the most recent file edit. If that write created a new file, undo removes it.\n\nParameters: none."
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false })
    }
    async fn execute(&self, _args: Value, ctx: &ToolContext) -> Result<String> {
        let entry = {
            let mut session = ctx.session.lock().await;
            session.undo_stack.pop_back()
        };
        let entry = entry.ok_or_else(|| anyhow!("no edits to undo"))?;
        // TOCTOU guard: refuse to restore if the file has been touched since
        // the edit. Without this, an `undo_last_edit` after the user manually
        // edits the file (in their editor, another shell, etc.) would
        // silently nuke those changes.
        if let Some(expected) = entry.post_edit_mtime {
            let current = snapshot_mtime(&entry.path);
            if current != Some(expected) {
                return Err(anyhow!(
                    "refusing to undo {}: file has been modified since the edit; restore manually if intended",
                    entry.path.display()
                ));
            }
        }
        match entry.original_content {
            Some(content) => {
                tokio::fs::write(&entry.path, content)
                    .await
                    .with_context(|| format!("restore {}", entry.path.display()))?;
                Ok(format!("Restored {}", entry.path.display()))
            }
            None => {
                tokio::fs::remove_file(&entry.path)
                    .await
                    .with_context(|| format!("remove {}", entry.path.display()))?;
                Ok(format!(
                    "Removed (was a new file): {}",
                    entry.path.display()
                ))
            }
        }
    }
}

/// mtime helper used by every edit/write tool to snapshot the file state
/// immediately after a successful write, and by `UndoLastEdit` to detect
/// post-edit modifications before restoring.
fn snapshot_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Resolve a model-supplied path against the sandbox `cwd`. Rejects absolute
/// paths and any relative path that escapes `cwd` after lexically normalising
/// `..` components. Without this guard the LLM could read `/etc/shadow`,
/// write to `~/.ssh/authorized_keys`, or otherwise escape the working tree.
fn resolve(cwd: &std::path::Path, p: &str) -> Result<std::path::PathBuf> {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        return Err(anyhow!(
            "absolute paths are not allowed inside the sandbox: {}",
            path.display()
        ));
    }
    let mut normalized = std::path::PathBuf::new();
    for comp in path.components() {
        use std::path::Component;
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(anyhow!("path escapes the sandbox: {}", path.display()));
                }
            }
            Component::Normal(s) => normalized.push(s),
            Component::Prefix(_) | Component::RootDir => {
                return Err(anyhow!("invalid path component: {}", path.display()));
            }
        }
    }
    Ok(cwd.join(normalized))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ApprovalMode, SessionState};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn ctx(cwd: std::path::PathBuf) -> ToolContext {
        ToolContext {
            cwd,
            approval: ApprovalMode::Auto,
            session: Arc::new(Mutex::new(SessionState::default())),
        }
    }

    fn read_args(path: &str, offset: Option<usize>, limit: Option<usize>) -> Value {
        json!({"path": path, "offset": offset, "limit": limit})
    }

    #[tokio::test]
    async fn read_file_empty_returns_system_reminder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();
        let out = ReadFile
            .execute(
                read_args("empty.txt", None, None),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(out.contains("exists but is empty"), "got: {out}");
        assert!(out.contains("<system-reminder>"), "got: {out}");
    }

    #[tokio::test]
    async fn read_file_caps_at_default_limit_and_emits_continuation_notice() {
        let dir = tempfile::tempdir().unwrap();
        // 2500 lines: hits the 2000-line default cap, leaves 500 remaining.
        let content: String = (1..=2500).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), &content).unwrap();
        let out = ReadFile
            .execute(
                read_args("big.txt", None, None),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        // First and last printed lines fall inside [1, 2000].
        assert!(out.contains("\tline 1\n"), "missing first line");
        assert!(out.contains("\tline 2000\n"), "missing 2000th line");
        assert!(
            !out.contains("\tline 2001\n"),
            "should have stopped at default cap"
        );
        assert!(
            out.contains("500 more line"),
            "missing continuation hint: {out}"
        );
        assert!(out.contains("offset=2000"), "missing offset hint: {out}");
    }

    #[tokio::test]
    async fn read_file_truncates_long_lines() {
        let dir = tempfile::tempdir().unwrap();
        // 3000 'a' characters on one line → must be truncated to 2000 + marker.
        let huge_line: String = "a".repeat(3000);
        std::fs::write(dir.path().join("min.js"), &huge_line).unwrap();
        let out = ReadFile
            .execute(
                read_args("min.js", None, None),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(
            out.contains("[line truncated]"),
            "missing truncation marker: {out}"
        );
        // The full 3000-char line must NOT have been emitted verbatim.
        assert!(
            !out.contains(&"a".repeat(3000)),
            "long line was not truncated"
        );
    }

    #[tokio::test]
    async fn read_file_respects_explicit_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=10).map(|i| format!("L{i}\n")).collect();
        std::fs::write(dir.path().join("small.txt"), &content).unwrap();
        let out = ReadFile
            .execute(
                read_args("small.txt", Some(3), Some(2)),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        // Lines 4 and 5 (offset is zero-indexed in the slice, displayed 1-indexed).
        assert!(out.contains("\tL4\n"), "got: {out}");
        assert!(out.contains("\tL5\n"), "got: {out}");
        assert!(!out.contains("\tL3\n"), "should not include L3");
        assert!(!out.contains("\tL6\n"), "should not include L6");
        // 5 lines after offset 5 remain → continuation notice expected.
        assert!(out.contains("more line"), "missing continuation: {out}");
    }
}
