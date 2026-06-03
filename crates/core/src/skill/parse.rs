//! Parsing of a `SKILL.md` file's YAML-ish frontmatter into a [`Skill`].

use std::path::Path;

use anyhow::{anyhow, Result};

use super::Skill;

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
    fn parse_rejects_missing_frontmatter() {
        let err = parse("no frontmatter\n", &fake_path("bad")).unwrap_err();
        assert!(err.to_string().contains("missing `---` frontmatter opener"));
    }
}
