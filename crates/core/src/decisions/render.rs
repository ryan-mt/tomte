use super::*;

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

pub(super) fn render_one(d: &DecisionRecord) -> String {
    let mut s = format!("{}\n", d.loc);
    s.push_str(&format!("  decision  {}\n", d.decision));
    s.push_str(&format!("  by        {}\n", d.model));
    s.push_str(&format!("  because   {}\n", d.why));
    for r in &d.rejected {
        s.push_str(&format!("  rejected  {r}\n"));
    }
    if let Some(ts) = d.supersedes {
        s.push_str(&format!("  supersedes decision #{ts}\n"));
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

/// Render one file's decisions for `tomte blame <file>` — one decision per line
/// and greppable (`tomte blame src/auth.rs | grep argon2`). Mirrors the injected
/// trail's one-liner so the on-disk view and the in-prompt view read the same.
/// Oldest first, matching `for_file`'s order.
pub fn render_blame(records: &[DecisionRecord], file: &str) -> String {
    if records.is_empty() {
        return format!("no decisions recorded for {file}. Try `tomte why --all`.");
    }
    let mut out = String::new();
    for d in records {
        out.push_str(&format!(
            "{} — {} (why: {}; by {})\n",
            d.loc, d.decision, d.why, d.model
        ));
    }
    out.trim_end().to_string()
}

/// A calm, one-glance summary of a Drift Watch (`reconcile`) pass: what
/// self-healed and what now needs a human's eyes. Shared by `tomte why
/// --reconcile` and the TUI `/why --reconcile` so both read identically.
/// Silent-on-a-tidy-house in spirit (Pillar 4).
pub fn render_reconcile(r: &ReconcileReport) -> String {
    if !r.changed() && r.stale() == 0 {
        return "decision trail is in order — every anchored decision still matches its code."
            .into();
    }
    let mut out = String::new();
    if r.changed() {
        out.push_str(&format!(
            "healed {} decision(s) that drifted:\n",
            r.moved.len()
        ));
        for (old, new) in &r.moved {
            out.push_str(&format!("  {old}  ->  {new}\n"));
        }
    }
    if r.stale() > 0 {
        out.push_str(&format!(
            "{} decision(s) no longer match their code — re-record or run `tomte why <loc>`:\n",
            r.stale()
        ));
        for loc in r.gone.iter().chain(r.ambiguous.iter()) {
            out.push_str(&format!("  {loc}\n"));
        }
    }
    out.trim_end().to_string()
}

pub(super) fn gist(s: &str, max: usize) -> String {
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
