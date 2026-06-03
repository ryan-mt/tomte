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
    std::os::unix::fs::symlink(outside.path().join("secret.rs"), root.join("escape.rs")).unwrap();

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
