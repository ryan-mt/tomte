//! The LSP-fallback analysis engine: read source files, walk the workspace,
//! extract language-aware symbols, locate the token at a position, and collect
//! plain-text references. No language-server daemon required.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use super::Symbol;

pub(super) async fn read_source_file(path: &Path) -> Result<String> {
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

pub(super) async fn collect_workspace_symbols(cwd: &Path) -> Result<Vec<Symbol>> {
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

pub(super) fn collect_source_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
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
            let target_ok = std::fs::metadata(&path)
                .map(|m| m.is_file())
                .unwrap_or(false)
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

pub(super) fn extract_symbols(text: &str, path: &Path) -> Vec<Symbol> {
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

pub(super) fn token_at_position(text: &str, line: usize, character: usize) -> Option<String> {
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

pub(super) async fn collect_references(
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

#[cfg(test)]
mod tests;
