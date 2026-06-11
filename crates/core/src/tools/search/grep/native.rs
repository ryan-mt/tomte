use super::*;

/// Native, dependency-free search used when ripgrep is unavailable (e.g. a stock
/// Windows box with no Unix tooling), and the only path that honors
/// `glob`/`file_type`/`multiline` without ripgrep — the external `grep` can't, so
/// `execute_grep_with_commands` routes those straight here. Emits ripgrep-shaped
/// lines (`path:lineno:text`, with `path:lineno-text` context lines and `--`
/// group separators) so the shared limiter applies unchanged. Binary
/// (NUL-containing), unreadable, or very large files are skipped, matching
/// grep/ripgrep's defaults.
pub(super) fn native_grep_search(
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
        files.push(prefix);
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

pub(super) fn normalize_grep_output_mode(mode: Option<&str>) -> Option<&'static str> {
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
pub(super) fn glob_matches_rel(pattern: &str, rel_path: &str) -> bool {
    // `!pattern` excludes matches, mirroring ripgrep's `--glob` (grep's own
    // parameter description suggests "!target/**") — without this the
    // fallback read `!` as a literal and silently matched nothing.
    if let Some(neg) = pattern.strip_prefix('!') {
        return !glob_matches_rel(neg, rel_path);
    }
    if pattern.contains('/') {
        crate::hooks::glob_match(pattern, rel_path)
    } else {
        let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
        crate::hooks::glob_match(pattern, base)
    }
}

/// True when `rel_path`'s basename has an extension (lowercased) in `exts`.
pub(super) fn file_has_extension(rel_path: &str, exts: &[&str]) -> bool {
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
pub(super) fn file_type_extensions(t: &str) -> Option<&'static [&'static str]> {
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
pub(super) fn line_match_flags(
    re: &regex::Regex,
    text: &str,
    lines: &[&str],
    multiline: bool,
) -> Vec<bool> {
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

pub(super) fn filter_zero_count_lines(stdout: &str) -> String {
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
