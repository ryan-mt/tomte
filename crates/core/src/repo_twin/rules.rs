//! Index 5 — project conventions, extracted from the repo's own docs.
//!
//! Research on agentic-coding setups keeps finding the same thing: context files
//! like `AGENTS.md` are how teams actually encode their conventions, far more
//! than skills or subagents. So the twin reads those docs and pulls out the
//! rule-like lines — bullets and numbered items — each kept with its source file
//! and line. A convention `why-context` surfaces always points at the doc line
//! it came from, so the agent can't claim "this project uses X" without a
//! citation.

use std::path::Path;

use super::{RuleDoc, RuleLine};

/// Convention docs checked at the repo root, in priority order.
const ROOT_DOCS: &[&str] = &[
    "AGENTS.md",
    "AGENTS.override.md",
    "CLAUDE.md",
    "README.md",
    "CONTRIBUTING.md",
    ".cursorrules",
    ".windsurfrules",
];

/// Cap the docs and per-doc rules captured, so a sprawling `docs/` tree can't
/// bloat the cache. The most-conventional files are the root ones, checked first.
const MAX_DOCS: usize = 16;
const MAX_RULES_PER_DOC: usize = 60;
/// Trim an over-long rule line so one giant paragraph-as-bullet can't dominate.
const MAX_RULE_LEN: usize = 240;

/// Collect convention docs and their rule lines: the root docs above, then the
/// top level of a `docs/` directory if present.
pub fn extract_all(root: &Path) -> Vec<RuleDoc> {
    let mut docs = Vec::new();

    for name in ROOT_DOCS {
        if docs.len() >= MAX_DOCS {
            break;
        }
        if let Some(doc) = read_doc(root, name) {
            docs.push(doc);
        }
    }

    // Shallow scan of `docs/` (non-recursive) for additional markdown.
    if let Ok(entries) = std::fs::read_dir(root.join("docs")) {
        let mut md: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let is_md = p
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x.eq_ignore_ascii_case("md"));
                if is_md && p.is_file() {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| format!("docs/{n}"))
                } else {
                    None
                }
            })
            .collect();
        md.sort(); // deterministic order
        for rel in md {
            if docs.len() >= MAX_DOCS {
                break;
            }
            if let Some(doc) = read_doc(root, &rel) {
                docs.push(doc);
            }
        }
    }

    docs
}

fn read_doc(root: &Path, rel: &str) -> Option<RuleDoc> {
    let text = std::fs::read_to_string(root.join(rel)).ok()?;
    let rules = extract_rules(&text);
    if rules.is_empty() {
        return None;
    }
    Some(RuleDoc {
        file: super::normalize(rel),
        rules,
    })
}

/// Pull rule-like lines (markdown bullets `- `/`* `/`+ ` and numbered `1.`
/// items) out of a doc, stripped of their marker and capped. Pure, so it's
/// tested directly.
fn extract_rules(text: &str) -> Vec<RuleLine> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        if out.len() >= MAX_RULES_PER_DOC {
            break;
        }
        let Some(rule) = rule_text(raw) else {
            continue;
        };
        out.push(RuleLine {
            line: i + 1,
            text: rule,
        });
    }
    out
}

/// The rule text of a line, or `None` if it isn't a bullet/numbered item. Strips
/// the list marker, collapses surrounding whitespace, and truncates on a char
/// boundary.
fn rule_text(raw: &str) -> Option<String> {
    let trimmed = raw.trim_start();
    let body = if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        rest
    } else {
        // Numbered list: `12. text`.
        let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        let after = &trimmed[digits.len()..];
        after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))?
    };
    let text = body.trim();
    if text.is_empty() {
        return None;
    }
    Some(truncate_chars(text, MAX_RULE_LEN))
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let t: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{t}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bullets_and_numbered_items_with_lines() {
        let text = "# Rules\n\n- always validate input\n* prefer small functions\n1. run tests first\n2) commit often\n\nnot a rule\n";
        let rules = extract_rules(text);
        let texts: Vec<&str> = rules.iter().map(|r| r.text.as_str()).collect();
        assert!(texts.contains(&"always validate input"));
        assert!(texts.contains(&"prefer small functions"));
        assert!(texts.contains(&"run tests first"));
        assert!(texts.contains(&"commit often"));
        assert!(!texts.iter().any(|t| t.contains("not a rule")));
        // Line numbers are 1-based and point at the source line.
        let first = rules
            .iter()
            .find(|r| r.text == "always validate input")
            .unwrap();
        assert_eq!(first.line, 3);
    }

    #[test]
    fn ignores_a_dashed_horizontal_rule_and_empty_bullets() {
        // `---` is not a list item (no space after the dash); an empty bullet is
        // skipped.
        assert!(rule_text("---").is_none());
        assert!(rule_text("-").is_none());
        assert!(rule_text("- ").is_none());
        assert_eq!(rule_text("- do the thing").as_deref(), Some("do the thing"));
    }

    #[test]
    fn reads_root_docs_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("AGENTS.md"), "- never push to main\n").unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/style.md"), "- two-space indent\n").unwrap();
        // A doc with no rule lines is dropped.
        std::fs::write(root.join("CONTRIBUTING.md"), "Thanks for helping!\n").unwrap();

        let docs = extract_all(root);
        let files: Vec<&str> = docs.iter().map(|d| d.file.as_str()).collect();
        assert!(files.contains(&"AGENTS.md"));
        assert!(files.contains(&"docs/style.md"));
        assert!(
            !files.contains(&"CONTRIBUTING.md"),
            "ruleless doc is dropped"
        );
    }
}
