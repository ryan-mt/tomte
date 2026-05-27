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
Use this whenever you need to inspect the contents of a file — and ALWAYS before calling `edit_file`, because `edit_file` requires the exact existing text. Prefer reading the whole file when feasible; use `offset` and `limit` only for very large files.\n\
\n\
Parameters:\n\
- `path`: Relative path inside the working directory. Absolute paths and `..` traversal are rejected.\n\
- `offset`: Zero-indexed line to start reading from, or `null` to start at the top.\n\
- `limit`: Maximum number of lines to return, or `null` to read to the end.\n\
\n\
Constraints: files larger than 5 MB must be read with an explicit `limit`. Binary files are not supported by this tool."
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
        let a: ReadArgs = serde_json::from_value(args)?;
        let path = resolve(&ctx.cwd, &a.path)?;
        // Bound the read so the LLM can't request /dev/zero or a multi-GB log
        // and OOM the process.
        const MAX_BYTES: u64 = 5_000_000;
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
        let lines: Vec<&str> = text.lines().collect();
        let start = a.offset.unwrap_or(0);
        let end = a
            .limit
            .map(|l| (start + l).min(lines.len()))
            .unwrap_or(lines.len());
        let mut out = String::new();
        for (i, line) in lines[start.min(lines.len())..end].iter().enumerate() {
            out.push_str(&format!("{:>6}\t{}\n", start + i + 1, line));
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
Use this for brand-new files. Do NOT use it to modify an existing file unless you genuinely want to replace its entire contents — use `edit_file` for targeted changes so you do not silently delete unrelated code.\n\
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
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: WriteArgs = serde_json::from_value(args)?;
        let path = resolve(&ctx.cwd, &a.path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(&path, a.content.as_bytes())
            .await
            .with_context(|| format!("write {}", path.display()))?;
        Ok(format!("Wrote {} bytes to {}", a.content.len(), path.display()))
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
        "Replace `old_string` with `new_string` in an existing file. By default `old_string` must appear exactly once; pass `replace_all: true` to substitute every occurrence.\n\
\n\
Always call `read_file` on the target first so you know the exact existing text. Include enough surrounding context (whole-line matches, neighboring lines) in `old_string` to make it unique — substring matches in the middle of a line are fragile.\n\
\n\
The replacement is atomic: the new content is staged in a sibling temp file and renamed into place, so a crash mid-write leaves the original intact.\n\
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
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: EditArgs = serde_json::from_value(args)?;
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
        Ok(format!("Replaced {} occurrence(s) in {}", count, path.display()))
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
Use this for a snapshot of one directory. Prefer `glob` for recursive matches, or `grep` for content search.\n\
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
        let a: ListArgs = serde_json::from_value(args)?;
        let path = resolve(&ctx.cwd, &a.path)?;
        let mut entries = tokio::fs::read_dir(&path).await?;
        let mut items: Vec<String> = Vec::new();
        while let Some(e) = entries.next_entry().await? {
            let ft = e.file_type().await?;
            let name = e.file_name().to_string_lossy().to_string();
            items.push(if ft.is_dir() { format!("{name}/") } else { name });
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
        "Apply a sequence of replacements to a single file atomically. Each edit is `{old_string, new_string, replace_all}` and runs in order against the output of the previous edit. The file is read once, transformed entirely in memory, and written back via a temp-file + rename so a crash mid-write leaves the original intact.\n\
\n\
Use this when you need to make several related changes to the same file. It is faster and safer than calling `edit_file` repeatedly: the model only spends one tool call, and a failure on edit #3 reverts edits #1 and #2 too.\n\
\n\
Always read the file first. `old_string` is matched literally; provide enough surrounding context to make it unique unless `replace_all` is true.\n\
\n\
Parameters:\n\
- `path`: Relative path inside the working directory; file must exist.\n\
- `edits`: Ordered list of replacements. Each entry must include `old_string`, `new_string`, and `replace_all` (boolean)."
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
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: MultiEditArgs = serde_json::from_value(args)?;
        if a.edits.is_empty() {
            return Err(anyhow!("`edits` must contain at least one entry"));
        }
        let path = resolve(&ctx.cwd, &a.path)?;
        let mut content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
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
        Ok(format!(
            "Applied {} edit(s) ({} total replacement(s)) to {}",
            a.edits.len(),
            total_replacements,
            path.display()
        ))
    }
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
                    return Err(anyhow!(
                        "path escapes the sandbox: {}",
                        path.display()
                    ));
                }
            }
            Component::Normal(s) => normalized.push(s),
            Component::Prefix(_) | Component::RootDir => {
                return Err(anyhow!(
                    "invalid path component: {}",
                    path.display()
                ));
            }
        }
    }
    Ok(cwd.join(normalized))
}
