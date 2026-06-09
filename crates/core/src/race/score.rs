//! The deterministic judge: turn raw [`AgentOutcome`]s into a ranked
//! [`RaceReport`]. Pure — given the same metrics it always picks the same winner,
//! so the tournament can't be talked into a different result. An LLM is never
//! consulted here; the reasons on each verdict are generated from the numbers.

use super::{AgentOutcome, Metrics, RaceReport, Verdict};

// Score weights. Tiers (passed / changed-but-failed / no-change) are decided
// first; these only order contestants *within* a tier.
const W_VERIFIED: i64 = 1000; // every defined check passed
const W_ADDED_TEST: i64 = 200; // added regression coverage
const W_PER_FILE: i64 = 20; // penalty per file touched (favor a focused change)
const W_PER_LINE: i64 = 1; // penalty per changed line (favor a minimal diff)
const W_PER_RISKY: i64 = 500; // penalty per risky shell command run

/// Score a single contestant's metrics. Higher is better. The score rewards a
/// verified, test-adding, *small* change and penalizes churn and risky commands.
pub fn score(m: &Metrics) -> i64 {
    let mut s = 0i64;
    if m.verified && m.any_check_ran {
        s += W_VERIFIED;
    }
    if m.added_test {
        s += W_ADDED_TEST;
    }
    s -= (m.files_changed as i64) * W_PER_FILE;
    s -= (m.insertions as i64 + m.deletions as i64) * W_PER_LINE;
    s -= (m.risky_commands as i64) * W_PER_RISKY;
    s
}

/// Tier a contestant before scoring: 0 = made a change and verification passed,
/// 1 = made a change but a check failed (or there were no checks to run), 2 = no
/// change or the run errored. Lower tiers always rank above higher ones, so a
/// clever-but-broken patch can never beat a working one.
fn tier(o: &AgentOutcome) -> u8 {
    if o.run_error.is_some() || !o.has_changes() {
        return 2;
    }
    if o.metrics.any_check_ran && o.metrics.verified {
        0
    } else {
        1
    }
}

/// Rank the contestants and choose a winner. Sort by tier, then score (desc),
/// then a minimal-diff tie-break, then label for stability. The winner is the
/// top contestant unless it's tier 2 (nobody produced a usable, working change).
pub fn rank(task: String, outcomes: Vec<AgentOutcome>) -> RaceReport {
    // Keep the diffs aside, keyed by label, so we can hand the winner's back.
    let mut diffs: Vec<(String, String)> = outcomes
        .iter()
        .map(|o| (o.label.clone(), o.diff.clone()))
        .collect();

    let mut ranked: Vec<(u8, Verdict)> = outcomes
        .into_iter()
        .map(|o| {
            let t = tier(&o);
            let s = score(&o.metrics);
            let reasons = reasons_for(&o, t);
            (
                t,
                Verdict {
                    label: o.label,
                    model: o.model,
                    metrics: o.metrics,
                    score: s,
                    tier: t,
                    reasons,
                },
            )
        })
        .collect();

    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0) // tier asc
            .then(b.1.score.cmp(&a.1.score)) // score desc
            .then(diff_size(&a.1).cmp(&diff_size(&b.1))) // smaller diff first
            .then(a.1.label.cmp(&b.1.label)) // stable
    });

    let verdicts: Vec<Verdict> = ranked.into_iter().map(|(_, v)| v).collect();
    let winner = verdicts
        .first()
        .filter(|v| v.tier < 2)
        .map(|v| v.label.clone());
    let winning_diff = winner.as_ref().and_then(|w| {
        diffs
            .iter()
            .position(|(l, _)| l == w)
            .map(|i| diffs.swap_remove(i).1)
    });

    RaceReport {
        task,
        verdicts,
        winner,
        patch_path: None,
        applied: false,
        notes: Vec::new(),
        winning_diff,
    }
}

fn diff_size(v: &Verdict) -> u64 {
    v.metrics.insertions + v.metrics.deletions
}

/// The one-line, deterministic reasons shown on the card — drawn straight from
/// the metrics so the explanation always matches the score.
fn reasons_for(o: &AgentOutcome, tier: u8) -> Vec<String> {
    let mut r = Vec::new();
    if let Some(err) = &o.run_error {
        r.push(err.clone());
        return r;
    }
    if !o.has_changes() {
        r.push("made no changes".into());
        return r;
    }
    let m = &o.metrics;
    if m.any_check_ran && m.verified {
        r.push(format!("all {} checks passed", m.checks_total));
    } else if m.checks_failed > 0 {
        r.push(format!("{} check(s) failed", m.checks_failed));
    } else if !m.any_check_ran {
        r.push("no verification checks to run".into());
    }
    if m.added_test {
        r.push("added a regression test".into());
    }
    r.push(format!(
        "diff: {} file(s), +{}/-{}",
        m.files_changed, m.insertions, m.deletions
    ));
    if m.risky_commands > 0 {
        r.push(format!("ran {} risky command(s)", m.risky_commands));
    }
    let _ = tier;
    r
}

/// Render the race result as the human card: the winner and why, then the field.
pub fn render(report: &RaceReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Agent Tournament  ·  task: {}\n", report.task));

    match &report.winner {
        Some(w) => {
            out.push_str(&format!("\nWinner: {w}\n"));
            if let Some(v) = report.verdicts.iter().find(|v| &v.label == w) {
                out.push_str(&format!("  model: {}  ·  score: {}\n", v.model, v.score));
                for reason in &v.reasons {
                    out.push_str(&format!("  - {reason}\n"));
                }
            }
            if report.applied {
                out.push_str("  ✔ applied to the working tree\n");
            } else if let Some(p) = &report.patch_path {
                out.push_str(&format!(
                    "  patch saved: {p}\n  apply it with: git apply {p}\n"
                ));
            }
        }
        None => out.push_str("\nNo winner — no contestant produced a working change.\n"),
    }

    out.push_str("\nField (best first):\n");
    for v in &report.verdicts {
        let crown = if report.winner.as_deref() == Some(v.label.as_str()) {
            "★"
        } else {
            " "
        };
        let status = match v.tier {
            0 => "verified",
            1 if !v.metrics.any_check_ran => "unverified (no checks)",
            1 => "checks failed",
            _ => "no usable change",
        };
        out.push_str(&format!(
            "  {crown} {:<8} {:<8}  score {:>5}  [{}]  {} file(s), +{}/-{}{}\n",
            v.label,
            v.model,
            v.score,
            status,
            v.metrics.files_changed,
            v.metrics.insertions,
            v.metrics.deletions,
            if v.metrics.added_test { "  +test" } else { "" },
        ));
    }

    for n in &report.notes {
        out.push_str(&format!("\nnote: {n}\n"));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(label: &str, m: Metrics) -> AgentOutcome {
        AgentOutcome {
            label: label.into(),
            model: "test-model".into(),
            diff: format!("diff for {label}"),
            metrics: m,
            run_error: None,
        }
    }

    fn verified(files: usize, lines: u64, added_test: bool) -> Metrics {
        Metrics {
            files_changed: files,
            insertions: lines,
            deletions: 0,
            added_test,
            risky_commands: 0,
            checks_total: 3,
            checks_passed: 3,
            checks_failed: 0,
            any_check_ran: true,
            verified: true,
        }
    }

    #[test]
    fn smaller_verified_diff_with_a_test_wins() {
        // The pitch: a small, clean, test-passing patch beats a big clever one.
        let big = outcome("agent-a", verified(12, 400, false));
        let small = outcome("agent-d", verified(2, 20, true));
        let report = rank("fix bug".into(), vec![big, small]);
        assert_eq!(report.winner.as_deref(), Some("agent-d"));
        // The winner's diff is handed back for applying.
        assert_eq!(report.winning_diff.as_deref(), Some("diff for agent-d"));
    }

    #[test]
    fn a_failing_patch_never_beats_a_passing_one_even_if_smaller() {
        let mut failing = verified(1, 5, true);
        failing.verified = false;
        failing.checks_failed = 1;
        failing.checks_passed = 2;
        let tiny_broken = outcome("agent-a", failing);
        let bigger_passing = outcome("agent-b", verified(8, 200, false));
        let report = rank("t".into(), vec![tiny_broken, bigger_passing]);
        // Tier beats score: the passing patch wins despite the larger diff.
        assert_eq!(report.winner.as_deref(), Some("agent-b"));
    }

    #[test]
    fn risky_commands_are_penalized() {
        let mut risky_m = verified(2, 20, true);
        risky_m.risky_commands = 1;
        let clean = outcome("agent-a", verified(2, 20, true));
        let risky = outcome("agent-b", risky_m);
        let report = rank("t".into(), vec![risky, clean]);
        // Same diff/coverage, but the risky one is penalized → clean wins.
        assert_eq!(report.winner.as_deref(), Some("agent-a"));
    }

    #[test]
    fn no_change_or_errored_contestants_cannot_win() {
        let nothing = outcome("agent-a", Metrics::default()); // no files changed
        let mut errored = outcome("agent-b", Metrics::default());
        errored.run_error = Some("boom".into());
        let report = rank("t".into(), vec![nothing, errored]);
        assert_eq!(report.winner, None);
        assert!(report.winning_diff.is_none());
        // Both are tier 2.
        assert!(report.verdicts.iter().all(|v| v.tier == 2));
    }

    #[test]
    fn winner_card_lists_evidence_reasons() {
        let report = rank(
            "fix it".into(),
            vec![outcome("agent-a", verified(2, 20, true))],
        );
        let card = render(&report);
        assert!(card.contains("Winner: agent-a"));
        assert!(card.contains("all 3 checks passed"));
        assert!(card.contains("added a regression test"));
    }

    #[test]
    fn render_reports_no_winner_calmly() {
        let report = rank("t".into(), vec![outcome("agent-a", Metrics::default())]);
        assert!(render(&report).contains("No winner"));
    }

    #[test]
    fn a_project_with_no_checks_reads_unverified_not_checks_failed() {
        // Tier 1 covers both "a check failed" and "there were no checks"; the
        // card must not claim a failure when nothing ran.
        let mut m = verified(1, 5, false);
        m.any_check_ran = false;
        m.verified = false;
        m.checks_total = 0;
        m.checks_passed = 0;
        let card = render(&rank("t".into(), vec![outcome("agent-a", m)]));
        assert!(card.contains("unverified (no checks)"), "card: {card}");
        assert!(!card.contains("checks failed"), "card: {card}");
    }
}
