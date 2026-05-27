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
}

#[async_trait]
impl BuiltinTool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "Search for a regular-expression pattern across files in the working directory. Uses ripgrep when available (which respects `.gitignore`), and falls back to plain `grep -rn`.\n\
\n\
Output format: one match per line, `path:lineno:content`. Output is capped at ~8000 bytes; refine the pattern, narrow with `glob`, or scope with `path` if you need to see more.\n\
\n\
Use this when you want to know *where* something appears. Prefer `glob` when you only need filenames matching a pattern. Prefer `read_file` when you already know the file and want full contents.\n\
\n\
Parameters:\n\
- `pattern`: Regex to search for (ripgrep / grep -E syntax).\n\
- `path`: Optional subdirectory or file to scope the search; `null` searches the whole working directory.\n\
- `glob`: Optional file glob to filter (e.g. `*.rs`, `**/*.test.ts`); `null` for all files.\n\
- `case_insensitive`: When true, ignore case."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex pattern to find."},
                "path": {"type": ["string", "null"], "description": "Optional subdirectory or file to scope the search; null searches everything."},
                "glob": {"type": ["string", "null"], "description": "Optional file glob filter (e.g. '*.rs'); null for all files."},
                "case_insensitive": {"type": "boolean", "description": "Match case-insensitively when true."}
            },
            "required": ["pattern", "path", "glob", "case_insensitive"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: GrepArgs = serde_json::from_value(args)?;
        let mut cmd = Command::new("rg");
        cmd.arg("--no-heading").arg("--line-number").arg("--color=never");
        if a.case_insensitive {
            cmd.arg("-i");
        }
        if let Some(g) = &a.glob {
            cmd.arg("--glob").arg(g);
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
            return Ok(truncate(&stdout, 8000));
        }
        // Fallback to grep. The previous `arg(if … { "-i" } else { "" })`
        // shifted positional args: an empty arg became the pattern and the
        // real pattern became a filename, so matches were silently lost.
        let mut grep = Command::new("grep");
        grep.arg("-rn");
        if a.case_insensitive {
            grep.arg("-i");
        }
        // `--` separates flags from positional args so a pattern starting
        // with `-` isn't misinterpreted as a flag.
        grep.arg("--").arg(&a.pattern);
        grep.arg(a.path.as_deref().unwrap_or("."));
        grep.current_dir(&ctx.cwd);
        let out = grep.output().await?;
        Ok(truncate(&String::from_utf8_lossy(&out.stdout), 8000))
    }
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl BuiltinTool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        "List files whose path matches a glob pattern. Supports `**` for recursive matches (e.g. `**/*.rs`, `src/**/*.test.ts`). Respects `.gitignore` when ripgrep is available.\n\
\n\
Use this to enumerate matching files when you already know the file shape but not where they live. For content search use `grep`.\n\
\n\
Parameters:\n\
- `pattern`: Glob pattern. `**` matches any depth; `*` matches one path segment.\n\
- `path`: Optional subdirectory to scope the search; `null` searches the whole working directory."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern (e.g. '**/*.rs')."},
                "path": {"type": ["string", "null"], "description": "Optional subdirectory; null searches the whole working directory."}
            },
            "required": ["pattern", "path"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: GlobArgs = serde_json::from_value(args)?;
        let cwd = a
            .path
            .as_deref()
            .map(|p| ctx.cwd.join(p))
            .unwrap_or_else(|| ctx.cwd.clone());

        // Prefer ripgrep — it understands `**` natively and respects .gitignore.
        if let Ok(out) = Command::new("rg")
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
            if out.status.success() {
                return Ok(truncate(&String::from_utf8_lossy(&out.stdout), 8000));
            }
        }

        // Fallback to find. Strip any `**/` so the trailing basename pattern
        // works with `-name`; coarse but never returns 0 falsely.
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
        let mut lines: Vec<&str> = find_out
            .stdout
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .take(200)
            .map(|l| std::str::from_utf8(l).unwrap_or(""))
            .collect();
        lines.retain(|s| !s.is_empty());
        Ok(truncate(&lines.join("\n"), 8000))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Walk back to the previous char boundary so we don't panic when `max`
    // lands inside a multi-byte UTF-8 sequence.
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n…(truncated, {} bytes remaining)",
        &s[..cut],
        s.len() - cut
    )
}
