//! The decision trail: a project-scoped, append-only log of *why* the agent
//! made a change — the decision, the reasoning, and the alternatives it
//! rejected — each stamped with the model that decided. Pillar 2 of docs/SOUL.md.
//!
//! It lives beside the memory store (`<config>/projects/<key>/decisions.jsonl`)
//! and reuses memory's project keying. It is a *separate*, structured store
//! rather than a freeform memory note for two reasons:
//! - It is queryable by code location (`tomte why <file:line>`).
//! - Each record carries the model in play, so the reasoning survives a mid-task
//!   model switch — a different vendor inherits the *why*, not a lossy summary.
//!   That cross-model trail is the moat.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One recorded decision: what was chosen, why, what was rejected, and which
/// model decided. Serialized as a single JSON line in `decisions.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Where the decision lives, e.g. `src/parser.rs:88`.
    pub loc: String,
    /// The choice that was made.
    pub decision: String,
    /// The reasoning behind it.
    pub why: String,
    /// Alternatives considered and dropped (each "alt -> consequence").
    #[serde(default)]
    pub rejected: Vec<String>,
    /// The model that recorded it. Stamped by the harness, not the model.
    pub model: String,
    /// Wall-clock epoch milliseconds the decision was recorded.
    pub ts: u64,
}

/// `<config>/projects/<key>/decisions.jsonl` — sibling of the memory store,
/// reusing memory's project keying so both share one per-project directory.
pub fn store_path(cwd: &Path) -> PathBuf {
    let memdir = crate::tools::memory::store_dir(cwd);
    memdir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::config::config_dir)
        .join("decisions.jsonl")
}

/// Append one decision to the trail, creating the store directory if needed.
pub fn append(cwd: &Path, record: &DecisionRecord) -> anyhow::Result<()> {
    append_at(&store_path(cwd), record)
}

pub(crate) fn append_at(path: &Path, record: &DecisionRecord) -> anyhow::Result<()> {
    use anyhow::Context as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let line = serde_json::to_string(record).context("serialize decision")?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(f, "{line}").with_context(|| format!("append {}", path.display()))?;
    Ok(())
}

/// Load the whole trail (oldest first). Malformed lines are skipped, not fatal,
/// so one bad hand-edit can't sink the rest of the trail.
pub fn load(cwd: &Path) -> Vec<DecisionRecord> {
    load_at(&store_path(cwd))
}

pub(crate) fn load_at(path: &Path) -> Vec<DecisionRecord> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<DecisionRecord>(l).ok())
        .collect()
}

/// Decisions recorded at a given location, in record order (oldest first).
pub fn for_loc(cwd: &Path, loc: &str) -> Vec<DecisionRecord> {
    let needle = loc.trim();
    load(cwd).into_iter().filter(|d| d.loc == needle).collect()
}

// ---- CLI rendering (`tomte why`) -------------------------------------------

/// Render one location's decisions for `tomte why <loc>`.
pub fn render_for_loc(records: &[DecisionRecord], loc: &str) -> String {
    if records.is_empty() {
        return format!("no decision recorded at {loc}. Try `tomte why --all`.");
    }
    let mut out = String::new();
    for (i, d) in records.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&render_one(d));
    }
    out.trim_end().to_string()
}

fn render_one(d: &DecisionRecord) -> String {
    let mut s = format!("{}\n", d.loc);
    s.push_str(&format!("  decision  {}\n", d.decision));
    s.push_str(&format!("  by        {}\n", d.model));
    s.push_str(&format!("  because   {}\n", d.why));
    for r in &d.rejected {
        s.push_str(&format!("  rejected  {r}\n"));
    }
    s
}

/// Render the whole trail for `tomte why --all`, one line per decision —
/// git-blame-for-decisions: location, choice, and the model that decided.
pub fn render_all(records: &[DecisionRecord]) -> String {
    if records.is_empty() {
        return "the decision trail is empty. Decisions are recorded as the agent works (record_decision); read them back here.".to_string();
    }
    let w = records
        .iter()
        .map(|d| d.loc.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for d in records {
        out.push_str(&format!(
            "{:<w$}  {}  ({})\n",
            d.loc,
            gist(&d.decision, 50),
            d.model,
            w = w
        ));
    }
    out.trim_end().to_string()
}

fn gist(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max - 1).collect();
        format!("{t}…")
    }
}

// ---- system-prompt injection (the moat) ------------------------------------
// Mirrors the memory store's marker-block injection so the trail is re-applied
// each session inside a replaceable block — including under a DIFFERENT model.

const TRAIL_BLOCK_BEGIN: &str = "\n\n<!-- tomte-decision-trail:start -->\n";
const TRAIL_BLOCK_END: &str = "\n<!-- tomte-decision-trail:end -->\n";
/// Cap the injected trail so it can't dominate the prompt.
const TRAIL_MAX_RECORDS: usize = 30;
const TRAIL_MAX_BYTES: usize = 12 * 1024;

/// Re-inject the project's decision trail into `prompt` inside a replaceable
/// marker block, so a fresh session — including one under a different model —
/// inherits the reasoning behind earlier changes, not a lossy summary. The moat.
/// Idempotent: strips any prior block first. No-op when the trail is empty.
pub fn apply_trail_to_prompt(prompt: &mut String, cwd: &Path) {
    apply_trail_at(prompt, &store_path(cwd));
}

pub(crate) fn apply_trail_at(prompt: &mut String, path: &Path) {
    strip_trail_block(prompt);
    let Some(block) = trail_block(&load_at(path)) else {
        return;
    };
    prompt.push_str(TRAIL_BLOCK_BEGIN);
    prompt.push_str(&block);
    prompt.push_str(TRAIL_BLOCK_END);
}

fn strip_trail_block(prompt: &mut String) {
    if let Some(start) = prompt.find(TRAIL_BLOCK_BEGIN) {
        prompt.truncate(start);
    }
}

fn trail_block(records: &[DecisionRecord]) -> Option<String> {
    if records.is_empty() {
        return None;
    }
    let mut s = String::from(
        "# Decision trail\n\nWhy earlier changes in this project were made — recorded with `record_decision` and carried across sessions and model switches, so you inherit the reasoning, not a summary. Treat these as established context; honor them unless the user changes course, and record new non-obvious decisions yourself.\n\n",
    );
    // Most recent first, capped by count and bytes.
    for d in records.iter().rev().take(TRAIL_MAX_RECORDS) {
        if s.len() >= TRAIL_MAX_BYTES {
            break;
        }
        s.push_str(&format!(
            "- {} — {} (why: {}; by {})\n",
            d.loc, d.decision, d.why, d.model
        ));
        for r in &d.rejected {
            s.push_str(&format!("    rejected: {r}\n"));
        }
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(loc: &str, model: &str) -> DecisionRecord {
        DecisionRecord {
            loc: loc.into(),
            decision: "return Err on empty input".into(),
            why: "validate at the boundary".into(),
            rejected: vec!["panic!() -> crashes callers".into()],
            model: model.into(),
            ts: 1,
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tomte_dec_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("decisions.jsonl")
    }

    #[test]
    fn append_then_load_roundtrips() {
        let path = tmp("rt");
        append_at(&path, &rec("src/a.rs:1", "gpt-5.5")).unwrap();
        append_at(&path, &rec("src/b.rs:2", "claude-opus-4-8")).unwrap();
        let all = load_at(&path);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].loc, "src/a.rs:1");
        assert_eq!(all[1].model, "claude-opus-4-8");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let path = tmp("bad");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "not json\n{\"loc\":\"x:1\",\"decision\":\"d\",\"why\":\"w\",\"model\":\"m\",\"ts\":1}\n",
        )
        .unwrap();
        let all = load_at(&path);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].loc, "x:1");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn injection_carries_why_and_is_idempotent() {
        let path = tmp("inj");
        append_at(&path, &rec("src/auth.rs:15", "gpt-5.5")).unwrap();
        let mut prompt = String::from("BASE");
        apply_trail_at(&mut prompt, &path);
        assert!(prompt.contains("# Decision trail"));
        assert!(prompt.contains("src/auth.rs:15"));
        // The *why* is inherited, not just the decision — that's the point.
        assert!(prompt.contains("validate at the boundary"));
        // Re-applying replaces the block, never duplicates it.
        apply_trail_at(&mut prompt, &path);
        assert_eq!(prompt.matches("# Decision trail").count(), 1);
        assert!(prompt.starts_with("BASE"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn renders_for_loc_and_all() {
        let records = vec![rec("src/a.rs:1", "gpt-5.5")];
        let one = render_for_loc(&records, "src/a.rs:1");
        assert!(one.contains("decision"));
        assert!(one.contains("gpt-5.5"));
        assert!(one.contains("validate at the boundary"));
        let all = render_all(&records);
        assert!(all.contains("src/a.rs:1"));
        assert!(render_for_loc(&[], "x:1").contains("no decision"));
        assert!(render_all(&[]).contains("empty"));
    }
}
