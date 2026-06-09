//! Index 1 — the file / import graph.
//!
//! Per-language regex extractors pull the import specifiers out of each source
//! file; [`resolve`] then maps a specifier to a concrete repo file when it points
//! inside the tree (relative paths, local Rust modules, in-module Go packages).
//! External packages stay unresolved (`to == None`) but are still recorded, so
//! the graph is honest about what it could and couldn't place.

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;

use super::{ImportEdge, Lang};

/// Extract every import edge from one file's text (with `to` left `None` until
/// [`resolve`] runs over the full file set).
pub fn extract(lang: Lang, from: &str, text: &str) -> Vec<ImportEdge> {
    match lang {
        Lang::Rust => extract_with(&RUST_MOD, from, text),
        Lang::Web => extract_web(from, text),
        Lang::Python => extract_python(from, text),
        Lang::Go => extract_go(from, text),
        Lang::Other => Vec::new(),
    }
}

/// Rust module declarations — `mod x;` / `pub mod x;`. These are the structural,
/// exactly-resolvable edges; `use` paths are left to the symbol graph (resolving
/// `use crate::…` to a file is guesswork and would manufacture wrong edges).
static RUST_MOD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;").unwrap()
});

/// `import …` / `export … from '…'` / `import('…')` / `require('…')` — the
/// quoted specifier in any of the JS/TS import forms.
static WEB_IMPORT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?m)(?:import\s+[^;\n]*?from\s*|export\s+[^;\n]*?from\s*|import\s*|require\s*\(\s*)['"]([^'"]+)['"]"#,
    )
    .unwrap()
});

/// `from .pkg import …` (captures the leading dots + module path).
static PY_FROM: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*from\s+(\.*)([A-Za-z0-9_.]*)\s+import\s+").unwrap());
/// `import a.b.c [as d]` (captures the dotted module path).
static PY_IMPORT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*import\s+([A-Za-z0-9_.]+)").unwrap());

/// A double-quoted Go import specifier — matches both the single-line form and
/// each line inside an `import ( … )` block.
static GO_IMPORT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*(?:[A-Za-z0-9_.]+\s+)?"([^"]+)""#).unwrap());

fn extract_with(re: &Regex, from: &str, text: &str) -> Vec<ImportEdge> {
    re.captures_iter(text)
        .filter_map(|c| {
            c.get(1)
                .map(|m| (m.as_str().to_string(), line_of(text, m.start())))
        })
        .map(|(raw, line)| ImportEdge {
            from: from.to_string(),
            raw,
            to: None,
            line,
        })
        .collect()
}

fn extract_web(from: &str, text: &str) -> Vec<ImportEdge> {
    WEB_IMPORT
        .captures_iter(text)
        .filter_map(|c| c.get(1))
        .map(|m| ImportEdge {
            from: from.to_string(),
            raw: m.as_str().to_string(),
            to: None,
            line: line_of(text, m.start()),
        })
        .collect()
}

fn extract_python(from: &str, text: &str) -> Vec<ImportEdge> {
    let mut out = Vec::new();
    for c in PY_FROM.captures_iter(text) {
        let dots = c.get(1).map(|m| m.as_str()).unwrap_or("");
        let path = c.get(2).map(|m| m.as_str()).unwrap_or("");
        let raw = format!("{dots}{path}");
        if raw.is_empty() {
            continue;
        }
        let at = c.get(0).map(|m| m.start()).unwrap_or(0);
        out.push(ImportEdge {
            from: from.to_string(),
            raw,
            to: None,
            line: line_of(text, at),
        });
    }
    for c in PY_IMPORT.captures_iter(text) {
        if let Some(m) = c.get(1) {
            out.push(ImportEdge {
                from: from.to_string(),
                raw: m.as_str().to_string(),
                to: None,
                line: line_of(text, m.start()),
            });
        }
    }
    out
}

fn extract_go(from: &str, text: &str) -> Vec<ImportEdge> {
    GO_IMPORT
        .captures_iter(text)
        .filter_map(|c| c.get(1))
        .map(|m| ImportEdge {
            from: from.to_string(),
            raw: m.as_str().to_string(),
            to: None,
            line: line_of(text, m.start()),
        })
        .collect()
}

/// Map an import specifier to a concrete repo file, or `None` for an external
/// package or a path that doesn't resolve. `file_set` is every repo file path;
/// `go_module` is the module path from `go.mod` (for placing Go imports).
pub fn resolve(
    lang: Lang,
    from: &str,
    raw: &str,
    file_set: &HashSet<&str>,
    go_module: Option<&str>,
) -> Option<String> {
    match lang {
        Lang::Rust => resolve_rust(from, raw, file_set),
        Lang::Web => resolve_web(from, raw, file_set),
        Lang::Python => resolve_python(from, raw, file_set),
        Lang::Go => resolve_go(raw, file_set, go_module),
        Lang::Other => None,
    }
}

/// `mod x;` in `dir/<stem>.rs` resolves under the module's directory: `mod.rs`,
/// `lib.rs` and `main.rs` keep their own directory; any other file `foo.rs`
/// owns the submodule directory `foo/` (the Rust 2018 layout).
fn resolve_rust(from: &str, name: &str, file_set: &HashSet<&str>) -> Option<String> {
    let (dir, stem) = split_dir_stem(from);
    let module_dir = if matches!(stem, "mod" | "lib" | "main") {
        dir.to_string()
    } else {
        join_under(dir, stem)
    };
    let candidates = [
        join_under(&module_dir, &format!("{name}.rs")),
        join_under(&module_dir, &format!("{name}/mod.rs")),
    ];
    first_existing(&candidates, file_set)
}

fn resolve_web(from: &str, raw: &str, file_set: &HashSet<&str>) -> Option<String> {
    // Only relative specifiers point inside the tree; bare/aliased ones are
    // external (or build-tool aliases we can't resolve) and stay unresolved.
    if !raw.starts_with('.') {
        return None;
    }
    let (dir, _) = split_dir_stem(from);
    let base = normalize_join(dir, raw)?;
    const EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];
    // Exact path (already has an extension).
    if file_set.contains(base.as_str()) {
        return Some(base);
    }
    // `./x` → x.ts, x.tsx, …
    for ext in EXTS {
        let cand = format!("{base}.{ext}");
        if file_set.contains(cand.as_str()) {
            return Some(cand);
        }
    }
    // `./x` → x/index.ts, …
    for ext in EXTS {
        let cand = format!("{base}/index.{ext}");
        if file_set.contains(cand.as_str()) {
            return Some(cand);
        }
    }
    None
}

fn resolve_python(from: &str, raw: &str, file_set: &HashSet<&str>) -> Option<String> {
    let dots = raw.chars().take_while(|c| *c == '.').count();
    let rest = &raw[dots..];
    let parts: Vec<&str> = rest.split('.').filter(|s| !s.is_empty()).collect();

    if dots > 0 {
        // Relative: climb `dots-1` directories from the importing file's package.
        let (dir, _) = split_dir_stem(from);
        let mut base = dir.to_string();
        for _ in 0..dots.saturating_sub(1) {
            base = parent_of(&base).to_string();
        }
        let joined = parts.iter().fold(base, |acc, p| join_under(&acc, p));
        return py_candidates(&joined, file_set);
    }
    // Absolute: try repo-root-relative, then under a `src/` layout.
    let rel = parts.join("/");
    py_candidates(&rel, file_set).or_else(|| py_candidates(&join_under("src", &rel), file_set))
}

fn py_candidates(base: &str, file_set: &HashSet<&str>) -> Option<String> {
    let candidates = [format!("{base}.py"), join_under(base, "__init__.py")];
    first_existing(&candidates, file_set)
}

fn resolve_go(raw: &str, file_set: &HashSet<&str>, go_module: Option<&str>) -> Option<String> {
    let module = go_module?;
    let rel_dir = if raw == module {
        String::new()
    } else {
        raw.strip_prefix(module)?
            .trim_start_matches('/')
            .to_string()
    };
    // A Go package is a directory; resolve to any `.go` file directly in it
    // (skipping nested packages and test files where possible).
    let prefix = if rel_dir.is_empty() {
        String::new()
    } else {
        format!("{rel_dir}/")
    };
    // Lexicographically-smallest candidate per class, so the resolved edge is
    // stable across rebuilds (`file_set` iteration order is not deterministic).
    let mut best: Option<&str> = None;
    let mut best_test: Option<&str> = None;
    for path in file_set {
        let in_dir = match path.strip_prefix(prefix.as_str()) {
            Some(tail) => !tail.contains('/'),
            None => prefix.is_empty() && !path.contains('/'),
        };
        if !(in_dir && path.ends_with(".go")) {
            continue;
        }
        // Prefer a non-test file as the package's representative node.
        let slot = if path.ends_with("_test.go") {
            &mut best_test
        } else {
            &mut best
        };
        if slot.is_none_or(|cur| *path < cur) {
            *slot = Some(path);
        }
    }
    best.or(best_test).map(|s| s.to_string())
}

// ---- path helpers -----------------------------------------------------------

fn first_existing(candidates: &[String], file_set: &HashSet<&str>) -> Option<String> {
    candidates
        .iter()
        .find(|c| file_set.contains(c.as_str()))
        .cloned()
}

/// `(directory, file_stem)` of a `/`-path. `src/a/b.rs` → (`src/a`, `b`).
fn split_dir_stem(path: &str) -> (&str, &str) {
    let (dir, file) = match path.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("", path),
    };
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    (dir, stem)
}

fn parent_of(dir: &str) -> &str {
    dir.rsplit_once('/').map(|(p, _)| p).unwrap_or("")
}

/// Join `name` under `base`, avoiding a leading `/` when `base` is empty.
fn join_under(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{base}/{name}")
    }
}

/// Resolve a relative specifier (`./x`, `../a/b`) against a base directory into a
/// normalized repo path, collapsing `.`/`..`. Returns `None` if it climbs above
/// the repo root.
fn normalize_join(base_dir: &str, rel: &str) -> Option<String> {
    let mut stack: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for comp in rel.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            other => stack.push(other),
        }
    }
    Some(stack.join("/"))
}

/// 1-based line number of the byte offset `at` within `text`.
fn line_of(text: &str, at: usize) -> usize {
    text[..at.min(text.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> HashSet<&'static str> {
        // Leak is fine in a test: keeps the &str borrow simple.
        paths
            .iter()
            .map(|p| Box::leak(p.to_string().into_boxed_str()) as &str)
            .collect()
    }

    #[test]
    fn rust_mod_extraction_and_resolution() {
        let edges = extract(Lang::Rust, "src/lib.rs", "pub mod util;\nmod inner;\n");
        assert_eq!(edges.len(), 2);
        let files = set(&["src/util.rs", "src/inner/mod.rs"]);
        assert_eq!(
            resolve(Lang::Rust, "src/lib.rs", "util", &files, None).as_deref(),
            Some("src/util.rs")
        );
        assert_eq!(
            resolve(Lang::Rust, "src/lib.rs", "inner", &files, None).as_deref(),
            Some("src/inner/mod.rs")
        );
    }

    #[test]
    fn rust_submodule_lives_under_file_stem_dir() {
        // `mod bar;` in `src/foo.rs` resolves to `src/foo/bar.rs`.
        let files = set(&["src/foo/bar.rs"]);
        assert_eq!(
            resolve(Lang::Rust, "src/foo.rs", "bar", &files, None).as_deref(),
            Some("src/foo/bar.rs")
        );
    }

    #[test]
    fn web_imports_extract_all_forms_and_resolve_relative() {
        let text = r#"
import { A } from './a';
import B from "../lib/b.ts";
export { C } from './c';
const d = require('./d.js');
import 'side-effect-only.css';
import React from 'react';
"#;
        let edges = extract(Lang::Web, "src/x.ts", text);
        let raws: Vec<&str> = edges.iter().map(|e| e.raw.as_str()).collect();
        assert!(raws.contains(&"./a"));
        assert!(raws.contains(&"../lib/b.ts"));
        assert!(raws.contains(&"./c"));
        assert!(raws.contains(&"./d.js"));
        assert!(raws.contains(&"react"));

        let files = set(&["src/a.ts", "lib/b.ts", "src/c/index.tsx", "src/d.js"]);
        assert_eq!(
            resolve(Lang::Web, "src/x.ts", "./a", &files, None).as_deref(),
            Some("src/a.ts")
        );
        assert_eq!(
            resolve(Lang::Web, "src/x.ts", "../lib/b.ts", &files, None).as_deref(),
            Some("lib/b.ts")
        );
        // Bare directory resolves to its index file.
        assert_eq!(
            resolve(Lang::Web, "src/x.ts", "./c", &files, None).as_deref(),
            Some("src/c/index.tsx")
        );
        // External package stays unresolved.
        assert_eq!(resolve(Lang::Web, "src/x.ts", "react", &files, None), None);
    }

    #[test]
    fn python_relative_and_absolute_resolution() {
        let text = "from .user import User\nfrom ..db import conn\nimport app.config\n";
        let edges = extract(Lang::Python, "app/auth/session.py", text);
        let raws: Vec<&str> = edges.iter().map(|e| e.raw.as_str()).collect();
        assert!(raws.contains(&".user"));
        assert!(raws.contains(&"..db"));
        assert!(raws.contains(&"app.config"));

        let files = set(&["app/auth/user.py", "app/db.py", "app/config.py"]);
        assert_eq!(
            resolve(Lang::Python, "app/auth/session.py", ".user", &files, None).as_deref(),
            Some("app/auth/user.py")
        );
        // `..db` climbs from app/auth → app, then db.py.
        assert_eq!(
            resolve(Lang::Python, "app/auth/session.py", "..db", &files, None).as_deref(),
            Some("app/db.py")
        );
        // Absolute dotted import resolved repo-root-relative.
        assert_eq!(
            resolve(
                Lang::Python,
                "app/auth/session.py",
                "app.config",
                &files,
                None
            )
            .as_deref(),
            Some("app/config.py")
        );
    }

    #[test]
    fn go_imports_resolve_inside_the_module() {
        let text = "import (\n\t\"fmt\"\n\t\"github.com/me/app/db\"\n)\n";
        let edges = extract(Lang::Go, "main.go", text);
        let raws: Vec<&str> = edges.iter().map(|e| e.raw.as_str()).collect();
        assert!(raws.contains(&"fmt"));
        assert!(raws.contains(&"github.com/me/app/db"));

        let files = set(&["db/db.go", "db/db_test.go"]);
        // The module-internal package resolves to its non-test .go file.
        assert_eq!(
            resolve(
                Lang::Go,
                "main.go",
                "github.com/me/app/db",
                &files,
                Some("github.com/me/app")
            )
            .as_deref(),
            Some("db/db.go")
        );
        // The stdlib package is external → unresolved.
        assert_eq!(
            resolve(
                Lang::Go,
                "main.go",
                "fmt",
                &files,
                Some("github.com/me/app")
            ),
            None
        );
    }

    #[test]
    fn go_package_representative_is_deterministic() {
        // Several non-test files in the package dir: the resolved representative
        // must not depend on HashSet iteration order (fresh set per round).
        for _ in 0..16 {
            let files = set(&["db/z.go", "db/m.go", "db/a.go", "db/a_test.go"]);
            assert_eq!(
                resolve(
                    Lang::Go,
                    "main.go",
                    "github.com/me/app/db",
                    &files,
                    Some("github.com/me/app")
                )
                .as_deref(),
                Some("db/a.go")
            );
        }
        // Only test files present → the smallest test file is the stable fallback.
        let files = set(&["db/z_test.go", "db/a_test.go"]);
        assert_eq!(
            resolve(
                Lang::Go,
                "main.go",
                "github.com/me/app/db",
                &files,
                Some("github.com/me/app")
            )
            .as_deref(),
            Some("db/a_test.go")
        );
    }

    #[test]
    fn normalize_join_collapses_dots_and_rejects_escapes() {
        // `./b` stays in the same directory; `../b` climbs one level.
        assert_eq!(normalize_join("src/a", "./b").as_deref(), Some("src/a/b"));
        assert_eq!(normalize_join("src/a", "../b").as_deref(), Some("src/b"));
        // Climbing above the root is rejected.
        assert_eq!(normalize_join("src", "../../x"), None);
    }

    #[test]
    fn line_numbers_are_one_based() {
        let edges = extract(Lang::Web, "x.ts", "\n\nimport './a';\n");
        assert_eq!(edges[0].line, 3);
    }
}
