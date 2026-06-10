use super::*;

fn rec(loc: &str, decision: &str, ts: u64) -> DecisionRecord {
    DecisionRecord {
        loc: loc.into(),
        decision: decision.into(),
        why: "because reasons".into(),
        rejected: vec![],
        model: "gpt-5.5".into(),
        ts,
        anchor: None,
        supersedes: None,
    }
}

const BASE_TS: u64 = 1_000;

fn report(records: Vec<DecisionRecord>, changed: &[&str]) -> WhyDiff {
    analyze(
        "main".into(),
        "abc1234".into(),
        records,
        changed.iter().map(|s| s.to_string()).collect(),
        BASE_TS,
    )
}

// The window splits on the merge-base timestamp: only decisions recorded after
// it are "new", each flagged by whether its file is part of the diff.
#[test]
fn new_decisions_split_on_base_ts_and_flag_outside_files() {
    let r = report(
        vec![
            rec("src/old.rs:1", "old call", 500),    // before base → not new
            rec("src/a.rs:10", "keep retry", 2_000), // new, in diff
            rec("src/other.rs:5", "off-diff", 3_000), // new, outside diff
        ],
        &["src/a.rs", "src/b.rs"],
    );
    assert_eq!(r.new_decisions.len(), 2);
    assert_eq!(r.new_decisions[0].0.loc, "src/a.rs:10");
    assert!(r.new_decisions[0].1, "src/a.rs is in the diff");
    assert_eq!(r.new_decisions[1].0.loc, "src/other.rs:5");
    assert!(!r.new_decisions[1].1, "src/other.rs is not in the diff");
}

// A supersede pair surfaces when the superseding decision lands in the range —
// the superseded one may be arbitrarily old. A dangling supersedes ts (the old
// record was hand-pruned) is skipped, never a panic.
#[test]
fn superseded_pairs_old_with_new_in_range() {
    let mut newer = rec("src/a.rs:10", "use jitter", 2_000);
    newer.supersedes = Some(500);
    let mut dangling = rec("src/b.rs:1", "no pair", 2_500);
    dangling.supersedes = Some(99_999);
    let r = report(
        vec![rec("src/a.rs:10", "fixed backoff", 500), newer, dangling],
        &["src/a.rs"],
    );
    assert_eq!(r.superseded.len(), 1);
    assert_eq!(r.superseded[0].old.decision, "fixed backoff");
    assert_eq!(r.superseded[0].new.decision, "use jitter");
}

// The gap list: changed files with no decision anywhere on the trail (an old
// decision still counts as coverage), path separators normalized.
#[test]
fn files_without_why_ignores_covered_files() {
    let r = report(
        vec![
            rec("src/a.rs:10", "old but present", 500),
            rec("src/c.rs:1", "new", 2_000),
        ],
        &["src/a.rs", "src/b.rs", "src\\c.rs"],
    );
    assert_eq!(r.files_without_why, vec!["src/b.rs"]);
}

#[test]
fn render_carries_all_three_sections() {
    let mut newer = rec("src/a.rs:10", "use jitter", 2_000);
    newer.supersedes = Some(500);
    let r = report(
        vec![rec("src/a.rs:10", "fixed backoff", 500), newer],
        &["src/a.rs", "src/b.rs"],
    );
    let out = render(&r);
    assert!(out.contains("Why diff — vs main (merge-base abc1234) · 2 changed file(s)"));
    assert!(out.contains("New decisions in this range (1):"));
    assert!(out.contains("src/a.rs:10 — use jitter"));
    assert!(out.contains("Superseded in this range (1)"));
    assert!(out.contains("was: fixed backoff"));
    assert!(out.contains("now: use jitter"));
    assert!(out.contains("Changed without a recorded why (1 of 2):"));
    assert!(out.contains("src/b.rs"));
}

// Calm on an empty trail: the card still renders, pointing at record_decision,
// and every changed file is honestly in the gap list.
#[test]
fn render_cold_start_is_calm() {
    let r = report(vec![], &["src/a.rs"]);
    let out = render(&r);
    assert!(out.contains("none recorded"));
    assert!(out.contains("Changed without a recorded why (1 of 1):"));

    // And the inverse: everything covered reads as a clean bill.
    let r = report(vec![rec("src/a.rs:1", "d", 500)], &["src/a.rs"]);
    assert!(render(&r).contains("every changed file carries at least one recorded decision"));
}

// Outside a repo, collect surfaces git's own complaint instead of panicking.
#[test]
fn collect_outside_a_repo_errors_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let err = collect(tmp.path(), Some("main")).unwrap_err();
    assert!(!err.is_empty());
}
