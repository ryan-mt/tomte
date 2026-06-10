use super::super::*;

#[test]
fn conscience_conflict_event_raises_the_card() {
    // Pillar 5 (A2 Tier 2): the event puts the three-way card into pending state
    // with the overturned decision's details, selection defaulting to abort.
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::ConscienceConflict {
            call_id: "c1".into(),
            tool_name: "edit_file".into(),
            file: "src/auth.rs".into(),
            ts: 42,
            prev_decision: "use argon2".into(),
            prev_model: "gpt-5.5".into(),
            reason: "switches to bcrypt".into(),
        },
    );
    let p = app
        .pending_conscience
        .as_ref()
        .expect("the conflict event should raise the card");
    assert_eq!(p.file, "src/auth.rs");
    assert_eq!(p.ts, 42);
    assert_eq!(p.prev_decision, "use argon2");
    assert_eq!(p.selected, 0);
}

#[test]
fn decision_overturned_event_logs_the_audit_line() {
    // Pillar 5 (A3): a supersede surfaces an on-the-record "superseded" line;
    // an edit-anyway (recorded=false) reads as "overridden".
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::DecisionOverturned {
            file: "src/auth.rs".into(),
            prev_decision: "use argon2".into(),
            prev_model: "gpt-5.5".into(),
            reason: "switched to bcrypt".into(),
            recorded: true,
        },
    );
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected an audit system block");
    };
    assert!(text.contains("superseded"), "{text}");
    assert!(
        text.contains("src/auth.rs") && text.contains("argon2"),
        "{text}"
    );

    let mut app2 = App::new();
    apply_agent_event(
        &mut app2,
        AgentEvent::DecisionOverturned {
            file: "x.rs".into(),
            prev_decision: "d".into(),
            prev_model: "m".into(),
            reason: "r".into(),
            recorded: false,
        },
    );
    let Some(Block::System(t2)) = app2.blocks.last() else {
        panic!("expected an audit system block");
    };
    assert!(t2.contains("overridden"), "{t2}");
}

// === Ctrl+C quit guard & draft stashing ======================================

#[tokio::test]
async fn blame_slash_reports_an_empty_trail_for_a_file() {
    let mut app = App::new();
    // A fresh, unique cwd keys to an empty trail, so the read is deterministic
    // and never touches a real project's decisions.jsonl.
    app.cwd = std::env::temp_dir().join(format!("tomte-blame-{}", rand::random::<u64>()));
    handle_slash(&mut app, "blame src/x.rs").await;
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from /blame");
    };
    assert!(text.contains("no decisions recorded"), "{text}");
    assert!(text.contains("src/x.rs"), "{text}");
}

#[tokio::test]
async fn blame_slash_without_a_file_shows_usage() {
    let mut app = App::new();
    handle_slash(&mut app, "blame").await;
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from /blame");
    };
    assert!(text.contains("Usage: /blame"), "{text}");
}

#[tokio::test]
async fn why_reconcile_reports_a_tidy_trail_in_the_tui() {
    let mut app = App::new();
    app.cwd = std::env::temp_dir().join(format!("tomte-why-recon-{}", rand::random::<u64>()));
    handle_slash(&mut app, "why --reconcile").await;
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from /why --reconcile");
    };
    assert!(text.contains("in order"), "{text}");
}

#[tokio::test]
async fn model_switch_announces_the_trail_follows_only_when_non_empty() {
    let mut app = App::new();
    app.cwd = std::env::temp_dir().join(format!("tomte-trail-follow-{}", rand::random::<u64>()));
    // Empty trail → nothing to carry → stay silent.
    app.note_trail_follows_model("claude-opus-4-8");
    assert!(
        !app.blocks
            .iter()
            .any(|b| matches!(b, Block::System(t) if t.contains("follow you"))),
        "an empty trail must not announce anything"
    );
    // Seed one decision, then switch → one announcement naming the target model.
    let rec = tomte_core::decisions::DecisionRecord {
        loc: "src/a.rs:1".into(),
        decision: "use argon2".into(),
        why: "memory-hard".into(),
        rejected: Vec::new(),
        model: "gpt-5.5".into(),
        ts: 1,
        anchor: None,
        supersedes: None,
    };
    tomte_core::decisions::append(&app.cwd, &rec).unwrap();
    app.note_trail_follows_model("claude-opus-4-8");
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from the model switch");
    };
    assert!(text.contains("follow you to claude-opus-4-8"), "{text}");
    assert!(text.contains("1 recorded decision"), "{text}");
    // Clean up the per-project artifact written under the config dir.
    let store = tomte_core::decisions::store_path(&app.cwd);
    let _ = std::fs::remove_file(&store);
    if let Some(parent) = store.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}
