//! Night Rounds — the custodian's read-only inspection pass.
//!
//! The tomte of the stories walks the farm at night and leaves a tidy yard by
//! morning. `tomte rounds` is that walk over a repository: it re-checks every
//! store tomte already keeps — the Repo Twin, the Pulse, the decision trail's
//! drift watch, the working tree's TODO marks, and (optionally) the Proof
//! Capsule's own checks — and reports **what changed since the last rounds**.
//!
//! Three properties make it the opposite of a background-autonomy agent:
//!
//! - **Read-only.** Rounds never edits the tree. The only writes are tomte's
//!   own per-project stores (the refreshed twin cache, healed decision locs —
//!   the same side effect `tomte handoff` already has — and the rounds
//!   baseline itself).
//! - **Measured, not guessed.** Every line is computed from real indexes and
//!   real exit codes; a model is never consulted, so two runs over the same
//!   tree say the same thing.
//! - **A gate, not a feed.** The run exits non-zero only when something is
//!   genuinely red — a recorded decision whose line is gone or ambiguous, or
//!   a project check that failed — so CI can run it as the morning gate.
//!
//! The delta baseline lives at `<config>/projects/<key>/rounds.json`, sibling
//! of the memory / decision / twin stores.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::decisions::ReconcileReport;
use crate::proof::{Outcome, ProofCapsule};

/// Bump when the snapshot schema changes; a mismatched baseline is treated as
/// "first rounds" rather than mis-parsed.
const SNAPSHOT_VERSION: u32 = 1;

/// How many risers / new TODO marks the report carries — rounds is a morning
/// briefing, not a database dump.
const MAX_RISERS: usize = 5;
const MAX_NEW_TODOS: usize = 20;
/// Cap on the TODO scan so a vendored tree can't balloon the baseline.
const MAX_TODO_MARKS: usize = 400;
/// A TODO line is kept to this many characters in the baseline and the report.
const TODO_TEXT_CAP: usize = 100;

/// One `TODO`/`FIXME` mark in the working tree. Matching across runs is by
/// `(file, text)`, never by line — code shifting under a mark must not make it
/// look new.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoMark {
    pub file: String,
    pub line: usize,
    pub text: String,
}

/// What last rounds saw — the baseline tonight's walk is diffed against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundsSnapshot {
    pub version: u32,
    /// Local wall-clock time the baseline was taken, for the "since" line.
    pub taken_at: String,
    pub taken_at_ms: u64,
    /// Full pulse risk per file (uncapped), so any riser is visible.
    pub risk: BTreeMap<String, u32>,
    /// Files that were hot (≥2 recent commits) and untested at baseline.
    pub hot_untested: Vec<String>,
    pub todos: Vec<TodoMark>,
    /// Twin summary counts for the Δ line.
    pub files: usize,
    pub test_edges: usize,
}

/// A file whose pulse risk rose since the baseline (`prev` is 0 for a file
/// that wasn't scored at all last rounds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Riser {
    pub file: String,
    pub prev: u32,
    pub now: u32,
}

/// The morning report. Serializes for `--json`; renders as the terminal card.
#[derive(Debug, Clone, Serialize)]
pub struct RoundsReport {
    /// Local wall-clock time of this walk.
    pub generated: String,
    pub root: String,
    /// `taken_at` of the previous rounds; `None` on the first walk.
    pub since: Option<String>,
    /// The twin's five-index summary; `None` when the index can't build.
    pub twin: Option<crate::repo_twin::Summary>,
    /// File / test-edge count deltas vs the baseline (first rounds: `None`).
    pub files_delta: Option<i64>,
    pub test_edges_delta: Option<i64>,
    /// Files hot & untested tonight that weren't at baseline.
    pub new_hot_untested: Vec<String>,
    /// Top risk risers since baseline, capped at [`MAX_RISERS`].
    pub risers: Vec<Riser>,
    /// Drift watch over the decision trail (same reconcile `handoff` runs).
    pub drift: ReconcileReport,
    /// TODO/FIXME marks present tonight but not at baseline.
    pub new_todos: Vec<TodoMark>,
    /// The re-run Proof Capsule; `None` when the proof pass was skipped.
    pub proof: Option<ProofCapsule>,
}

impl RoundsReport {
    /// True when the morning gate should close: a recorded decision lost its
    /// line (gone or ambiguous), or a project check the proof pass ran failed.
    pub fn red(&self) -> bool {
        !self.drift.gone.is_empty()
            || !self.drift.ambiguous.is_empty()
            || self.proof.as_ref().is_some_and(|p| !p.verified())
    }

    /// True when nothing needs a human's attention — red or amber. Risers are
    /// informational (any commit moves risk) and don't break the quiet.
    pub fn quiet(&self) -> bool {
        !self.red() && self.new_hot_untested.is_empty() && self.new_todos.is_empty()
    }
}

/// `<config>/projects/<key>/rounds.json` — sibling of the memory, decision,
/// and twin stores, reusing memory's project keying.
pub fn snapshot_path(cwd: &Path) -> PathBuf {
    let memdir = crate::tools::memory::store_dir(cwd);
    memdir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::config::config_dir)
        .join("rounds.json")
}

fn load_snapshot_at(path: &Path) -> Option<RoundsSnapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    let snap: RoundsSnapshot = serde_json::from_str(&text).ok()?;
    (snap.version == SNAPSHOT_VERSION).then_some(snap)
}

fn save_snapshot_at(path: &Path, snap: &RoundsSnapshot) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = serde_json::to_string(snap).context("serialize rounds baseline")?;
    // Stage → flush → atomic rename, mirroring the twin cache writer, so a
    // crash mid-write can't leave a torn baseline that fails to parse.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), nanos));
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(body.as_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("flush {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Walk tonight's rounds: rebuild the twin, score the pulse, reconcile the
/// decision trail, scan TODO marks, optionally re-run the proof checks, diff
/// it all against the previous baseline, and record tonight as the new one.
pub async fn collect(cwd: &Path, run_proof: bool) -> RoundsReport {
    let generated = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let prev = load_snapshot_at(&snapshot_path(cwd));

    // Same reconcile (and the same heal-side-effect) handoff already runs.
    let drift = crate::decisions::reconcile(cwd);

    // A fresh build, not the cache: rounds is an inspection, and an inspection
    // that trusts yesterday's map defeats itself.
    let (twin, root, risk_now, hot_now, todos_now) = match crate::repo_twin::rebuild(cwd) {
        Ok(t) => {
            let entries = crate::repo_twin::pulse::score(&t);
            let risk: BTreeMap<String, u32> =
                entries.iter().map(|e| (e.file.clone(), e.risk)).collect();
            let hot: Vec<String> = entries
                .iter()
                .filter(|e| e.commits >= 2 && !e.tested)
                .map(|e| e.file.clone())
                .collect();
            let todos = scan_todos(Path::new(&t.root), &t);
            (Some(t.summary()), t.root.clone(), risk, hot, todos)
        }
        Err(_) => (
            None,
            cwd.display().to_string(),
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
        ),
    };

    let proof = if run_proof {
        Some(crate::proof::collect(cwd).await)
    } else {
        None
    };

    let (since, risers, new_hot, new_todos, files_delta, edges_delta) = match &prev {
        Some(p) => (
            Some(p.taken_at.clone()),
            risers(&p.risk, &risk_now),
            hot_now
                .iter()
                .filter(|f| !p.hot_untested.contains(f))
                .cloned()
                .collect(),
            new_marks(&p.todos, &todos_now),
            twin.as_ref().map(|s| s.files as i64 - p.files as i64),
            twin.as_ref()
                .map(|s| s.test_edges as i64 - p.test_edges as i64),
        ),
        None => (None, Vec::new(), Vec::new(), Vec::new(), None, None),
    };

    // Tonight becomes the next baseline — but only when the twin built, so a
    // transient index failure can't wipe a good baseline.
    if let Some(summary) = &twin {
        let snap = RoundsSnapshot {
            version: SNAPSHOT_VERSION,
            taken_at: generated.clone(),
            taken_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            risk: risk_now,
            hot_untested: hot_now,
            todos: todos_now,
            files: summary.files,
            test_edges: summary.test_edges,
        };
        if let Err(e) = save_snapshot_at(&snapshot_path(cwd), &snap) {
            tracing::warn!("rounds: could not record the new baseline: {e:#}");
        }
    }

    RoundsReport {
        generated,
        root,
        since,
        twin,
        files_delta,
        test_edges_delta: edges_delta,
        new_hot_untested: new_hot,
        risers,
        drift,
        new_todos,
        proof,
    }
}

/// Scan the twin's source files for `TODO`/`FIXME` marks. Bounded by
/// [`MAX_TODO_MARKS`]; a file the tree no longer has (or can't be read) is
/// simply skipped — rounds reports, it never errors over one file.
fn scan_todos(root: &Path, twin: &crate::repo_twin::RepoTwin) -> Vec<TodoMark> {
    // Skip oversized files instead of slurping them whole: the twin keeps
    // >512 KiB files as nodes, so a giant non-gitignored bundle would be read
    // in full on every walk (the twin extractors cap for the same reason).
    const MAX_TODO_SCAN_BYTES: u64 = 512 * 1024;
    let mut marks = Vec::new();
    for f in &twin.files {
        if !f.lang.is_source() {
            continue;
        }
        let path = root.join(&f.path);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_TODO_SCAN_BYTES) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if !(line.contains("TODO") || line.contains("FIXME")) {
                continue;
            }
            marks.push(TodoMark {
                file: f.path.clone(),
                line: i + 1,
                text: line.trim().chars().take(TODO_TEXT_CAP).collect(),
            });
            if marks.len() >= MAX_TODO_MARKS {
                return marks;
            }
        }
    }
    marks
}

/// Files whose risk rose since the baseline, by delta desc then path, capped
/// at [`MAX_RISERS`]. Pure, so the ranking is unit-tested directly.
fn risers(prev: &BTreeMap<String, u32>, now: &BTreeMap<String, u32>) -> Vec<Riser> {
    let mut out: Vec<Riser> = now
        .iter()
        .filter_map(|(file, &now_risk)| {
            let prev_risk = prev.get(file).copied().unwrap_or(0);
            (now_risk > prev_risk).then(|| Riser {
                file: file.clone(),
                prev: prev_risk,
                now: now_risk,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (b.now - b.prev)
            .cmp(&(a.now - a.prev))
            .then(a.file.cmp(&b.file))
    });
    out.truncate(MAX_RISERS);
    out
}

/// Marks present tonight but not at baseline — matched on `(file, text)` so a
/// mark that merely moved lines stays old. Capped at [`MAX_NEW_TODOS`].
fn new_marks(prev: &[TodoMark], now: &[TodoMark]) -> Vec<TodoMark> {
    use std::collections::HashSet;
    let seen: HashSet<(&str, &str)> = prev
        .iter()
        .map(|m| (m.file.as_str(), m.text.as_str()))
        .collect();
    let mut out: Vec<TodoMark> = now
        .iter()
        .filter(|m| !seen.contains(&(m.file.as_str(), m.text.as_str())))
        .cloned()
        .collect();
    out.truncate(MAX_NEW_TODOS);
    out
}

/// The terminal card `tomte rounds` prints — the morning report.
pub fn render(r: &RoundsReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Night Rounds — {}\n", r.root));
    match &r.since {
        Some(since) => out.push_str(&format!(
            "  {} · since last rounds at {}\n",
            r.generated, since
        )),
        None => out.push_str(&format!(
            "  {} · first rounds — baseline recorded, deltas start next walk\n",
            r.generated
        )),
    }
    out.push('\n');

    if let Some(s) = &r.twin {
        let delta = match (r.files_delta, r.test_edges_delta) {
            (Some(fd), Some(td)) => format!(" · Δ {fd:+} file(s), {td:+} test edge(s)"),
            _ => String::new(),
        };
        out.push_str(&format!(
            "  map     {} files ({} source · {} test) · {} test→source edges{}\n",
            s.files, s.source_files, s.test_files, s.test_edges, delta
        ));
    } else {
        out.push_str(
            "  map     the twin could not build here — twin, pulse and TODO sections skipped\n",
        );
    }

    if !r.risers.is_empty() {
        let list = r
            .risers
            .iter()
            .map(|x| format!("{} {}→{} (+{})", x.file, x.prev, x.now, x.now - x.prev))
            .collect::<Vec<_>>()
            .join(" · ");
        out.push_str(&format!("  pulse   rising: {list}\n"));
    }

    if !r.new_hot_untested.is_empty() {
        out.push_str(&format!(
            "  hot     newly hot & untested ⚠ : {}\n",
            r.new_hot_untested.join(", ")
        ));
    }

    let d = &r.drift;
    if d.present + d.skipped + d.moved.len() + d.gone.len() + d.ambiguous.len() > 0 {
        out.push_str(&format!(
            "  drift   {} anchor(s) hold · {} healed",
            d.present,
            d.moved.len()
        ));
        for loc in &d.gone {
            out.push_str(&format!(" · GONE {loc}"));
        }
        for loc in &d.ambiguous {
            out.push_str(&format!(" · AMBIGUOUS {loc}"));
        }
        out.push('\n');
    }

    if !r.new_todos.is_empty() {
        out.push_str(&format!(
            "  todo    {} new TODO/FIXME mark(s):\n",
            r.new_todos.len()
        ));
        for m in &r.new_todos {
            out.push_str(&format!("            {}:{}  {}\n", m.file, m.line, m.text));
        }
    }

    match &r.proof {
        Some(p) => {
            let line = p
                .checks
                .iter()
                .map(|c| {
                    let glyph = match c.outcome {
                        Outcome::Passed => "✓",
                        Outcome::Failed { .. } | Outcome::Errored { .. } => "✗",
                        Outcome::Skipped => "—",
                    };
                    format!("{} {}", glyph, c.name)
                })
                .collect::<Vec<_>>()
                .join(" · ");
            if line.is_empty() {
                out.push_str("  proof   nothing to verify — no recognized project checks\n");
            } else {
                out.push_str(&format!("  proof   {line}\n"));
            }
            if !p.verified() {
                out.push_str("          a check failed — `tomte prove` has the output tail\n");
            }
        }
        None => out.push_str("  proof   skipped (--no-proof)\n"),
    }

    out.push('\n');
    if r.quiet() {
        out.push_str("  A quiet night — nothing out of order.\n");
    } else if r.red() {
        out.push_str("  Something needs eyes before the day starts (exit 1).\n");
    } else {
        out.push_str("  Nothing red — the marks above are worth a look when you have a minute.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(file: &str, line: usize, text: &str) -> TodoMark {
        TodoMark {
            file: file.into(),
            line,
            text: text.into(),
        }
    }

    fn empty_report() -> RoundsReport {
        RoundsReport {
            generated: "2026-06-09 23:00".into(),
            root: "/repo".into(),
            since: None,
            twin: None,
            files_delta: None,
            test_edges_delta: None,
            new_hot_untested: vec![],
            risers: vec![],
            drift: ReconcileReport::default(),
            new_todos: vec![],
            proof: None,
        }
    }

    // Risers rank by delta (desc), tie-break by path, include files that were
    // not scored at baseline (prev = 0), and cap the list.
    #[test]
    fn risers_rank_by_delta_and_cap() {
        let prev: BTreeMap<String, u32> =
            [("a.rs".to_string(), 10), ("b.rs".to_string(), 10)].into();
        let mut now: BTreeMap<String, u32> = [
            ("a.rs".to_string(), 30), // +20
            ("b.rs".to_string(), 5),  // fell — not a riser
            ("c.rs".to_string(), 8),  // new file, +8
        ]
        .into();
        let r = risers(&prev, &now);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].file.as_str(), r[0].prev, r[0].now), ("a.rs", 10, 30));
        assert_eq!((r[1].file.as_str(), r[1].prev, r[1].now), ("c.rs", 0, 8));

        for i in 0..10 {
            now.insert(format!("m{i}.rs"), 100);
        }
        assert_eq!(risers(&prev, &now).len(), MAX_RISERS, "capped");
    }

    // A mark that only moved lines is not new; a new text or a new file is.
    #[test]
    fn new_marks_match_on_file_and_text_not_line() {
        let prev = vec![mark("a.rs", 10, "TODO: tidy")];
        let now = vec![
            mark("a.rs", 99, "TODO: tidy"),  // moved — old
            mark("a.rs", 12, "FIXME: leak"), // new text
            mark("b.rs", 1, "TODO: tidy"),   // same text, new file
        ];
        let fresh = new_marks(&prev, &now);
        assert_eq!(fresh.len(), 2);
        assert_eq!(fresh[0].text, "FIXME: leak");
        assert_eq!(fresh[1].file, "b.rs");
    }

    // The gate: gone/ambiguous decisions or a failed proof check turn the
    // report red; risers and new TODOs alone only break the quiet.
    #[test]
    fn red_and_quiet_track_drift_and_proof() {
        let mut r = empty_report();
        assert!(!r.red());
        assert!(r.quiet());

        r.risers = vec![Riser {
            file: "a.rs".into(),
            prev: 1,
            now: 9,
        }];
        assert!(r.quiet(), "risers are informational");

        r.new_todos = vec![mark("a.rs", 1, "TODO: x")];
        assert!(!r.quiet());
        assert!(!r.red(), "a new TODO is amber, not red");

        r.drift.gone.push("a.rs:10".into());
        assert!(r.red());
    }

    // First rounds says so; a later walk shows the since-line and the deltas.
    #[test]
    fn render_covers_first_and_delta_walks() {
        let r = empty_report();
        let card = render(&r);
        assert!(card.contains("first rounds — baseline recorded"));
        assert!(card.contains("A quiet night"));
        assert!(card.contains("proof   skipped (--no-proof)"));

        let mut r = empty_report();
        r.since = Some("2026-06-08 23:00".into());
        r.new_todos = vec![mark("src/x.rs", 88, "TODO: cover the md links")];
        r.drift.gone.push("src/a.rs:12".into());
        let card = render(&r);
        assert!(card.contains("since last rounds at 2026-06-08 23:00"));
        assert!(card.contains("GONE src/a.rs:12"));
        assert!(card.contains("src/x.rs:88  TODO: cover the md links"));
        assert!(card.contains("needs eyes before the day starts"));
    }

    // The baseline survives a save/load roundtrip, and a version bump (or torn
    // file) reads as "no baseline" instead of mis-parsing.
    #[test]
    fn snapshot_roundtrip_and_version_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rounds.json");
        let snap = RoundsSnapshot {
            version: SNAPSHOT_VERSION,
            taken_at: "2026-06-09 23:00".into(),
            taken_at_ms: 1,
            risk: [("a.rs".to_string(), 18)].into(),
            hot_untested: vec!["a.rs".into()],
            todos: vec![mark("a.rs", 3, "TODO: x")],
            files: 10,
            test_edges: 4,
        };
        save_snapshot_at(&path, &snap).unwrap();
        let back = load_snapshot_at(&path).expect("roundtrips");
        assert_eq!(back.risk.get("a.rs"), Some(&18));
        assert_eq!(back.todos, snap.todos);

        let mut old = snap.clone();
        old.version = SNAPSHOT_VERSION + 1;
        save_snapshot_at(&path, &old).unwrap();
        assert!(load_snapshot_at(&path).is_none(), "version gate");

        std::fs::write(&path, "{ torn").unwrap();
        assert!(load_snapshot_at(&path).is_none(), "torn file");
    }

    // `collect` on a plain temp dir (no git, no stores) must not error — every
    // section degrades; with the proof pass off it runs no project checks.
    #[tokio::test]
    async fn collect_outside_a_repo_degrades() {
        let tmp = tempfile::tempdir().unwrap();
        let r = collect(tmp.path(), false).await;
        assert!(r.proof.is_none());
        assert!(r.new_todos.is_empty());
        assert!(!r.red());
    }
}
