//! The shift handoff — one capsule the next keeper can pick the house up from.
//!
//! `tomte handoff` (and `/handoff` in a session) collects what a maintainer
//! coming in cold actually needs, from stores that already exist — nothing here
//! asks a model anything:
//!
//! - **where the tree stands** — branch, HEAD, dirty files, recent commits
//!   (straight from git, best-effort outside a repo);
//! - **why things are the way they are** — the newest recorded decisions, with
//!   a drift-watch line saying how many anchors still hold;
//! - **the map** — the Repo Twin's five-index summary;
//! - **where it breaks next** — the top of the Repo Pulse.
//!
//! The capsule is markdown on stdout, so it pastes into a PR description, an
//! issue, or the first prompt of a *different* model's session — tomte's
//! decision trail is cross-model on purpose, and this is the door it walks
//! through. Every line is collected by the CLI from real state, so the handoff
//! can't drift into wishful summary.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::repo_twin::pulse::PulseEntry;

/// How many of each list the capsule carries — a handoff is a briefing, not an
/// archive. The full stores stay one command away (`tomte why --all`, `pulse`).
const MAX_DIRTY: usize = 15;
const MAX_COMMITS: usize = 5;
const MAX_DECISIONS: usize = 5;
const MAX_PULSE: usize = 3;

/// One decision, trimmed to what a reader needs to decide whether to dig.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionBrief {
    pub loc: String,
    pub decision: String,
    pub why: String,
    /// The model that recorded it — the cross-model part of the story.
    pub model: String,
}

/// Drift-watch counts from reconciling the trail against the working tree.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DriftBrief {
    /// Anchored decisions whose line is where the trail says.
    pub present: usize,
    /// Decisions whose line moved and was healed in place.
    pub healed: usize,
    /// Decisions whose anchored line is gone or ambiguous — need eyes.
    pub stale: usize,
}

/// The whole capsule. Serializes for `--json`; renders for humans and models.
#[derive(Debug, Clone, Serialize)]
pub struct Handoff {
    /// Local wall-clock time the capsule was collected.
    pub generated: String,
    pub root: String,
    /// Current branch, empty outside a git repo.
    pub branch: String,
    /// `<short-hash> <subject>` of HEAD, empty outside a git repo.
    pub head: String,
    /// `git status --porcelain` lines, capped at [`MAX_DIRTY`].
    pub dirty: Vec<String>,
    /// Total dirty count before the cap.
    pub dirty_total: usize,
    /// `<short-hash> <subject>`, newest first, capped at [`MAX_COMMITS`].
    pub recent_commits: Vec<String>,
    /// Newest decisions first, capped at [`MAX_DECISIONS`].
    pub decisions: Vec<DecisionBrief>,
    /// Total decisions on the trail before the cap.
    pub decisions_total: usize,
    pub drift: DriftBrief,
    /// The twin's five-index summary; `None` when the index can't build.
    pub twin: Option<crate::repo_twin::Summary>,
    /// Top of the pulse, capped at [`MAX_PULSE`].
    pub pulse_top: Vec<PulseEntry>,
}

/// Run a git command in `root` and return trimmed stdout, `None` on any
/// failure — outside a repo, no git on PATH — so the capsule degrades to the
/// stores that do exist instead of erroring.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(root);
    crate::secret_env::scrub_secret_env_std(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Collect the capsule from the working tree and tomte's own stores. The twin
/// builds on first use (cached after); reconcile heals drifted decision anchors
/// exactly like `tomte why --reconcile` does.
pub fn collect(cwd: &Path) -> Handoff {
    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let head = git(cwd, &["log", "-1", "--pretty=%h %s"]).unwrap_or_default();
    let porcelain = git(cwd, &["status", "--porcelain"]).unwrap_or_default();
    let all_dirty: Vec<String> = porcelain
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    let dirty_total = all_dirty.len();
    let mut dirty = all_dirty;
    dirty.truncate(MAX_DIRTY);
    let recent_commits = git(
        cwd,
        &[
            "log",
            &format!("-{MAX_COMMITS}"),
            "--no-merges",
            "--pretty=%h %s",
        ],
    )
    .unwrap_or_default()
    .lines()
    .map(str::to_string)
    .collect();

    // Decision trail: newest first, then the drift-watch reconcile (which also
    // heals moved anchors, the same side effect `/why --reconcile` has).
    let mut records = crate::decisions::load(cwd);
    let decisions_total = records.len();
    records.sort_by_key(|r| std::cmp::Reverse(r.ts));
    let decisions = records
        .into_iter()
        .take(MAX_DECISIONS)
        .map(|r| DecisionBrief {
            loc: r.loc,
            decision: r.decision,
            why: r.why,
            model: r.model,
        })
        .collect();
    let rec = crate::decisions::reconcile(cwd);
    let drift = DriftBrief {
        present: rec.present,
        healed: rec.moved.len(),
        stale: rec.stale(),
    };

    let (twin, pulse_top, root) = match crate::repo_twin::load_or_build(cwd) {
        Ok(t) => {
            let mut p = crate::repo_twin::pulse::pulse(&t).entries;
            p.truncate(MAX_PULSE);
            (Some(t.summary()), p, t.root)
        }
        Err(_) => (None, Vec::new(), cwd.display().to_string()),
    };

    Handoff {
        generated: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        root,
        branch,
        head,
        dirty,
        dirty_total,
        recent_commits,
        decisions,
        decisions_total,
        drift,
        twin,
        pulse_top,
    }
}

/// Render the capsule as paste-ready markdown.
pub fn render_markdown(h: &Handoff) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Handoff — {}\n\n", h.root));
    out.push_str(&format!(
        "_The shift report, collected by tomte on {} — every line from real \
         state (git, the decision trail, the repo twin), none from a model's \
         summary._\n\n",
        h.generated
    ));

    out.push_str("## Where the tree stands\n\n");
    if h.branch.is_empty() && h.head.is_empty() {
        out.push_str("- not a git repository (or git is not installed)\n");
    } else {
        out.push_str(&format!("- branch `{}` · HEAD `{}`\n", h.branch, h.head));
        if h.dirty_total == 0 {
            out.push_str("- working tree clean\n");
        } else {
            out.push_str(&format!("- {} uncommitted change(s):\n", h.dirty_total));
            for d in &h.dirty {
                out.push_str(&format!("  - `{}`\n", d));
            }
            if h.dirty_total > h.dirty.len() {
                out.push_str(&format!(
                    "  - … and {} more\n",
                    h.dirty_total - h.dirty.len()
                ));
            }
        }
        if !h.recent_commits.is_empty() {
            out.push_str("- recent commits:\n");
            for c in &h.recent_commits {
                out.push_str(&format!("  - `{}`\n", c));
            }
        }
    }

    out.push_str("\n## Why things are the way they are\n\n");
    if h.decisions.is_empty() {
        out.push_str(
            "- no decisions recorded yet — `record_decision` writes the why, \
             `tomte why <loc>` reads it back\n",
        );
    } else {
        for d in &h.decisions {
            out.push_str(&format!(
                "- `{}` — {} — because {} _(recorded by {})_\n",
                d.loc, d.decision, d.why, d.model
            ));
        }
        if h.decisions_total > h.decisions.len() {
            out.push_str(&format!(
                "- … {} more on the trail: `tomte why --all`\n",
                h.decisions_total - h.decisions.len()
            ));
        }
        out.push_str(&format!(
            "- drift watch: {} anchor(s) hold · {} healed · {} need eyes\n",
            h.drift.present, h.drift.healed, h.drift.stale
        ));
    }

    if let Some(s) = &h.twin {
        out.push_str(&format!(
            "\n## The map\n\n- {} files ({} source · {} test) · {} import edges \
             · {} symbols · {} test→source edges · {} convention doc(s)\n- ask \
             it questions: `tomte why-context <file|symbol>`\n",
            s.files,
            s.source_files,
            s.test_files,
            s.import_edges,
            s.symbols,
            s.test_edges,
            s.rule_docs
        ));
    }

    if !h.pulse_top.is_empty() {
        out.push_str("\n## Where it's most likely to break next\n\n");
        for (i, e) in h.pulse_top.iter().enumerate() {
            let cover = if e.tested { "tested" } else { "untested" };
            out.push_str(&format!(
                "{}. `{}` — risk {} ({} recent commit(s), {} importer(s), {})\n",
                i + 1,
                e.file,
                e.risk,
                e.commits,
                e.importers,
                cover
            ));
        }
        out.push_str("- the full card: `tomte pulse`\n");
    }

    out.push_str(
        "\n---\n_Before you call anything done here: `tomte prove` — done \
         means verified._\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capsule() -> Handoff {
        Handoff {
            generated: "2026-06-09 12:00".into(),
            root: "/repo".into(),
            branch: "main".into(),
            head: "abc1234 fix the thing".into(),
            dirty: vec![" M src/a.rs".into()],
            dirty_total: 1,
            recent_commits: vec!["abc1234 fix the thing".into()],
            decisions: vec![DecisionBrief {
                loc: "src/a.rs:10".into(),
                decision: "keep the retry".into(),
                why: "the API flakes".into(),
                model: "gpt-5.5".into(),
            }],
            decisions_total: 7,
            drift: DriftBrief {
                present: 4,
                healed: 2,
                stale: 1,
            },
            twin: None,
            pulse_top: vec![],
        }
    }

    // The populated sections render with their data and the trimmed counts
    // point at the full stores.
    #[test]
    fn renders_git_state_and_decisions() {
        let md = render_markdown(&capsule());
        assert!(md.contains("branch `main` · HEAD `abc1234 fix the thing`"));
        assert!(md.contains("1 uncommitted change(s):"));
        assert!(md.contains("`src/a.rs:10` — keep the retry — because the API flakes"));
        assert!(md.contains("_(recorded by gpt-5.5)_"));
        assert!(md.contains("… 6 more on the trail"));
        assert!(md.contains("4 anchor(s) hold · 2 healed · 1 need eyes"));
        assert!(md.contains("`tomte prove` — done\u{20}means verified"));
    }

    // Empty stores degrade to calm pointers, never to errors or blank headers.
    #[test]
    fn renders_cold_start_gracefully() {
        let h = Handoff {
            generated: "2026-06-09 12:00".into(),
            root: "/repo".into(),
            branch: String::new(),
            head: String::new(),
            dirty: vec![],
            dirty_total: 0,
            recent_commits: vec![],
            decisions: vec![],
            decisions_total: 0,
            drift: DriftBrief::default(),
            twin: None,
            pulse_top: vec![],
        };
        let md = render_markdown(&h);
        assert!(md.contains("not a git repository"));
        assert!(md.contains("no decisions recorded yet"));
        // Sections with nothing to say are omitted entirely.
        assert!(!md.contains("## The map"));
        assert!(!md.contains("most likely to break"));
    }

    // A clean tree says so; the dirty list is capped with an "and N more".
    #[test]
    fn dirty_cap_and_clean_tree() {
        let mut h = capsule();
        h.dirty_total = 0;
        h.dirty = vec![];
        assert!(render_markdown(&h).contains("working tree clean"));

        let mut h = capsule();
        h.dirty = (0..15).map(|i| format!(" M f{i}.rs")).collect();
        h.dirty_total = 40;
        let md = render_markdown(&h);
        assert!(md.contains("40 uncommitted change(s)"));
        assert!(md.contains("… and 25 more"));
    }

    // `collect` on a plain temp dir (no git repo, no stores) must not error —
    // the capsule degrades per section. The twin may or may not build there;
    // what matters is no panic and the git fields are empty.
    #[test]
    fn collect_outside_a_repo_degrades() {
        let tmp = tempfile::tempdir().unwrap();
        let h = collect(tmp.path());
        assert!(h.branch.is_empty());
        assert!(h.head.is_empty());
        assert!(h.decisions.is_empty());
        assert_eq!(h.dirty_total, 0);
    }
}
