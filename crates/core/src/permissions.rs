//! Per-project tool-permission rules, mirroring Claude Code's `permissions`.
//! Two sources are merged (see [`load`]): the in-repo
//! `<cwd>/.opencli/permissions.json` is honored for `deny` ONLY — a cloned repo
//! may *tighten* what the agent can do but must never silently *grant* it —
//! while the user's own "allow in this project" choices persist in an
//! owner-only user-level store outside the repo (keyed by project path), so they
//! survive across sessions without letting a cloned repo pre-grant them.
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

mod glob;
mod shell;
mod store;

use glob::{glob_match, normalize_rule_path, path_argument};
use shell::{run_shell_rule_matches, shell_program};
use store::{
    add_allow_rule_at, load_project_file, merge_permissions, read_permissions_at,
    user_permissions_path,
};

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

/// `<cwd>/.opencli/permissions.json` — the in-repo project file. Honored for
/// `deny` only (see [`load`]); `allow` lives in the user-level store.
pub fn permissions_path(cwd: &Path) -> PathBuf {
    cwd.join(".opencli").join("permissions.json")
}

/// Load the effective rules for `cwd`: the repo file's `deny` plus the
/// user-level store's `allow`/`deny`. A missing or malformed file on either side
/// is treated as empty so a bad edit never blocks the agent.
pub fn load(cwd: &Path) -> ProjectPermissions {
    let user = read_permissions_at(&user_permissions_path(cwd));
    merge_permissions(load_project_file(cwd), user)
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

/// Which list a rule is being matched against — `run_shell` matching is
/// deliberately asymmetric (see [`run_shell_rule_matches`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    Allow,
    Deny,
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
        Some(path) => path_rule_matches(spec, path),
        None => false,
    }
}

/// Match a file-tool path spec (`.git/**`, `src/**`) against one path, lexically
/// normalized. Shared by [`rule_matches`] and [`deny_matches_resolved`].
fn path_rule_matches(spec: &str, path: &str) -> bool {
    glob_match(spec, &normalize_rule_path(path))
}

/// Re-evaluate `deny` rules against the *symlink-resolved* real path of a file
/// tool's path argument. [`decide`] matches the raw model-supplied string, but
/// the fs tools act on the canonicalized target, so an in-repo symlink whose
/// name doesn't match a deny glob (e.g. `link -> .git/config`) — or a
/// case-variant on a case-insensitive filesystem — would otherwise launder a
/// denied path. Returns true when the resolved sandbox-relative path matches a
/// file-tool deny rule. Best-effort: a path that fails to resolve (escaping or
/// invalid) is left to the tool's own `resolve` rejection.
pub fn deny_matches_resolved(
    perms: &ProjectPermissions,
    tool_name: &str,
    args: &Value,
    cwd: &Path,
) -> bool {
    if perms.deny.is_empty() || tool_name == "run_shell" {
        return false;
    }
    let Some(raw) = path_argument(tool_name, args) else {
        return false;
    };
    let (Ok(resolved), Ok(sandbox)) = (crate::tools::fs::resolve(cwd, raw), cwd.canonicalize())
    else {
        return false;
    };
    let Ok(rel) = resolved.strip_prefix(&sandbox) else {
        return false;
    };
    // Deny globs are written with `/`; normalize Windows separators.
    let rel = rel.to_string_lossy().replace('\\', "/");
    perms.deny.iter().any(|rule| match rule.split_once('(') {
        Some((t, rest)) => {
            let spec = rest.strip_suffix(')').unwrap_or(rest);
            t.trim() == tool_name && !spec.is_empty() && path_rule_matches(spec, &rel)
        }
        None => false,
    })
}

/// Persist an "allow in this project" grant to the user-level store (outside the
/// repo, under the owner-only config dir, keyed by project path). The in-repo
/// `.opencli/permissions.json` is intentionally NOT used for `allow` — a cloned
/// repo could otherwise pre-grant silent execution — only for `deny`. Returns
/// the rule that was recorded so the caller can report it.
pub fn allow_in_project(cwd: &Path, tool_name: &str, args: &Value) -> io::Result<String> {
    let rule = rule_for(tool_name, args);
    add_allow_rule_at(&user_permissions_path(cwd), rule.clone())?;
    Ok(rule)
}

#[cfg(test)]
mod tests;
