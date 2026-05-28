//! Skills: reusable reference material that auto-loads into the system
//! prompt when a configured trigger matches the user message.
//!
//! Layout (Claude Code-compatible):
//!
//! ```text
//! ~/.config/opencli/skills/
//! └── git-workflow/
//!     └── SKILL.md
//! ```
//!
//! `SKILL.md` format:
//!
//! ```markdown
//! ---
//! name: git-workflow
//! description: Conventional commits + safe push patterns
//! triggers: commit, pull request, git workflow
//! ---
//! <body — appended to the system prompt when triggered>
//! ```
//!
//! Triggers are case-insensitive substring matches against the user's
//! incoming text. Empty `triggers` means "always available, never auto
//! injected" — only used by `/skills` listing or explicit invocation.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
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

/// Walk every immediate sub-directory of `skills_dir()` and load each
/// `SKILL.md`. Errors are logged and skipped, never abort.
pub fn load_all() -> Vec<Skill> {
    let dir = skills_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        match std::fs::read_to_string(&skill_md) {
            Ok(text) => match parse(&text, &skill_md) {
                Ok(skill) => out.push(skill),
                Err(e) => tracing::warn!(path = %skill_md.display(), error = %e, "skill parse failed"),
            },
            Err(e) => tracing::warn!(path = %skill_md.display(), error = %e, "skill read failed"),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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
    let end_idx = rest.find("\n---").ok_or_else(|| {
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
        if let Some(parent) = path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()) {
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
        assert!(!select_triggered(&all, "build the project").is_empty() == false);
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
}
