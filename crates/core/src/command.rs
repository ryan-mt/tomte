//! Custom slash commands: user-defined prompt templates loaded from
//! `~/.config/opencli/commands/<name>.md` (global) and
//! `<cwd>/.opencli/commands/<name>.md` (project-local; project wins on name
//! collision).
//!
//! File format — Claude Code-compatible:
//!
//! ```markdown
//! ---
//! description: One-line hint shown in the slash menu
//! argument-hint: [<file>] [--flag]
//! ---
//! Body text — becomes the user message when the command runs.
//! Use $ARGUMENTS for the verbatim trailing argument string.
//! Use $1, $2, ... for whitespace-split positional arguments.
//! ```
//!
//! On execution, opencli substitutes `$ARGUMENTS`, `$1` ... `$9`, then
//! sends the expanded body to the model as the next user turn.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub body: String,
    pub source_path: PathBuf,
}

pub fn global_commands_dir() -> PathBuf {
    crate::config::config_dir().join("commands")
}

pub fn project_commands_dir(cwd: &Path) -> PathBuf {
    cwd.join(".opencli").join("commands")
}

/// Load commands from global first, then project — project files with the
/// same `name` override globals. Result is sorted by name.
pub fn load_all(cwd: &Path) -> Vec<CustomCommand> {
    let mut by_name: std::collections::BTreeMap<String, CustomCommand> =
        std::collections::BTreeMap::new();
    for dir in [global_commands_dir(), project_commands_dir(cwd)] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match parse(&text, &path) {
                Ok(cmd) => {
                    by_name.insert(cmd.name.clone(), cmd);
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "custom command parse failed");
                }
            }
        }
    }
    by_name.into_values().collect()
}

/// Parse one command markdown. The `name` is derived from the filename
/// (without `.md`); frontmatter is optional.
pub fn parse(text: &str, path: &Path) -> Result<CustomCommand> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("invalid command filename at {}", path.display()))?
        .to_string();
    if name.is_empty() {
        return Err(anyhow!("empty command name at {}", path.display()));
    }

    let mut description = String::new();
    let mut argument_hint = String::new();
    let mut body_start = 0usize;

    let trimmed = text.trim_start_matches('\u{feff}');
    if let Some(rest) = trimmed.strip_prefix("---") {
        let rest = rest.strip_prefix('\n').unwrap_or(rest);
        if let Some(end_idx) = rest.find("\n---") {
            let frontmatter = &rest[..end_idx];
            // body offset relative to original text
            let consumed = text.len() - rest.len() + end_idx + "\n---".len();
            let mut body = &text[consumed..];
            body = body.strip_prefix('\r').unwrap_or(body);
            body = body.strip_prefix('\n').unwrap_or(body);
            body_start = text.len() - body.len();
            for raw_line in frontmatter.lines() {
                let line = raw_line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some((k, v)) = line.split_once(':') else {
                    continue;
                };
                let key = k.trim().to_ascii_lowercase();
                let value = v.trim();
                let value = strip_quotes(value);
                match key.as_str() {
                    "description" => description = value.to_string(),
                    "argument-hint" | "argument_hint" => argument_hint = value.to_string(),
                    _ => {}
                }
            }
        }
    }
    let body = text[body_start..].to_string();
    Ok(CustomCommand {
        name,
        description,
        argument_hint,
        body,
        source_path: path.to_path_buf(),
    })
}

/// Expand `$ARGUMENTS`, `$1`..`$9` placeholders in the command body.
/// `args_string` is the full text after the command name; positional args
/// are whitespace-split. `$0` expands to the command name.
pub fn expand(body: &str, command_name: &str, args_string: &str) -> String {
    let positional: Vec<&str> = args_string.split_whitespace().collect();
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            // Try $ARGUMENTS literal
            if chars.clone().take(9).collect::<String>() == "ARGUMENTS" {
                let next_is_word = chars
                    .clone()
                    .nth(9)
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false);
                if !next_is_word {
                    for _ in 0..9 {
                        chars.next();
                    }
                    out.push_str(args_string);
                    continue;
                }
            }
            // Try $0..$9
            if let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    chars.next();
                    let idx = d.to_digit(10).unwrap() as usize;
                    if idx == 0 {
                        out.push_str(command_name);
                    } else if let Some(arg) = positional.get(idx - 1) {
                        out.push_str(arg);
                    }
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
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

    fn fake(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/cmd/{name}.md"))
    }

    #[test]
    fn parse_with_frontmatter() {
        let text = "---\ndescription: do a thing\nargument-hint: <file>\n---\nrun on $1\n";
        let cmd = parse(text, &fake("dothing")).unwrap();
        assert_eq!(cmd.name, "dothing");
        assert_eq!(cmd.description, "do a thing");
        assert_eq!(cmd.argument_hint, "<file>");
        assert_eq!(cmd.body, "run on $1\n");
    }

    #[test]
    fn parse_without_frontmatter() {
        let text = "Hello $ARGUMENTS\n";
        let cmd = parse(text, &fake("hi")).unwrap();
        assert_eq!(cmd.name, "hi");
        assert!(cmd.description.is_empty());
        assert_eq!(cmd.body, "Hello $ARGUMENTS\n");
    }

    #[test]
    fn expand_arguments_literal() {
        let out = expand("Greet $ARGUMENTS now", "hi", "world friend");
        assert_eq!(out, "Greet world friend now");
    }

    #[test]
    fn expand_positional_args() {
        let out = expand("$1 then $2 then $3", "x", "alpha beta");
        assert_eq!(out, "alpha then beta then ");
    }

    #[test]
    fn expand_dollar0_is_command_name() {
        let out = expand("running $0", "deploy", "");
        assert_eq!(out, "running deploy");
    }

    #[test]
    fn expand_leaves_unrelated_dollars_alone() {
        let out = expand("cost $5.00 USD; $x stays", "x", "a b");
        assert_eq!(out, "cost .00 USD; $x stays");
    }
}
