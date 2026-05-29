//! Sub-agent (a.k.a. "Task") definitions and loader.
//!
//! A subagent is a named bundle of (system prompt, tool whitelist, optional
//! model override) stored as a markdown file under
//! `~/.config/opencli/agents/<name>.md`. The host can spawn a child agent
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
//! `tools` accepts both opencli's comma-separated form (`read_file, grep`)
//! and Claude Code's JSON-array form (`["Read", "Grep"]`); tool names are
//! canonicalised to opencli built-ins at registry-build time. `model` is
//! optional and may be a Claude Code alias (`sonnet`, `opus`, `haiku`) which
//! is resolved to a concrete model id. Unrecognised keys are ignored. The
//! body after the closing `---` becomes the system prompt.
//!
//! Definitions are discovered across multiple sources (most-specific first;
//! first occurrence of a `name` wins) so opencli can use the sub-agents of
//! other tools directly:
//!   - `<cwd>/.opencli/agents/`     project, opencli-native
//!   - `<cwd>/.claude/agents/`      project, Claude Code
//!   - `~/.config/opencli/agents/`  opencli global
//!   - `~/.claude/agents/`          Claude Code global

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

pub fn subagents_dir() -> PathBuf {
    crate::config::config_dir().join("agents")
}

/// Ordered sub-agent root directories, most-specific first. The first root
/// that defines a given `name` wins, so a project or opencli-native agent can
/// shadow a global one. Includes `~/.claude/agents/` so opencli can use
/// Claude Code's sub-agents directly.
pub fn subagent_roots(cwd: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        cwd.join(".opencli").join("agents"),
        cwd.join(".claude").join("agents"),
        subagents_dir(),
    ];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".claude").join("agents"));
    }
    roots
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
            match std::fs::read_to_string(&path) {
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
        if let Ok(text) = std::fs::read_to_string(&path) {
            return parse(&text, &path);
        }
    }
    if let Some(def) = load_all(cwd).into_iter().find(|def| def.name == name) {
        return Ok(def);
    }
    Err(anyhow!(
        "subagent `{name}` not found in any agents directory (looked in ~/.config/opencli/agents, ~/.claude/agents, and project .opencli/.claude)"
    ))
}

/// Resolve a Claude Code model alias to a concrete Anthropic model id. A
/// `~/.claude/agents/*.md` file commonly sets `model: sonnet`; sent verbatim
/// that 404s at the API, so map the well-known aliases. Anything already
/// concrete (or an OpenAI id) passes through unchanged.
pub fn resolve_model_alias(model: &str) -> String {
    match model.trim().to_ascii_lowercase().as_str() {
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

    for raw_line in frontmatter.lines() {
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
                tools = parse_tool_list(value);
            }
            "model" if !value.is_empty() => {
                model = Some(value.to_string());
            }
            _ => {
                // Unrecognised keys are ignored, allowing forward-compat
                // additions in user files without breaking older opencli.
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

/// Parse a `tools:` value into a list of tool names. Handles both opencli's
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
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/{name}.md"))
    }

    #[test]
    fn parse_minimal_definition() {
        let text = "---\nname: explorer\ndescription: walks the tree\n---\nbody here\n";
        let def = parse(text, &fake("explorer")).unwrap();
        assert_eq!(def.name, "explorer");
        assert_eq!(def.description, "walks the tree");
        assert!(def.tools.is_empty());
        assert!(def.model.is_none());
        assert_eq!(def.system_prompt, "body here\n");
    }

    #[test]
    fn parse_with_tools_and_model() {
        let text = "---\nname: x\ndescription: y\ntools: read_file, grep, glob\nmodel: gpt-5-mini\n---\nsys\n";
        let def = parse(text, &fake("x")).unwrap();
        assert_eq!(def.tools, vec!["read_file", "grep", "glob"]);
        assert_eq!(def.model.as_deref(), Some("gpt-5-mini"));
    }

    #[test]
    fn parse_tolerates_bom_and_crlf() {
        let text = "\u{feff}---\r\nname: bom\r\ndescription: ok\r\n---\r\nbody\r\n";
        let def = parse(text, &fake("bom")).unwrap();
        assert_eq!(def.name, "bom");
        assert_eq!(def.system_prompt, "body\r\n");
    }

    #[test]
    fn parse_strips_quoted_values() {
        let text = "---\nname: \"quoted-name\"\ndescription: 'single quoted'\n---\nx\n";
        let def = parse(text, &fake("q")).unwrap();
        assert_eq!(def.name, "quoted-name");
        assert_eq!(def.description, "single quoted");
    }

    #[test]
    fn parse_rejects_missing_frontmatter() {
        let err = parse("no front matter here\n", &fake("bad")).unwrap_err();
        assert!(err.to_string().contains("missing `---` frontmatter opener"));
    }

    #[test]
    fn parse_rejects_unterminated_frontmatter() {
        let err = parse("---\nname: x\n", &fake("bad")).unwrap_err();
        assert!(err.to_string().contains("missing closing `---`"));
    }

    #[test]
    fn parse_rejects_missing_name() {
        let err = parse("---\ndescription: only desc\n---\nbody\n", &fake("bad")).unwrap_err();
        assert!(err.to_string().contains("missing required `name`"));
    }

    #[test]
    fn load_by_name_rejects_path_traversal() {
        let cwd = std::path::Path::new(".");
        for bad in ["../etc/passwd", "agents/sub", "a.b", ""] {
            let err = load_by_name(cwd, bad).unwrap_err();
            assert!(err.to_string().contains("invalid") || err.to_string().contains("not found"));
        }
    }

    #[test]
    fn load_by_name_falls_back_to_frontmatter_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".opencli").join("agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("filename.md"),
            "---\nname: frontmatter-name\ndescription: d\n---\nbody\n",
        )
        .unwrap();

        let def = load_by_name(tmp.path(), "frontmatter-name").unwrap();
        assert_eq!(def.name, "frontmatter-name");
        assert_eq!(def.system_prompt, "body\n");
    }

    #[test]
    fn ignores_unknown_keys_for_forward_compat() {
        let text = "---\nname: fwd\ndescription: d\nfuture_field: foo\nmax_turns: 5\n---\nbody\n";
        let def = parse(text, &fake("fwd")).unwrap();
        assert_eq!(def.name, "fwd");
    }

    #[test]
    fn parse_tools_claude_code_json_array() {
        // Quoted JSON array, as written by ~/.claude/agents/*.md.
        let text =
            "---\nname: cc\ndescription: d\ntools: [\"Read\", \"Grep\", \"Bash\"]\n---\nbody\n";
        let def = parse(text, &fake("cc")).unwrap();
        assert_eq!(def.tools, vec!["Read", "Grep", "Bash"]);
    }

    #[test]
    fn parse_tools_unquoted_array_and_comma_forms() {
        let unquoted = parse(
            "---\nname: a\ndescription: d\ntools: [Read, Grep]\n---\nx\n",
            &fake("a"),
        )
        .unwrap();
        assert_eq!(unquoted.tools, vec!["Read", "Grep"]);

        let comma = parse(
            "---\nname: b\ndescription: d\ntools: read_file, grep\n---\nx\n",
            &fake("b"),
        )
        .unwrap();
        assert_eq!(comma.tools, vec!["read_file", "grep"]);
    }

    #[test]
    fn resolve_model_alias_maps_claude_aliases() {
        assert_eq!(resolve_model_alias("sonnet"), "claude-sonnet-4-6");
        assert_eq!(resolve_model_alias("opus"), "claude-opus-4-8");
        assert_eq!(resolve_model_alias("haiku"), "claude-haiku-4-5");
        // Concrete ids and OpenAI ids pass through unchanged.
        assert_eq!(resolve_model_alias("claude-opus-4-8"), "claude-opus-4-8");
        assert_eq!(resolve_model_alias("gpt-5.5"), "gpt-5.5");
    }
}
