//! The `glob` tool: list files matching a glob pattern via ripgrep
//! `--files` with a native recursive-walk fallback. Split out of `search`.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::tools::{BuiltinTool, ToolContext};

use super::shared::{
    apply_limits, normalize_path_separators, path_to_slash_string, walk_files_relative,
};

pub struct Glob;

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    /// "name" (alpha, default) or "mtime" (newest modified first).
    #[serde(default)]
    sort: Option<String>,
    /// Cap on output lines.
    #[serde(default, deserialize_with = "crate::tools::deserialize_optional_usize")]
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
        let a: GlobArgs = crate::tools::parse_args("glob", args)?;
        let Some(sort) = normalize_glob_sort(a.sort.as_deref()) else {
            let sort = a.sort.as_deref().unwrap_or("<null>");
            return Err(anyhow::anyhow!(
                "sort must be 'name' or 'mtime' (got '{sort}')"
            ));
        };
        let root = ctx.cwd.canonicalize().unwrap_or_else(|_| ctx.cwd.clone());
        let cwd = match a.path.as_deref() {
            Some(p) => crate::tools::fs::resolve(&ctx.cwd, p)?,
            None => ctx.cwd.clone(),
        };

        // Collect raw file list via ripgrep when available.
        let mut rg = Command::new("rg");
        rg.arg("--files")
            .arg("--hidden")
            .arg("--glob")
            .arg(&a.pattern)
            .arg("--glob")
            .arg("!.git")
            .current_dir(&cwd);
        crate::secret_env::scrub_secret_env(&mut rg);
        let raw: Option<Vec<String>> = match rg.output().await {
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
            // No ripgrep: enumerate files with a native recursive walk, then
            // filter in-process with a matcher that respects path structure.
            // A native walk works identically on every platform — the Unix
            // `find` this used to shell out to is absent on Windows (or worse,
            // resolves to System32's unrelated `find.exe`), which silently
            // returned no results there.
            let mut found = Vec::new();
            walk_files_relative(&cwd, &mut found);
            found
                .into_iter()
                .filter(|l| glob_fallback_matches(&a.pattern, l))
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

/// Match a glob against a relative path for the no-ripgrep `find` fallback,
/// approximating ripgrep's `--glob` semantics: a pattern containing no `/`
/// matches the file's basename at any depth (gitignore-style), while a pattern
/// with a `/` matches the full relative path. Built on the shared
/// [`crate::hooks::glob_match`] so `**`/`*`/`?` behave consistently.
fn glob_fallback_matches(pattern: &str, rel_path: &str) -> bool {
    // `!pattern` excludes matches, mirroring ripgrep's `--glob` — without
    // this the fallback read `!` as a literal and silently dropped every file.
    if let Some(neg) = pattern.strip_prefix('!') {
        return !glob_fallback_matches(neg, rel_path);
    }
    if pattern.contains('/') {
        crate::hooks::glob_match(pattern, rel_path)
    } else {
        let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
        crate::hooks::glob_match(pattern, base)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{ctx, rg_available, write};
    use super::*;

    #[test]
    fn fallback_glob_supports_braces_and_negation() {
        // Brace alternation — used to be escaped literally and match nothing.
        assert!(glob_fallback_matches("*.{ts,tsx}", "src/app.tsx"));
        assert!(glob_fallback_matches("*.{ts,tsx}", "app.ts"));
        assert!(!glob_fallback_matches("*.{ts,tsx}", "app.rs"));
        assert!(glob_fallback_matches(
            "src/**/*.{rs,toml}",
            "src/a/b/x.toml"
        ));
        // Negation — the grep description itself suggests `!target/**`.
        assert!(!glob_fallback_matches("!target/**", "target/debug/x.d"));
        assert!(glob_fallback_matches("!target/**", "src/main.rs"));
        // Unbalanced braces stay literal instead of erroring.
        assert!(glob_fallback_matches("a{b", "a{b"));
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
    fn walk_collects_files_recursively_with_forward_slashes_and_skips_git() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "x");
        write(dir.path(), "sub/b.rs", "y");
        write(dir.path(), "sub/deep/c.txt", "z");
        // A `.git` directory must be skipped entirely (mirrors rg's `!.git`).
        write(dir.path(), ".git/config", "ignored");
        let mut found = Vec::new();
        walk_files_relative(dir.path(), &mut found);
        found.sort();
        assert_eq!(
            found,
            vec![
                "a.rs".to_string(),
                "sub/b.rs".to_string(),
                "sub/deep/c.txt".to_string(),
            ],
            "walk must recurse, use forward slashes, and skip .git"
        );
    }
}
