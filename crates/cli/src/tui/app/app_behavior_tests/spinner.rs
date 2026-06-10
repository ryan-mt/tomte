use super::super::*;

#[test]
fn fleet_idle_verb_is_stable_per_agent_and_from_the_pool() {
    // A finished sub-agent's settled verb must be deterministic per id (no drift
    // once done) and always a real entry from the past-tense pool.
    let v = fleet_idle_verb("agent-7");
    assert_eq!(v, fleet_idle_verb("agent-7"), "same id → same verb");
    assert!(FLEET_IDLE_VERBS.contains(&v), "verb comes from the pool");
    assert!(
        FLEET_IDLE_VERBS.contains(&fleet_idle_verb("a-different-agent-id")),
        "any id maps into the pool"
    );
}

#[test]
fn spinner_words_are_a_distinct_hundreds_strong_pool() {
    use std::collections::HashSet;
    // Hundreds of words, every entry unique, so the drift never stalls and
    // never shows the same word twice in a row.
    assert!(
        SPINNER_WORDS.len() >= 150,
        "expected a large pool, got {}",
        SPINNER_WORDS.len()
    );
    let unique: HashSet<&&str> = SPINNER_WORDS.iter().collect();
    assert_eq!(
        unique.len(),
        SPINNER_WORDS.len(),
        "spinner words must all be distinct"
    );
}

#[test]
fn spinner_word_holds_in_window_then_drifts() {
    use std::time::Duration;
    let len = SPINNER_WORDS.len();
    let seed = 7u32;
    let i0 = spinner_word_index(seed, Duration::from_secs(0), len);
    // Holds steady for the whole drift window — no flicker between draws.
    assert_eq!(
        i0,
        spinner_word_index(seed, Duration::from_secs(SPINNER_WORD_SECS - 1), len)
    );
    // Then steps to a different word in the next window.
    assert_ne!(
        i0,
        spinner_word_index(seed, Duration::from_secs(SPINNER_WORD_SECS), len)
    );
    // Any seed / elapsed / pool length yields a valid in-range index (no panic),
    // and a zero-length pool clamps to 0 instead of dividing by zero.
    for seed in [0u32, 1, 42, u32::MAX] {
        for secs in [0u64, SPINNER_WORD_SECS, 999, 1_000_000] {
            for n in [1usize, len, 200] {
                assert!(spinner_word_index(seed, Duration::from_secs(secs), n) < n);
            }
        }
    }
    assert_eq!(spinner_word_index(0, Duration::from_secs(5), 0), 0);
}

#[test]
fn resolve_spinner_words_appends_or_replaces_like_claude() {
    use tomte_core::config::SpinnerVerbs;
    // Empty JSON applies every serde default → a real default Config.
    let mut cfg: tomte_core::config::Config = serde_json::from_str("{}").unwrap();
    let base = SPINNER_WORDS.len();

    // No override → the built-in pool verbatim.
    assert_eq!(resolve_spinner_words(&cfg).len(), base);

    // Append (default): built-in pool + the user's words.
    cfg.spinner_verbs = Some(SpinnerVerbs {
        verbs: vec!["Hacking".into(), "Vibing".into()],
        exclude_default: false,
    });
    let appended = resolve_spinner_words(&cfg);
    assert_eq!(appended.len(), base + 2);
    assert!(appended.iter().any(|w| w == "Hacking"));
    assert!(appended.iter().any(|w| w == "Pottering"), "built-in kept");

    // Replace: only the user's words.
    cfg.spinner_verbs = Some(SpinnerVerbs {
        verbs: vec!["Solo".into()],
        exclude_default: true,
    });
    assert_eq!(resolve_spinner_words(&cfg), vec!["Solo".to_string()]);

    // Replace with no words → keep the built-in pool (never leave nothing).
    cfg.spinner_verbs = Some(SpinnerVerbs {
        verbs: vec![],
        exclude_default: true,
    });
    assert_eq!(resolve_spinner_words(&cfg).len(), base);
}

#[test]
fn spinner_prefers_the_active_task_then_a_pool_word() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let render_to_string = |app: &mut App| -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::tui::ui::render(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    };

    let mut app = App::new();
    app.render_mode = RenderMode::AltScreen;
    app.busy = true;
    app.turn_started_at = Some(std::time::Instant::now());

    // Claude-parity: an in-progress task shows ITS active form on the spinner.
    app.session_todos = vec![tomte_core::tools::TodoItem {
        content: "refactor the parser".into(),
        status: tomte_core::tools::TodoStatus::InProgress,
        active_form: "Refactoring the parser".into(),
        id: None,
        blocked_by: vec![],
    }];
    assert!(
        render_to_string(&mut app).contains("Refactoring the parser"),
        "spinner shows the active task's form"
    );

    // No task in progress → it falls back to a word from the pool. Seed 0 at ~0s
    // maps to index 0, so the first pool word must appear.
    app.session_todos.clear();
    app.spinner_seed = 0;
    let first = app.spinner_words[0].clone();
    assert!(
        render_to_string(&mut app).contains(first.as_str()),
        "spinner falls back to a pool word ({first})"
    );
}

// ---- decision trail surfaced inside the TUI (Pillar 2 parity with the CLI) ----
