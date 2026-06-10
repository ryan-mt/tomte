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
    // Write the record and its newline in ONE call: under POSIX `O_APPEND` a
    // single small write lands atomically, so two tomte sessions appending to the
    // same project trail can't interleave a half-line — a `writeln!` lowers to
    // multiple writes and could. `sync_all` then flushes it so a crash right after
    // we report a decision "recorded" can't drop it (the durability bar the
    // session/config writers already hold); the dir fsync covers a freshly-created
    // trail file's directory entry.
    f.write_all(format!("{line}\n").as_bytes())
        .with_context(|| format!("append {}", path.display()))?;
    f.sync_all()
        .with_context(|| format!("flush {}", path.display()))?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent);
    }
    Ok(())
}

/// Best-effort directory fsync so a preceding append/rename is durable across a
/// crash. A no-op where directory fsync isn't supported (e.g. Windows std).
fn fsync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
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
pub(crate) fn normalize_file(file: &str) -> String {
    file.trim().replace('\\', "/")
}

// ---- Drift Watch: reconcile the trail against the working tree (Pillar 5) ---

/// Split a `loc` into its file part and an optional 1-based line number. A
/// trailing `:<digits>` is the line; anything else is a file-only location.
/// `src/a.rs:88` -> (`src/a.rs`, Some(88)); `src/a.rs` -> (`src/a.rs`, None).
pub(crate) fn parse_loc(loc: &str) -> (&str, Option<usize>) {
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

/// What `reconcile` found, for the `tomte why --reconcile` summary. Derives
/// `Serialize` so `tomte why --reconcile --json` can emit it for a CI drift-gate.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
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
        // Re-load and re-heal immediately before the rewrite. `heal_locs` scans the
        // whole working tree, so the gap between the first `load_at` and the rename
        // is wide; a decision another tomte session appends in that gap would be
        // clobbered by our stale snapshot (the trail is shared per-project, and the
        // moat must not silently lose an entry). Re-reading just before persisting
        // carries any such append into the rewrite, shrinking the lost-append
        // window to load→rename; `heal_locs` is deterministic, so re-healing the
        // fresh set yields the same result. (A fully airtight fix needs a
        // cross-process lock; this closes the realistic case without adding one.)
        let mut fresh = load_at(store);
        heal_locs(&mut fresh, root);
        if let Err(e) = save_all(store, &fresh) {
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
    // Stage → flush → atomic rename → flush the directory, so a crash leaves either
    // the old trail or the whole new one, never a torn file (mirrors the session
    // writer). `std::fs::write` alone left the staged bytes unflushed before the
    // rename, so a crash could publish an empty/partial trail.
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(body.as_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("flush {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent);
    }
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
mod tests;
