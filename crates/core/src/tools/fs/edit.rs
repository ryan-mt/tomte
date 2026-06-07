//! The `edit_file` and `multi_edit` tools. Split out of `fs`; logic unchanged.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tools::{BuiltinTool, ToolContext, UndoEntry};

use super::common::{
    atomic_write_preserving_permissions, ensure_not_stale, rand_suffix, resolve, snapshot_meta,
};

pub struct EditFile;

#[derive(Deserialize)]
struct EditArgs {
    #[serde(alias = "file_path", alias = "filePath")]
    path: String,
    #[serde(alias = "oldString", alias = "old_text", alias = "oldText")]
    old_string: String,
    #[serde(alias = "newString", alias = "new_text", alias = "newText")]
    new_string: String,
    #[serde(
        default,
        alias = "replaceAll",
        deserialize_with = "crate::tools::deserialize_bool"
    )]
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
- Calling this without first calling `read_file` — the edit is REFUSED until you read the file this session (so `old_string` matches real bytes, not a guess).\n\
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
                "old_string": {"type": "string", "minLength": 1, "description": "Exact non-empty text to find. Whitespace matters."},
                "new_string": {"type": "string", "description": "Text to substitute in."},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence when true; require uniqueness when false."}
            },
            "required": ["path", "old_string", "new_string", "replace_all"],
            "additionalProperties": false
        })
    }
    async fn compute_preview(&self, args: &Value, _ctx: &ToolContext) -> Option<String> {
        let a: EditArgs = crate::tools::parse_args("edit_file", args.clone()).ok()?;
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
        let a: EditArgs = crate::tools::parse_args("edit_file", args)?;
        if a.old_string.is_empty() {
            return Err(anyhow!("old_string must not be empty"));
        }
        if a.old_string == a.new_string {
            return Err(anyhow!(
                "old_string and new_string are identical; nothing to change"
            ));
        }
        let path = resolve(&ctx.cwd, &a.path)?;
        // Read-before-edit safety: old_string can only be trusted if the model
        // actually read the file this session (write_file/read_file both record
        // it), AND the file hasn't changed on disk since that read. Refuse
        // otherwise rather than edit against a guessed or stale match.
        {
            let session = ctx.session.lock().await;
            if !session.read_files.contains(&path) {
                return Err(anyhow!(
                    "edit_file requires reading {} first so old_string matches the real bytes. Call read_file on it.",
                    path.display()
                ));
            }
            ensure_not_stale(&session, &path, "edit_file")?;
        }
        let original = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let (old_string, new_string) = match_line_endings(&original, &a.old_string, &a.new_string);
        let count = original.matches(&old_string).count();
        if count == 0 {
            return Err(anyhow!("old_string not found in {}", path.display()));
        }
        if count > 1 && !a.replace_all {
            return Err(anyhow!(
                "old_string occurs {count} times; set replace_all=true or supply more surrounding context"
            ));
        }
        let new_content = if a.replace_all {
            original.replace(&old_string, &new_string)
        } else {
            original.replacen(&old_string, &new_string, 1)
        };
        // Atomic write: stage in a sibling tempfile, then rename. Prevents the
        // "killed mid-write -> file is now empty or half-written" failure mode.
        let tmp = path.with_extension(format!("edit-{}.tmp", rand_suffix()));
        atomic_write_preserving_permissions(&path, &tmp, new_content.as_bytes()).await?;
        let (post_edit_mtime, post_edit_size) = snapshot_meta(&path);
        {
            let mut session = ctx.session.lock().await;
            // Refresh the snapshot to the bytes we just wrote so a follow-up
            // edit to the same file isn't flagged as stale by our own change.
            session
                .read_file_meta
                .insert(path.clone(), (post_edit_mtime, post_edit_size));
            session.push_undo_entry(UndoEntry {
                path: path.clone(),
                original_content: Some(original.into_bytes()),
                post_edit_mtime,
                post_edit_size,
            });
        }
        Ok(format!(
            "Replaced {} occurrence(s) in {}",
            count,
            path.display()
        ))
    }
}

pub struct MultiEdit;

#[derive(Deserialize)]
struct MultiEditArgs {
    #[serde(alias = "file_path", alias = "filePath")]
    path: String,
    edits: Vec<EditOp>,
}

#[derive(Deserialize)]
struct EditOp {
    #[serde(alias = "oldString", alias = "old_text", alias = "oldText")]
    old_string: String,
    #[serde(alias = "newString", alias = "new_text", alias = "newText")]
    new_string: String,
    #[serde(
        default,
        alias = "replaceAll",
        deserialize_with = "crate::tools::deserialize_bool"
    )]
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
                            "old_string": {"type": "string", "minLength": 1, "description": "Exact non-empty text to find."},
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
        let a: MultiEditArgs = crate::tools::parse_args("multi_edit", args.clone()).ok()?;
        Some(format!("Multi-edit {} ({} edit(s))", a.path, a.edits.len()))
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: MultiEditArgs = crate::tools::parse_args("multi_edit", args)?;
        if a.edits.is_empty() {
            return Err(anyhow!("`edits` must contain at least one entry"));
        }
        let path = resolve(&ctx.cwd, &a.path)?;
        // Read-before-edit safety (same as edit_file): refuse to touch a file
        // the model never read this session, or that changed on disk since.
        {
            let session = ctx.session.lock().await;
            if !session.read_files.contains(&path) {
                return Err(anyhow!(
                    "multi_edit requires reading {} first so each old_string matches the real bytes. Call read_file on it.",
                    path.display()
                ));
            }
            ensure_not_stale(&session, &path, "multi_edit")?;
        }
        let mut content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let original_for_undo = content.clone();
        let mut total_replacements = 0usize;
        for (i, edit) in a.edits.iter().enumerate() {
            if edit.old_string.is_empty() {
                return Err(anyhow!("edit #{}: old_string must not be empty", i + 1));
            }
            if edit.old_string == edit.new_string {
                return Err(anyhow!(
                    "edit #{}: old_string and new_string are identical; nothing to change",
                    i + 1
                ));
            }
            let (old_string, new_string) =
                match_line_endings(&content, &edit.old_string, &edit.new_string);
            let count = content.matches(&old_string).count();
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
                content.replace(&old_string, &new_string)
            } else {
                total_replacements += 1;
                content.replacen(&old_string, &new_string, 1)
            };
        }
        let tmp = path.with_extension(format!("medit-{}.tmp", rand_suffix()));
        atomic_write_preserving_permissions(&path, &tmp, content.as_bytes()).await?;
        let (post_edit_mtime, post_edit_size) = snapshot_meta(&path);
        {
            let mut session = ctx.session.lock().await;
            session
                .read_file_meta
                .insert(path.clone(), (post_edit_mtime, post_edit_size));
            session.push_undo_entry(UndoEntry {
                path: path.clone(),
                original_content: Some(original_for_undo.into_bytes()),
                post_edit_mtime,
                post_edit_size,
            });
        }
        Ok(format!(
            "Applied {} edit(s) ({} total replacement(s)) to {}",
            a.edits.len(),
            total_replacements,
            path.display()
        ))
    }
}

/// Reconcile an edit's line endings with the file's. `read_file` renders content
/// through `str::lines`, which strips `\r`, so on a CRLF file the model can only
/// build an `\n`-joined `old_string` that won't match the `\r\n` bytes on disk.
/// When the haystack uses CRLF and the `\n`-joined `old_string` doesn't match
/// verbatim, translate both strings to CRLF so the match succeeds and the file's
/// endings are preserved. A file that is already LF — or an `old_string` that
/// matches verbatim — is returned unchanged, so this never alters a working edit.
fn match_line_endings(haystack: &str, old: &str, new: &str) -> (String, String) {
    if haystack.contains("\r\n")
        && old.contains('\n')
        && !old.contains("\r\n")
        && !haystack.contains(old)
    {
        (lf_to_crlf(old), lf_to_crlf(new))
    } else {
        (old.to_string(), new.to_string())
    }
}

/// Convert every line ending in `s` to CRLF without doubling an existing CRLF.
fn lf_to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}
