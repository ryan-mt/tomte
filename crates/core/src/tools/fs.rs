use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::ffi::OsString;

use super::{BuiltinTool, ToolContext};

fn rand_suffix() -> String {
    use rand::RngCore;
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub(super) async fn atomic_write_preserving_permissions(
    path: &std::path::Path,
    tmp: &std::path::Path,
    bytes: &[u8],
) -> Result<()> {
    let permissions = tokio::fs::metadata(path)
        .await
        .ok()
        .map(|meta| meta.permissions());
    tokio::fs::write(tmp, bytes)
        .await
        .with_context(|| format!("write temp {}", tmp.display()))?;
    if let Some(permissions) = permissions {
        tokio::fs::set_permissions(tmp, permissions)
            .await
            .with_context(|| format!("set permissions on temp {}", tmp.display()))?;
    }
    tokio::fs::rename(tmp, path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

pub struct ReadFile;
pub struct WriteFile;
pub struct EditFile;
pub struct ListDir;

#[derive(Deserialize)]
struct ReadArgs {
    #[serde(alias = "file_path", alias = "filePath")]
    path: String,
    #[serde(default, deserialize_with = "super::deserialize_optional_usize")]
    offset: Option<usize>,
    #[serde(default, deserialize_with = "super::deserialize_optional_usize")]
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
- `limit`: Maximum number of lines to return (1..=2000), or `null` to use the default cap.\n\
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
                "limit": {"type": ["integer", "null"], "minimum": 1, "maximum": 2000, "description": "Maximum number of lines to return; null uses the default cap."}
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
        const MAX_LINE_LIMIT: usize = DEFAULT_LINE_LIMIT;
        // Per-line truncation so a minified bundle (one giant line) can't
        // blow out the context. Mirrors Claude Code's 2000-char-per-line cap.
        const MAX_LINE_CHARS: usize = 2000;

        let meta = match tokio::fs::metadata(&path).await {
            Ok(meta) => meta,
            // A clear "not found" beats leaking the `stat` syscall name, and
            // tells the model the path is wrong rather than the tool being broken.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(anyhow!("file not found: {}", a.path));
            }
            Err(e) => return Err(e).with_context(|| format!("read {}", a.path)),
        };
        // Record that this file was read this session so write_file/edit_file
        // can refuse to clobber a file the model never looked at. Keyed on the
        // canonical resolved path so later write/edit lookups match regardless
        // of how the path was spelled. Recorded for every read variant (full,
        // slice, empty, large) since they all pass through here first. The
        // (mtime, size) snapshot lets a later edit detect the file changed on
        // disk since this read and force a re-read.
        {
            let mut session = ctx.session.lock().await;
            session.read_files.insert(path.clone());
            session
                .read_file_meta
                .insert(path.clone(), (meta.modified().ok(), Some(meta.len())));
        }
        if a.limit == Some(0) {
            return Err(anyhow!("limit must be greater than 0"));
        }
        if a.limit.is_some_and(|limit| limit > MAX_LINE_LIMIT) {
            return Err(anyhow!("limit must be <= {MAX_LINE_LIMIT}"));
        }
        if meta.len() > MAX_BYTES && a.limit.is_none() {
            return Err(anyhow!(
                "file is too large ({} bytes > {} byte cap); pass `limit` to read a slice",
                meta.len(),
                MAX_BYTES
            ));
        }
        let start = a.offset.unwrap_or(0);
        let effective_limit = a.limit.unwrap_or(DEFAULT_LINE_LIMIT);
        if meta.len() > MAX_BYTES {
            return read_large_text_slice(&path, &a.path, start, effective_limit, MAX_LINE_CHARS);
        }
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                // Non-UTF-8: an image, PDF, or other binary. read_file can't
                // render it as text, so return an informative summary instead
                // of a cryptic decode error. The file is already recorded as
                // read above, so write_file may overwrite it if intended.
                return Ok(describe_binary(&a.path, e.as_bytes()));
            }
        };
        if text.is_empty() {
            return Ok(format!(
                "<system-reminder>The file `{}` exists but is empty.</system-reminder>\n",
                a.path
            ));
        }
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let start = start.min(total);
        let end = start.saturating_add(effective_limit).min(total);
        let mut out = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            out.push_str(&numbered_line(start + i + 1, line, false, MAX_LINE_CHARS));
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

/// One-line summary for a binary file that `read_file` can't show as text —
/// the kind (sniffed from magic bytes) and size, plus how to view an image.
fn describe_binary(display_path: &str, bytes: &[u8]) -> String {
    let kind = sniff_binary_kind(bytes);
    let is_image = matches!(
        kind,
        "PNG image" | "JPEG image" | "GIF image" | "WebP image"
    );
    let hint = if is_image {
        " To have the model see it, attach it with /img."
    } else {
        ""
    };
    format!(
        "<system-reminder>`{}` is a {} ({} bytes); read_file shows text only, not its contents. \
         It is recorded as read, so write_file may overwrite it if you intend to replace it.{}</system-reminder>\n",
        display_path,
        kind,
        bytes.len(),
        hint
    )
}

/// Best-effort binary type from leading magic bytes (more reliable than the
/// extension). Falls back to a generic label.
fn sniff_binary_kind(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "PNG image"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "JPEG image"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "GIF image"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "WebP image"
    } else if bytes.starts_with(b"%PDF-") {
        "PDF document"
    } else {
        "binary file"
    }
}

fn numbered_line(
    line_no: usize,
    line: &str,
    was_byte_truncated: bool,
    max_line_chars: usize,
) -> String {
    let printed: String = if was_byte_truncated || line.chars().count() > max_line_chars {
        let head: String = line.chars().take(max_line_chars).collect();
        format!("{head}… [line truncated]")
    } else {
        line.to_string()
    };
    format!("{line_no:>6}\t{printed}\n")
}

fn read_large_text_slice(
    path: &std::path::Path,
    display_path: &str,
    start: usize,
    limit: usize,
    max_line_chars: usize,
) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut out = String::new();
    let mut line_no = 0usize;
    let mut printed = 0usize;
    let max_line_bytes = max_line_chars.saturating_mul(4);

    while let Some((bytes, was_byte_truncated)) =
        read_next_line_capped(&mut reader, max_line_bytes)?
    {
        if line_no >= start {
            if printed >= limit {
                out.push_str(&format!(
                    "<system-reminder>Showing a slice of large file `{display_path}`. More lines remain — call read_file again with offset={line_no} and an explicit limit to continue.</system-reminder>\n"
                ));
                break;
            }
            let text = bytes_to_line(&bytes, was_byte_truncated)?;
            out.push_str(&numbered_line(
                line_no + 1,
                &text,
                was_byte_truncated,
                max_line_chars,
            ));
            printed += 1;
        }
        line_no = line_no.saturating_add(1);
    }
    Ok(out)
}

fn read_next_line_capped<R: std::io::BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<(Vec<u8>, bool)>> {
    let mut out = Vec::new();
    let mut truncated = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return if out.is_empty() && !truncated {
                Ok(None)
            } else {
                Ok(Some((out, truncated)))
            };
        }
        let newline = buf.iter().position(|b| *b == b'\n');
        let take_len = newline.map(|i| i + 1).unwrap_or(buf.len());
        let chunk = &buf[..take_len];
        if !truncated {
            let remaining = max_bytes.saturating_sub(out.len());
            if chunk.len() <= remaining {
                out.extend_from_slice(chunk);
            } else {
                out.extend_from_slice(&chunk[..remaining]);
                truncated = true;
            }
        }
        reader.consume(take_len);
        if newline.is_some() {
            return Ok(Some((out, truncated)));
        }
    }
}

fn bytes_to_line(bytes: &[u8], was_byte_truncated: bool) -> Result<String> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) if was_byte_truncated && e.valid_up_to() > 0 => {
            std::str::from_utf8(&bytes[..e.valid_up_to()])?
        }
        Err(e) => return Err(anyhow!("file is not valid UTF-8: {e}")),
    };
    Ok(text.trim_end_matches(['\r', '\n']).to_string())
}

#[derive(Deserialize)]
struct WriteArgs {
    #[serde(alias = "file_path", alias = "filePath")]
    path: String,
    #[serde(alias = "contents", alias = "text")]
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
Safety: overwriting an EXISTING file is refused unless you read it first this session (new files need no read), so you can't discard content you never saw.\n\
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
        let existing_meta = match tokio::fs::metadata(&path).await {
            Ok(meta) => Some(meta),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("stat {}", path.display())),
        };
        if existing_meta.as_ref().is_some_and(|meta| meta.is_dir()) {
            return Err(anyhow!(
                "cannot write file over directory {}",
                path.display()
            ));
        }
        // Read-before-overwrite safety: refuse to replace an EXISTING file the
        // model hasn't read this session, so it can't silently discard content
        // it never saw. Creating a new file (no existing_meta) needs no read.
        if existing_meta.is_some() {
            let session = ctx.session.lock().await;
            if !session.read_files.contains(&path) {
                return Err(anyhow!(
                    "write_file refuses to overwrite {} because it was not read this session. \
                     Call read_file on it first so you don't discard unseen content (or use edit_file for a targeted change).",
                    path.display()
                ));
            }
            ensure_not_stale(&session, &path, "write_file")?;
        }
        // Snapshot prior contents as RAW BYTES, not UTF-8: a binary file that
        // exists must be restorable on undo. If an existing file cannot be
        // read, fail before overwriting — otherwise undo would treat it as a
        // newly-created file and delete it.
        let original = if existing_meta.is_some() {
            Some(
                tokio::fs::read(&path)
                    .await
                    .with_context(|| format!("read original {}", path.display()))?,
            )
        } else {
            None
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create parent dirs for {}", parent.display()))?;
        }
        let tmp = path.with_extension(format!("write-{}.tmp", rand_suffix()));
        atomic_write_preserving_permissions(&path, &tmp, a.content.as_bytes()).await?;
        let (post_edit_mtime, post_edit_size) = snapshot_meta(&path);
        {
            // A freshly written file counts as "read" so a follow-up edit_file
            // doesn't spuriously demand a read_file of bytes the model just
            // authored. Refresh the (mtime, size) snapshot to the bytes we just
            // wrote so the staleness guard doesn't fire on the model's own
            // write. Same lock also records undo.
            let mut session = ctx.session.lock().await;
            session.read_files.insert(path.clone());
            session
                .read_file_meta
                .insert(path.clone(), (post_edit_mtime, post_edit_size));
            session.push_undo_entry(super::UndoEntry {
                path: path.clone(),
                original_content: original,
                post_edit_mtime,
                post_edit_size,
            });
        }
        Ok(format!(
            "Wrote {} bytes to {}",
            a.content.len(),
            path.display()
        ))
    }
}

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
        deserialize_with = "super::deserialize_bool"
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
        let a: EditArgs = super::parse_args("edit_file", args.clone()).ok()?;
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
        atomic_write_preserving_permissions(&path, &tmp, new_content.as_bytes()).await?;
        let (post_edit_mtime, post_edit_size) = snapshot_meta(&path);
        {
            let mut session = ctx.session.lock().await;
            // Refresh the snapshot to the bytes we just wrote so a follow-up
            // edit to the same file isn't flagged as stale by our own change.
            session
                .read_file_meta
                .insert(path.clone(), (post_edit_mtime, post_edit_size));
            session.push_undo_entry(super::UndoEntry {
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

#[derive(Deserialize)]
struct ListArgs {
    #[serde(
        alias = "file_path",
        alias = "filePath",
        alias = "directory",
        alias = "dir",
        alias = "folder"
    )]
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
        deserialize_with = "super::deserialize_bool"
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
        let a: MultiEditArgs = super::parse_args("multi_edit", args.clone()).ok()?;
        Some(format!("Multi-edit {} ({} edit(s))", a.path, a.edits.len()))
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: MultiEditArgs = super::parse_args("multi_edit", args)?;
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
        atomic_write_preserving_permissions(&path, &tmp, content.as_bytes()).await?;
        let (post_edit_mtime, post_edit_size) = snapshot_meta(&path);
        {
            let mut session = ctx.session.lock().await;
            session
                .read_file_meta
                .insert(path.clone(), (post_edit_mtime, post_edit_size));
            session.push_undo_entry(super::UndoEntry {
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

pub struct UndoLastEdit;

#[async_trait]
impl BuiltinTool for UndoLastEdit {
    fn name(&self) -> &'static str {
        "undo_last_edit"
    }
    fn description(&self) -> &'static str {
        "Roll back the most recent file edit you made via `edit_file`, `write_file`, or `multi_edit`. Undo is a stack: each call reverts one edit, most-recent first. If the edit created a new file, undo deletes it; otherwise it restores the previous contents.\n\
\n\
Refuses to undo when the file changed since your edit (the user, their editor, or another tool touched it) so it can't silently destroy work you didn't make — restore manually in that case.\n\
\n\
Use it to recover from a bad edit right after you notice. It does not undo `run_shell` side effects or anything outside the file-edit stack.\n\
\n\
Parameters: none."
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false })
    }
    async fn execute(&self, _args: Value, ctx: &ToolContext) -> Result<String> {
        let mut session = ctx.session.lock().await;
        let entry = session
            .undo_stack
            .back()
            .cloned()
            .ok_or_else(|| anyhow!("no edits to undo"))?;
        // TOCTOU guard: refuse to restore if the file has been touched since
        // the edit. Without this, an `undo_last_edit` after the user manually
        // edits the file (in their editor, another shell, etc.) would
        // silently nuke those changes.
        if let Some(expected) = entry.post_edit_mtime {
            let (current_mtime, current_size) = snapshot_meta(&entry.path);
            if current_mtime != Some(expected) || current_size != entry.post_edit_size {
                return Err(anyhow!(
                    "refusing to undo {}: file has been modified since the edit; restore manually if intended",
                    entry.path.display()
                ));
            }
        }
        let message = match entry.original_content {
            Some(content) => {
                // Atomic restore (temp + rename), matching the edit/write tools,
                // so a crash mid-restore can't leave a half-written file.
                let tmp = entry
                    .path
                    .with_extension(format!("undo-{}.tmp", rand_suffix()));
                atomic_write_preserving_permissions(&entry.path, &tmp, &content).await?;
                format!("Restored {}", entry.path.display())
            }
            None => {
                tokio::fs::remove_file(&entry.path)
                    .await
                    .with_context(|| format!("remove {}", entry.path.display()))?;
                format!("Removed (was a new file): {}", entry.path.display())
            }
        };
        session.undo_stack.pop_back();
        Ok(message)
    }
}

/// Snapshots (mtime, size) used by every edit/write tool immediately after a
/// successful write, and by `UndoLastEdit` to detect post-edit modifications
/// before restoring. Both come from one `metadata()` call so they're
/// consistent. Comparing size as well as mtime catches same-second external
/// edits a coarse mtime alone would miss.
pub(super) fn snapshot_meta(
    path: &std::path::Path,
) -> (Option<std::time::SystemTime>, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(m) => (m.modified().ok(), Some(m.len())),
        Err(_) => (None, None),
    }
}

/// Refuse an edit/overwrite when the file changed on disk since the model last
/// read it this session, forcing a fresh `read_file` so the edit targets the
/// current bytes. The caller has already confirmed the path was read this
/// session; this adds the "still fresh?" half. Best-effort: only fires when a
/// read-time snapshot exists — a resumed session has none and falls back to the
/// plain read-once guard. A snapshot recorded after the model's own write/edit
/// keeps back-to-back edits from tripping the check.
fn ensure_not_stale(
    session: &super::SessionState,
    path: &std::path::Path,
    tool: &str,
) -> Result<()> {
    if let Some(recorded) = session.read_file_meta.get(path) {
        if snapshot_meta(path) != *recorded {
            return Err(anyhow!(
                "{tool}: {} changed on disk since you read it. Call read_file on it again so your edit matches the current bytes (it may contain changes you haven't seen).",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Resolve a model-supplied path against the sandbox `cwd`. Accepts either a
/// relative path or a Claude-style absolute path that is lexically inside
/// `cwd`. Rejects absolute paths outside `cwd`, lexical `..` escapes, and
/// symlinks whose resolved target leaves `cwd`. Without this guard the LLM
/// could read `/etc/shadow`, write to `~/.ssh/authorized_keys`, or otherwise
/// escape the working tree.
pub(crate) fn resolve(cwd: &std::path::Path, p: &str) -> Result<std::path::PathBuf> {
    let raw_path = std::path::Path::new(p);
    let sandbox = cwd
        .canonicalize()
        .with_context(|| format!("resolve sandbox cwd {}", cwd.display()))?;
    let path = if raw_path.is_absolute() {
        let absolute = canonicalize_with_missing(raw_path)
            .with_context(|| format!("resolve {}", raw_path.display()))?;
        absolute
            .strip_prefix(&sandbox)
            .map_err(|_| {
                anyhow!(
                    "absolute path escapes the sandbox (cwd {}): {}",
                    sandbox.display(),
                    raw_path.display()
                )
            })?
            .to_path_buf()
    } else {
        raw_path.to_path_buf()
    };
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
                return Err(anyhow!("invalid path component: {}", raw_path.display()));
            }
        }
    }

    let mut existing = sandbox.clone();
    let mut missing: Vec<OsString> = Vec::new();
    let mut found_missing = false;
    for comp in normalized.components() {
        let name = comp.as_os_str();
        if found_missing {
            missing.push(name.to_os_string());
            continue;
        }
        let next = existing.join(name);
        match std::fs::symlink_metadata(&next) {
            Ok(_) => existing = next,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                found_missing = true;
                missing.push(name.to_os_string());
            }
            Err(e) => return Err(e).with_context(|| format!("stat {}", next.display())),
        }
    }

    let resolved_existing = existing
        .canonicalize()
        .with_context(|| format!("resolve {}", existing.display()))?;
    if !resolved_existing.starts_with(&sandbox) {
        return Err(anyhow!("path escapes the sandbox: {}", path.display()));
    }

    let mut resolved = resolved_existing;
    for comp in missing {
        resolved.push(comp);
    }
    Ok(resolved)
}

fn canonicalize_with_missing(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing: Vec<OsString> = Vec::new();

    loop {
        match existing.canonicalize() {
            Ok(mut resolved) => {
                for comp in missing.iter().rev() {
                    resolved.push(comp);
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .ok_or_else(|| anyhow!("path has no existing parent: {}", path.display()))?;
                missing.push(name.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| anyhow!("path has no existing parent: {}", path.display()))?
                    .to_path_buf();
            }
            Err(e) => {
                return Err(e).with_context(|| format!("canonicalize {}", existing.display()))
            }
        }
    }
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
            require_approval: false,
            auto_approve_edits: false,
            session: Arc::new(Mutex::new(SessionState::default())),
            config: crate::config::Config::default(),
            cwd_override: Arc::new(Mutex::new(None)),
            events: None,
        }
    }

    fn read_args(path: &str, offset: Option<usize>, limit: Option<usize>) -> Value {
        json!({"path": path, "offset": offset, "limit": limit})
    }

    #[tokio::test]
    async fn read_file_missing_path_gives_clear_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = ReadFile
            .execute(
                read_args("nope.py", None, None),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("file not found"), "got: {err}");
        assert!(err.contains("nope.py"), "got: {err}");
        assert!(
            !err.contains("stat "),
            "must not leak the syscall name: {err}"
        );
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

    #[tokio::test]
    async fn read_file_accepts_string_offset_and_limit_args() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=5).map(|i| format!("L{i}\n")).collect();
        std::fs::write(dir.path().join("small.txt"), &content).unwrap();

        let out = ReadFile
            .execute(
                json!({"path": "small.txt", "offset": "2", "limit": "1"}),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();

        assert!(out.contains("\tL3\n"), "got: {out}");
        assert!(!out.contains("\tL2\n"), "got: {out}");
        assert!(!out.contains("\tL4\n"), "got: {out}");
    }

    #[tokio::test]
    async fn read_file_accepts_claude_file_path_alias_and_absolute_path_inside_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "hello\n").unwrap();

        let out = ReadFile
            .execute(
                json!({"file_path": path.to_string_lossy()}),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();

        assert!(out.contains("\thello\n"), "got: {out}");
    }

    #[tokio::test]
    async fn absolute_path_outside_cwd_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("secret.txt");
        std::fs::write(&path, "secret\n").unwrap();

        let err = ReadFile
            .execute(
                json!({"file_path": path.to_string_lossy()}),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("escapes the sandbox"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn read_file_rejects_zero_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("small.txt"), "hello\n").unwrap();

        let err = ReadFile
            .execute(
                read_args("small.txt", None, Some(0)),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("limit must be greater than 0"));
    }

    #[tokio::test]
    async fn read_file_rejects_limit_above_hard_cap() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("small.txt"), "hello\n").unwrap();

        let err = ReadFile
            .execute(
                read_args("small.txt", None, Some(2001)),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("limit must be <= 2000"));
    }

    #[tokio::test]
    async fn list_dir_accepts_common_directory_aliases() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "mod tests;\n").unwrap();

        let out = ListDir
            .execute(json!({"directory": "src"}), &ctx(dir.path().to_path_buf()))
            .await
            .unwrap();

        assert_eq!(out.trim(), "lib.rs");
    }

    #[tokio::test]
    async fn read_file_streams_large_file_when_limit_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.log");
        let mut content = String::new();
        for i in 1..=470_000 {
            content.push_str(&format!("line {i:06}\n"));
        }
        assert!(content.len() > 5_000_000);
        std::fs::write(&path, content).unwrap();

        let out = ReadFile
            .execute(
                read_args("large.log", Some(2), Some(2)),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();

        assert!(out.contains("\tline 000003\n"), "got: {out}");
        assert!(out.contains("\tline 000004\n"), "got: {out}");
        assert!(!out.contains("\tline 000005\n"), "got: {out}");
        assert!(out.contains("More lines remain"), "got: {out}");
        assert!(out.contains("offset=4"), "got: {out}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_file_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("outside")).unwrap();

        let err = ReadFile
            .execute(
                read_args("outside/secret.txt", None, None),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("path escapes the sandbox"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_file_rejects_symlink_escape_through_parent() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("outside")).unwrap();

        let err = WriteFile
            .execute(
                json!({"path": "outside/owned.txt", "content": "owned"}),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("path escapes the sandbox"));
        assert!(!outside.path().join("owned.txt").exists());
    }

    #[tokio::test]
    async fn write_file_rejects_directory_targets() {
        let dir = tempfile::tempdir().unwrap();

        let err = WriteFile
            .execute(
                json!({"path": ".", "content": "not a directory"}),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("cannot write file over directory"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_file_refuses_existing_unreadable_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("write-only.txt");
        std::fs::write(&path, "secret").unwrap();
        let ctx = ctx(dir.path().to_path_buf());

        // Read it first so the read-before-write guard is satisfied, then revoke
        // read permission to exercise the "can't snapshot original" guard — a
        // TOCTOU where the file becomes unreadable between read and write.
        ReadFile
            .execute(json!({"path": "write-only.txt"}), &ctx)
            .await
            .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o200)).unwrap();

        let err = WriteFile
            .execute(
                json!({"path": "write-only.txt", "content": "replacement"}),
                &ctx,
            )
            .await
            .unwrap_err();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(err.to_string().contains("read original"), "got: {err}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "secret");
    }

    #[tokio::test]
    async fn write_file_refuses_unread_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "important").unwrap();
        let err = WriteFile
            .execute(
                json!({"path": "keep.txt", "content": "clobbered"}),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not read this session"),
            "got: {err}"
        );
        // The original survives — the write was refused, not partially applied.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
            "important"
        );
    }

    #[tokio::test]
    async fn write_file_allows_new_file_without_read() {
        let dir = tempfile::tempdir().unwrap();
        WriteFile
            .execute(
                json!({"path": "fresh.txt", "content": "hello"}),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("fresh.txt")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn read_then_write_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("doc.txt"), "v1").unwrap();
        let ctx = ctx(dir.path().to_path_buf());
        ReadFile
            .execute(json!({"path": "doc.txt"}), &ctx)
            .await
            .unwrap();
        WriteFile
            .execute(json!({"path": "doc.txt", "content": "v2"}), &ctx)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("doc.txt")).unwrap(),
            "v2"
        );
    }

    #[tokio::test]
    async fn write_edit_and_multi_edit_accept_common_argument_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path().to_path_buf());
        let path = dir.path().join("doc.txt");
        let file_path = path.to_string_lossy();
        let other_path = dir.path().join("other.txt");
        let other_file_path = other_path.to_string_lossy();

        WriteFile
            .execute(
                json!({"file_path": file_path, "text": "one two three"}),
                &ctx,
            )
            .await
            .unwrap();
        WriteFile
            .execute(
                json!({"filePath": other_file_path, "contents": "alias content"}),
                &ctx,
            )
            .await
            .unwrap();
        EditFile
            .execute(
                json!({"file_path": file_path, "old_text": "two", "new_text": "2"}),
                &ctx,
            )
            .await
            .unwrap();
        EditFile
            .execute(
                json!({
                    "filePath": file_path,
                    "oldText": "2",
                    "newText": "two",
                    "replaceAll": "false"
                }),
                &ctx,
            )
            .await
            .unwrap();
        MultiEdit
            .execute(
                json!({
                    "filePath": file_path,
                    "edits": [
                        {"old_text": "one", "new_text": "1"},
                        {"oldText": "three", "newText": "3"}
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap(), "1 two 3");
        assert_eq!(
            std::fs::read_to_string(other_path).unwrap(),
            "alias content"
        );
    }

    #[tokio::test]
    async fn write_then_edit_without_reread_succeeds() {
        // A file the model just authored counts as read, so a follow-up
        // edit_file must not spuriously demand a read_file.
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path().to_path_buf());
        WriteFile
            .execute(json!({"path": "gen.txt", "content": "fn main() {}"}), &ctx)
            .await
            .unwrap();
        EditFile
            .execute(
                json!({"path": "gen.txt", "old_string": "main", "new_string": "run"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("gen.txt")).unwrap(),
            "fn run() {}"
        );
    }

    #[tokio::test]
    async fn edit_file_refuses_unread_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("e.txt"), "foo").unwrap();
        let err = EditFile
            .execute(
                json!({"path": "e.txt", "old_string": "foo", "new_string": "bar"}),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("requires reading"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("e.txt")).unwrap(),
            "foo"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_identical_old_and_new() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("id.txt"), "same").unwrap();
        let ctx = ctx(dir.path().to_path_buf());
        ReadFile
            .execute(json!({"path": "id.txt"}), &ctx)
            .await
            .unwrap();
        let err = EditFile
            .execute(
                json!({"path": "id.txt", "old_string": "same", "new_string": "same"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("identical"), "got: {err}");
    }

    #[tokio::test]
    async fn read_file_describes_binary_instead_of_erroring() {
        let dir = tempfile::tempdir().unwrap();
        // PNG magic header + a 0xFF byte → not valid UTF-8.
        let png = [
            0x89u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0xFF, 0x00,
        ];
        std::fs::write(dir.path().join("logo.png"), png).unwrap();
        let out = ReadFile
            .execute(json!({"path": "logo.png"}), &ctx(dir.path().to_path_buf()))
            .await
            .unwrap();
        assert!(out.contains("PNG image"), "got: {out}");
        assert!(out.contains("recorded as read"), "got: {out}");
    }

    #[tokio::test]
    async fn read_binary_then_write_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let png = [
            0x89u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0xFF, 0x00,
        ];
        std::fs::write(dir.path().join("img.png"), png).unwrap();
        let ctx = ctx(dir.path().to_path_buf());
        // Reading a binary records it as read even though contents aren't shown,
        // so a deliberate overwrite is allowed (the read-before-write guard,
        // which can't read binary as text, no longer blocks regeneration).
        ReadFile
            .execute(json!({"path": "img.png"}), &ctx)
            .await
            .unwrap();
        WriteFile
            .execute(json!({"path": "img.png", "content": "regenerated"}), &ctx)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("img.png")).unwrap(),
            "regenerated"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_file_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ctx = ctx(dir.path().to_path_buf());

        ReadFile
            .execute(json!({"path": "script.sh"}), &ctx)
            .await
            .unwrap();
        WriteFile
            .execute(
                json!({"path": "script.sh", "content": "#!/bin/sh\necho new\n"}),
                &ctx,
            )
            .await
            .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_and_undo_preserve_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ctx = ctx(dir.path().to_path_buf());

        ReadFile
            .execute(json!({"path": "script.sh"}), &ctx)
            .await
            .unwrap();
        EditFile
            .execute(
                json!({
                    "path": "script.sh",
                    "old_string": "old",
                    "new_string": "new",
                    "replace_all": false
                }),
                &ctx,
            )
            .await
            .unwrap();
        let mode_after_edit = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode_after_edit, 0o755);

        UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#!/bin/sh\necho old\n"
        );
        let mode_after_undo = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode_after_undo, 0o755);
    }

    #[tokio::test]
    async fn edit_file_refuses_after_external_modification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.txt");
        std::fs::write(&path, "alpha beta\n").unwrap();
        let ctx = ctx(dir.path().to_path_buf());
        // Read records the (mtime, size) snapshot.
        ReadFile
            .execute(json!({"path": "m.txt"}), &ctx)
            .await
            .unwrap();
        // Something else changes the file on disk after the read. Changing the
        // length means staleness is caught even when the mtime resolution is
        // too coarse to distinguish a same-second write.
        std::fs::write(&path, "alpha beta gamma delta\n").unwrap();
        let err = EditFile
            .execute(
                json!({"path": "m.txt", "old_string": "alpha", "new_string": "ALPHA"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("changed on disk"), "got: {err}");
        // The refused edit left the on-disk content untouched.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha beta gamma delta\n"
        );
        // Re-reading refreshes the snapshot, so the edit then goes through.
        ReadFile
            .execute(json!({"path": "m.txt"}), &ctx)
            .await
            .unwrap();
        EditFile
            .execute(
                json!({"path": "m.txt", "old_string": "alpha", "new_string": "ALPHA"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("ALPHA"));
    }

    #[tokio::test]
    async fn consecutive_edits_after_one_read_are_allowed() {
        // The model's own edit changes (mtime, size); the staleness guard must
        // not fire on that, so a second edit after a single read still works.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.txt");
        std::fs::write(&path, "one two three").unwrap();
        let ctx = ctx(dir.path().to_path_buf());
        ReadFile
            .execute(json!({"path": "c.txt"}), &ctx)
            .await
            .unwrap();
        EditFile
            .execute(
                json!({"path": "c.txt", "old_string": "one", "new_string": "1"}),
                &ctx,
            )
            .await
            .unwrap();
        EditFile
            .execute(
                json!({"path": "c.txt", "old_string": "three", "new_string": "3"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1 two 3");
    }
}
