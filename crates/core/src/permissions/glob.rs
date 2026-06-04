//! Path normalization and the minimal glob matcher for path permission
//! rules, split out of `permissions`; logic unchanged.

use serde_json::Value;

/// Lexically normalize a path argument for glob matching: drop `./` and empty
/// (`//`) segments and resolve `..` without touching disk (the target may not
/// exist yet). A `..` that would climb above the start is kept literal, so an
/// escaping path like `../secret` can't normalize into an in-tree `secret` and
/// thereby match a clean relative rule. An absolute path stays absolute.
///
/// Note: a relative rule (`.git/**`) still won't match an *absolute* argument
/// (`/home/u/proj/.git/config`); writes are separately confined to the project
/// by `tools::fs::resolve`, and rules meant for absolute paths must be written
/// in absolute form.
pub(super) fn normalize_rule_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                if matches!(out.last(), Some(&s) if s != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

pub(super) fn path_argument<'a>(tool_name: &str, args: &'a Value) -> Option<&'a str> {
    let obj = args.as_object()?;
    let keys: &[&str] = match tool_name {
        "read_file" | "write_file" | "edit_file" | "multi_edit" | "list_dir" => &[
            "path",
            "file_path",
            "filePath",
            "directory",
            "dir",
            "folder",
        ],
        "notebook_edit" => &[
            "notebook_path",
            "notebookPath",
            "path",
            "file_path",
            "filePath",
        ],
        _ => &["path"],
    };
    keys.iter().find_map(|key| obj.get(*key)?.as_str())
}

/// Minimal glob matcher for path rules. `*` matches any run of non-`/`
/// characters, `**` matches across `/` (any chars), `?` matches one non-`/`
/// char; everything else is literal. No brace/char-class support — not needed
/// for permission paths.
pub(super) fn glob_match(pattern: &str, text: &str) -> bool {
    // Case-insensitive filesystems (the default on macOS and Windows) resolve
    // `.GIT/config` and `.git/config` to the same file, so a path rule must fold
    // case there or a deny glob is bypassed by changing case. Linux stays
    // case-sensitive (exact byte match).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let matched = {
        let pattern = pattern.to_ascii_lowercase();
        let text = text.to_ascii_lowercase();
        glob_inner(&collapse_globstars(&pattern), text.as_bytes())
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let matched = glob_inner(&collapse_globstars(pattern), text.as_bytes());
    matched
}

/// Collapse runs of `*` so a pattern like `***` or `**********` can't trigger
/// `glob_inner`'s O(n^k) backtracking — adjacent `**` groups each branch over
/// every text offset (line 499), and the source pattern is untrusted (it comes
/// from `.tomte/permissions.json`). A run of length 1 stays `*` (within-segment
/// wildcard); any run of length >= 2 becomes a single `**` (cross-`/` wildcard),
/// which is exactly what a user writing `***` intends, so no valid pattern
/// changes meaning.
fn collapse_globstars(pattern: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(pattern.len());
    let mut stars = 0usize;
    for b in pattern.bytes() {
        if b == b'*' {
            stars += 1;
            continue;
        }
        match stars {
            0 => {}
            1 => out.push(b'*'),
            _ => out.extend_from_slice(b"**"),
        }
        stars = 0;
        out.push(b);
    }
    match stars {
        0 => {}
        1 => out.push(b'*'),
        _ => out.extend_from_slice(b"**"),
    }
    out
}

fn glob_inner(p: &[u8], t: &[u8]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        b'*' if p.get(1) == Some(&b'*') => {
            // `**` — match any chars including `/`.
            let rest = &p[2..];
            // `**/` may match zero leading dirs: allow eliding the slash so
            // `**/*.rs` also matches a root-level `main.rs`.
            if rest.first() == Some(&b'/') && glob_inner(&rest[1..], t) {
                return true;
            }
            (0..=t.len()).any(|i| glob_inner(rest, &t[i..]))
        }
        b'*' => {
            // single `*` — match a run of non-`/` chars (including empty).
            let rest = &p[1..];
            let mut i = 0;
            loop {
                if glob_inner(rest, &t[i..]) {
                    return true;
                }
                if i >= t.len() || t[i] == b'/' {
                    return false;
                }
                i += 1;
            }
        }
        b'?' => !t.is_empty() && t[0] != b'/' && glob_inner(&p[1..], &t[1..]),
        c => !t.is_empty() && t[0] == c && glob_inner(&p[1..], &t[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{decide, Decision, ProjectPermissions};
    use super::*;
    use serde_json::json;

    #[test]
    fn glob_matches_paths() {
        assert!(glob_match("src/**", "src/a/b.rs"));
        assert!(glob_match("src/**", "src/x.rs"));
        assert!(!glob_match("src/**", "tests/x.rs"));
        assert!(glob_match("*.rs", "main.rs"));
        // A single `*` does not cross a path separator.
        assert!(!glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "src/a/main.rs"));
        assert!(glob_match("**/*.rs", "main.rs"));
        assert!(glob_match(".git/**", ".git/config"));
        assert!(glob_match("?.txt", "a.txt"));
        assert!(!glob_match("?.txt", "ab.txt"));
    }

    #[test]
    fn glob_match_collapses_star_runs_and_stays_linear() {
        // A run of `*` collapses to `**`/`*`, so a long run can't blow up.
        assert_eq!(collapse_globstars("***"), b"**");
        assert_eq!(collapse_globstars("*"), b"*");
        assert_eq!(collapse_globstars("a***b*c"), b"a**b*c");
        // Semantics preserved: `***` behaves like `**`.
        assert!(glob_match("***", "a/b.rs"));
        assert!(glob_match("**", "a/b.rs"));
        // A pathological all-stars pattern returns immediately instead of
        // backtracking O(n^k). A trailing literal that can't match still
        // terminates fast (no deep recursion).
        let many = "*".repeat(64);
        assert!(glob_match(&many, "a/b/c/d/e/f.rs"));
        assert!(!glob_match(&format!("{many}x"), "a/b/c/d/e/f.rs"));
    }

    #[test]
    fn glob_allow_rule_scopes_to_path() {
        let perms = ProjectPermissions {
            allow: vec!["write_file(src/**)".into()],
            deny: vec![],
        };
        assert_eq!(
            decide(&perms, "write_file", &json!({"path": "src/a.rs"})),
            Decision::Allow
        );
        assert_eq!(
            decide(&perms, "write_file", &json!({"path": "secrets.txt"})),
            Decision::Ask
        );
    }

    #[test]
    fn glob_rules_match_file_path_aliases() {
        let perms = ProjectPermissions {
            allow: vec!["write_file(src/**)".into()],
            deny: vec!["write_file(.git/**)".into()],
        };
        assert_eq!(
            decide(&perms, "write_file", &json!({"file_path": ".git/config"})),
            Decision::Deny
        );
        assert_eq!(
            decide(&perms, "write_file", &json!({"file_path": "src/a.rs"})),
            Decision::Allow
        );
        assert_eq!(
            decide(&perms, "write_file", &json!({"filePath": ".git/config"})),
            Decision::Deny
        );
        assert_eq!(
            decide(&perms, "write_file", &json!({"filePath": "src/a.rs"})),
            Decision::Allow
        );
    }

    #[test]
    fn glob_rules_match_list_dir_aliases() {
        let perms = ProjectPermissions {
            allow: vec!["list_dir(src/**)".into()],
            deny: vec!["list_dir(.git/**)".into()],
        };
        assert_eq!(
            decide(&perms, "list_dir", &json!({"directory": ".git/config"})),
            Decision::Deny
        );
        assert_eq!(
            decide(&perms, "list_dir", &json!({"folder": "src/components"})),
            Decision::Allow
        );
    }

    #[test]
    fn glob_rules_match_notebook_path_aliases() {
        let perms = ProjectPermissions {
            allow: vec!["notebook_edit(notebooks/**)".into()],
            deny: vec!["notebook_edit(.git/**)".into()],
        };
        assert_eq!(
            decide(
                &perms,
                "notebook_edit",
                &json!({"notebook_path": ".git/config"})
            ),
            Decision::Deny
        );
        assert_eq!(
            decide(
                &perms,
                "notebook_edit",
                &json!({"notebook_path": "notebooks/demo.ipynb"})
            ),
            Decision::Allow
        );
        assert_eq!(
            decide(
                &perms,
                "notebook_edit",
                &json!({"path": "notebooks/demo.ipynb"})
            ),
            Decision::Allow
        );
        assert_eq!(
            decide(
                &perms,
                "notebook_edit",
                &json!({"notebookPath": ".git/config"})
            ),
            Decision::Deny
        );
        assert_eq!(
            decide(
                &perms,
                "notebook_edit",
                &json!({"notebookPath": "notebooks/demo.ipynb"})
            ),
            Decision::Allow
        );
    }

    #[test]
    fn deny_glob_is_not_bypassed_by_path_spelling() {
        let perms = ProjectPermissions {
            allow: vec![],
            deny: vec!["write_file(.git/**)".into()],
        };
        for p in [
            ".git/config",
            "./.git/config",
            ".git//config",
            ".git/x/../config",
        ] {
            assert_eq!(
                decide(&perms, "write_file", &json!({ "path": p })),
                Decision::Deny,
                "expected deny for spelling: {p}"
            );
        }
    }

    #[test]
    fn normalize_keeps_escaping_paths_from_matching_in_tree_rules() {
        // `../secret` must NOT normalize to `secret` and match a relative rule.
        assert_eq!(normalize_rule_path("../secret"), "../secret");
        assert_eq!(normalize_rule_path("./a//b/../c"), "a/c");
        assert_eq!(normalize_rule_path("/abs/./p"), "/abs/p");
        let perms = ProjectPermissions {
            allow: vec!["write_file(secret)".into()],
            deny: vec![],
        };
        assert_eq!(
            decide(&perms, "write_file", &json!({"path": "../secret"})),
            Decision::Ask
        );
    }

    #[test]
    fn deny_blocks_even_without_an_allow() {
        let perms = ProjectPermissions {
            allow: vec![],
            deny: vec!["write_file(.git/**)".into()],
        };
        assert_eq!(
            decide(&perms, "write_file", &json!({"path": ".git/config"})),
            Decision::Deny
        );
        assert_eq!(
            decide(&perms, "write_file", &json!({"path": "src/a.rs"})),
            Decision::Ask
        );
    }
}
