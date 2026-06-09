//! Sub-agent (a.k.a. "Task") definitions and loader.
//!
//! A subagent is a named bundle of (system prompt, tool whitelist, optional
//! model override) stored as a markdown file under
//! `~/.config/tomte/agents/<name>.md`. The host can spawn a child agent
//! that runs a single turn with this configuration and returns the final
//! assistant text to the parent.
//!
//! File format — Claude Code-compatible:
//!
//! ```markdown
//! ---
//! name: code-explorer
//! description: Search the codebase to answer questions about it.
//! tools: read_file, grep, glob, list_dir
//! model: gpt-5-mini
//! ---
//! You are a focused code explorer. Use the tools …
//! ```
//!
//! Frontmatter is a minimal YAML-ish subset: one `key: value` per line.
//! `tools` accepts both tomte's comma-separated form (`read_file, grep`)
//! and Claude Code's JSON-array form (`["Read", "Grep"]`); tool names are
//! canonicalised to tomte built-ins at registry-build time. `model` is
//! optional and may be a Claude Code alias (`sonnet`, `opus`, `haiku`) which
//! is resolved to a concrete model id. Unrecognised keys are ignored. The
//! body after the closing `---` becomes the system prompt.
//!
//! Definitions are discovered across multiple sources (most-specific first;
//! first occurrence of a `name` wins) so tomte can use the sub-agents of
//! other tools directly:
//!   - `<cwd>/.tomte/agents/`     project, tomte-native
//!   - `<cwd>/.claude/agents/`      project, Claude Code
//!   - `<cwd>/.codex/agents/`       project, Codex-compatible
//!   - `~/.config/tomte/agents/`  tomte global
//!   - `~/.claude/agents/`          Claude Code global
//!   - `$CODEX_HOME/agents` or `~/.codex/agents`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// A loaded sub-agent definition ready to drive a child `Agent` turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentDefinition {
    pub name: String,
    pub description: String,
    /// Whitelist of built-in tool names the sub-agent is allowed to call.
    /// Empty or `["*"]` means "all built-ins". Unknown names are dropped on
    /// load with a warning.
    pub tools: Vec<String>,
    /// Optional model override (e.g. `gpt-5-mini`). When None, the parent
    /// agent's model is inherited.
    pub model: Option<String>,
    pub system_prompt: String,
}

/// Max bytes for a subagent `<name>.md`. Cloned repos control the cwd-relative
/// agent roots, so a planted multi-GB file (or a `/dev/zero` symlink) must not
/// OOM the CLI when definitions are enumerated at startup.
const MAX_SUBAGENT_BYTES: u64 = 1024 * 1024;

pub fn subagents_dir() -> PathBuf {
    crate::config::config_dir().join("agents")
}

/// Ordered sub-agent root directories, most-specific first. The first root
/// that defines a given `name` wins, so a project or tomte-native agent can
/// shadow a global one. Includes Claude Code and Codex agent directories so
/// tomte can use those sub-agents directly.
pub fn subagent_roots(cwd: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        cwd.join(".tomte").join("agents"),
        cwd.join(".claude").join("agents"),
        cwd.join(".codex").join("agents"),
        subagents_dir(),
    ];
    if let Some(home) = dirs::home_dir() {
        push_unique(&mut roots, home.join(".claude").join("agents"));
        push_unique(&mut roots, home.join(".codex").join("agents"));
    }
    if let Some(codex_home) = env_path("CODEX_HOME") {
        push_unique(&mut roots, codex_home.join("agents"));
    }
    roots
}

/// Whether `name` resolves (with `load_by_name`'s precedence) to a project-local
/// definition file — one under a cwd-relative root (`<cwd>/.tomte|.claude|
/// .codex/agents/`). Such a file ships in a cloned repo and is attacker-
/// controlled, so the dispatcher confines it to read-only tools regardless of
/// the parent's approval mode; a global/user agent is trusted and unaffected.
///
/// This MUST mirror `load_by_name`'s resolution — including its frontmatter-name
/// fallback — or the two disagree about which file wins and the confinement is
/// bypassed: a planted `innocuous.md` with `name: deploy`, dispatched as
/// `deploy`, would load locally yet (without the fallback below) be judged
/// non-local and run with mutating tools under Auto / --dangerously-skip.
pub fn is_project_local(cwd: &Path, name: &str) -> bool {
    if name.is_empty() || name.contains(['/', '\\', '.']) {
        return false;
    }
    let roots = subagent_roots(cwd);
    // Phase 1, like load_by_name: `<root>/<name>.md`, first readable file wins.
    for root in &roots {
        let path = root.join(format!("{name}.md"));
        if crate::config::read_text_file_capped(&path, MAX_SUBAGENT_BYTES).is_ok() {
            return root.starts_with(cwd);
        }
    }
    // Phase 2, like load_by_name's fallback: the first root (in precedence order)
    // holding a `*.md` whose frontmatter `name` matches.
    for root in &roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(text) = crate::config::read_text_file_capped(&path, MAX_SUBAGENT_BYTES) else {
                continue;
            };
            if parse(&text, &path).is_ok_and(|def| def.name == name) {
                return root.starts_with(cwd);
            }
        }
    }
    false
}

fn env_path(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    (!path.as_os_str().is_empty()).then_some(path)
}

fn push_unique(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if !roots.iter().any(|root| root == &path) {
        roots.push(path);
    }
}

/// System prompt for the read-only `Explore` built-in: find code and report,
/// never mutate. Flush-left (like [`crate::agent::default_system_prompt`]) so
/// no source indentation leaks into the prompt the model actually receives.
const EXPLORE_PROMPT: &str = r#"You are a focused codebase exploration agent running as a sub-agent. Your job is to LOCATE and REPORT — never to modify.

- Use `grep`/`glob`/`list_dir` to find the files that matter, then `read_file` to read the relevant parts. Search broadly (multiple patterns, plausible naming variants) before you conclude.
- The parent agent sees ONLY your final text, not your tool calls — so lead with the direct answer, then back it with concrete `path:line` evidence.
- Never speculate about code you have not read. If you cannot find something, say so and say where you looked.
- Stay read-only: do not write files, run mutating commands, or change state."#;

/// System prompt for the read-only `Plan` built-in: investigate, then return a
/// concrete implementation plan grounded in real files.
const PLAN_PROMPT: &str = r#"You are a software architect agent running as a sub-agent. Investigate the codebase read-only and return a concrete, step-by-step implementation plan — you do NOT write code.

- Read the relevant code first; ground every step in real files and `path:line` references rather than assumptions.
- Output, in order: the goal restated in one line, the files that must change, the ordered implementation steps, the key trade-offs, and the risks or unknowns.
- Prefer the simplest approach that satisfies the requirement. If the requester's stated approach is more complex than it needs to be or is risky, say so and propose the better path.
- Stay read-only: do not write files or run mutating commands."#;

/// System prompt for the read-only `code-reviewer` built-in: evidence-backed
/// review of code or a diff.
const REVIEW_PROMPT: &str = r#"You are a code review agent running as a sub-agent. Review the code or diff you are pointed at for correctness bugs, security issues, and clear simplifications.

- Inspect the ACTUAL code; when reviewing changes, read the diff with `git diff`/`git log` (via `run_shell`) and the surrounding code — never review from assumption.
- Report only evidence-backed findings, each with a `path:line` and a concrete fix, ordered by severity (correctness/security first, style last).
- If you find nothing substantive, say so plainly rather than inventing nits."#;

/// Built-in subagent definitions that need no file on disk. These mirror the
/// agent types models trained on Claude Code reach for by default, so a fresh
/// install — with no agent files yet — resolves the common `subagent_type`
/// values instead of failing every `dispatch_agent` call:
///   - `general-purpose` — every built-in tool, inherits the parent's prompt
///     (empty `system_prompt`) and model; the catch-all and dispatch fallback.
///   - `Explore` — read-only code search ("find X and report").
///   - `Plan` — read-only architecture planning ("design the change").
///   - `code-reviewer` — read-only review of code or a diff.
///
/// The read-only built-ins carry only search/read tools (plus `run_shell` for
/// the reviewer's `git diff`), so they cannot mutate even outside enforced plan
/// mode. A file of the same name in any agents root overrides the built-in
/// (see [`load_all`]'s `or_insert`), and loose-spelled names fold onto these via
/// [`builtin_alias`].
pub fn builtin_subagents() -> Vec<SubagentDefinition> {
    vec![
        SubagentDefinition {
            name: "general-purpose".to_string(),
            description: "General-purpose agent for researching complex questions and executing \
                          multi-step tasks; has access to all tools."
                .to_string(),
            tools: Vec::new(),            // empty = all built-ins
            model: None,                  // inherit the parent's model
            system_prompt: String::new(), // empty = keep the parent agent's system prompt
        },
        SubagentDefinition {
            name: "Explore".to_string(),
            description: "Read-only code-search agent: locates code across the repo and reports \
                          findings with file:line evidence."
                .to_string(),
            tools: vec![
                "read_file".to_string(),
                "grep".to_string(),
                "glob".to_string(),
                "list_dir".to_string(),
            ],
            model: None,
            system_prompt: EXPLORE_PROMPT.to_string(),
        },
        SubagentDefinition {
            name: "Plan".to_string(),
            description: "Read-only architect agent: investigates the codebase and returns a \
                          step-by-step implementation plan."
                .to_string(),
            tools: vec![
                "read_file".to_string(),
                "grep".to_string(),
                "glob".to_string(),
                "list_dir".to_string(),
            ],
            model: None,
            system_prompt: PLAN_PROMPT.to_string(),
        },
        SubagentDefinition {
            name: "code-reviewer".to_string(),
            description: "Read-only review agent: reviews code or a diff for correctness, \
                          security, and simplifications with file:line evidence."
                .to_string(),
            tools: vec![
                "read_file".to_string(),
                "grep".to_string(),
                "glob".to_string(),
                "list_dir".to_string(),
                "run_shell".to_string(),
            ],
            model: None,
            system_prompt: REVIEW_PROMPT.to_string(),
        },
    ]
}

/// Names of the built-in subagents, in their canonical (resolvable) spelling.
/// The single source of truth shared by the system-prompt roster and its guard
/// test, so the advertised list can never drift from [`builtin_subagents`].
pub fn builtin_subagent_names() -> Vec<&'static str> {
    vec!["general-purpose", "Explore", "Plan", "code-reviewer"]
}

/// Fold a loosely-spelled `subagent_type` onto a built-in name. Models trained
/// on Claude Code emit many names for the same intent (`code-explorer`,
/// `explorer`, `search-agent`, `planner`, `security-reviewer`, …); rather than
/// fail the dispatch, map the common synonyms to the built-in that fits.
/// Matching is case-insensitive and ignores separators (`-`/`_`/space), so
/// `Code_Explorer` and `code-explorer` both resolve. Returns the built-in's
/// exact name, or `None` when nothing sensible matches (the caller then errors
/// or falls back to `general-purpose`). Only ever maps to a built-in, never to a
/// project-local file, so the dispatcher's plan-mode confinement is unaffected.
pub fn builtin_alias(name: &str) -> Option<&'static str> {
    let key: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();
    let target = match key.as_str() {
        "explore" | "explorer" | "codeexplorer" | "codeexploration" | "search" | "searcher"
        | "searchagent" | "codesearch" | "locator" | "finder" | "explorers" => "Explore",
        "plan" | "planner" | "planning" | "planmode" | "architect" | "designer" => "Plan",
        "review" | "reviewer" | "codereviewer" | "codereview" | "securityreviewer"
        | "securityreview" | "security" | "auditor" | "audit" => "code-reviewer",
        "generalpurpose" | "general" | "researcher" | "research" | "assistant" | "default"
        | "agent" | "task" => "general-purpose",
        _ => return None,
    };
    Some(target)
}

/// Load every `*.md` sub-agent across all roots, deduplicated by `name`
/// (first root in precedence order wins) and sorted by name. Files that fail
/// to parse are logged and skipped — never abort the host process.
pub fn load_all(cwd: &Path) -> Vec<SubagentDefinition> {
    let mut by_name: BTreeMap<String, SubagentDefinition> = BTreeMap::new();
    for dir in subagent_roots(cwd) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match crate::config::read_text_file_capped(&path, MAX_SUBAGENT_BYTES) {
                Ok(text) => match parse(&text, &path) {
                    // First root wins: don't overwrite a name already seen.
                    Ok(def) => {
                        by_name.entry(def.name.clone()).or_insert(def);
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "subagent parse failed; skipping");
                    }
                },
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "subagent read failed; skipping");
                }
            }
        }
    }
    // Built-in subagents (e.g. general-purpose) need no file; a same-named file
    // loaded above keeps precedence via `or_insert`.
    for def in builtin_subagents() {
        by_name.entry(def.name.clone()).or_insert(def);
    }
    by_name.into_values().collect()
}

/// Load a single subagent definition by name, searching each root in
/// precedence order. Prefer `<root>/<name>.md`, then fall back to the parsed
/// frontmatter `name` so `/agents` never advertises an un-callable name.
pub fn load_by_name(cwd: &Path, name: &str) -> Result<SubagentDefinition> {
    if name.is_empty() || name.contains(['/', '\\', '.']) {
        return Err(anyhow!(
            "invalid subagent name `{name}`; must be a bare identifier"
        ));
    }
    for root in subagent_roots(cwd) {
        let path = root.join(format!("{name}.md"));
        // A readable file ends the search: surface its parse result (so a
        // malformed definition reports its own error rather than NotFound).
        if let Ok(text) = crate::config::read_text_file_capped(&path, MAX_SUBAGENT_BYTES) {
            return parse(&text, &path);
        }
    }
    if let Some(def) = load_all(cwd).into_iter().find(|def| def.name == name) {
        return Ok(def);
    }
    // Loose-name fallback: fold a synonym (`code-explorer`, `planner`, …) onto a
    // built-in so a model that guesses a Claude-Code-ish name still gets a
    // sensible agent. `target != name` guards the recursion (the built-in name
    // already resolved above if it existed), and a user file overriding that
    // built-in still wins because the recursive call hits `load_all` first.
    if let Some(target) = builtin_alias(name) {
        if target != name {
            return load_by_name(cwd, target);
        }
    }
    Err(anyhow!(
        "subagent `{name}` not found in any agents directory (looked in project .tomte/.claude/.codex, ~/.config/tomte/agents, ~/.claude/agents, and ~/.codex/agents)"
    ))
}

/// Resolve a model alias to a concrete Anthropic model id. A
/// `~/.claude/agents/*.md` file commonly sets `model: sonnet`; sent verbatim
/// that 404s at the API, so map the well-known aliases. Anything already
/// concrete (or an OpenAI id) passes through unchanged.
pub fn resolve_model_alias(model: &str) -> String {
    match model.trim().to_ascii_lowercase().as_str() {
        "fable" => "claude-fable-5".to_string(),
        "sonnet" => "claude-sonnet-4-6".to_string(),
        "opus" => "claude-opus-4-8".to_string(),
        "haiku" => "claude-haiku-4-5".to_string(),
        _ => model.to_string(),
    }
}

/// Parse a subagent markdown with YAML-ish frontmatter delimited by `---`.
///
/// Tolerant of:
/// - Leading whitespace / BOM
/// - CRLF line endings
/// - Trailing whitespace in values
/// - Quoted string values (single or double)
pub fn parse(text: &str, path: &Path) -> Result<SubagentDefinition> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    let rest = trimmed.strip_prefix("---").ok_or_else(|| {
        anyhow!(
            "subagent at {} missing `---` frontmatter opener",
            path.display()
        )
    })?;
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    // Closing fence: a `\n---` where the `---` ends the line (newline or EOF).
    // A bare `rest.find("\n---")` also matched `\n----`/`\n---foo`, truncating
    // the frontmatter early.
    let end_idx = rest
        .match_indices("\n---")
        .find(|(i, _)| {
            let after = &rest[i + 4..];
            after.is_empty() || after.starts_with('\n') || after.starts_with('\r')
        })
        .map(|(i, _)| i)
        .ok_or_else(|| {
            anyhow!(
                "subagent at {} missing closing `---` frontmatter line",
                path.display()
            )
        })?;
    let frontmatter = &rest[..end_idx];
    let mut body = &rest[end_idx + "\n---".len()..];
    body = body.strip_prefix('\r').unwrap_or(body);
    body = body.strip_prefix('\n').unwrap_or(body);

    let mut name = String::new();
    let mut description = String::new();
    let mut tools: Vec<String> = Vec::new();
    let mut model: Option<String> = None;

    let mut lines = frontmatter.lines().peekable();
    while let Some(raw_line) = lines.next() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let value = strip_quotes(v.trim());
        match key.as_str() {
            "name" => name = value.to_string(),
            "description" => description = value.to_string(),
            "tools" => {
                tools = if value.is_empty() {
                    // YAML block-sequence form:
                    //   tools:
                    //     - Read
                    //     - Grep
                    // Without this the `- item` lines (no `:`) are skipped and
                    // `tools` stays empty → treated as wildcard → the subagent
                    // silently gets ALL tools instead of the whitelisted set.
                    let mut collected = Vec::new();
                    while let Some(peek) = lines.peek() {
                        let Some(item) = peek.trim().strip_prefix('-') else {
                            break;
                        };
                        let item = strip_quotes(item.trim()).trim().to_string();
                        if !item.is_empty() {
                            collected.push(item);
                        }
                        lines.next();
                    }
                    collected
                } else {
                    // Inline form: `tools: read_file, grep` or `["Read","Grep"]`.
                    parse_tool_list(value)
                };
            }
            "model" if !value.is_empty() => {
                model = Some(value.to_string());
            }
            _ => {
                // Unrecognised keys are ignored, allowing forward-compat
                // additions in user files without breaking older tomte.
            }
        }
    }

    if name.is_empty() {
        return Err(anyhow!(
            "subagent at {} missing required `name` field",
            path.display()
        ));
    }

    Ok(SubagentDefinition {
        name,
        description,
        tools,
        model,
        system_prompt: body.to_string(),
    })
}

fn strip_quotes(s: &str) -> &str {
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Parse a `tools:` value into a list of tool names. Handles both tomte's
/// comma form (`read_file, grep`) and Claude Code's JSON-array form
/// (`["Read", "Grep"]` or unquoted `[Read, Grep]`) by stripping surrounding
/// brackets and per-item quotes. Names are NOT canonicalised here — that
/// happens in `Registry::filtered`.
fn parse_tool_list(value: &str) -> Vec<String> {
    let v = value.trim();
    let v = v.strip_prefix('[').unwrap_or(v);
    let v = v.strip_suffix(']').unwrap_or(v);
    v.split(',')
        .map(|s| strip_quotes(s.trim()).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests;
