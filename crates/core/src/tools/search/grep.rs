//! The `grep` tool: regex search via ripgrep with a `grep -rn` fallback.
//! Split out of `search`; logic unchanged.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::tools::{BuiltinTool, ToolContext};

use super::shared::{
    apply_limits, normalize_path_separators, normalize_search_output_paths,
    resolved_relative_to_cwd, run_capped, walk_files_relative, SEARCH_OUTPUT_CAP_BYTES,
};

pub struct Grep;

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
        deserialize_with = "crate::tools::deserialize_bool"
    )]
    case_insensitive: bool,
    /// "content" (default), "files_with_matches", or "count".
    #[serde(default, alias = "outputMode")]
    output_mode: Option<String>,
    /// Cap on lines of output after the byte cap.
    #[serde(
        default,
        alias = "headLimit",
        deserialize_with = "crate::tools::deserialize_optional_usize"
    )]
    head_limit: Option<usize>,
    /// Skip this many output lines before applying head_limit.
    #[serde(default, deserialize_with = "crate::tools::deserialize_optional_usize")]
    offset: Option<usize>,
    /// Lines of context after each match (rg -A).
    #[serde(
        default,
        alias = "-A",
        alias = "contextAfter",
        deserialize_with = "crate::tools::deserialize_optional_usize"
    )]
    context_after: Option<usize>,
    /// Lines of context before each match (rg -B).
    #[serde(
        default,
        alias = "-B",
        alias = "contextBefore",
        deserialize_with = "crate::tools::deserialize_optional_usize"
    )]
    context_before: Option<usize>,
    /// Lines of context before and after each match (rg -C).
    #[serde(
        default,
        alias = "-C",
        alias = "contextLines",
        deserialize_with = "crate::tools::deserialize_optional_usize"
    )]
    context: Option<usize>,
    /// rg --multiline. Allows patterns to span newlines.
    #[serde(
        default,
        alias = "multiLine",
        deserialize_with = "crate::tools::deserialize_optional_bool"
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
        let a: GrepArgs = crate::tools::parse_args("grep", args)?;
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
    // ripgrep could not be spawned. The external `grep` can't honor
    // `glob`/`file_type`/`multiline`, so when any is requested route straight to
    // the native engine (which now supports all three). Otherwise try external
    // `grep` first, falling back to native if that can't spawn either.
    if a.glob.is_some() || a.file_type.is_some() || a.multiline.unwrap_or(false) {
        return native_grep_search(a, ctx, mode, context_before, context_after);
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
    let (out, overran) = match run_capped(grep, SEARCH_OUTPUT_CAP_BYTES).await {
        Ok(v) => v,
        // Neither ripgrep nor an external `grep` could be spawned (e.g. a stock
        // Windows box with no Unix tooling): fall back to a native, dependency-
        // free search instead of erroring out.
        Err(_) => return native_grep_search(a, ctx, mode, context_before, context_after),
    };
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

/// Native, dependency-free search used when ripgrep is unavailable (e.g. a stock
/// Windows box with no Unix tooling), and the only path that honors
/// `glob`/`file_type`/`multiline` without ripgrep — the external `grep` can't, so
/// `execute_grep_with_commands` routes those straight here. Emits ripgrep-shaped
/// lines (`path:lineno:text`, with `path:lineno-text` context lines and `--`
/// group separators) so the shared limiter applies unchanged. Binary
/// (NUL-containing), unreadable, or very large files are skipped, matching
/// grep/ripgrep's defaults.
fn native_grep_search(
    a: &GrepArgs,
    ctx: &ToolContext,
    mode: &str,
    context_before: Option<usize>,
    context_after: Option<usize>,
) -> Result<String> {
    let multiline = a.multiline.unwrap_or(false);
    let re = regex::RegexBuilder::new(&a.pattern)
        .case_insensitive(a.case_insensitive)
        .multi_line(multiline)
        .dot_matches_new_line(multiline)
        .build()
        .map_err(|e| anyhow::anyhow!("invalid regex: {e}"))?;

    // File list as forward-slash paths relative to ctx.cwd, scoped to `path`.
    let rel_root = match a.path.as_deref() {
        Some(p) => resolved_relative_to_cwd(&ctx.cwd, p)?,
        None => std::path::PathBuf::new(),
    };
    let abs_root = ctx.cwd.join(&rel_root);
    let prefix = normalize_path_separators(&rel_root.to_string_lossy());
    let mut files: Vec<String> = Vec::new();
    if abs_root.is_file() {
        files.push(prefix.clone());
    } else {
        let mut found = Vec::new();
        walk_files_relative(&abs_root, &mut found);
        for f in found {
            files.push(if prefix.is_empty() {
                f
            } else {
                format!("{prefix}/{f}")
            });
        }
    }
    files.sort();

    // The no-ripgrep equivalents of rg's `--glob` / `--type`: drop files whose
    // path doesn't match the glob, or whose basename extension isn't in the
    // requested file type. ANDed when both are present.
    if let Some(g) = a.glob.as_deref() {
        files.retain(|rel| glob_matches_rel(g, rel));
    }
    if let Some(t) = a.file_type.as_deref() {
        let exts = file_type_extensions(t).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown file_type '{t}' for the no-ripgrep fallback; install ripgrep for full --type support, or filter with `glob` instead"
            )
        })?;
        files.retain(|rel| file_has_extension(rel, exts));
    }

    let before = context_before.unwrap_or(0);
    let after = context_after.unwrap_or(0);
    let mut out = String::new();
    for rel in &files {
        let abs = ctx.cwd.join(rel);
        let Ok(meta) = std::fs::metadata(&abs) else {
            continue;
        };
        if meta.len() > 50 * 1024 * 1024 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&abs) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let is_match = line_match_flags(&re, &text, &lines, multiline);
        match mode {
            "files_with_matches" => {
                if is_match.iter().any(|&m| m) {
                    out.push_str(rel);
                    out.push('\n');
                }
            }
            "count" => {
                let n = is_match.iter().filter(|&&m| m).count();
                if n > 0 {
                    out.push_str(&format!("{rel}:{n}\n"));
                }
            }
            _ => {
                let n = lines.len();
                let mut want = vec![false; n];
                for (i, &matched) in is_match.iter().enumerate() {
                    if matched {
                        let lo = i.saturating_sub(before);
                        let hi = (i + after).min(n.saturating_sub(1));
                        for w in want.iter_mut().take(hi + 1).skip(lo) {
                            *w = true;
                        }
                    }
                }
                let mut prev: Option<usize> = None;
                for i in 0..n {
                    if !want[i] {
                        continue;
                    }
                    if prev.is_some_and(|p| i > p + 1) {
                        out.push_str("--\n");
                    }
                    let sep = if is_match[i] { ':' } else { '-' };
                    out.push_str(&format!("{rel}:{}{}{}\n", i + 1, sep, lines[i]));
                    prev = Some(i);
                }
            }
        }
    }

    Ok(apply_limits(
        out.trim_end_matches('\n'),
        a.head_limit,
        a.offset,
        8000,
    ))
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

/// Match a glob against a cwd-relative path for the no-ripgrep fallback,
/// mirroring ripgrep's `--glob`: a pattern with no `/` matches the basename at
/// any depth (gitignore-style), while a slashed pattern matches the full
/// relative path. Same semantics as the `glob` tool's own fallback matcher.
fn glob_matches_rel(pattern: &str, rel_path: &str) -> bool {
    if pattern.contains('/') {
        crate::hooks::glob_match(pattern, rel_path)
    } else {
        let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
        crate::hooks::glob_match(pattern, base)
    }
}

/// True when `rel_path`'s basename has an extension (lowercased) in `exts`.
fn file_has_extension(rel_path: &str, exts: &[&str]) -> bool {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    match base.rsplit_once('.') {
        Some((_, ext)) => {
            let ext = ext.to_ascii_lowercase();
            exts.iter().any(|&x| x == ext)
        }
        None => false,
    }
}

/// Map a subset of ripgrep's `--type` names to file extensions for the
/// no-ripgrep fallback. Not exhaustive (ripgrep ships a large table); covers the
/// common languages a model reaches for. `None` for an unrecognized type, which
/// the caller surfaces as a clear error rather than silently matching nothing.
fn file_type_extensions(t: &str) -> Option<&'static [&'static str]> {
    Some(match t.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => &["rs"],
        "ts" | "typescript" => &["ts", "cts", "mts"],
        "tsx" => &["tsx"],
        "js" | "javascript" => &["js", "jsx", "mjs", "cjs"],
        "jsx" => &["jsx"],
        "py" | "python" => &["py", "pyi"],
        "go" | "golang" => &["go"],
        "c" => &["c", "h"],
        "cpp" | "c++" | "cxx" | "cc" => &["cpp", "cc", "cxx", "hpp", "hh", "hxx", "h"],
        "java" => &["java"],
        "kotlin" | "kt" => &["kt", "kts"],
        "rb" | "ruby" => &["rb"],
        "php" => &["php"],
        "swift" => &["swift"],
        "sh" | "shell" | "bash" => &["sh", "bash", "zsh"],
        "json" => &["json"],
        "yaml" | "yml" => &["yaml", "yml"],
        "toml" => &["toml"],
        "md" | "markdown" => &["md", "markdown"],
        "html" => &["html", "htm"],
        "css" => &["css"],
        "scss" | "sass" => &["scss", "sass"],
        "xml" => &["xml"],
        "sql" => &["sql"],
        _ => return None,
    })
}

/// Per-line match flags for `lines`. Non-multiline: each line is tested on its
/// own (matching grep's line-oriented default). Multiline: match the whole
/// `text`, then flag every line a match overlaps — so a pattern spanning a
/// newline reports each line it touches.
fn line_match_flags(re: &regex::Regex, text: &str, lines: &[&str], multiline: bool) -> Vec<bool> {
    if !multiline {
        return lines.iter().map(|l| re.is_match(l)).collect();
    }
    let mut flags = vec![false; lines.len()];
    if lines.is_empty() {
        return flags;
    }
    // Byte offset where each `lines()` entry begins in `text`. `str::lines`
    // splits on '\n' and strips a trailing '\r', so step past both terminators.
    let bytes = text.as_bytes();
    let mut starts = Vec::with_capacity(lines.len());
    let mut off = 0usize;
    for l in lines {
        starts.push(off);
        off += l.len();
        if off < bytes.len() && bytes[off] == b'\r' {
            off += 1;
        }
        if off < bytes.len() && bytes[off] == b'\n' {
            off += 1;
        }
    }
    let line_of = |pos: usize| match starts.binary_search(&pos) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    for m in re.find_iter(text) {
        let first = line_of(m.start());
        // Inclusive last byte of the match; an empty match maps to its start line.
        let last = line_of(m.end().saturating_sub(1).max(m.start())).min(flags.len() - 1);
        for f in flags.iter_mut().take(last + 1).skip(first) {
            *f = true;
        }
    }
    flags
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

#[cfg(test)]
mod tests;
