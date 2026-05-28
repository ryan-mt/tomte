//! Integration: verify hooks matcher dispatcher behaves as documented.
use opencli_core::hooks::{glob_match, matches};

#[test]
fn matches_dispatch_table() {
    assert!(matches("*", "any_tool", None));
    assert!(matches("*", "", None));
    assert!(matches("run_shell", "run_shell", None));
    assert!(!matches("run_shell", "read_file", None));
    assert!(matches("re:edit_", "edit_file", None));
    assert!(matches("re:edit_", "multi_edit_x", None));
    assert!(!matches("re:edit_", "read_file", None));
    assert!(matches(
        "file:**/*.rs",
        "edit_file",
        Some("crates/core/src/lib.rs")
    ));
    assert!(matches("file:src/*.ts", "write_file", Some("src/app.ts")));
    assert!(!matches("file:**/*.rs", "edit_file", Some("README.md")));
    assert!(!matches("file:**/*.rs", "edit_file", None));
}

#[test]
fn glob_match_edge_cases() {
    assert!(glob_match("", ""));
    assert!(!glob_match("", "x"));
    assert!(glob_match("**", "anything/anywhere/yes"));
    assert!(glob_match("a/**/c.rs", "a/b/c.rs"));
    assert!(glob_match("a/**/c.rs", "a/b/d/c.rs"));
    assert!(glob_match("?bc", "abc"));
    assert!(!glob_match("?bc", "abbc"));
    assert!(glob_match("file_*", "file_log"));
    assert!(!glob_match("file_*", "log_file"));
}
