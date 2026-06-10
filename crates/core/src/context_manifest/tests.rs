use super::*;
use crate::repo_twin::select::{IgnoredFile, Reason, SeededFile, SelectedFile, Selection};

fn selection(selected: Vec<SelectedFile>, ignored: Vec<IgnoredFile>) -> Selection {
    Selection {
        seed: "src/a.rs".into(),
        seed_kind: "file".into(),
        resolved_seeds: vec![SeededFile {
            path: "src/a.rs".into(),
            detail: "the seed".into(),
        }],
        selected,
        ignored,
        rules: vec![],
        decisions: vec![],
        notes: vec![],
    }
}

fn picked(path: &str, because: &str) -> SelectedFile {
    SelectedFile {
        path: path.into(),
        score: 50,
        reasons: vec![Reason {
            source: "import".into(),
            detail: because.into(),
        }],
        last_change: None,
    }
}

fn out(path: &str, reason: &str) -> IgnoredFile {
    IgnoredFile {
        path: path.into(),
        reason: reason.into(),
    }
}

// The manifest pairs every pulled file with its real index edge AND the
// session's own read log — claimed context is checked, not asserted.
#[test]
fn lines_carry_because_and_read_state() {
    let sel = selection(
        vec![
            picked("src/b.rs", "imported by src/a.rs"),
            picked("tests/a_test.rs", "tests src/a.rs"),
        ],
        vec![out("src/zz.rs", "no path from the seed")],
    );
    let lines = manifest_lines(&sel, true, &|p| p == "src/b.rs");
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[0],
        "pulling src/b.rs — imported by src/a.rs · ✓ read this session"
    );
    assert_eq!(
        lines[1],
        "pulling tests/a_test.rs — tests src/a.rs · not read yet"
    );
    assert_eq!(lines[2], "leaving out src/zz.rs — no path from the seed");
}

// An isolated file (the twin connects nothing) produces NO card — a tidy house
// is quiet — and a stale cache is labeled, never passed off as current.
#[test]
fn empty_selection_is_silent_and_stale_cache_is_labeled() {
    assert!(manifest_lines(&selection(vec![], vec![]), true, &|_| false).is_empty());

    let sel = selection(vec![picked("src/b.rs", "imported")], vec![]);
    let lines = manifest_lines(&sel, false, &|_| false);
    assert!(lines
        .last()
        .unwrap()
        .contains("the tree has changed since it was built"));
}

// Caps: at most 5 pulled (with an honest "and N more" pointer) and 3 left-out.
#[test]
fn caps_are_applied_with_an_overflow_pointer() {
    let many: Vec<SelectedFile> = (0..8)
        .map(|i| picked(&format!("src/f{i}.rs"), "imported"))
        .collect();
    let outs: Vec<IgnoredFile> = (0..5)
        .map(|i| out(&format!("src/o{i}.rs"), "unreachable"))
        .collect();
    let lines = manifest_lines(&selection(many, outs), true, &|_| false);
    let pulled = lines.iter().filter(|l| l.starts_with("pulling ")).count();
    let left = lines
        .iter()
        .filter(|l| l.starts_with("leaving out "))
        .count();
    assert_eq!(pulled, 5);
    assert_eq!(left, 3);
    assert!(lines
        .iter()
        .any(|l| l.contains("and 3 more connected file(s): tomte why-context src/a.rs")));
}

// No cached twin → no manifest: the hot path never builds the index inline.
#[test]
fn for_edit_without_a_cache_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "fn main() {}\n").unwrap();
    let lines = for_edit(tmp.path(), "a.rs", &std::collections::HashSet::new());
    assert!(lines.is_empty());
}
