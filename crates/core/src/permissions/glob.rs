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
    // `glob_inner` fills an O(pattern·text) DP table. The pattern comes from an
    // untrusted `.tomte/permissions.json` deny rule and the text from a
    // model-supplied path, so a pathologically long input would pin memory and
    // stall every file-tool decision (the ReDoS fix bounded `**` backtracking but
    // not raw length). No real path glob or path approaches this cap, so an
    // over-long input is a non-match — consistent with this module's "a malformed
    // rule is ignored, never blocks" stance; only deny is untrusted, and a deny
    // that fails to match merely fails to tighten.
    const MAX_GLOB_LEN: usize = 4096;
    if pattern.len() > MAX_GLOB_LEN || text.len() > MAX_GLOB_LEN {
        return false;
    }
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

/// Collapse runs of `*` so a pattern like `***` or `**********` normalizes to a
/// single `**`. A run of length 1 stays `*` (within-segment wildcard); any run
/// of length >= 2 becomes a single `**` (cross-`/` wildcard), which is exactly
/// what a user writing `***` intends, so no valid pattern changes meaning.
/// (`glob_inner` fills a bounded DP table, so adjacent `**` groups no longer
/// backtrack exponentially; the source pattern is untrusted —
/// `.tomte/permissions.json`.)
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
    let (pn, tn) = (p.len(), t.len());
    // Bottom-up DP: dp[pi][ti] = does `p[pi..]` match `t[ti..]`. Filling it from
    // the high offsets down means each cell reads only already-filled cells (a
    // larger pattern offset, or the same offset with a larger text offset), so
    // the matcher never recurses. That bounds the work to O(pattern * text) —
    // immune both to the exponential backtracking adjacent `**` groups used to
    // cause and to a deep-recursion blowup on a long path, either reachable from
    // an untrusted `.tomte/permissions.json` pattern. The arms are the original
    // matcher, rewritten as table lookups.
    let mut dp = vec![vec![false; tn + 1]; pn + 1];
    for pi in (0..=pn).rev() {
        for ti in (0..=tn).rev() {
            dp[pi][ti] = if pi == pn {
                ti == tn
            } else if &p[pi..] == b"/**" {
                // A trailing `/**` also matches the bare prefix directory itself,
                // not only its children: a deny rule `dir/**` covers operating on
                // `dir` (e.g. `list_dir(dir)`), so `.git/**` matches `.git`. The
                // text is exhausted when the literal prefix consumed the whole
                // path; a `/`-led remainder is the children case `**` handles.
                ti == tn || t.get(ti) == Some(&b'/')
            } else if p[pi] == b'*' && p.get(pi + 1) == Some(&b'*') {
                // `**` — match any chars including `/`. Match the rest here, let a
                // `**/` elide its slash (so `**/*.rs` matches a root-level
                // `main.rs`), or let `**` consume one more text char and stay put.
                let rest = pi + 2;
                (p.get(rest) == Some(&b'/') && dp[rest + 1][ti])
                    || dp[rest][ti]
                    || (ti < tn && dp[pi][ti + 1])
            } else if p[pi] == b'*' {
                // single `*` — match a run of non-`/` chars (including empty).
                dp[pi + 1][ti] || (ti < tn && t[ti] != b'/' && dp[pi][ti + 1])
            } else if p[pi] == b'?' {
                ti < tn && t[ti] != b'/' && dp[pi + 1][ti + 1]
            } else {
                ti < tn && t[ti] == p[pi] && dp[pi + 1][ti + 1]
            };
        }
    }
    dp[0][0]
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
    fn adjacent_globstars_stay_polynomial_not_exponential() {
        // Adjacent `**` groups separated by literals each branched over every
        // text offset (O(text_len ^ globstars)) — a planted deny rule could hang
        // the agent. Memoization bounds it; both a matching and a worst-case
        // non-matching pattern must return promptly rather than spin.
        let text = "a".repeat(48);
        // ...ends in `a`, so 16 `**a` groups match (the `**`s absorb the slack).
        assert!(glob_match(&"**a".repeat(16), &text));
        // A trailing literal absent from the text cannot match, but must still
        // return (the exponential blowup happened on exactly this shape).
        assert!(!glob_match(&format!("{}X", "**a".repeat(16)), &text));
    }

    #[test]
    fn over_long_glob_is_ignored_not_a_dos() {
        // The matcher fills an O(pattern·text) DP table; an untrusted deny rule
        // with a pathologically long pattern (or a model-supplied giant path)
        // must return promptly as a non-match instead of allocating on every
        // file-tool decision. No real glob or path approaches the length cap.
        let huge = "a".repeat(10_000);
        assert!(!glob_match(&huge, "src/main.rs"));
        assert!(!glob_match("src/**", &"a/".repeat(10_000)));
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
    fn deny_dir_globstar_also_blocks_the_bare_dir() {
        // `dir/**` must cover operating on `dir` itself (e.g. listing it), not
        // only its children — otherwise `list_dir(.git)` leaks the denied tree.
        assert!(glob_match(".git/**", ".git"));
        assert!(glob_match("src/**", "src"));
        // A non-`/` continuation is still a different path, not a match.
        assert!(!glob_match("src/**", "srcfoo"));
        let perms = ProjectPermissions {
            allow: vec![],
            deny: vec!["list_dir(.git/**)".into()],
        };
        assert_eq!(
            decide(&perms, "list_dir", &json!({"path": ".git"})),
            Decision::Deny
        );
        // Children stay denied too (no regression).
        assert_eq!(
            decide(&perms, "list_dir", &json!({"path": ".git/config"})),
            Decision::Deny
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
