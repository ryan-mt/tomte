//! The decision trail: a project-scoped, append-only log of *why* the agent
//! made a change — the decision, the reasoning, and the alternatives it
//! rejected — each stamped with the model that decided.
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
    /// A snapshot of the trimmed source line at `loc` when the decision was
    /// recorded. Lets `reconcile` re-locate the decision after the code moves,
    /// so `tomte why` never cites a line that has drifted. `None` for older
    /// records and for file-only locations (no `:line`). Pillar 5 — Drift Watch.
    #[serde(default)]
    pub anchor: Option<String>,
    /// When this decision overturns an earlier one, the `ts` of the decision it
    /// supersedes — so the trail becomes an audit log of promises kept and
    /// deliberately broken. `None` for an ordinary, non-superseding decision.
    /// Pillar 5 — On the Record (A3).
    #[serde(default)]
    pub supersedes: Option<u64>,
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

/// Like [`for_loc`], but first heals drifted lines in memory against the working
/// tree (without persisting) — so `tomte why <file:line>` finds a decision even
/// after the code shifted since it was recorded, matching the drift tolerance
/// the injected trail already gets. A read-only query never mutates the store.
pub fn for_loc_live(cwd: &Path, loc: &str) -> Vec<DecisionRecord> {
    for_loc_live_at(&store_path(cwd), cwd, loc)
}

pub(crate) fn for_loc_live_at(store: &Path, root: &Path, loc: &str) -> Vec<DecisionRecord> {
    let needle = loc.trim();
    let mut records = load_at(store);
    let _ = heal_locs(&mut records, root);
    records.into_iter().filter(|d| d.loc == needle).collect()
}

/// Decisions recorded anywhere in a given file, in record order (oldest first).
/// Unlike `for_loc` (which pins to an exact `file:line`), this matches on the
/// *file* component of each `loc`, so it returns every decision in the file
/// regardless of line. The query may be a bare path or a `file:line` — the line
/// suffix is ignored — and `\` is normalized to `/` so a Windows-style query
/// matches the forward-slash `loc`s the agent records. Powers `tomte blame
/// <file>`, and is the file-scoped lookup the conscience lane (A2) reuses.
pub fn for_file(cwd: &Path, file: &str) -> Vec<DecisionRecord> {
    filter_for_file(load(cwd), file)
}

/// The pure filter behind `for_file`, over an already-loaded trail so the
/// matching logic is testable without touching the real store.
fn filter_for_file(records: Vec<DecisionRecord>, file: &str) -> Vec<DecisionRecord> {
    let needle = normalize_file(parse_loc(file.trim()).0);
    records
        .into_iter()
        .filter(|d| normalize_file(parse_loc(&d.loc).0) == needle)
        .collect()
}

/// Render the recorded decisions for `file` as short "house rules" lines for the
/// Pillar-1 pre-flight: up to 3, most-recent-first, each `<decision> — <why>
/// (<model>)`, plus a `+k more · tomte why <file>` overflow line when there are
/// more. Empty when the file has no recorded decisions. Pillar 5 (A2 Tier 1):
/// pure surfacing at the instant of an edit — recall at the moment of risk, not
/// detection, so it can never be wrong.
pub fn house_rules(cwd: &Path, file: &str) -> Vec<String> {
    house_rules_from(for_file(cwd, file), file)
}

/// The pure renderer behind [`house_rules`], over an already-loaded set so the
/// cap/overflow logic is testable without touching the real store.
fn house_rules_from(records: Vec<DecisionRecord>, file: &str) -> Vec<String> {
    const MAX: usize = 3;
    if records.is_empty() {
        return Vec::new();
    }
    let total = records.len();
    let mut out: Vec<String> = records
        .iter()
        .rev()
        .take(MAX)
        .map(|d| {
            format!(
                "{} — {} ({})",
                gist(&d.decision, 48),
                gist(&d.why, 48),
                d.model
            )
        })
        .collect();
    if total > MAX {
        out.push(format!(
            "+{} more · tomte why {}",
            total - MAX,
            normalize_file(parse_loc(file).0)
        ));
    }
    out
}

/// Normalize a file path for trail matching: trim and fold `\` to `/`, so a
/// query typed with Windows separators lines up with the forward-slash `loc`s.
fn normalize_file(file: &str) -> String {
    file.trim().replace('\\', "/")
}

// ---- Drift Watch: reconcile the trail against the working tree (Pillar 5) ---

/// Split a `loc` into its file part and an optional 1-based line number. A
/// trailing `:<digits>` is the line; anything else is a file-only location.
/// `src/a.rs:88` -> (`src/a.rs`, Some(88)); `src/a.rs` -> (`src/a.rs`, None).
fn parse_loc(loc: &str) -> (&str, Option<usize>) {
    match loc.rsplit_once(':') {
        Some((file, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => {
            match line.parse::<usize>() {
                Ok(n) if n >= 1 => (file, Some(n)),
                _ => (loc, None),
            }
        }
        _ => (loc, None),
    }
}

/// Snapshot the trimmed source line at `loc` for use as a drift anchor. Returns
/// `None` for a file-only `loc`, a missing file, an out-of-range line, or a
/// blank line (a blank anchor would match everywhere and is useless).
pub fn capture_anchor(cwd: &Path, loc: &str) -> Option<String> {
    let (file, line) = parse_loc(loc.trim());
    let n = line?;
    let text = std::fs::read_to_string(cwd.join(file)).ok()?;
    let raw = text.lines().nth(n - 1)?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

// ---- auto-capture: parse a self-check answer into a decision (Pillar 2) ------
// After a turn that changed files, the agent asks the active model whether it
// made a non-obvious decision worth keeping (provider-agnostic — see
// `Agent::maybe_capture_decision`). The model's reply is parsed here, so the
// trail populates itself without the model having to call `record_decision`.

/// A decision parsed from the auto-capture self-check, before the harness stamps
/// the model, timestamp, and drift anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedDecision {
    pub loc: String,
    pub decision: String,
    pub why: String,
    pub rejected: Vec<String>,
}

/// Parse the self-check answer into a decision, or `None` when the model said
/// NONE or returned nothing usable. Lenient by design — models vary, so we take
/// the first `{ … }` span and parse that, tolerating surrounding prose or a
/// markdown fence. Returns `None` on any parse failure or a record missing
/// `loc`/`decision`/`why`, so a malformed answer never writes trail litter. Pure
/// and provider-agnostic (no model is special-cased), hence unit-testable.
pub fn parse_captured(answer: &str) -> Option<CapturedDecision> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    if end < start {
        return None;
    }
    #[derive(Deserialize)]
    struct Raw {
        loc: String,
        decision: String,
        why: String,
        #[serde(default, alias = "rejected_alternatives", alias = "alternatives")]
        rejected: Vec<String>,
    }
    let raw: Raw = serde_json::from_str(answer.get(start..=end)?).ok()?;
    let loc = raw.loc.trim().to_string();
    let decision = raw.decision.trim().to_string();
    let why = raw.why.trim().to_string();
    if loc.is_empty() || decision.is_empty() || why.is_empty() {
        return None;
    }
    let rejected = raw
        .rejected
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some(CapturedDecision {
        loc,
        decision,
        why,
        rejected,
    })
}

impl CapturedDecision {
    /// Stamp the live model, a timestamp, and a drift anchor onto a parsed
    /// decision, yielding the record to append — so an auto-captured decision is
    /// indistinguishable from one the `record_decision` tool wrote by hand
    /// (including the `anchor` that lets Drift Watch re-locate it later).
    pub fn into_record(self, cwd: &Path, model: &str) -> DecisionRecord {
        DecisionRecord {
            anchor: capture_anchor(cwd, &self.loc),
            loc: self.loc,
            decision: self.decision,
            why: self.why,
            rejected: self.rejected,
            model: model.to_string(),
            ts: now_ms(),
            supersedes: None,
        }
    }
}

/// Wall-clock epoch milliseconds, for stamping a freshly captured decision.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What `reconcile` found, for the `tomte why --reconcile` summary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Records whose anchored line is still where the `loc` says — left as is.
    pub present: usize,
    /// Records with no anchor or a file-only `loc` — nothing to reconcile.
    pub skipped: usize,
    /// `(old_loc, new_loc)` for records whose line drifted and self-healed.
    pub moved: Vec<(String, String)>,
    /// `loc`s whose anchored line is gone from the file entirely.
    pub gone: Vec<String>,
    /// `loc`s whose anchored line now appears in 2+ places — can't auto-heal.
    pub ambiguous: Vec<String>,
}

impl ReconcileReport {
    /// True when at least one record drifted and a rewrite is warranted.
    pub fn changed(&self) -> bool {
        !self.moved.is_empty()
    }
    /// `loc`s that need a human's eyes: gone or ambiguous.
    pub fn stale(&self) -> usize {
        self.gone.len() + self.ambiguous.len()
    }
}

/// Reconcile every anchored decision against the current working tree: heal the
/// `loc` of any whose line merely moved, and flag any whose line is gone or
/// ambiguous. Records that moved are persisted (atomic rewrite of the trail);
/// nothing else is touched. Records with no anchor (older format) or a file-only
/// `loc` are left exactly as they are. The fix for `for_loc`'s exact-line match
/// going stale the moment code shifts. Pillar 5 — Drift Watch (A1).
pub fn reconcile(cwd: &Path) -> ReconcileReport {
    reconcile_at(&store_path(cwd), cwd)
}

/// `reconcile` with the store path and the source-file root passed separately,
/// so tests can drive the logic without touching the real config directory.
pub(crate) fn reconcile_at(store: &Path, root: &Path) -> ReconcileReport {
    let mut records = load_at(store);
    let report = heal_locs(&mut records, root);
    if report.changed() {
        if let Err(e) = save_all(store, &records) {
            tracing::warn!("decision-trail reconcile could not persist healed locs: {e:#}");
        }
    }
    report
}

/// Heal the in-memory `loc` of every anchored record whose line merely moved, and
/// tally what is present / moved / gone / ambiguous / skipped. Reads the working
/// tree but never persists — the I/O-free core shared by `reconcile_at` (which
/// then saves) and the read-only `*_live` query paths (which heal only so a
/// lookup matches the current line, without mutating the store).
fn heal_locs(records: &mut [DecisionRecord], root: &Path) -> ReconcileReport {
    let mut report = ReconcileReport::default();
    for rec in records.iter_mut() {
        let Some(anchor) = rec.anchor.clone() else {
            report.skipped += 1;
            continue;
        };
        let (file, line) = parse_loc(&rec.loc);
        let Some(n) = line else {
            report.skipped += 1;
            continue;
        };
        let Ok(text) = std::fs::read_to_string(root.join(file)) else {
            report.gone.push(rec.loc.clone());
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        if lines.get(n - 1).map(|l| l.trim()) == Some(anchor.as_str()) {
            report.present += 1;
            continue;
        }
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim() == anchor)
            .map(|(i, _)| i + 1)
            .collect();
        match hits.as_slice() {
            [only] => {
                let new_loc = format!("{file}:{only}");
                report.moved.push((rec.loc.clone(), new_loc.clone()));
                rec.loc = new_loc;
            }
            [] => report.gone.push(rec.loc.clone()),
            _ => report.ambiguous.push(rec.loc.clone()),
        }
    }
    report
}

/// Atomically rewrite the whole trail (used by `reconcile` after a heal): write
/// to a sibling temp file, then rename over the store so a crash can't leave a
/// half-written trail. Malformed lines that `load_at` skipped are not preserved
/// — a reconcile normalizes the file.
fn save_all(path: &Path, records: &[DecisionRecord]) -> anyhow::Result<()> {
    use anyhow::Context as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut body = String::new();
    for r in records {
        body.push_str(&serde_json::to_string(r).context("serialize decision")?);
        body.push('\n');
    }
    // Unique per-process temp name so two concurrent reconciles can't clobber
    // each other's staging file before the rename (mirrors session/config saves).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("jsonl.tmp.{}.{}", std::process::id(), nanos));
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
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
            anchor: None,
            supersedes: None,
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

    #[test]
    fn render_shows_supersede_link_only_when_present() {
        let mut d = rec("src/auth.rs:10", "claude-opus-4-8");
        d.supersedes = Some(1_736_000_000_000);
        let out = render_for_loc(&[d], "src/auth.rs:10");
        assert!(out.contains("supersedes decision #1736000000000"), "{out}");
        // An ordinary, non-superseding decision shows no supersede line.
        let plain = render_for_loc(&[rec("src/a.rs:1", "gpt-5.5")], "src/a.rs:1");
        assert!(!plain.contains("supersedes"));
    }

    #[test]
    fn for_file_filters_by_file_component() {
        let records = vec![
            rec("src/auth.rs:10", "gpt-5.5"),
            rec("src/auth.rs:42", "claude-opus-4-8"),
            rec("src/other.rs:1", "gpt-5.5"),
        ];
        // Every decision in the file, regardless of line.
        assert_eq!(filter_for_file(records.clone(), "src/auth.rs").len(), 2);
        // A `file:line` query keeps the whole file (the line is ignored).
        assert_eq!(filter_for_file(records.clone(), "src/auth.rs:999").len(), 2);
        // A Windows-style query still matches a forward-slash loc.
        assert_eq!(filter_for_file(records.clone(), "src\\auth.rs").len(), 2);
        // No match → empty, not an error.
        assert!(filter_for_file(records, "src/missing.rs").is_empty());
    }

    #[test]
    fn house_rules_cap_overflow_and_order() {
        let records = vec![
            rec("src/auth.rs:10", "m1"),
            rec("src/auth.rs:20", "m2"),
            rec("src/auth.rs:30", "m3"),
            rec("src/auth.rs:40", "m4"),
        ];
        // A Windows-style query still resolves; the path is normalized for the hint.
        let lines = house_rules_from(records, "src\\auth.rs");
        // Capped at 3 rules + 1 overflow line, most-recent (last-appended) first.
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("m4"), "most-recent first: {lines:?}");
        assert!(lines[0].contains("return Err on empty input"));
        assert_eq!(lines[3], "+1 more · tomte why src/auth.rs");
        // Within the cap → no overflow line.
        let few = house_rules_from(vec![rec("src/auth.rs:10", "m1")], "src/auth.rs");
        assert_eq!(few.len(), 1);
        assert!(!few[0].starts_with('+'));
        // No decisions → empty (nothing to surface).
        assert!(house_rules_from(vec![], "src/x.rs").is_empty());
    }

    #[test]
    fn render_blame_is_one_greppable_line_per_decision() {
        let records = vec![
            rec("src/auth.rs:10", "gpt-5.5"),
            rec("src/auth.rs:42", "claude-opus-4-8"),
        ];
        let out = render_blame(&records, "src/auth.rs");
        assert_eq!(out.lines().count(), 2);
        // loc + decision + why + model are all greppable on the first line.
        let first = out.lines().next().unwrap();
        assert!(first.contains("src/auth.rs:10"));
        assert!(first.contains("return Err on empty input"));
        assert!(first.contains("validate at the boundary"));
        assert!(first.contains("gpt-5.5"));
        assert!(render_blame(&[], "src/x.rs").contains("no decisions recorded"));
    }

    #[test]
    fn render_reconcile_is_calm_on_a_tidy_trail_and_lists_drift() {
        // Tidy: nothing moved, nothing stale → one reassuring line, no noise.
        assert!(render_reconcile(&ReconcileReport::default()).contains("in order"));
        // Drift: a heal and a gone loc are both surfaced for the user.
        let drifted = ReconcileReport {
            present: 1,
            moved: vec![("src/a.rs:1".into(), "src/a.rs:3".into())],
            gone: vec!["src/gone.rs:1".into()],
            ..Default::default()
        };
        let out = render_reconcile(&drifted);
        assert!(out.contains("healed 1"), "{out}");
        assert!(out.contains("src/a.rs:1  ->  src/a.rs:3"), "{out}");
        assert!(out.contains("src/gone.rs:1"), "{out}");
    }

    // ---- Drift Watch (Pillar 5, A1) ----

    fn anchored(loc: &str, anchor: &str) -> DecisionRecord {
        let mut r = rec(loc, "gpt-5.5");
        r.anchor = Some(anchor.into());
        r
    }

    fn src_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tomte_src_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        dir
    }

    #[test]
    fn parse_loc_splits_trailing_line_number() {
        assert_eq!(parse_loc("src/a.rs:88"), ("src/a.rs", Some(88)));
        assert_eq!(parse_loc("src/a.rs"), ("src/a.rs", None));
        // 0 is not a 1-based line; a non-numeric suffix is part of the path.
        assert_eq!(parse_loc("src/a.rs:0"), ("src/a.rs:0", None));
        assert_eq!(parse_loc("weird:name"), ("weird:name", None));
    }

    #[test]
    fn capture_anchor_snapshots_the_trimmed_line() {
        let root = src_root("cap");
        std::fs::write(root.join("src/c.rs"), "fn a() {}\n    let z = 1;\n").unwrap();
        assert_eq!(
            capture_anchor(&root, "src/c.rs:2"),
            Some("let z = 1;".to_string())
        );
        assert_eq!(capture_anchor(&root, "src/c.rs"), None); // file-only loc
        assert_eq!(capture_anchor(&root, "src/c.rs:99"), None); // out of range
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_heals_a_moved_line() {
        let store = tmp("rec_move");
        let root = src_root("rec_move");
        // The anchored line drifted from line 1 to line 3.
        std::fs::write(
            root.join("src/a.rs"),
            "// header\n// added\nlet token = mint();\n",
        )
        .unwrap();
        append_at(&store, &anchored("src/a.rs:1", "let token = mint();")).unwrap();

        let report = reconcile_at(&store, &root);
        assert_eq!(
            report.moved,
            vec![("src/a.rs:1".to_string(), "src/a.rs:3".to_string())]
        );
        // The heal is persisted, so `tomte why src/a.rs:3` now finds it.
        assert_eq!(load_at(&store)[0].loc, "src/a.rs:3");
        let _ = std::fs::remove_dir_all(store.parent().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_flags_gone_and_ambiguous() {
        let store = tmp("rec_stale");
        let root = src_root("rec_stale");
        std::fs::write(root.join("src/dup.rs"), "a();\nx();\nx();\n").unwrap();
        std::fs::write(root.join("src/gone.rs"), "// nothing here\n").unwrap();
        append_at(&store, &anchored("src/dup.rs:1", "x();")).unwrap();
        append_at(&store, &anchored("src/gone.rs:1", "removed();")).unwrap();

        let report = reconcile_at(&store, &root);
        assert_eq!(report.ambiguous, vec!["src/dup.rs:1".to_string()]);
        assert_eq!(report.gone, vec!["src/gone.rs:1".to_string()]);
        assert!(!report.changed());
        let _ = std::fs::remove_dir_all(store.parent().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_skips_unanchored_and_keeps_present() {
        let store = tmp("rec_keep");
        let root = src_root("rec_keep");
        std::fs::write(root.join("src/p.rs"), "fn main() {}\n").unwrap();
        append_at(&store, &anchored("src/p.rs:1", "fn main() {}")).unwrap(); // present
        append_at(&store, &rec("src/p.rs:1", "gpt-5.5")).unwrap(); // no anchor → skipped

        let report = reconcile_at(&store, &root);
        assert_eq!(report.present, 1);
        assert_eq!(report.skipped, 1);
        assert!(!report.changed());
        let _ = std::fs::remove_dir_all(store.parent().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_then_inject_feeds_the_healed_loc_not_the_stale_one() {
        // The agent's `apply_decision_trail` now reconciles before injecting.
        // Prove that chain (reconcile_at -> apply_trail_at) feeds the model the
        // CURRENT line, not the drifted one it used to cite as authority — the
        // shipped stale-citation defect this closes.
        let store = tmp("auto_rec");
        let root = src_root("auto_rec");
        // Recorded at line 1; the anchored line has since drifted to line 3.
        std::fs::write(
            root.join("src/a.rs"),
            "// added\n// added\nlet token = mint();\n",
        )
        .unwrap();
        append_at(&store, &anchored("src/a.rs:1", "let token = mint();")).unwrap();

        reconcile_at(&store, &root);
        let mut prompt = String::from("BASE");
        apply_trail_at(&mut prompt, &store);

        assert!(
            prompt.contains("src/a.rs:3"),
            "injects the healed loc: {prompt}"
        );
        assert!(
            !prompt.contains("src/a.rs:1"),
            "never the stale loc: {prompt}"
        );
        let _ = std::fs::remove_dir_all(store.parent().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn for_loc_live_finds_a_decision_after_its_line_drifted_without_persisting() {
        let store = tmp("live_loc");
        let root = src_root("live_loc");
        // Recorded at line 1; the anchored line has since drifted to line 3.
        std::fs::write(
            root.join("src/a.rs"),
            "// added\n// added\nlet token = mint();\n",
        )
        .unwrap();
        append_at(&store, &anchored("src/a.rs:1", "let token = mint();")).unwrap();

        // The query heals in memory, so asking for the CURRENT line finds it —
        // a plain `for_loc` exact-match against the stale `:1` would miss.
        assert_eq!(for_loc_live_at(&store, &root, "src/a.rs:3").len(), 1);
        // ...but a read-only query must never mutate the on-disk trail.
        assert_eq!(load_at(&store)[0].loc, "src/a.rs:1");
        let _ = std::fs::remove_dir_all(store.parent().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- auto-capture parsing (Pillar 2) ----

    #[test]
    fn parse_captured_reads_a_bare_json_object() {
        let a = r#"{"loc":"src/a.rs:10","decision":"return Err on empty","why":"validate at the boundary","rejected":["panic!() -> crashes callers"]}"#;
        let c = parse_captured(a).unwrap();
        assert_eq!(c.loc, "src/a.rs:10");
        assert_eq!(c.decision, "return Err on empty");
        assert_eq!(c.why, "validate at the boundary");
        assert_eq!(c.rejected, vec!["panic!() -> crashes callers".to_string()]);
    }

    #[test]
    fn parse_captured_tolerates_prose_and_a_markdown_fence() {
        // A model may wrap the object in prose or a ```json fence; we still find it.
        let a = "Sure — here is the decision:\n```json\n{\"loc\":\"src/a.rs:1\",\"decision\":\"d\",\"why\":\"w\"}\n```\n";
        let c = parse_captured(a).unwrap();
        assert_eq!(c.loc, "src/a.rs:1");
        assert!(c.rejected.is_empty(), "rejected defaults to empty");
    }

    #[test]
    fn parse_captured_is_none_on_none_garbage_or_missing_fields() {
        assert!(parse_captured("NONE").is_none());
        assert!(parse_captured("").is_none());
        assert!(parse_captured("no json here at all").is_none());
        // A missing required field records nothing (no trail litter).
        assert!(parse_captured(r#"{"loc":"a:1","decision":"d"}"#).is_none());
        // A whitespace-only required field is treated as empty → rejected.
        assert!(parse_captured(r#"{"loc":"  ","decision":"d","why":"w"}"#).is_none());
    }

    #[test]
    fn parse_captured_accepts_rejected_aliases() {
        let a = r#"{"loc":"a:1","decision":"d","why":"w","alternatives":["x -> y"]}"#;
        assert_eq!(
            parse_captured(a).unwrap().rejected,
            vec!["x -> y".to_string()]
        );
    }

    #[test]
    fn into_record_stamps_the_live_model_and_a_drift_anchor() {
        let root = src_root("into_rec");
        std::fs::write(root.join("src/a.rs"), "fn a() {}\nlet z = 1;\n").unwrap();
        let record = CapturedDecision {
            loc: "src/a.rs:2".into(),
            decision: "d".into(),
            why: "w".into(),
            rejected: vec![],
        }
        .into_record(&root, "gpt-5.5");
        assert_eq!(record.model, "gpt-5.5");
        assert_eq!(record.loc, "src/a.rs:2");
        // The anchor snapshots the line so Drift Watch can re-locate it later.
        assert_eq!(record.anchor.as_deref(), Some("let z = 1;"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
