use super::*;

pub(super) const TRAIL_BLOCK_BEGIN: &str = "\n\n<!-- tomte-decision-trail:start -->\n";

pub(super) const TRAIL_BLOCK_END: &str = "\n<!-- tomte-decision-trail:end -->\n";

/// Cap the injected trail so it can't dominate the prompt.
pub(super) const TRAIL_MAX_RECORDS: usize = 30;

pub(super) const TRAIL_MAX_BYTES: usize = 12 * 1024;

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

pub(super) fn strip_trail_block(prompt: &mut String) {
    if let Some(start) = prompt.find(TRAIL_BLOCK_BEGIN) {
        prompt.truncate(start);
    }
}

pub(super) fn trail_block(records: &[DecisionRecord]) -> Option<String> {
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
