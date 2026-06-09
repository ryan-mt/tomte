//! Index 3 — the test map: which test files cover which source files.
//!
//! Two grounded signals, never a guess:
//! - **import** — a test that imports a source file covers it (the strongest
//!   signal, straight from the import graph).
//! - **name** — the cross-language filename conventions (`foo.test.ts` ↔
//!   `foo.ts`, `test_foo.py` ↔ `foo.py`, `foo_test.go` ↔ `foo.go`).
//! - **inline** — a Rust file with an in-file `#[cfg(test)]` module covers
//!   itself.
//!
//! Every edge records which signal produced it, so `why-context` can say *why*
//! a test is the closest regression coverage.

use std::collections::HashSet;

use super::{FileNode, ImportEdge, Lang, TestEdge};

/// Whether a path is a test by directory or filename convention (language
/// agnostic; the Rust in-file case is handled separately via `#[cfg(test)]`).
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);

    // A `test/` or `tests/` or `__tests__/` path segment.
    let in_test_dir = lower
        .split('/')
        .any(|seg| seg == "test" || seg == "tests" || seg == "__tests__" || seg == "spec");
    if in_test_dir {
        return true;
    }
    // Filename conventions across ecosystems.
    name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.starts_with("test_")
}

/// Build the test→source coverage edges from the file set, the resolved import
/// graph, and the list of Rust files that carry an in-file test module.
pub fn build_edges(
    files: &[FileNode],
    imports: &[ImportEdge],
    rust_inline_tests: &[String],
) -> Vec<TestEdge> {
    let file_set: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let mut edges: Vec<TestEdge> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    let mut push = |edges: &mut Vec<TestEdge>, test: &str, covers: &str, via: &str| {
        if test == covers && via != "inline" {
            return;
        }
        if seen.insert((test.to_string(), covers.to_string())) {
            edges.push(TestEdge {
                test: test.to_string(),
                covers: covers.to_string(),
                via: via.to_string(),
            });
        }
    };

    // Signal 1: imports from a test file to a source file.
    for edge in imports {
        if !is_test_path(&edge.from) {
            continue;
        }
        if let Some(to) = &edge.to {
            if file_set.contains(to.as_str()) {
                push(&mut edges, &edge.from, to, "import");
            }
        }
    }

    // Signal 2: filename conventions.
    for f in files.iter().filter(|f| f.is_test) {
        for covered in name_targets(&f.path) {
            if file_set.contains(covered.as_str()) {
                push(&mut edges, &f.path, &covered, "name");
            }
        }
    }

    // Signal 3: Rust in-file tests cover their own file.
    for path in rust_inline_tests {
        if file_set.contains(path.as_str()) {
            push(&mut edges, path, path, "inline");
        }
    }

    edges
}

/// Candidate source files a test filename points at, by convention. Returns the
/// paths to check against the real file set (callers filter to those that exist).
fn name_targets(test_path: &str) -> Vec<String> {
    let (dir, file) = match test_path.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("", test_path),
    };
    let join = |name: &str| {
        if dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}/{name}")
        }
    };

    match Lang::of(test_path) {
        Lang::Web => {
            // foo.test.ts / foo.spec.tsx → foo.<ext>
            let stem = file.replace(".test.", ".").replace(".spec.", ".");
            let base = stem.rsplit_once('.').map(|(s, _)| s).unwrap_or(&stem);
            ["ts", "tsx", "js", "jsx", "mjs", "cjs"]
                .iter()
                .map(|ext| join(&format!("{base}.{ext}")))
                .collect()
        }
        Lang::Python => {
            // test_foo.py → foo.py; foo_test.py → foo.py
            let base = file.trim_end_matches(".py");
            let core = base
                .strip_prefix("test_")
                .or_else(|| base.strip_suffix("_test"))
                .unwrap_or(base);
            vec![join(&format!("{core}.py"))]
        }
        Lang::Go => {
            // foo_test.go → foo.go
            let base = file.trim_end_matches("_test.go");
            vec![join(&format!("{base}.go"))]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(path: &str, is_test: bool) -> FileNode {
        FileNode {
            path: path.to_string(),
            lang: Lang::of(path),
            is_test,
            loc: 1,
        }
    }

    #[test]
    fn test_path_detection_across_ecosystems() {
        assert!(is_test_path("tests/auth.rs"));
        assert!(is_test_path("src/__tests__/x.ts"));
        assert!(is_test_path("src/auth.test.ts"));
        assert!(is_test_path("api/user.spec.js"));
        assert!(is_test_path("db/user_test.go"));
        assert!(is_test_path("app/test_user.py"));
        assert!(is_test_path("app/user_test.py"));
        assert!(!is_test_path("src/auth.ts"));
        assert!(!is_test_path("src/contest.rs")); // "test" substring, not a segment
    }

    #[test]
    fn import_edge_makes_the_strongest_coverage_link() {
        let files = vec![
            node("tests/auth.test.ts", true),
            node("src/session.ts", false),
        ];
        let imports = vec![ImportEdge {
            from: "tests/auth.test.ts".into(),
            raw: "../src/session".into(),
            to: Some("src/session.ts".into()),
            line: 1,
        }];
        let edges = build_edges(&files, &imports, &[]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].covers, "src/session.ts");
        assert_eq!(edges[0].via, "import");
    }

    #[test]
    fn name_convention_links_when_the_source_exists() {
        let files = vec![
            node("src/session.test.ts", true),
            node("src/session.ts", false),
            node("db/user_test.go", true),
            node("db/user.go", false),
            node("app/test_login.py", true),
            node("app/login.py", false),
        ];
        let edges = build_edges(&files, &[], &[]);
        let covered: Vec<&str> = edges.iter().map(|e| e.covers.as_str()).collect();
        assert!(covered.contains(&"src/session.ts"));
        assert!(covered.contains(&"db/user.go"));
        assert!(covered.contains(&"app/login.py"));
        assert!(edges.iter().all(|e| e.via == "name"));
    }

    #[test]
    fn inline_rust_test_covers_its_own_file() {
        let files = vec![node("src/parser.rs", true)];
        let edges = build_edges(&files, &[], &["src/parser.rs".to_string()]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].test, "src/parser.rs");
        assert_eq!(edges[0].covers, "src/parser.rs");
        assert_eq!(edges[0].via, "inline");
    }

    #[test]
    fn no_edge_when_the_named_source_is_absent() {
        // A test whose conventional source isn't in the repo yields nothing —
        // the map never points at a file that doesn't exist.
        let files = vec![node("src/ghost.test.ts", true)];
        assert!(build_edges(&files, &[], &[]).is_empty());
    }
}
