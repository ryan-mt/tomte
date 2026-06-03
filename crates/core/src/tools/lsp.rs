use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{fs, BuiltinTool, ToolContext};

mod symbols;
use symbols::{
    collect_references, collect_workspace_symbols, extract_symbols, read_source_file,
    token_at_position,
};

pub struct Lsp;

#[derive(Debug, Deserialize)]
struct LspArgs {
    operation: String,
    #[serde(default, alias = "filePath", alias = "path")]
    file_path: Option<String>,
    #[serde(default, deserialize_with = "super::deserialize_optional_usize")]
    line: Option<usize>,
    #[serde(default, deserialize_with = "super::deserialize_optional_usize")]
    character: Option<usize>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default, deserialize_with = "super::deserialize_optional_usize")]
    limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Symbol {
    name: String,
    kind: &'static str,
    path: PathBuf,
    line: usize,
    column: usize,
    signature: String,
}

#[async_trait]
impl BuiltinTool for Lsp {
    fn name(&self) -> &'static str {
        "lsp"
    }

    fn description(&self) -> &'static str {
        "Best-effort code intelligence over the current workspace. Provides LSP-style operations without requiring a language-server daemon: document symbols, workspace symbols, definitions, references, and hover context.\n\
\n\
When to use:\n\
- Finding definitions/references/symbols more precisely than broad grep.\n\
- Inspecting the symbol at a file position before editing or refactoring.\n\
- Getting a quick outline of a source file or workspace.\n\
\n\
Operations:\n\
- `document_symbols`: Requires `file_path`; returns functions/classes/types/constants found in the file.\n\
- `workspace_symbols`: Optional `query`; scans source files and returns matching symbols.\n\
- `definition`: Requires `file_path`, `line`, and `character`; extracts the token at that 1-based position and finds likely declarations.\n\
- `references`: Requires `file_path`, `line`, and `character`; extracts the token and finds workspace references.\n\
- `hover`: Requires `file_path`, `line`, and `character`; returns local context and likely declarations.\n\
\n\
Notes:\n\
- `line` and `character` are 1-based, as shown in editors.\n\
- This fallback is language-aware for Rust, TypeScript/JavaScript, Python, and Go, and degrades gracefully to text scanning.\n\
- Results are capped; pass `limit` to narrow output."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["document_symbols", "workspace_symbols", "definition", "references", "hover"],
                    "description": "LSP-style operation to perform."
                },
                "file_path": {"type": ["string", "null"], "description": "Source file path for document/position operations."},
                "line": {"type": ["integer", "null"], "minimum": 1, "description": "1-based line number for position operations."},
                "character": {"type": ["integer", "null"], "minimum": 1, "description": "1-based character offset for position operations."},
                "query": {"type": ["string", "null"], "description": "Optional symbol query for workspace_symbols."},
                "limit": {"type": ["integer", "null"], "minimum": 1, "maximum": 200, "description": "Maximum results to return; null uses a sensible default."}
            },
            "required": ["operation", "file_path", "line", "character", "query", "limit"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let args: LspArgs = super::parse_args("lsp", args)?;
        let limit = args.limit.unwrap_or(50).clamp(1, 200);
        match args.operation.as_str() {
            "document_symbols" => document_symbols(ctx, args.file_path.as_deref(), limit).await,
            "workspace_symbols" => workspace_symbols(ctx, args.query.as_deref(), limit).await,
            "definition" => definition(ctx, &args, limit).await,
            "references" => references(ctx, &args, limit).await,
            "hover" => hover(ctx, &args, limit).await,
            other => Err(anyhow!(
                "unsupported lsp operation `{other}`; expected document_symbols, workspace_symbols, definition, references, or hover"
            )),
        }
    }
}

async fn document_symbols(
    ctx: &ToolContext,
    file_path: Option<&str>,
    limit: usize,
) -> Result<String> {
    let display = required_file_path(file_path)?;
    let path = fs::resolve(&ctx.cwd, display)?;
    let text = read_source_file(&path).await?;
    let symbols = extract_symbols(&text, &path);
    if symbols.is_empty() {
        return Ok(format!("No symbols found in `{display}`."));
    }
    Ok(format_symbols(
        "Document symbols",
        &ctx.cwd,
        symbols.into_iter().take(limit),
        limit,
    ))
}

async fn workspace_symbols(ctx: &ToolContext, query: Option<&str>, limit: usize) -> Result<String> {
    let mut symbols = collect_workspace_symbols(&ctx.cwd).await?;
    if let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) {
        let needle = query.to_ascii_lowercase();
        symbols.retain(|s| s.name.to_ascii_lowercase().contains(&needle));
    }
    symbols.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    if symbols.is_empty() {
        return Ok("No workspace symbols found.".to_string());
    }
    Ok(format_symbols(
        "Workspace symbols",
        &ctx.cwd,
        symbols.into_iter().take(limit),
        limit,
    ))
}

async fn definition(ctx: &ToolContext, args: &LspArgs, limit: usize) -> Result<String> {
    let (display, path, text, line, character) = position_input(ctx, args).await?;
    let token = token_at_position(&text, line, character)
        .ok_or_else(|| anyhow!("no identifier found at {display}:{line}:{character}"))?;
    let mut matches: Vec<Symbol> = collect_workspace_symbols(&ctx.cwd)
        .await?
        .into_iter()
        .filter(|s| s.name == token)
        .collect();
    matches.sort_by_key(|s| (s.path != path, s.path.clone(), s.line));
    if matches.is_empty() {
        return Ok(format!("No likely definition found for `{token}`."));
    }
    Ok(format_symbols(
        &format!("Likely definitions for `{token}`"),
        &ctx.cwd,
        matches.into_iter().take(limit),
        limit,
    ))
}

async fn references(ctx: &ToolContext, args: &LspArgs, limit: usize) -> Result<String> {
    let (_display, _path, text, line, character) = position_input(ctx, args).await?;
    let token = token_at_position(&text, line, character)
        .ok_or_else(|| anyhow!("no identifier found at line {line}, character {character}"))?;
    let refs = collect_references(&ctx.cwd, &token, limit).await?;
    if refs.is_empty() {
        return Ok(format!("No references found for `{token}`."));
    }
    let mut out = format!("References for `{token}` (showing up to {limit}):\n");
    for (path, line_no, line_text) in refs {
        out.push_str(&format!(
            "{}:{}:{}\n",
            rel_display(&ctx.cwd, &path),
            line_no,
            line_text.trim_end()
        ));
    }
    Ok(out)
}

async fn hover(ctx: &ToolContext, args: &LspArgs, limit: usize) -> Result<String> {
    let (display, _path, text, line, character) = position_input(ctx, args).await?;
    let lines: Vec<&str> = text.lines().collect();
    let token = token_at_position(&text, line, character).unwrap_or_default();
    let start = line.saturating_sub(3).max(1);
    let end = line.saturating_add(2).min(lines.len());
    let mut out = format!(
        "Hover context for `{}` at {display}:{line}:{character}:\n",
        if token.is_empty() {
            "<no identifier>"
        } else {
            &token
        }
    );
    for line_no in start..=end {
        if let Some(line_text) = lines.get(line_no - 1) {
            let marker = if line_no == line { ">" } else { " " };
            out.push_str(&format!("{marker} {line_no:>5}\t{line_text}\n"));
        }
    }
    if !token.is_empty() {
        let defs: Vec<Symbol> = collect_workspace_symbols(&ctx.cwd)
            .await?
            .into_iter()
            .filter(|s| s.name == token)
            .take(limit.min(10))
            .collect();
        if !defs.is_empty() {
            out.push('\n');
            out.push_str(&format_symbols(
                "Likely declarations",
                &ctx.cwd,
                defs,
                limit.min(10),
            ));
        }
    }
    Ok(out)
}

async fn position_input(
    ctx: &ToolContext,
    args: &LspArgs,
) -> Result<(String, PathBuf, String, usize, usize)> {
    let display = required_file_path(args.file_path.as_deref())?.to_string();
    let line = args
        .line
        .ok_or_else(|| anyhow!("line is required for `{}`", args.operation))?;
    let character = args
        .character
        .ok_or_else(|| anyhow!("character is required for `{}`", args.operation))?;
    if line == 0 || character == 0 {
        return Err(anyhow!("line and character are 1-based and must be > 0"));
    }
    let path = fs::resolve(&ctx.cwd, &display)?;
    let text = read_source_file(&path).await?;
    Ok((display, path, text, line, character))
}

fn required_file_path(file_path: Option<&str>) -> Result<&str> {
    file_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| anyhow!("file_path is required for this lsp operation"))
}

fn format_symbols<I>(title: &str, cwd: &Path, symbols: I, limit: usize) -> String
where
    I: IntoIterator<Item = Symbol>,
{
    let mut out = format!("{title} (showing up to {limit}):\n");
    let mut count = 0usize;
    for s in symbols {
        count += 1;
        out.push_str(&format!(
            "{}:{}:{}  {}  {} — {}\n",
            rel_display(cwd, &s.path),
            s.line,
            s.column,
            s.kind,
            s.name,
            s.signature
        ));
    }
    if count == 0 {
        out.push_str("(none)\n");
    }
    out
}

fn rel_display(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .trim_start_matches(std::path::MAIN_SEPARATOR)
        .to_string()
}
