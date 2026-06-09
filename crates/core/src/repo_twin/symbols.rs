//! Index 2 — the symbol / function graph.
//!
//! Regex extractors record each top-level definition (function, type, class,
//! const, …) with its file and line. The point isn't a full parse — it's a
//! *grounded* index: a symbol the `why-context` engine cites always points at a
//! real definition line, so "User flows into Session" can be backed by the line
//! where `User` is defined and the lines that mention it, never invented.

use once_cell::sync::Lazy;
use regex::Regex;

use super::{line_at, Lang, SymbolDef};

/// Extract symbol definitions from one file.
pub fn extract(lang: Lang, file: &str, text: &str) -> Vec<SymbolDef> {
    let rules: &[(&Lazy<Regex>, &str)] = match lang {
        Lang::Rust => RUST.as_slice(),
        Lang::Web => WEB.as_slice(),
        Lang::Python => PYTHON.as_slice(),
        Lang::Go => GO.as_slice(),
        Lang::Other => return Vec::new(),
    };
    let mut out = Vec::new();
    for (re, kind) in rules {
        for c in re.captures_iter(text) {
            if let Some(m) = c.get(1) {
                out.push(SymbolDef {
                    file: file.to_string(),
                    name: m.as_str().to_string(),
                    kind: (*kind).to_string(),
                    line: line_at(text, m.start()),
                });
            }
        }
    }
    out
}

/// Whether a Rust file carries an in-file test module — `#[cfg(test)]` or a bare
/// `#[test]`. Used to mark the file as a test and to add an `inline` coverage
/// edge to itself.
pub fn rust_has_inline_test(text: &str) -> bool {
    text.contains("#[cfg(test)]") || text.contains("#[test]")
}

// A `(regex, kind)` table per language. Each regex captures the symbol name in
// group 1 at a definition site anchored to the start of a (possibly indented)
// line, so a mention inside an expression isn't mistaken for a definition.

static RUST: Lazy<Vec<(&'static Lazy<Regex>, &'static str)>> = Lazy::new(|| {
    vec![
        (&R_FN, "fn"),
        (&R_STRUCT, "type"),
        (&R_ENUM, "type"),
        (&R_TRAIT, "type"),
        (&R_TYPE, "type"),
        (&R_CONST, "const"),
        (&R_STATIC, "const"),
        (&R_MACRO, "macro"),
    ]
});
static R_FN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?(?:extern\s+\x22[^\x22]*\x22\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});
static R_STRUCT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});
static R_ENUM: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});
static R_TRAIT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)")
        .unwrap()
});
static R_TYPE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});
static R_CONST: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?const\s+([A-Z_][A-Za-z0-9_]*)\s*:").unwrap()
});
static R_STATIC: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?static\s+(?:mut\s+)?([A-Z_][A-Za-z0-9_]*)\s*:")
        .unwrap()
});
static R_MACRO: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*macro_rules!\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap());

static WEB: Lazy<Vec<(&'static Lazy<Regex>, &'static str)>> = Lazy::new(|| {
    vec![
        (&W_FN, "fn"),
        (&W_CLASS, "class"),
        (&W_INTERFACE, "type"),
        (&W_TYPE, "type"),
        (&W_ENUM, "type"),
        (&W_CONST, "const"),
    ]
});
static W_FN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][A-Za-z0-9_$]*)").unwrap()
});
static W_CLASS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?m)^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)",
    )
    .unwrap()
});
static W_INTERFACE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:export\s+)?interface\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap()
});
static W_TYPE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:export\s+)?type\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*[=<]").unwrap()
});
static W_ENUM: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:export\s+)?(?:const\s+)?enum\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap()
});
/// Exported consts only — capturing every local `const` would flood the index
/// with noise; an export is a deliberate, name-worthy surface (a util, a
/// component, a hook).
static W_CONST: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*export\s+(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap()
});

static PYTHON: Lazy<Vec<(&'static Lazy<Regex>, &'static str)>> =
    Lazy::new(|| vec![(&P_DEF, "fn"), (&P_CLASS, "class")]);
static P_DEF: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap());
static P_CLASS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap());

static GO: Lazy<Vec<(&'static Lazy<Regex>, &'static str)>> =
    Lazy::new(|| vec![(&G_FUNC, "fn"), (&G_TYPE, "type")]);
/// `func Name(` and `func (recv T) Name(` — the optional receiver group is
/// skipped so the captured name is the function/method, not the receiver.
static G_FUNC: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*func\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)").unwrap());
static G_TYPE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap());

#[cfg(test)]
mod tests {
    use super::*;

    fn names(defs: &[SymbolDef]) -> Vec<&str> {
        defs.iter().map(|d| d.name.as_str()).collect()
    }

    #[test]
    fn rust_definitions_are_captured_with_kinds() {
        let text = r#"
pub fn run() {}
async fn fetch() {}
struct User;
pub enum State { On }
trait Store {}
type Id = u64;
const MAX: usize = 3;
static REG: u8 = 0;
macro_rules! m { () => {} }
"#;
        let defs = extract(Lang::Rust, "src/a.rs", text);
        let n = names(&defs);
        for want in [
            "run", "fetch", "User", "State", "Store", "Id", "MAX", "REG", "m",
        ] {
            assert!(n.contains(&want), "missing {want} in {n:?}");
        }
        // A call site, not a definition, must not be captured as `fn`.
        let calls = extract(Lang::Rust, "src/b.rs", "fn caller() { run(); }\n");
        assert_eq!(names(&calls), vec!["caller"]);
    }

    #[test]
    fn web_definitions_cover_the_common_forms() {
        let text = r#"
export function createSession() {}
class Service {}
export interface User {}
type Id = string;
export const helper = () => {};
function plain() {}
"#;
        let defs = extract(Lang::Web, "src/x.ts", text);
        let n = names(&defs);
        for want in ["createSession", "Service", "User", "Id", "helper", "plain"] {
            assert!(n.contains(&want), "missing {want} in {n:?}");
        }
    }

    #[test]
    fn python_and_go_definitions() {
        let py = extract(
            Lang::Python,
            "a.py",
            "def f():\n    pass\nclass C:\n    pass\n",
        );
        assert_eq!(names(&py), vec!["f", "C"]);

        let go = extract(
            Lang::Go,
            "a.go",
            "func Run() {}\nfunc (s *S) Method() {}\ntype Widget struct{}\n",
        );
        let n = names(&go);
        assert!(n.contains(&"Run"));
        assert!(n.contains(&"Method"));
        assert!(n.contains(&"Widget"));
    }

    #[test]
    fn inline_rust_test_is_detected() {
        assert!(rust_has_inline_test("#[cfg(test)]\nmod tests {}"));
        assert!(rust_has_inline_test("#[test]\nfn t() {}"));
        assert!(!rust_has_inline_test("pub fn x() {}"));
    }

    #[test]
    fn line_numbers_point_at_the_definition() {
        let defs = extract(Lang::Rust, "a.rs", "\n\npub fn here() {}\n");
        assert_eq!(defs[0].line, 3);
    }
}
