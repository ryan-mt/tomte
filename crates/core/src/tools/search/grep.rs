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

mod native;
mod rg;

use native::*;
use rg::*;

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

#[cfg(test)]
mod tests;
