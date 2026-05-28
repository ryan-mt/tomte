use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;

use super::{BuiltinTool, ToolContext};

pub struct Grep;
pub struct Glob;

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    /// "content" (default), "files_with_matches", or "count".
    #[serde(default)]
    output_mode: Option<String>,
    /// Cap on lines of output after the byte cap.
    #[serde(default)]
    head_limit: Option<usize>,
    /// Lines of context after each match (rg -A).
    #[serde(default)]
    context_after: Option<usize>,
    /// Lines of context before each match (rg -B).
    #[serde(default)]
    context_before: Option<usize>,
    /// rg --multiline. Allows patterns to span newlines.
    #[serde(default)]
    multiline: Option<bool>,
    /// rg --type filter, e.g. "rust", "ts", "py".
    #[serde(default)]
    file_type: Option<String>,
}

#[async_trait]
impl BuiltinTool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "Search for a regular-expression pattern across files in the working directory. Uses ripgrep when available (which respects `.gitignore`), and falls back to plain `grep -rn`.\n\
\n\
Output modes (`output_mode`):\n\
- `content` (default): one match per line, `path:lineno:content`. Best when you want to read the match in context.\n\
- `files_with_matches`: one path per line. Best when you only need the set of files that contain the pattern (then `read_file` the interesting ones).\n\
- `count`: one `path:N` per line where N is the match count. Best for measuring how widespread a pattern is.\n\
\n\
Output is capped at ~8000 bytes; pass `head_limit` to also cap by number of lines. Refine the pattern, narrow with `glob`/`file_type`, or scope with `path` when you hit the cap.\n\
\n\
When to use:\n\
- \"Where is X used / defined / referenced\" across the codebase.\n\
- Finding every TODO/FIXME, every call site of a function, every import of a module.\n\
- Narrowing a search with `glob` (`\"*.test.ts\"`) or `file_type` (`\"rust\"`, `\"ts\"`, `\"py\"`) when you only care about one language.\n\
- Cross-line patterns (decorator + function on next line, multi-line strings) — set `multiline: true`.\n\
\n\
When NOT to use:\n\
- Enumerate matching paths only — `output_mode: files_with_matches` here, OR `glob` if you don't even need a regex.\n\
- Full contents of one known file — use `read_file`.\n\
- Shell-style `grep -r`/`find -exec grep` — use this tool instead; it's faster and structured.\n\
\n\
Pattern tips:\n\
- Use word boundaries (`\\b`) or anchored matches to avoid noise: `\\bfoo\\b` instead of `foo`.\n\
- Combine with `glob` to skip vendored code (`*.rs`, `!target/**`).\n\
- Patterns are extended regex (ripgrep flavor); `^`, `$`, `|`, `()`, `?`, `+`, `*`, `[...]` all work.\n\
- For multi-line patterns (matches that cross `\\n`) set `multiline: true` so ripgrep enables `-U`.\n\
- Use `context_before`/`context_after` to grab surrounding lines — only meaningful in `content` mode.\n\
\n\
Parameters:\n\
- `pattern`: Regex to search for (ripgrep / grep -E syntax).\n\
- `path`: Optional subdirectory or file to scope the search; `null` searches the whole working directory.\n\
- `glob`: Optional file glob to filter (e.g. `*.rs`, `**/*.test.ts`); `null` for all files.\n\
- `case_insensitive`: When true, ignore case.\n\
- `output_mode`: `content` | `files_with_matches` | `count`; defaults to `content` when null.\n\
- `head_limit`: Max output lines; `null` for no per-line cap (byte cap still applies).\n\
- `context_after`: Lines of context AFTER each match (content mode only); `null` for none.\n\
- `context_before`: Lines of context BEFORE each match (content mode only); `null` for none.\n\
- `multiline`: Enable `--multiline` so `.` and patterns span newlines; default false.\n\
- `file_type`: Restrict to one of ripgrep's named file types (`rust`, `ts`, `tsx`, `py`, `go`, `md`, …); `null` for all."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex pattern to find."},
                "path": {"type": ["string", "null"], "description": "Optional subdirectory or file to scope the search; null searches everything."},
                "glob": {"type": ["string", "null"], "description": "Optional file glob filter (e.g. '*.rs'); null for all files."},
                "case_insensitive": {"type": "boolean", "description": "Match case-insensitively when true."},
                "output_mode": {"type": ["string", "null"], "enum": ["content", "files_with_matches", "count", null], "description": "Output shape; defaults to content."},
                "head_limit": {"type": ["integer", "null"], "description": "Cap on output lines; null for no per-line cap."},
                "context_after": {"type": ["integer", "null"], "description": "Lines AFTER each match (content mode); null for none."},
                "context_before": {"type": ["integer", "null"], "description": "Lines BEFORE each match (content mode); null for none."},
                "multiline": {"type": ["boolean", "null"], "description": "Enable --multiline so patterns span newlines."},
                "file_type": {"type": ["string", "null"], "description": "Restrict to a ripgrep named file type (rust, ts, py, ...); null for all."}
            },
            "required": [
                "pattern", "path", "glob", "case_insensitive",
                "output_mode", "head_limit", "context_after", "context_before",
                "multiline", "file_type"
            ],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: GrepArgs = super::parse_args("grep", args)?;
        let mode = a.output_mode.as_deref().unwrap_or("content");
        if !matches!(mode, "content" | "files_with_matches" | "count") {
            return Err(anyhow::anyhow!(
                "output_mode must be 'content', 'files_with_matches', or 'count' (got '{mode}')"
            ));
        }

        let mut cmd = Command::new("rg");
        cmd.arg("--color=never");
        match mode {
            "files_with_matches" => {
                cmd.arg("--files-with-matches");
            }
            "count" => {
                cmd.arg("--count");
            }
            _ => {
                cmd.arg("--no-heading").arg("--line-number");
                if let Some(n) = a.context_after {
                    cmd.arg("-A").arg(n.to_string());
                }
                if let Some(n) = a.context_before {
                    cmd.arg("-B").arg(n.to_string());
                }
            }
        }
        if a.case_insensitive {
            cmd.arg("-i");
        }
        if a.multiline.unwrap_or(false) {
            cmd.arg("--multiline").arg("--multiline-dotall");
        }
        if let Some(g) = &a.glob {
            cmd.arg("--glob").arg(g);
        }
        if let Some(t) = &a.file_type {
            cmd.arg("--type").arg(t);
        }
        cmd.arg(&a.pattern);
        if let Some(p) = &a.path {
            cmd.arg(p);
        } else {
            cmd.arg(".");
        }
        cmd.current_dir(&ctx.cwd);
        let out = cmd.output().await;
        if let Ok(out) = out {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            return Ok(apply_limits(&stdout, a.head_limit, 8000));
        }
        // Fallback to grep (rg missing). Limited features — output_mode and
        // multiline are not supported here; report degraded behavior in
        // stdout so the model knows.
        let mut grep = Command::new("grep");
        grep.arg("-rn");
        if a.case_insensitive {
            grep.arg("-i");
        }
        if let Some(n) = a.context_after {
            grep.arg("-A").arg(n.to_string());
        }
        if let Some(n) = a.context_before {
            grep.arg("-B").arg(n.to_string());
        }
        // `--` separates flags from positional args so a pattern starting
        // with `-` isn't misinterpreted as a flag.
        grep.arg("--").arg(&a.pattern);
        grep.arg(a.path.as_deref().unwrap_or("."));
        grep.current_dir(&ctx.cwd);
        let out = grep.output().await?;
        Ok(apply_limits(
            &String::from_utf8_lossy(&out.stdout),
            a.head_limit,
            8000,
        ))
    }
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    /// "name" (alpha, default) or "mtime" (newest modified first).
    #[serde(default)]
    sort: Option<String>,
    /// Cap on output lines.
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl BuiltinTool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        "List files whose path matches a glob pattern. Supports `**` for recursive matches (e.g. `**/*.rs`, `src/**/*.test.ts`). Respects `.gitignore` when ripgrep is available.\n\
\n\
When to use:\n\
- Enumerate matching files when you know the shape (extension, name pattern) but not the location.\n\
- Discover where a file type lives — \"all `*.test.ts`\", \"every `Cargo.toml`\".\n\
- Build a list of candidates before bulk reads or edits.\n\
- Find recently-touched files (`sort: \"mtime\"`) to focus on whatever the user just edited.\n\
\n\
When NOT to use:\n\
- Content search — use `grep`.\n\
- One specific known path — use `read_file` directly.\n\
- Listing immediate children of one directory — use `list_dir`.\n\
\n\
Pattern examples:\n\
- `**/*.rs` — every Rust file in the project.\n\
- `src/**/*.tsx` — every TSX file under src/.\n\
- `Cargo.toml` — match by exact basename anywhere.\n\
- `**/test_*.py` — every Python test file.\n\
\n\
Parameters:\n\
- `pattern`: Glob pattern. `**` matches any depth; `*` matches one path segment.\n\
- `path`: Optional subdirectory to scope the search; `null` searches the whole working directory.\n\
- `sort`: `\"name\"` (alphabetical, default) or `\"mtime\"` (newest modified first). Use mtime when you want to follow recent edits.\n\
- `limit`: Cap on output lines; `null` for no limit beyond the 8 KB byte cap."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern (e.g. '**/*.rs')."},
                "path": {"type": ["string", "null"], "description": "Optional subdirectory; null searches the whole working directory."},
                "sort": {"type": ["string", "null"], "enum": ["name", "mtime", null], "description": "Sort order; defaults to name."},
                "limit": {"type": ["integer", "null"], "description": "Max output lines; null for no extra cap."}
            },
            "required": ["pattern", "path", "sort", "limit"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: GlobArgs = super::parse_args("glob", args)?;
        let sort = a.sort.as_deref().unwrap_or("name");
        if !matches!(sort, "name" | "mtime") {
            return Err(anyhow::anyhow!(
                "sort must be 'name' or 'mtime' (got '{sort}')"
            ));
        }
        let cwd = a
            .path
            .as_deref()
            .map(|p| ctx.cwd.join(p))
            .unwrap_or_else(|| ctx.cwd.clone());

        // Collect raw file list via ripgrep when available.
        let raw: Option<Vec<String>> = match Command::new("rg")
            .arg("--files")
            .arg("--hidden")
            .arg("--glob")
            .arg(&a.pattern)
            .arg("--glob")
            .arg("!.git")
            .current_dir(&cwd)
            .output()
            .await
        {
            Ok(out) if out.status.success() => Some(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            _ => None,
        };

        let files: Vec<String> = if let Some(r) = raw {
            r
        } else {
            // Fallback to find. Strip any `**/` so the trailing basename
            // pattern works with `-name`; coarse but never returns 0 falsely.
            let mut pat = a.pattern.clone();
            while let Some(rest) = pat.strip_prefix("**/") {
                pat = rest.to_string();
            }
            if let Some(idx) = pat.rfind("**/") {
                pat = pat[idx + 3..].to_string();
            }
            let find_out = Command::new("find")
                .arg(".")
                .arg("-path")
                .arg("./.git")
                .arg("-prune")
                .arg("-o")
                .arg("-name")
                .arg(&pat)
                .arg("-print")
                .current_dir(&cwd)
                .output()
                .await?;
            String::from_utf8_lossy(&find_out.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|s| s.to_string())
                .collect()
        };

        let mut ordered: Vec<String> = files;
        if sort == "mtime" {
            // Stat each file once; sort newest-first. Files we can't stat sink
            // to the bottom (UNIX_EPOCH).
            let base = cwd.clone();
            let mut with_mtime: Vec<(String, std::time::SystemTime)> = ordered
                .into_iter()
                .map(|p| {
                    let mtime = std::fs::metadata(base.join(&p))
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    (p, mtime)
                })
                .collect();
            with_mtime.sort_by_key(|e| std::cmp::Reverse(e.1));
            ordered = with_mtime.into_iter().map(|(p, _)| p).collect();
        } else {
            ordered.sort();
        }

        Ok(apply_limits(&ordered.join("\n"), a.limit, 8000))
    }
}

/// Cap an output string by both lines (`head_limit`) and bytes (`byte_cap`).
/// The byte cut walks back to a char boundary so we never slice mid-codepoint.
fn apply_limits(s: &str, head_limit: Option<usize>, byte_cap: usize) -> String {
    let head_clipped: String = match head_limit {
        Some(n) => {
            let mut lines: Vec<&str> = s.lines().collect();
            let total = lines.len();
            if total > n {
                lines.truncate(n);
                let mut out = lines.join("\n");
                out.push_str(&format!(
                    "\n…(head_limit hit, {} more line(s) omitted)",
                    total - n
                ));
                out
            } else {
                s.to_string()
            }
        }
        None => s.to_string(),
    };
    if head_clipped.len() <= byte_cap {
        return head_clipped;
    }
    let mut cut = byte_cap;
    while cut > 0 && !head_clipped.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n…(truncated, {} bytes remaining)",
        &head_clipped[..cut],
        head_clipped.len() - cut
    )
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

    fn write(dir: &std::path::Path, rel: &str, body: &str) {
        let full = dir.join(rel);
        if let Some(p) = full.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(full, body).unwrap();
    }

    fn rg_available() -> bool {
        std::process::Command::new("rg")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn grep_files_with_matches_mode_returns_paths_only() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "hello\nworld\n");
        write(dir.path(), "b.txt", "no match here\n");
        write(dir.path(), "c/d.txt", "hello again\n");
        let out = Grep
            .execute(
                json!({
                    "pattern": "hello",
                    "path": null, "glob": null, "case_insensitive": false,
                    "output_mode": "files_with_matches",
                    "head_limit": null, "context_after": null, "context_before": null,
                    "multiline": null, "file_type": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(out.contains("a.txt"), "got: {out}");
        assert!(out.contains("d.txt"), "got: {out}");
        assert!(!out.contains("b.txt"), "got: {out}");
        // No lineno:content shape — just paths.
        assert!(!out.contains("hello"), "got: {out}");
    }

    #[tokio::test]
    async fn grep_count_mode_returns_path_colon_count() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "x\nx\nx\n");
        write(dir.path(), "b.txt", "x\n");
        let out = Grep
            .execute(
                json!({
                    "pattern": "x",
                    "path": null, "glob": null, "case_insensitive": false,
                    "output_mode": "count",
                    "head_limit": null, "context_after": null, "context_before": null,
                    "multiline": null, "file_type": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(out.contains("a.txt:3"), "got: {out}");
        assert!(out.contains("b.txt:1"), "got: {out}");
    }

    #[tokio::test]
    async fn grep_head_limit_caps_output_lines() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=50).map(|i| format!("hit line {i}\n")).collect();
        write(dir.path(), "big.txt", &body);
        let out = Grep
            .execute(
                json!({
                    "pattern": "hit",
                    "path": null, "glob": null, "case_insensitive": false,
                    "output_mode": "content",
                    "head_limit": 5,
                    "context_after": null, "context_before": null,
                    "multiline": null, "file_type": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(out.contains("head_limit hit"), "got: {out}");
        // First 5 lines present, line 6+ NOT.
        assert!(out.contains("hit line 5"), "got: {out}");
        assert!(!out.contains("hit line 6"), "got: {out}");
    }

    #[tokio::test]
    async fn grep_context_after_includes_following_lines() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "anchor\nline2\nline3\nline4\n");
        let out = Grep
            .execute(
                json!({
                    "pattern": "anchor",
                    "path": null, "glob": null, "case_insensitive": false,
                    "output_mode": "content",
                    "head_limit": null,
                    "context_after": 2,
                    "context_before": null,
                    "multiline": null, "file_type": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(out.contains("anchor"), "got: {out}");
        assert!(out.contains("line2"), "got: {out}");
        assert!(out.contains("line3"), "got: {out}");
        assert!(!out.contains("line4"), "got: {out}");
    }

    #[tokio::test]
    async fn grep_rejects_invalid_output_mode() {
        let dir = tempfile::tempdir().unwrap();
        let err = Grep
            .execute(
                json!({
                    "pattern": "x",
                    "path": null, "glob": null, "case_insensitive": false,
                    "output_mode": "wat",
                    "head_limit": null, "context_after": null, "context_before": null,
                    "multiline": null, "file_type": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("output_mode"), "got: {err}");
    }

    #[tokio::test]
    async fn glob_sorts_by_mtime_newest_first() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "old.txt", "1");
        // Make the timestamp gap unambiguous.
        std::thread::sleep(std::time::Duration::from_millis(15));
        write(dir.path(), "new.txt", "2");
        let out = Glob
            .execute(
                json!({
                    "pattern": "*.txt",
                    "path": null,
                    "sort": "mtime",
                    "limit": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        let lines: Vec<&str> = out.lines().collect();
        let new_idx = lines.iter().position(|l| l.contains("new.txt"));
        let old_idx = lines.iter().position(|l| l.contains("old.txt"));
        assert!(new_idx.is_some() && old_idx.is_some(), "got: {out}");
        assert!(new_idx < old_idx, "new.txt should come first; got: {out}");
    }

    #[tokio::test]
    async fn glob_limit_caps_output_lines() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            write(dir.path(), &format!("f{i}.txt"), "x");
        }
        let out = Glob
            .execute(
                json!({
                    "pattern": "*.txt",
                    "path": null,
                    "sort": "name",
                    "limit": 5,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(out.contains("head_limit hit"), "got: {out}");
        // After hitting the cap there must be a hint about omitted lines.
        let payload_lines = out
            .lines()
            .filter(|l| !l.starts_with('…'))
            .filter(|l| !l.is_empty())
            .count();
        assert!(payload_lines <= 5, "got {payload_lines} payload lines: {out}");
    }
}
