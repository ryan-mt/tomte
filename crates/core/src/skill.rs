//! Skills: curated, reusable playbooks that opencli can load on demand.
//!
//! Model — progressive disclosure, exactly like Claude Code:
//!   1. At session start we *discover* every installed skill across all
//!      known sources and inject a compact manifest (one `name: description`
//!      line each) into the system prompt. That manifest is part of the
//!      prompt-cache prefix, so after the first turn it is re-read at ~10%
//!      cost — owning hundreds of skills stays cheap.
//!   2. The model loads a skill's full body ON DEMAND via the `skill` tool
//!      (see `tools/skill.rs`) only when a task matches it. Bodies never sit
//!      in context speculatively.
//!
//! Sources (most-specific first; first occurrence of a `name` wins):
//!   - `<cwd>/.opencli/skills/`         project, opencli-native
//!   - `<cwd>/.claude/skills/`          project, Claude Code
//!   - `<cwd>/.codex/skills/`           project, Codex
//!   - `~/.config/opencli/skills/`      opencli global
//!   - `~/.claude/skills/` + plugins    Claude Code global
//!   - `$CODEX_HOME/skills` or `~/.codex/skills` + plugins
//!
//! Each skill lives at `<root>/…/<skill>/SKILL.md`; we search recursively so
//! namespaced layouts like `~/.claude/skills/ecc/<skill>/SKILL.md` are found.
//!
//! `SKILL.md` frontmatter (Claude Code-compatible):
//!
//! ```markdown
//! ---
//! name: git-workflow
//! description: Conventional commits + safe push patterns
//! ---
//! <body — returned by the `skill` tool when the model loads it>
//! ```
//!
//! `triggers:` (comma-separated) is an optional opencli-native extension kept
//! for backward compatibility; the manifest+tool model does not require it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub body: String,
}

pub fn skills_dir() -> PathBuf {
    crate::config::config_dir().join("skills")
}

/// Lightweight skill descriptor: just enough to list in the manifest and
/// locate the full body on demand. The body is deliberately NOT held — that
/// is what keeps owning hundreds of skills cheap.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub path: PathBuf,
}

/// Recursion cap when walking a skill root. Deep enough for namespaced and
/// plugin layouts (`skills/<ns>/<skill>/SKILL.md`), shallow enough to avoid
/// wandering into large unrelated trees.
const MAX_SKILL_DEPTH: usize = 6;

/// Hard ceiling on how many skills the manifest will list, so a pathological
/// install can't blow out the system prompt. 192 real skills today; the cap
/// is a backstop, not a target.
pub const MANIFEST_MAX: usize = 600;

/// Ordered skill root directories, most-specific first. The first directory
/// that defines a given skill `name` wins, so a project or opencli-native
/// skill can shadow a global one. Includes the skill libraries of other
/// agents (Claude Code, Codex) so opencli can use them directly.
pub fn skill_roots(cwd: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        cwd.join(".opencli").join("skills"),
        cwd.join(".claude").join("skills"),
        cwd.join(".codex").join("skills"),
        skills_dir(),
    ];
    if let Some(home) = dirs::home_dir() {
        append_tool_skill_roots(&mut roots, home.join(".claude"));
        append_tool_skill_roots(&mut roots, home.join(".codex"));
    }
    if let Some(codex_home) = env_path("CODEX_HOME") {
        append_tool_skill_roots(&mut roots, codex_home);
    }
    roots
}

fn append_tool_skill_roots(roots: &mut Vec<PathBuf>, tool_home: PathBuf) {
    push_unique(roots, tool_home.join("skills"));
    push_unique(roots, tool_home.join("plugins"));
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

/// Recursively collect every `SKILL.md` under `root`, depth-capped and
/// skipping noisy directories. Missing roots are silently ignored.
fn collect_skill_files(root: &Path, out: &mut Vec<PathBuf>) {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // metadata() follows symlinks (file_type() does not), so a symlinked
            // skill directory — common for shared/team checkouts — is traversed
            // instead of silently skipped. MAX_SKILL_DEPTH bounds any loop.
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if depth + 1 > MAX_SKILL_DEPTH {
                    continue;
                }
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if matches!(name, "node_modules" | ".git" | "target") {
                    continue;
                }
                stack.push((path, depth + 1));
            } else if path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md") {
                out.push(path);
            }
        }
    }
}

/// Discover every installed skill across all sources, deduplicated by `name`
/// (first root in precedence order wins) and sorted by name. Cheap: parses
/// only frontmatter and never retains bodies. Never aborts — unreadable or
/// malformed `SKILL.md`s are logged and skipped.
pub fn discover(cwd: &Path) -> Vec<SkillEntry> {
    discover_in(&skill_roots(cwd))
}

/// Discovery against an explicit, ordered root list. Crate-visible so tests
/// can run hermetically against a temp directory without reading the real
/// global skill libraries.
pub(crate) fn discover_in(roots: &[PathBuf]) -> Vec<SkillEntry> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<SkillEntry> = Vec::new();
    for root in roots {
        let mut files = Vec::new();
        collect_skill_files(root, &mut files);
        files.sort();
        for path in files {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skill read failed");
                    continue;
                }
            };
            match parse(&text, &path) {
                Ok(skill) => {
                    if seen.insert(skill.name.clone()) {
                        out.push(SkillEntry {
                            name: skill.name,
                            description: skill.description,
                            path,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skill parse failed")
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Render discovered skills as a compact manifest for the system prompt: one
/// `- name: description` line each, description collapsed to a single line.
pub fn manifest(entries: &[SkillEntry]) -> String {
    let mut s = String::new();
    for e in entries.iter().take(MANIFEST_MAX) {
        s.push_str("- ");
        s.push_str(&e.name);
        let desc = one_line(&e.description, 200);
        if !desc.is_empty() {
            s.push_str(": ");
            s.push_str(&desc);
        }
        s.push('\n');
    }
    if entries.len() > MANIFEST_MAX {
        s.push_str(&format!(
            "… and {} more (not listed; raise the manifest cap to see them)\n",
            entries.len() - MANIFEST_MAX
        ));
    }
    s
}

/// Load a skill's full body by name. Returns the skill's directory (so the
/// model can resolve files the skill references) and its markdown body.
pub fn load_body(cwd: &Path, name: &str) -> Result<(PathBuf, String)> {
    load_body_from(&skill_roots(cwd), name)
}

/// Body-load against an explicit root list. Crate-visible for hermetic tests.
pub(crate) fn load_body_from(roots: &[PathBuf], name: &str) -> Result<(PathBuf, String)> {
    let entry = discover_in(roots)
        .into_iter()
        .find(|e| e.name == name)
        .ok_or_else(|| anyhow!("skill `{name}` not found"))?;
    let text = std::fs::read_to_string(&entry.path)
        .with_context(|| format!("read skill {}", entry.path.display()))?;
    let skill = parse(&text, &entry.path)?;
    let dir = entry
        .path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok((dir, skill.body))
}

/// Collapse all whitespace runs to single spaces, trim, and truncate to
/// `max` chars (appending `…` if cut) so a multi-line description renders as
/// one compact manifest line.
fn one_line(s: &str, max: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > max {
        let head: String = collapsed.chars().take(max).collect();
        format!("{head}…")
    } else {
        collapsed
    }
}

/// Select skills whose any trigger appears (case-insensitive substring) in
/// `user_text`. Skills with empty triggers are never auto-selected.
pub fn select_triggered(all: &[Skill], user_text: &str) -> Vec<Skill> {
    let lower = user_text.to_ascii_lowercase();
    all.iter()
        .filter(|s| {
            s.triggers
                .iter()
                .any(|t| !t.is_empty() && lower.contains(&t.to_ascii_lowercase()))
        })
        .cloned()
        .collect()
}

/// Parse a `SKILL.md` with YAML-ish frontmatter.
pub fn parse(text: &str, path: &Path) -> Result<Skill> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    let rest = trimmed.strip_prefix("---").ok_or_else(|| {
        anyhow!(
            "skill at {} missing `---` frontmatter opener",
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
                "skill at {} missing closing `---` frontmatter line",
                path.display()
            )
        })?;
    let frontmatter = &rest[..end_idx];
    let mut body = &rest[end_idx + "\n---".len()..];
    body = body.strip_prefix('\r').unwrap_or(body);
    body = body.strip_prefix('\n').unwrap_or(body);

    let mut name = String::new();
    let mut description = String::new();
    let mut triggers: Vec<String> = Vec::new();
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
            "triggers" => {
                triggers = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => {}
        }
    }
    if name.is_empty() {
        // Fall back to the parent directory name so a skill that forgets
        // `name:` still loads usefully.
        if let Some(parent) = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
        {
            name = parent.to_string();
        } else {
            return Err(anyhow!(
                "skill at {} missing required `name` field",
                path.display()
            ));
        }
    }
    Ok(Skill {
        name,
        description,
        triggers,
        body: body.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_path(dir: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/skills/{dir}/SKILL.md"))
    }

    #[test]
    fn parse_minimal_skill() {
        let text = "---\nname: g\ndescription: d\ntriggers: commit, push\n---\nbody\n";
        let s = parse(text, &fake_path("g")).unwrap();
        assert_eq!(s.name, "g");
        assert_eq!(s.description, "d");
        assert_eq!(s.triggers, vec!["commit", "push"]);
        assert_eq!(s.body, "body\n");
    }

    #[test]
    fn parse_falls_back_to_parent_dir_name_for_name() {
        let text = "---\ndescription: no name\n---\nbody\n";
        let s = parse(text, &fake_path("auto-named")).unwrap();
        assert_eq!(s.name, "auto-named");
    }

    #[test]
    fn select_triggered_substring_case_insensitive() {
        let s = Skill {
            name: "x".into(),
            description: "x".into(),
            triggers: vec!["commit".into(), "RELEASE".into()],
            body: "x".into(),
        };
        let all = vec![s.clone()];
        assert!(select_triggered(&all, "build the project").is_empty());
        assert!(!select_triggered(&all, "let's COMMIT this").is_empty());
        assert!(!select_triggered(&all, "release notes").is_empty());
        assert!(select_triggered(&all, "unrelated").is_empty());
    }

    #[test]
    fn empty_triggers_never_auto_selected() {
        let s = Skill {
            name: "x".into(),
            description: "x".into(),
            triggers: vec![],
            body: "x".into(),
        };
        assert!(select_triggered(&[s], "anything").is_empty());
    }

    #[test]
    fn parse_rejects_missing_frontmatter() {
        let err = parse("no frontmatter\n", &fake_path("bad")).unwrap_err();
        assert!(err.to_string().contains("missing `---` frontmatter opener"));
    }

    fn write_skill(root: &std::path::Path, rel_dir: &str, name: &str, desc: &str, body: &str) {
        let dir = root.join(rel_dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn skill_roots_include_project_codex_and_external_plugin_libraries() {
        let cwd = PathBuf::from("/repo");
        let roots = skill_roots(&cwd);
        assert!(roots.contains(&PathBuf::from("/repo/.codex/skills")));

        let mut external = Vec::new();
        append_tool_skill_roots(&mut external, PathBuf::from("/home/me/.claude"));
        append_tool_skill_roots(&mut external, PathBuf::from("/home/me/.codex"));
        append_tool_skill_roots(&mut external, PathBuf::from("/home/me/.codex"));

        assert_eq!(
            external,
            vec![
                PathBuf::from("/home/me/.claude/skills"),
                PathBuf::from("/home/me/.claude/plugins"),
                PathBuf::from("/home/me/.codex/skills"),
                PathBuf::from("/home/me/.codex/plugins"),
            ]
        );
    }

    #[test]
    fn discover_finds_skills_inside_plugin_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_root = tmp.path().join("plugins");
        write_skill(
            &plugin_root,
            "marketplace/plugin-a/skills/plugin-skill",
            "plugin-skill",
            "from plugin",
            "plugin body",
        );

        let entries = discover_in(&[plugin_root]);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "plugin-skill");
    }

    #[test]
    fn discover_finds_nested_skills_and_loads_body_on_demand() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("skills");
        let roots = vec![root.clone()];
        // Namespaced layout like ~/.claude/skills/ecc/<skill>/SKILL.md.
        write_skill(
            &root,
            "ecc/git-workflow",
            "git-workflow",
            "Conventional commits + safe push",
            "Step 1: branch. Step 2: commit.",
        );
        let entries = discover_in(&roots);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "git-workflow");

        let (dir, body) = load_body_from(&roots, "git-workflow").unwrap();
        assert!(body.contains("Step 1: branch"));
        assert!(dir.ends_with("git-workflow"));
        assert!(load_body_from(&roots, "does-not-exist").is_err());
    }

    #[test]
    fn discover_dedupes_by_name_first_root_wins() {
        let tmp = tempfile::tempdir().unwrap();
        // Same name in two roots; the first root in the list wins.
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        write_skill(&a, "dup", "dup", "first version", "from a");
        write_skill(&b, "dup", "dup", "second version", "from b");
        let entries = discover_in(&[a, b]);
        assert_eq!(entries.len(), 1, "duplicate name must collapse to one");
        assert_eq!(entries[0].description, "first version");
    }

    #[test]
    fn manifest_is_one_line_per_skill() {
        let entries = vec![
            SkillEntry {
                name: "a".into(),
                description: "first\nskill   with   spaces".into(),
                path: PathBuf::from("/x/a/SKILL.md"),
            },
            SkillEntry {
                name: "b".into(),
                description: String::new(),
                path: PathBuf::from("/x/b/SKILL.md"),
            },
        ];
        let m = manifest(&entries);
        assert_eq!(m, "- a: first skill with spaces\n- b\n");
    }
}
