//! Per-project tool-permission allow-list, mirroring Claude Code's
//! `permissions.allow`. Persisted at `<cwd>/.opencli/permissions.json` so that
//! choosing "allow in this project" on an approval prompt survives across
//! sessions and the agent stops re-asking for that tool or command.
//!
//! Rules use a `Tool(specifier)` shape, a deliberately small subset of Claude
//! Code's rule syntax:
//!   - `run_shell(<prog>:*)` — any shell command whose program (first word) is
//!     `<prog>`, so allowing `cargo build` also allows `cargo test`.
//!   - `<tool_name>`         — that tool unconditionally, in this project.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The on-disk permission rules. `deny` takes precedence over `allow`; a call
/// matching no rule falls through to the normal approval prompt. Older files
/// without a `deny` key still parse (serde defaults it empty).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectPermissions {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// What the project rules say about a tool call, consulted before prompting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// A `deny` rule matched — block the call outright, no prompt.
    Deny,
    /// An `allow` rule matched (and no `deny`) — run without prompting.
    Allow,
    /// No rule matched — fall through to the normal approval prompt.
    Ask,
}

/// `<cwd>/.opencli/permissions.json` — the project-local allow-list file.
pub fn permissions_path(cwd: &Path) -> PathBuf {
    cwd.join(".opencli").join("permissions.json")
}

/// Load the project allow-list, treating a missing or malformed file as empty
/// so a bad edit never blocks the agent (it just falls back to prompting).
pub fn load(cwd: &Path) -> ProjectPermissions {
    match std::fs::read_to_string(permissions_path(cwd)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => ProjectPermissions::default(),
    }
}

/// The allow-rule that "allow in this project" should persist for a tool call.
pub fn rule_for(tool_name: &str, args: &Value) -> String {
    if tool_name == "run_shell" {
        if let Some(prog) = shell_program(args) {
            return format!("run_shell({prog}:*)");
        }
    }
    tool_name.to_string()
}

/// Human-readable description of what granting a rule covers, for the modal
/// option label (e.g. "all `cargo` commands").
pub fn rule_label(tool_name: &str, args: &Value) -> String {
    if tool_name == "run_shell" {
        if let Some(prog) = shell_program(args) {
            return format!("all `{prog}` commands");
        }
    }
    format!("all `{tool_name}` calls")
}

/// Program name of a shell command: the first whitespace-delimited word with
/// any leading path stripped, so `/usr/bin/git` and `git` share one rule.
fn shell_program(args: &Value) -> Option<String> {
    let cmd = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .and_then(|v| v.as_str())?;
    let first = cmd.split_whitespace().next()?;
    let prog = first.rsplit('/').next().unwrap_or(first);
    (!prog.is_empty()).then(|| prog.to_string())
}

/// Resolve the project rules for a tool call. `deny` wins over `allow`; no
/// match means "ask". Checked before prompting so a previously-allowed
/// tool/command runs silently and a denied one is blocked outright.
pub fn decide(perms: &ProjectPermissions, tool_name: &str, args: &Value) -> Decision {
    if perms.deny.iter().any(|r| rule_matches(r, tool_name, args)) {
        return Decision::Deny;
    }
    if perms.allow.iter().any(|r| rule_matches(r, tool_name, args)) {
        return Decision::Allow;
    }
    Decision::Ask
}

/// Convenience wrapper kept for call sites that only care about the allow case.
pub fn is_allowed(perms: &ProjectPermissions, tool_name: &str, args: &Value) -> bool {
    matches!(decide(perms, tool_name, args), Decision::Allow)
}

/// Whether a stored rule matches a tool call. A rule is either a bare tool name
/// (`write_file` — any call of that tool) or `tool(spec)`:
///   - `run_shell(<prog>:*)` — shell command whose program is `<prog>`.
///   - `<file_tool>(<glob>)` — path argument matching the glob (`src/**`,
///     `*.rs`, `**/*.test.ts`, `.git/**`).
fn rule_matches(rule: &str, tool_name: &str, args: &Value) -> bool {
    let (rule_tool, spec) = match rule.split_once('(') {
        Some((t, rest)) => (t.trim(), rest.strip_suffix(')').unwrap_or(rest)),
        None => (rule.trim(), ""),
    };
    if rule_tool != tool_name {
        return false;
    }
    if spec.is_empty() {
        return true; // bare tool name: any call of this tool
    }
    if tool_name == "run_shell" {
        let prog = spec.strip_suffix(":*").unwrap_or(spec);
        return shell_program(args).as_deref() == Some(prog);
    }
    // File tools: the spec is a glob over the path argument as accepted by the
    // target tool. Keep this in sync with the runtime aliases so permission
    // rules don't silently miss camelCase/provider-shaped calls.
    match path_argument(tool_name, args) {
        Some(path) => glob_match(spec, path),
        None => false,
    }
}

fn path_argument<'a>(tool_name: &str, args: &'a Value) -> Option<&'a str> {
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
fn glob_match(pattern: &str, text: &str) -> bool {
    glob_inner(pattern.as_bytes(), text.as_bytes())
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

/// Add a rule for this tool call to `<cwd>/.opencli/permissions.json`
/// (idempotent), creating the directory and file as needed. Returns the rule
/// that was recorded so the caller can report it.
pub fn allow_in_project(cwd: &Path, tool_name: &str, args: &Value) -> std::io::Result<String> {
    let rule = rule_for(tool_name, args);
    let mut perms = load(cwd);
    if !perms.allow.iter().any(|r| r == &rule) {
        perms.allow.push(rule.clone());
    }
    std::fs::create_dir_all(cwd.join(".opencli"))?;
    let text = serde_json::to_string_pretty(&perms).unwrap_or_default();
    std::fs::write(permissions_path(cwd), text)?;
    Ok(rule)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shell_rule_keys_on_program() {
        let args = json!({"command": "cargo build --release"});
        assert_eq!(rule_for("run_shell", &args), "run_shell(cargo:*)");
        let args_cmd = json!({"cmd": "cargo test"});
        assert_eq!(rule_for("run_shell", &args_cmd), "run_shell(cargo:*)");
        // Leading path is stripped so the rule matches the bare program too.
        let args2 = json!({"command": "/usr/bin/git status"});
        assert_eq!(rule_for("run_shell", &args2), "run_shell(git:*)");
    }

    #[test]
    fn non_shell_rule_is_the_tool_name() {
        assert_eq!(rule_for("write_file", &json!({"path": "a"})), "write_file");
    }

    #[test]
    fn is_allowed_matches_program_and_whole_tool_rules() {
        let perms = ProjectPermissions {
            allow: vec!["run_shell(cargo:*)".into(), "write_file".into()],
            deny: vec![],
        };
        // Same program, different args → allowed.
        assert!(is_allowed(
            &perms,
            "run_shell",
            &json!({"command": "cargo test"})
        ));
        assert!(is_allowed(
            &perms,
            "run_shell",
            &json!({"cmd": "cargo clippy"})
        ));
        // Different program → still prompts.
        assert!(!is_allowed(
            &perms,
            "run_shell",
            &json!({"command": "rm -rf /"})
        ));
        // Whole-tool rule covers any args.
        assert!(is_allowed(&perms, "write_file", &json!({"path": "x"})));
        // Tool with no rule → prompts.
        assert!(!is_allowed(&perms, "edit_file", &json!({"path": "x"})));
    }

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
    fn deny_takes_precedence_over_allow() {
        let perms = ProjectPermissions {
            allow: vec!["run_shell(rm:*)".into()],
            deny: vec!["run_shell(rm:*)".into()],
        };
        assert_eq!(
            decide(&perms, "run_shell", &json!({"command": "rm -rf x"})),
            Decision::Deny
        );
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

    #[test]
    fn allow_in_project_persists_and_is_idempotent() {
        let tmp = std::env::temp_dir().join(format!("opencli-perm-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let args = json!({"command": "cargo build"});
        let rule = allow_in_project(&tmp, "run_shell", &args).unwrap();
        assert_eq!(rule, "run_shell(cargo:*)");
        // Re-adding the same call does not duplicate the rule.
        allow_in_project(&tmp, "run_shell", &json!({"command": "cargo test"})).unwrap();
        let perms = load(&tmp);
        assert_eq!(perms.allow, vec!["run_shell(cargo:*)".to_string()]);
        assert!(is_allowed(
            &perms,
            "run_shell",
            &json!({"command": "cargo run"})
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
