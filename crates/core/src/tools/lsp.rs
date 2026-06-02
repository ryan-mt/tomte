use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{fs, BuiltinTool, ToolContext};

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

async fn read_source_file(path: &Path) -> Result<String> {
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("stat {}", path.display()))?;
    if !meta.is_file() {
        return Err(anyhow!("path is not a file: {}", path.display()));
    }
    if meta.len() > 5_000_000 {
        return Err(anyhow!(
            "file is too large for lsp fallback: {} bytes",
            meta.len()
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    String::from_utf8(bytes).map_err(|_| anyhow!("file is not valid UTF-8: {}", path.display()))
}

async fn collect_workspace_symbols(cwd: &Path) -> Result<Vec<Symbol>> {
    let mut files = Vec::new();
    collect_source_files(cwd, cwd, &mut files)?;
    let mut out = Vec::new();
    for path in files.into_iter().take(2_000) {
        if let Ok(text) = read_source_file(&path).await {
            out.extend(extract_symbols(&text, &path));
        }
    }
    Ok(out)
}

/// Recursion depth cap for the source-file walk. A belt-and-suspenders guard
/// against stack exhaustion on pathologically deep trees, in addition to not
/// following directory symlinks below.
const MAX_WALK_DEPTH: usize = 64;

fn collect_source_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    collect_source_files_at(root, dir, out, 0)
}

fn collect_source_files_at(
    root: &Path,
    dir: &Path,
    out: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<()> {
    if out.len() >= 2_000 || depth >= MAX_WALK_DEPTH {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Use the directory entry's own type, which — unlike `Path::is_dir` —
        // does not traverse symlinks. Skipping symlinked directories stops a
        // symlink cycle from recursing until the stack overflows.
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            if should_skip_dir(&name) {
                continue;
            }
            collect_source_files_at(root, &path, out, depth + 1)?;
        } else if file_type.is_file() && is_source_path(&path) && path.starts_with(root) {
            out.push(path);
        } else if file_type.is_symlink() && is_source_path(&path) {
            // A symlinked *file* can't form a cycle (only directories can, and
            // those are skipped above), so include it — but only when it points
            // at a regular file that still resolves inside the workspace root,
            // so a symlink can't pull source content in from outside the sandbox.
            // Canonicalize BOTH sides: `root` (== cwd) may itself contain a
            // symlink component (e.g. macOS `/tmp` → `/private/tmp`), which would
            // otherwise make a legitimately in-tree target fail `starts_with`.
            let target_ok = std::fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false)
                && match (path.canonicalize(), root.canonicalize()) {
                    (Ok(target), Ok(root)) => target.starts_with(root),
                    _ => false,
                };
            if target_ok {
                out.push(path);
            }
        }
        if out.len() >= 2_000 {
            break;
        }
    }
    Ok(())
}

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | "target"
            | "node_modules"
            | "dist"
            | "build"
            | ".next"
            | ".turbo"
            | ".venv"
            | "venv"
            | "vendor"
    )
}

fn is_source_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cs"
            | "rb"
            | "php"
    )
}

fn extract_symbols(text: &str, path: &Path) -> Vec<Symbol> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut out = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim_start();
        let indent = raw.len().saturating_sub(line.len());
        if line.starts_with("//") || line.starts_with('#') && ext != "py" || line.starts_with('*') {
            continue;
        }
        match ext {
            "rs" => extract_rust_symbol(path, line, line_no, indent, &mut out),
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
                extract_js_symbol(path, line, line_no, indent, &mut out)
            }
            "py" => extract_python_symbol(path, line, line_no, indent, &mut out),
            "go" => extract_go_symbol(path, line, line_no, indent, &mut out),
            _ => extract_generic_symbol(path, line, line_no, indent, &mut out),
        }
    }
    out
}

fn extract_rust_symbol(
    path: &Path,
    line: &str,
    line_no: usize,
    indent: usize,
    out: &mut Vec<Symbol>,
) {
    let trimmed = strip_visibility(line);
    for (prefix, kind) in [
        ("async fn ", "function"),
        ("fn ", "function"),
        ("struct ", "struct"),
        ("enum ", "enum"),
        ("trait ", "trait"),
        ("impl ", "impl"),
        ("type ", "type"),
        ("const ", "constant"),
        ("static ", "static"),
        ("mod ", "module"),
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if let Some(name) = symbol_name(rest) {
                push_symbol(out, path, name, kind, line_no, indent + 1, line);
            }
            return;
        }
    }
}

fn strip_visibility(line: &str) -> &str {
    let mut s = line.trim_start();
    if let Some(rest) = s.strip_prefix("pub ") {
        s = rest.trim_start();
    } else if let Some(rest) = s.strip_prefix("pub(") {
        if let Some((_, after)) = rest.split_once(")") {
            s = after.trim_start();
        }
    }
    s
}

fn extract_js_symbol(
    path: &Path,
    line: &str,
    line_no: usize,
    indent: usize,
    out: &mut Vec<Symbol>,
) {
    let line = line
        .strip_prefix("export default ")
        .or_else(|| line.strip_prefix("export "))
        .unwrap_or(line)
        .trim_start();
    for (prefix, kind) in [
        ("async function ", "function"),
        ("function ", "function"),
        ("class ", "class"),
        ("interface ", "interface"),
        ("type ", "type"),
        ("enum ", "enum"),
        ("namespace ", "namespace"),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            if let Some(name) = symbol_name(rest) {
                push_symbol(out, path, name, kind, line_no, indent + 1, line);
            }
            return;
        }
    }
    for prefix in ["const ", "let ", "var "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            if let Some(name) = symbol_name(rest) {
                let kind = if rest.contains("=>") || rest.contains("function") {
                    "function"
                } else {
                    "variable"
                };
                push_symbol(out, path, name, kind, line_no, indent + 1, line);
            }
            return;
        }
    }
}

fn extract_python_symbol(
    path: &Path,
    line: &str,
    line_no: usize,
    indent: usize,
    out: &mut Vec<Symbol>,
) {
    for (prefix, kind) in [
        ("async def ", "function"),
        ("def ", "function"),
        ("class ", "class"),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            if let Some(name) = symbol_name(rest) {
                push_symbol(out, path, name, kind, line_no, indent + 1, line);
            }
            return;
        }
    }
}

fn extract_go_symbol(
    path: &Path,
    line: &str,
    line_no: usize,
    indent: usize,
    out: &mut Vec<Symbol>,
) {
    for (prefix, kind) in [
        ("func ", "function"),
        ("type ", "type"),
        ("const ", "constant"),
        ("var ", "variable"),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let rest = if let Some(after_receiver) = rest.strip_prefix('(') {
                after_receiver
                    .split_once(')')
                    .map(|(_, after)| after.trim_start())
                    .unwrap_or(rest)
            } else {
                rest
            };
            if let Some(name) = symbol_name(rest) {
                push_symbol(out, path, name, kind, line_no, indent + 1, line);
            }
            return;
        }
    }
}

fn extract_generic_symbol(
    path: &Path,
    line: &str,
    line_no: usize,
    indent: usize,
    out: &mut Vec<Symbol>,
) {
    for (prefix, kind) in [
        ("function ", "function"),
        ("class ", "class"),
        ("interface ", "interface"),
        ("struct ", "struct"),
        ("enum ", "enum"),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            if let Some(name) = symbol_name(rest) {
                push_symbol(out, path, name, kind, line_no, indent + 1, line);
            }
            return;
        }
    }
}

fn symbol_name(rest: &str) -> Option<&str> {
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("r#").unwrap_or(rest);
    let end = rest
        .char_indices()
        .find(|(_, c)| !is_ident_char(*c))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    (end > 0).then_some(&rest[..end])
}

fn push_symbol(
    out: &mut Vec<Symbol>,
    path: &Path,
    name: &str,
    kind: &'static str,
    line: usize,
    column: usize,
    signature: &str,
) {
    out.push(Symbol {
        name: name.to_string(),
        kind,
        path: path.to_path_buf(),
        line,
        column,
        signature: signature.trim().chars().take(240).collect(),
    });
}

fn token_at_position(text: &str, line: usize, character: usize) -> Option<String> {
    let line_text = text.lines().nth(line.checked_sub(1)?)?;
    let chars: Vec<char> = line_text.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let idx0 = character.saturating_sub(1);
    // A position more than one past the last character points at nothing; don't
    // clamp it onto the trailing identifier and return a confidently-wrong
    // token. (idx0 == chars.len() — the cursor resting just after the last
    // char — still resolves to that char, the usual editor convention.)
    if idx0 > chars.len() {
        return None;
    }
    let mut idx = idx0.min(chars.len().saturating_sub(1));
    if !is_ident_char(chars[idx]) && idx > 0 && is_ident_char(chars[idx - 1]) {
        idx -= 1;
    }
    if !is_ident_char(chars[idx]) {
        return None;
    }
    let mut start = idx;
    while start > 0 && is_ident_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = idx + 1;
    while end < chars.len() && is_ident_char(chars[end]) {
        end += 1;
    }
    Some(chars[start..end].iter().collect())
}

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

async fn collect_references(
    cwd: &Path,
    token: &str,
    limit: usize,
) -> Result<Vec<(PathBuf, usize, String)>> {
    let mut files = Vec::new();
    collect_source_files(cwd, cwd, &mut files)?;
    let mut out = Vec::new();
    for path in files.into_iter().take(2_000) {
        let Ok(text) = read_source_file(&path).await else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            if line_contains_word(line, token) {
                out.push((path.clone(), idx + 1, line.to_string()));
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

fn line_contains_word(line: &str, token: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = token.as_bytes();
    if needle.is_empty() || needle.len() > bytes.len() {
        return false;
    }
    bytes
        .windows(needle.len())
        .enumerate()
        .any(|(idx, window)| {
            if window != needle {
                return false;
            }
            let before = idx
                .checked_sub(1)
                .and_then(|i| line[idx_byte_to_char_boundary(line, i)..].chars().next());
            let after_idx = idx + needle.len();
            let after = (after_idx < line.len())
                .then(|| line[after_idx..].chars().next())
                .flatten();
            !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
        })
}

fn idx_byte_to_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_symbols() {
        let path = PathBuf::from("src/lib.rs");
        let symbols = extract_symbols(
            "pub struct User {}\nimpl User {}\npub async fn load_user() {}\n",
            &path,
        );
        assert!(symbols
            .iter()
            .any(|s| s.name == "User" && s.kind == "struct"));
        assert!(symbols
            .iter()
            .any(|s| s.name == "load_user" && s.kind == "function"));
    }

    #[test]
    fn extracts_token_at_position() {
        let text = "let answer_count = answer + 1;\n";
        assert_eq!(
            token_at_position(text, 1, 6).as_deref(),
            Some("answer_count")
        );
        assert_eq!(token_at_position(text, 1, 21).as_deref(), Some("answer"));
    }

    #[test]
    fn word_reference_respects_boundaries() {
        assert!(line_contains_word("let answer = 1", "answer"));
        assert!(!line_contains_word("let answer_count = 1", "answer"));
    }

    #[cfg(unix)]
    #[test]
    fn collect_source_files_does_not_follow_symlink_cycles() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("real.rs"), "fn main() {}").unwrap();
        // A cycle: sub/loop -> root. Following it would recurse forever.
        std::os::unix::fs::symlink(root, sub.join("loop")).unwrap();

        let mut out = Vec::new();
        // Returns instead of overflowing the stack, and still finds real files.
        collect_source_files(root, root, &mut out).unwrap();

        assert!(out.iter().any(|p| p.ends_with("real.rs")));
    }

    #[cfg(unix)]
    #[test]
    fn collect_source_files_includes_in_root_symlinked_files_but_not_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("target.rs"), "fn t() {}").unwrap();
        // A symlink to a source file that lives inside the workspace: include it.
        std::os::unix::fs::symlink(root.join("target.rs"), root.join("linked.rs")).unwrap();
        // A symlink to a source file OUTSIDE the workspace: must be excluded so a
        // symlink can't pull content in from outside the sandbox.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.rs"), "fn s() {}").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.rs"), root.join("escape.rs"))
            .unwrap();

        let mut out = Vec::new();
        collect_source_files(root, root, &mut out).unwrap();

        assert!(
            out.iter().any(|p| p.ends_with("linked.rs")),
            "in-root symlinked source file should be collected"
        );
        assert!(
            !out.iter().any(|p| p.ends_with("escape.rs")),
            "symlink escaping the workspace root must be excluded"
        );
    }
}
