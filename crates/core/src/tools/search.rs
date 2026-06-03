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
    #[serde(
        default,
        alias = "-i",
        alias = "ignore_case",
        alias = "ignoreCase",
        alias = "caseInsensitive",
        deserialize_with = "super::deserialize_bool"
    )]
    case_insensitive: bool,
    /// "content" (default), "files_with_matches", or "count".
    #[serde(default, alias = "outputMode")]
    output_mode: Option<String>,
    /// Cap on lines of output after the byte cap.
    #[serde(
        default,
        alias = "headLimit",
        deserialize_with = "super::deserialize_optional_usize"
    )]
    head_limit: Option<usize>,
    /// Skip this many output lines before applying head_limit.
    #[serde(default, deserialize_with = "super::deserialize_optional_usize")]
    offset: Option<usize>,
    /// Lines of context after each match (rg -A).
    #[serde(
        default,
        alias = "-A",
        alias = "contextAfter",
        deserialize_with = "super::deserialize_optional_usize"
    )]
    context_after: Option<usize>,
    /// Lines of context before each match (rg -B).
    #[serde(
        default,
        alias = "-B",
        alias = "contextBefore",
        deserialize_with = "super::deserialize_optional_usize"
    )]
    context_before: Option<usize>,
    /// Lines of context before and after each match (rg -C).
    #[serde(
        default,
        alias = "-C",
        alias = "contextLines",
        deserialize_with = "super::deserialize_optional_usize"
    )]
    context: Option<usize>,
    /// rg --multiline. Allows patterns to span newlines.
    #[serde(
        default,
        alias = "multiLine",
        deserialize_with = "super::deserialize_optional_bool"
    )]
    multiline: Option<bool>,
    /// rg --type filter, e.g. "rust", "ts", "py".
    #[serde(default, alias = "type", alias = "fileType")]
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
- `offset`: Skip this many output lines before applying `head_limit`; useful for paging broad results.\n\
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
                "offset": {"type": ["integer", "null"], "description": "Skip this many output lines before applying head_limit; null for none."},
                "context_after": {"type": ["integer", "null"], "description": "Lines AFTER each match (content mode); null for none."},
                "context_before": {"type": ["integer", "null"], "description": "Lines BEFORE each match (content mode); null for none."},
                "multiline": {"type": ["boolean", "null"], "description": "Enable --multiline so patterns span newlines."},
                "file_type": {"type": ["string", "null"], "description": "Restrict to a ripgrep named file type (rust, ts, py, ...); null for all."}
            },
            "required": [
                "pattern", "path", "glob", "case_insensitive",
                "output_mode", "head_limit", "offset", "context_after", "context_before",
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
        execute_grep_with_commands(&a, ctx, "rg", "grep").await
    }
}

async fn execute_grep_with_commands(
    a: &GrepArgs,
    ctx: &ToolContext,
    rg_program: &str,
    grep_program: &str,
) -> Result<String> {
    let Some(mode) = normalize_grep_output_mode(a.output_mode.as_deref()) else {
        let mode = a.output_mode.as_deref().unwrap_or("<null>");
        return Err(anyhow::anyhow!(
            "output_mode must be 'content', 'files_with_matches', or 'count' (got '{mode}')"
        ));
    };
    let context_after = a.context_after.or(a.context);
    let context_before = a.context_before.or(a.context);

    let mut cmd = Command::new(rg_program);
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
            if let Some(n) = context_after {
                cmd.arg("-A").arg(n.to_string());
            }
            if let Some(n) = context_before {
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
    // `--` stops flag parsing so a pattern beginning with `-` (e.g. `-rf`)
    // is searched literally instead of being read as ripgrep flags. The
    // grep fallback below already does this.
    cmd.arg("--").arg(&a.pattern);
    if let Some(p) = &a.path {
        cmd.arg(resolved_relative_to_cwd(&ctx.cwd, p)?);
    } else {
        cmd.arg(".");
    }
    cmd.current_dir(&ctx.cwd);
    let out = run_capped(cmd, SEARCH_OUTPUT_CAP_BYTES).await;
    if let Ok((out, overran)) = out {
        // rg exits 0 on matches and 1 on "no matches" (both fine); exit 2+
        // is a real error (invalid regex, bad glob). Surface that instead of
        // returning empty stdout, which the model reads as "no matches".
        // When `overran`, we killed the child at the output cap ourselves, so
        // its non-success/signal status is expected — keep the capped matches.
        if !overran && !out.status.success() && out.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let msg = stderr.trim();
            return Err(anyhow::anyhow!(
                "ripgrep failed: {}",
                if msg.is_empty() { "unknown error" } else { msg }
            ));
        }
        let stdout = normalize_search_output_paths(&String::from_utf8_lossy(&out.stdout), mode);
        return Ok(apply_limits(&stdout, a.head_limit, a.offset, 8000));
    }
    if a.multiline.unwrap_or(false) {
        return Err(grep_fallback_unsupported("multiline"));
    }
    if a.glob.is_some() {
        return Err(grep_fallback_unsupported("glob"));
    }
    if a.file_type.is_some() {
        return Err(grep_fallback_unsupported("file_type"));
    }

    let mut grep = Command::new(grep_program);
    grep.arg("-E").arg("-r");
    match mode {
        "files_with_matches" => {
            grep.arg("-l");
        }
        "count" => {
            grep.arg("-c");
        }
        _ => {
            grep.arg("-n");
            if let Some(n) = context_after {
                grep.arg("-A").arg(n.to_string());
            }
            if let Some(n) = context_before {
                grep.arg("-B").arg(n.to_string());
            }
        }
    }
    if a.case_insensitive {
        grep.arg("-i");
    }
    // `--` separates flags from positional args so a pattern starting
    // with `-` isn't misinterpreted as a flag.
    grep.arg("--").arg(&a.pattern);
    match a.path.as_deref() {
        Some(p) => grep.arg(resolved_relative_to_cwd(&ctx.cwd, p)?),
        None => grep.arg("."),
    };
    grep.current_dir(&ctx.cwd);
    let (out, overran) = run_capped(grep, SEARCH_OUTPUT_CAP_BYTES).await?;
    if !overran && !out.status.success() && out.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = stderr.trim();
        return Err(anyhow::anyhow!(
            "grep fallback failed: {}",
            if msg.is_empty() { "unknown error" } else { msg }
        ));
    }
    let stdout = normalize_search_output_paths(&String::from_utf8_lossy(&out.stdout), mode);
    let stdout = if mode == "count" {
        filter_zero_count_lines(&stdout)
    } else {
        stdout
    };
    Ok(apply_limits(&stdout, a.head_limit, a.offset, 8000))
}

/// Hard ceiling on raw search stdout captured into memory before
/// [`apply_limits`] trims it. Generous enough to preserve deep `offset`
/// pagination, but bounded so a pathological match set (e.g. every line of a
/// giant minified file) can't balloon memory. ~4 MiB.
const SEARCH_OUTPUT_CAP_BYTES: usize = 4 * 1024 * 1024;

/// Like [`Command::output`], but stops capturing stdout at `cap` bytes and
/// kills the child if it overruns, so it can neither balloon memory nor block
/// forever writing to a full pipe once we stop reading. Returns the output
/// alongside a flag that is `true` when the stdout cap was hit (and the child
/// was therefore killed) — callers use it to tell our own cap-kill apart from a
/// genuine non-zero exit.
async fn run_capped(mut cmd: Command, cap: usize) -> std::io::Result<(std::process::Output, bool)> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    // Drain stderr concurrently and all the way to EOF (retaining only the
    // first 64 KiB) in its own task. A child that floods stderr — e.g. `grep`
    // emitting one "Permission denied" line per directory over a large tree —
    // would otherwise block on a full stderr pipe and never close stdout,
    // hanging the stdout read below until the outer tool timeout.
    let stderr_task = tokio::spawn(async move {
        let mut err = Vec::new();
        let _ = drain_capped(&mut stderr, &mut err, 64 * 1024).await;
        err
    });

    let mut out = Vec::new();
    let overran = read_to_cap(&mut stdout, &mut out, cap).await?;
    if overran {
        // We stopped reading; kill so the child doesn't block writing to a
        // full pipe while we wait for it to exit.
        let _ = child.start_kill();
    }
    let status = child.wait().await?;
    let err = stderr_task.await.unwrap_or_default();
    Ok((
        std::process::Output {
            status,
            stdout: out,
            stderr: err,
        },
        overran,
    ))
}

/// Read from `r` into `buf` until EOF or `cap` bytes. Returns `true` when the
/// cap was reached (more data may remain unread).
async fn read_to_cap<R>(r: &mut R, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<bool>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut chunk = [0u8; 8192];
    while buf.len() < cap {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(false);
        }
        let room = cap - buf.len();
        buf.extend_from_slice(&chunk[..n.min(room)]);
        if n > room {
            return Ok(true);
        }
    }
    Ok(true)
}

/// Read from `r` all the way to EOF, retaining at most `cap` bytes in `buf` and
/// discarding the rest. Unlike [`read_to_cap`], this never stops early, so the
/// child can never block on a full pipe — used for stderr, which we keep only a
/// bounded prefix of but must fully drain to avoid deadlocking the stdout read.
async fn drain_capped<R>(r: &mut R, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut chunk = [0u8; 8192];
    loop {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        if buf.len() < cap {
            let room = cap - buf.len();
            buf.extend_from_slice(&chunk[..n.min(room)]);
        }
    }
}

fn normalize_grep_output_mode(mode: Option<&str>) -> Option<&'static str> {
    let Some(mode) = mode else {
        return Some("content");
    };
    let normalized = mode.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match normalized.as_str() {
        "" | "null" | "content" | "match" | "matches" | "lines" => Some("content"),
        "files_with_matches" | "fileswithmatches" | "files" | "paths" | "filenames"
        | "files_only" | "filesonly" | "paths_only" | "pathsonly" => Some("files_with_matches"),
        "count" | "counts" | "count_matches" | "countmatches" => Some("count"),
        _ => None,
    }
}

fn grep_fallback_unsupported(feature: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "ripgrep is not available and the grep fallback does not support '{feature}'; install ripgrep or remove that option"
    )
}

fn resolved_relative_to_cwd(cwd: &std::path::Path, path: &str) -> Result<std::path::PathBuf> {
    let resolved = super::fs::resolve(cwd, path)?;
    let root = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    Ok(resolved
        .strip_prefix(&root)
        .map(|p| p.to_path_buf())
        .unwrap_or(resolved))
}

fn filter_zero_count_lines(stdout: &str) -> String {
    stdout
        .lines()
        .filter(|line| {
            let Some((_, count)) = line.rsplit_once(':') else {
                return true;
            };
            count.trim() != "0"
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    #[serde(default, deserialize_with = "super::deserialize_optional_usize")]
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
        let Some(sort) = normalize_glob_sort(a.sort.as_deref()) else {
            let sort = a.sort.as_deref().unwrap_or("<null>");
            return Err(anyhow::anyhow!(
                "sort must be 'name' or 'mtime' (got '{sort}')"
            ));
        };
        let root = ctx.cwd.canonicalize().unwrap_or_else(|_| ctx.cwd.clone());
        let cwd = match a.path.as_deref() {
            Some(p) => super::fs::resolve(&ctx.cwd, p)?,
            None => ctx.cwd.clone(),
        };

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
            // exit 0 = matches, exit 1 = no matches — both authoritative, so an
            // empty result must NOT fall through to the looser basename-only
            // `find` matcher. Only a missing/errored rg (spawn err, exit 2+)
            // falls back.
            Ok(out) if out.status.success() || out.status.code() == Some(1) => Some(
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
            // No ripgrep: enumerate files with `find`, then filter in-process
            // with a matcher that respects path structure. The previous
            // `-name <basename>` degraded `src/**/*.rs` to matching `*.rs` in
            // every directory and dropped any pattern containing a slash
            // (`-name` never matches a `/`).
            let find_out = Command::new("find")
                .arg(".")
                .arg("-path")
                .arg("./.git")
                .arg("-prune")
                .arg("-o")
                .arg("-type")
                .arg("f")
                .arg("-print")
                .current_dir(&cwd)
                .output()
                .await?;
            String::from_utf8_lossy(&find_out.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .filter(|l| glob_fallback_matches(&a.pattern, l.strip_prefix("./").unwrap_or(l)))
                .map(|s| s.to_string())
                .collect()
        };

        let mut ordered = relativize_glob_results(files, &cwd, &root);
        if sort == "mtime" {
            // Stat each file once; sort newest-first. Files we can't stat sink
            // to the bottom (UNIX_EPOCH).
            let base = root.clone();
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

        Ok(apply_limits(&ordered.join("\n"), a.limit, None, 8000))
    }
}

fn normalize_glob_sort(sort: Option<&str>) -> Option<&'static str> {
    let Some(sort) = sort else {
        return Some("name");
    };
    let normalized = sort.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match normalized.as_str() {
        "" | "null" | "name" | "names" | "alpha" | "alphabetical" | "alphabetic" | "filename"
        | "file_name" | "path" | "paths" => Some("name"),
        "mtime" | "modified" | "modified_time" | "modtime" | "time" | "recent" | "recently"
        | "newest" | "date" => Some("mtime"),
        _ => None,
    }
}

fn relativize_glob_results(
    files: Vec<String>,
    search_root: &std::path::Path,
    cwd: &std::path::Path,
) -> Vec<String> {
    files
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix("./").unwrap_or(&p);
            let full = search_root.join(rel);
            full.strip_prefix(cwd)
                .map(path_to_slash_string)
                .unwrap_or_else(|_| normalize_path_separators(rel))
        })
        .collect()
}

fn path_to_slash_string(path: &std::path::Path) -> String {
    normalize_path_separators(&path.to_string_lossy())
}

fn normalize_path_separators(path: &str) -> String {
    path.replace('\\', "/")
}

fn normalize_search_output_paths(output: &str, mode: &str) -> String {
    output
        .lines()
        .map(|line| normalize_search_output_line(line, mode))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_search_output_line(line: &str, mode: &str) -> String {
    if line.is_empty() || line == "--" {
        return line.to_string();
    }

    if matches!(mode, "files_with_matches") {
        return normalize_path_separators(line);
    }

    let Some(idx) = line.find([':', '-']) else {
        return normalize_path_separators(line);
    };
    let (path, rest) = line.split_at(idx);
    format!("{}{}", normalize_path_separators(path), rest)
}

/// Match a glob against a relative path for the no-ripgrep `find` fallback,
/// approximating ripgrep's `--glob` semantics: a pattern containing no `/`
/// matches the file's basename at any depth (gitignore-style), while a pattern
/// with a `/` matches the full relative path. Built on the shared
/// [`crate::hooks::glob_match`] so `**`/`*`/`?` behave consistently.
fn glob_fallback_matches(pattern: &str, rel_path: &str) -> bool {
    if pattern.contains('/') {
        crate::hooks::glob_match(pattern, rel_path)
    } else {
        let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
        crate::hooks::glob_match(pattern, base)
    }
}

/// Cap an output string by offset, lines (`head_limit`), and bytes (`byte_cap`).
/// The byte cut walks back to a char boundary so we never slice mid-codepoint.
fn apply_limits(
    s: &str,
    head_limit: Option<usize>,
    offset: Option<usize>,
    byte_cap: usize,
) -> String {
    let offset = offset.unwrap_or(0);
    let line_clipped: String = if offset > 0 || head_limit.is_some() {
        let lines: Vec<&str> = s.lines().collect();
        let total = lines.len();
        let start = offset.min(total);
        let mut end = total;
        if let Some(n) = head_limit {
            end = (start + n).min(total);
        }
        let mut out = lines[start..end].join("\n");
        if offset > 0 {
            let skipped = start;
            let note = format!("…(offset skipped {skipped} line(s))");
            if out.is_empty() {
                out = note;
            } else {
                out = format!("{note}\n{out}");
            }
        }
        if end < total {
            out.push_str(&format!(
                "\n…(head_limit hit, {} more line(s) omitted)",
                total - end
            ));
        }
        out
    } else {
        s.to_string()
    };
    if line_clipped.len() <= byte_cap {
        return line_clipped;
    }
    let mut cut = byte_cap;
    while cut > 0 && !line_clipped.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n…(truncated, {} bytes remaining)",
        &line_clipped[..cut],
        line_clipped.len() - cut
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
            require_approval: false,
            auto_approve_edits: false,
            non_interactive: false,
            session: Arc::new(Mutex::new(SessionState::default())),
            config: crate::config::Config::default(),
            cwd_override: Arc::new(Mutex::new(None)),
            events: None,
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

    fn grep_available() -> bool {
        std::process::Command::new("grep")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn grep_args(pattern: &str) -> GrepArgs {
        GrepArgs {
            pattern: pattern.to_string(),
            path: None,
            glob: None,
            case_insensitive: false,
            output_mode: None,
            head_limit: None,
            offset: None,
            context_after: None,
            context_before: None,
            context: None,
            multiline: None,
            file_type: None,
        }
    }

    #[test]
    fn grep_args_accept_camel_case_aliases() {
        let args: GrepArgs = serde_json::from_value(json!({
            "pattern": "needle",
            "caseInsensitive": "true",
            "outputMode": "files-with-matches",
            "headLimit": "10",
            "offset": "3",
            "contextAfter": "2",
            "contextBefore": 1,
            "contextLines": null,
            "multiLine": "yes",
            "fileType": "rust"
        }))
        .unwrap();

        assert!(args.case_insensitive);
        assert_eq!(args.output_mode.as_deref(), Some("files-with-matches"));
        assert_eq!(args.head_limit, Some(10));
        assert_eq!(args.offset, Some(3));
        assert_eq!(args.context_after, Some(2));
        assert_eq!(args.context_before, Some(1));
        assert_eq!(args.context, None);
        assert_eq!(args.multiline, Some(true));
        assert_eq!(args.file_type.as_deref(), Some("rust"));
    }

    #[test]
    fn grep_output_mode_accepts_common_model_aliases() {
        assert_eq!(normalize_grep_output_mode(None), Some("content"));
        assert_eq!(normalize_grep_output_mode(Some("matches")), Some("content"));
        assert_eq!(
            normalize_grep_output_mode(Some("files-with-matches")),
            Some("files_with_matches")
        );
        assert_eq!(
            normalize_grep_output_mode(Some("paths_only")),
            Some("files_with_matches")
        );
        assert_eq!(normalize_grep_output_mode(Some("counts")), Some("count"));
        assert_eq!(normalize_grep_output_mode(Some("wat")), None);
    }

    fn missing_rg(dir: &std::path::Path) -> String {
        dir.join("opencli-missing-rg").display().to_string()
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
    async fn grep_fallback_files_with_matches_mode_returns_paths_only() {
        if !grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "hello\nworld\n");
        write(dir.path(), "b.txt", "no match here\n");
        write(dir.path(), "c/d.txt", "hello again\n");

        let mut args = grep_args("hello");
        args.output_mode = Some("files_with_matches".to_string());
        let out = execute_grep_with_commands(
            &args,
            &ctx(dir.path().to_path_buf()),
            &missing_rg(dir.path()),
            "grep",
        )
        .await
        .unwrap();

        assert!(out.contains("a.txt"), "got: {out}");
        assert!(out.contains("d.txt"), "got: {out}");
        assert!(!out.contains("b.txt"), "got: {out}");
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
                    "head_limit": null, "offset": null, "context_after": null, "context_before": null,
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
    async fn grep_fallback_count_mode_returns_matching_files_only() {
        if !grep_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "x\nx\n");
        write(dir.path(), "b.txt", "no hit\n");

        let mut args = grep_args("x");
        args.output_mode = Some("count".to_string());
        let out = execute_grep_with_commands(
            &args,
            &ctx(dir.path().to_path_buf()),
            &missing_rg(dir.path()),
            "grep",
        )
        .await
        .unwrap();

        assert!(out.contains("a.txt:2"), "got: {out}");
        assert!(!out.contains("b.txt:0"), "got: {out}");
    }

    #[tokio::test]
    async fn grep_fallback_rejects_unsupported_glob_instead_of_ignoring_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = grep_args("x");
        args.glob = Some("*.rs".to_string());

        let err = execute_grep_with_commands(
            &args,
            &ctx(dir.path().to_path_buf()),
            &missing_rg(dir.path()),
            "grep",
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string().contains("does not support 'glob'"),
            "got: {err}"
        );
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
                    "offset": null,
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
    async fn grep_offset_skips_lines_before_head_limit() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=8).map(|i| format!("hit line {i}\n")).collect();
        write(dir.path(), "big.txt", &body);
        let out = Grep
            .execute(
                json!({
                    "pattern": "hit",
                    "path": null, "glob": null, "case_insensitive": false,
                    "output_mode": "content",
                    "head_limit": 2,
                    "offset": 3,
                    "context_after": null, "context_before": null,
                    "multiline": null, "file_type": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(out.contains("offset skipped 3"), "got: {out}");
        assert!(!out.contains("hit line 3"), "got: {out}");
        assert!(out.contains("hit line 4"), "got: {out}");
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
                    "offset": null,
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
    async fn grep_accepts_claude_flag_aliases() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "before\nALPHA\nafter\n");

        let out = Grep
            .execute(
                json!({
                    "pattern": "alpha",
                    "output_mode": "content",
                    "-i": "true",
                    "-C": "1"
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();

        assert!(out.contains("before"), "got: {out}");
        assert!(out.contains("ALPHA"), "got: {out}");
        assert!(out.contains("after"), "got: {out}");
    }

    #[tokio::test]
    async fn grep_with_path_returns_paths_relative_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/lib.rs", "needle\n");

        let out = Grep
            .execute(
                json!({
                    "pattern": "needle",
                    "path": "src",
                    "glob": null,
                    "case_insensitive": false,
                    "output_mode": "content",
                    "head_limit": null,
                    "offset": null,
                    "context_after": null,
                    "context_before": null,
                    "multiline": null,
                    "file_type": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();

        assert!(out.contains("src/lib.rs:1:needle"), "got: {out}");
        assert!(
            !out.contains(&dir.path().display().to_string()),
            "grep output should be cwd-relative: {out}"
        );
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

    #[test]
    fn glob_sort_accepts_common_model_aliases() {
        assert_eq!(normalize_glob_sort(None), Some("name"));
        assert_eq!(normalize_glob_sort(Some("alphabetical")), Some("name"));
        assert_eq!(normalize_glob_sort(Some("file-name")), Some("name"));
        assert_eq!(normalize_glob_sort(Some("modified")), Some("mtime"));
        assert_eq!(normalize_glob_sort(Some("recent")), Some("mtime"));
        assert_eq!(normalize_glob_sort(Some("newest")), Some("mtime"));
        assert_eq!(normalize_glob_sort(Some("random")), None);
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
        assert!(
            payload_lines <= 5,
            "got {payload_lines} payload lines: {out}"
        );
    }

    #[tokio::test]
    async fn glob_with_path_returns_paths_relative_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/lib.rs", "x");

        let out = Glob
            .execute(
                json!({
                    "pattern": "*.rs",
                    "path": "src",
                    "sort": "name",
                    "limit": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();

        assert_eq!(out.trim(), "src/lib.rs");
    }

    #[tokio::test]
    async fn grep_dash_pattern_is_searched_literally() {
        // Regression: a pattern starting with `-` must be searched literally,
        // not parsed as ripgrep flags (fixed by the `--` separator).
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "this line has -rf in it\nplain line\n");
        let out = Grep
            .execute(
                json!({
                    "pattern": "-rf",
                    "path": null, "glob": null, "case_insensitive": false,
                    "output_mode": "content",
                    "head_limit": null, "offset": null, "context_after": null, "context_before": null,
                    "multiline": null, "file_type": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(
            out.contains("-rf"),
            "dash pattern must match literally; got: {out}"
        );
    }

    #[tokio::test]
    async fn grep_invalid_regex_surfaces_error_not_empty() {
        // Regression: an invalid regex (rg exit 2) must surface an error, not an
        // empty string the model reads as "no matches found".
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "content\n");
        let res = Grep
            .execute(
                json!({
                    "pattern": "(",
                    "path": null, "glob": null, "case_insensitive": false,
                    "output_mode": "content",
                    "head_limit": null, "offset": null, "context_after": null, "context_before": null,
                    "multiline": null, "file_type": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await;
        assert!(
            res.is_err(),
            "invalid regex must surface an error, not empty output; got: {res:?}"
        );
    }

    #[tokio::test]
    async fn glob_no_match_returns_empty_not_all_files() {
        // Regression: a non-matching glob must return empty (rg exit 1 is
        // authoritative), not fall through to the looser basename `find` that
        // returned every file.
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "x");
        write(dir.path(), "b.rs", "y");
        let out = Glob
            .execute(
                json!({
                    "pattern": "nonexistent_dir_xyz/**/*.rs",
                    "path": null,
                    "sort": "name",
                    "limit": null,
                }),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(
            out.trim().is_empty(),
            "non-matching glob must return empty, not all .rs files; got: {out}"
        );
    }

    #[test]
    fn glob_fallback_respects_path_structure() {
        // Regression for the no-rg fallback: a directory-scoped pattern must
        // not match files outside that directory (old `-name` matched the
        // basename everywhere), and a slashed pattern must still match (old
        // `-name 'src/foo*.rs'` matched nothing because `-name` ignores `/`).
        assert!(glob_fallback_matches("src/**/*.tsx", "src/a/b.tsx"));
        assert!(!glob_fallback_matches("src/**/*.tsx", "other/b.tsx"));
        assert!(glob_fallback_matches("src/foo*.rs", "src/foobar.rs"));
        assert!(!glob_fallback_matches("src/foo*.rs", "src/sub/foo.rs"));
        // A pattern without a slash matches the basename at any depth.
        assert!(glob_fallback_matches("*.rs", "a/b/c.rs"));
        assert!(!glob_fallback_matches("*.rs", "a/b/c.tsx"));
    }

    #[test]
    fn search_output_paths_use_forward_slashes() {
        assert_eq!(
            normalize_search_output_line(r"src\lib.rs:1:needle", "content"),
            "src/lib.rs:1:needle"
        );
        assert_eq!(
            normalize_search_output_line(r"src\lib.rs-2-context", "content"),
            "src/lib.rs-2-context"
        );
        assert_eq!(
            normalize_search_output_line(r"src\lib.rs", "files_with_matches"),
            "src/lib.rs"
        );
    }

    #[tokio::test]
    async fn read_to_cap_stops_at_limit() {
        let data = [b'x'; 10_000];
        let mut buf = Vec::new();
        let overran = read_to_cap(&mut &data[..], &mut buf, 4096).await.unwrap();
        assert!(overran);
        assert_eq!(buf.len(), 4096);
    }

    #[tokio::test]
    async fn read_to_cap_reads_everything_under_limit() {
        let data = [b'x'; 100];
        let mut buf = Vec::new();
        let overran = read_to_cap(&mut &data[..], &mut buf, 4096).await.unwrap();
        assert!(!overran);
        assert_eq!(buf.len(), 100);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_capped_bounds_and_kills_a_stdout_flood() {
        // `yes` writes to stdout forever; run_capped must bound it and return
        // instead of hanging or growing without limit, and report the overrun.
        let (out, overran) = run_capped(Command::new("yes"), 8192).await.unwrap();
        assert!(out.stdout.len() <= 8192 + 8192);
        assert!(overran, "a stdout flood must report overran=true");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_capped_does_not_deadlock_on_a_stderr_flood() {
        // A child that floods stderr while writing little/no stdout must not
        // hang run_capped: stderr is drained concurrently to EOF. We bound the
        // test with our own timeout so a regression fails fast instead of
        // hanging the suite.
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            // ~512 KiB to stderr (well past the 64 KiB stderr cap and the OS
            // pipe buffer), a tiny bit to stdout.
            .arg("yes ERR | head -c 524288 1>&2; echo ok");
        let fut = run_capped(cmd, 4 * 1024 * 1024);
        let (out, overran) = tokio::time::timeout(std::time::Duration::from_secs(10), fut)
            .await
            .expect("run_capped must not deadlock on a stderr flood")
            .unwrap();
        assert!(!overran, "stdout stayed under cap");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
        assert!(out.stderr.len() <= 64 * 1024, "stderr retained is capped");
    }
}
