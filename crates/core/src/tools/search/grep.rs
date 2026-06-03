//! The `grep` tool: regex search via ripgrep with a `grep -rn` fallback.
//! Split out of `search`; logic unchanged.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::tools::{BuiltinTool, ToolContext};

use super::shared::{
    apply_limits, normalize_search_output_paths, resolved_relative_to_cwd, run_capped,
    SEARCH_OUTPUT_CAP_BYTES,
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
