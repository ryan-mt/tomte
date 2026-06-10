
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
