//! Integration test for session persistence: save records to a scratch
//! `TOMTE_CONFIG_DIR`, then exercise list + load + the not-found path.
//!
//! We override `TOMTE_CONFIG_DIR` (honored on every platform) rather than
//! `XDG_CONFIG_HOME` (which `dirs` reads only on Unix, so on Windows the test
//! would silently write to the real `%APPDATA%` and accumulate sessions across
//! runs). Cargo runs integration tests in parallel by default and `set_var` is
//! process-global, so each `#[test]` racing on the override would corrupt every
//! other test's view of the sessions directory. We collapse the coverage into
//! a single function so the env override is set once and used serially.

use std::path::{Path, PathBuf};
use tomte_core::openai::{InputItem, MessageContent};
use tomte_core::session::{
    self, ModelUsage, SessionGoalSnapshot, SessionMeta, SessionRecord, SessionSnapshot,
};
use tomte_core::tools::{TodoItem, TodoStatus};

fn sample_history(prompt: &str) -> Vec<InputItem> {
    vec![
        InputItem::Message {
            role: "user".to_string(),
            content: vec![MessageContent::text(prompt)],
        },
        InputItem::Message {
            role: "assistant".to_string(),
            content: vec![MessageContent::OutputText {
                text: "ok, on it".to_string(),
            }],
        },
    ]
}

fn record(cwd: &Path, id: &str, ts: u64) -> SessionRecord {
    SessionRecord {
        meta: SessionMeta {
            id: id.into(),
            cwd: cwd.to_path_buf(),
            model: "gpt-5".into(),
            created_at_ms: ts,
            updated_at_ms: ts,
            message_count: 2,
            preview: id.into(),
        },
        state: SessionSnapshot::default(),
        history: sample_history(id),
    }
}

#[test]
fn session_save_load_list_and_missing_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::env::set_var("TOMTE_CONFIG_DIR", tmp.path());

    let cwd_a = PathBuf::from("/tmp/tomte-test-proj-a");
    let cwd_b = PathBuf::from("/tmp/tomte-test-proj-b");

    // --- save/load roundtrip ---------------------------------------------
    let mut r = record(&cwd_a, "alpha", 1_000);
    r.state.todos.push(TodoItem {
        content: "Run tests".to_string(),
        status: TodoStatus::InProgress,
        active_form: "Running tests".to_string(),
        id: None,
        blocked_by: Vec::new(),
    });
    r.state.read_files.push(cwd_a.join("src/lib.rs"));
    r.state.usage.push(ModelUsage {
        model: "claude-opus-4-8".into(),
        input_tokens: 1_000,
        output_tokens: 500,
        cache_read_tokens: 2_000,
        cache_write_tokens: 100,
    });
    r.state.active_goal = Some(SessionGoalSnapshot {
        objective: "finish release".to_string(),
        turns_completed: 3,
        waiting_for_user: false,
        last_summary: Some("tests next".to_string()),
        started_at_ms: 123,
    });
    session::save(&r).expect("save alpha");
    let loaded = session::load(&cwd_a, "alpha").expect("load alpha");
    assert_eq!(loaded.meta.id, "alpha");
    assert_eq!(loaded.meta.cwd, cwd_a);
    assert_eq!(loaded.meta.message_count, 2);
    assert_eq!(loaded.history.len(), 2);
    assert_eq!(loaded.state.todos.len(), 1);
    assert_eq!(loaded.state.todos[0].active_form, "Running tests");
    assert_eq!(loaded.state.read_files, vec![cwd_a.join("src/lib.rs")]);
    assert_eq!(
        loaded.state.usage.len(),
        1,
        "per-model usage should roundtrip"
    );
    assert_eq!(loaded.state.usage[0].model, "claude-opus-4-8");
    assert_eq!(loaded.state.usage[0].input_tokens, 1_000);
    assert_eq!(loaded.state.usage[0].cache_read_tokens, 2_000);
    assert_eq!(loaded.state.usage[0].cache_write_tokens, 100);
    let loaded_goal = loaded
        .state
        .active_goal
        .as_ref()
        .expect("active goal should roundtrip");
    assert_eq!(loaded_goal.objective, "finish release");
    assert_eq!(loaded_goal.turns_completed, 3);
    assert_eq!(loaded_goal.last_summary.as_deref(), Some("tests next"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let session_dir = session::sessions_dir_for(&cwd_a);
        let session_file = session_dir.join("alpha.json");
        let mode = std::fs::metadata(&session_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "session files must not be group/world-readable"
        );
        // The directory itself must be owner-only too, or its slug/timing
        // metadata leaks to other local users despite the 0o600 files.
        let dir_mode = std::fs::metadata(&session_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "session dir must be owner-only");
    }

    // --- list returns newest-first per cwd -------------------------------
    session::save(&record(&cwd_a, "beta", 20_000)).expect("save beta");
    session::save(&record(&cwd_a, "gamma", 15_000)).expect("save gamma");

    let list = session::list(&cwd_a);
    assert_eq!(list.len(), 3, "expected 3 sessions, got: {list:#?}");
    assert!(
        list[0].updated_at_ms >= list[1].updated_at_ms
            && list[1].updated_at_ms >= list[2].updated_at_ms,
        "sort order broken: {list:#?}"
    );
    assert_eq!(list[0].id, "beta", "got: {list:#?}");

    // --- latest_id backs `tomte --continue` ------------------------------
    assert_eq!(
        session::latest_id(&cwd_a).as_deref(),
        Some("beta"),
        "latest_id is the newest-updated session"
    );
    assert_eq!(
        session::latest_id(&PathBuf::from("/tmp/tomte-test-proj-empty")),
        None,
        "a directory with no sessions has no latest id"
    );

    // --- cwd isolation ---------------------------------------------------
    session::save(&record(&cwd_b, "delta", 5_000)).expect("save delta");
    let la = session::list(&cwd_a);
    let lb = session::list(&cwd_b);
    assert_eq!(la.len(), 3);
    assert_eq!(lb.len(), 1);
    assert_eq!(lb[0].id, "delta");

    // --- legacy records without persisted state still load ----------------
    let legacy = serde_json::json!({
        "id": "legacy",
        "cwd": cwd_a,
        "model": "gpt-5",
        "created_at_ms": 7_000,
        "updated_at_ms": 7_000,
        "message_count": 2,
        "preview": "legacy",
        "history": sample_history("legacy")
    });
    std::fs::write(
        session::sessions_dir_for(&cwd_a).join("legacy.json"),
        serde_json::to_string(&legacy).unwrap(),
    )
    .unwrap();
    let legacy_loaded = session::load(&cwd_a, "legacy").expect("load legacy session");
    assert!(legacy_loaded.state.todos.is_empty());
    assert!(legacy_loaded.state.read_files.is_empty());
    assert!(legacy_loaded.state.active_goal.is_none());
    assert!(legacy_loaded.state.usage.is_empty());

    // --- missing id is NotFound -----------------------------------------
    let err = session::load(&cwd_a, "does-not-exist").unwrap_err();
    assert!(
        matches!(err.kind(), std::io::ErrorKind::NotFound),
        "got: {err:?}"
    );

    // --- malicious / corrupt ids are not used as paths -------------------
    let err = session::load(&cwd_a, "../escape").unwrap_err();
    assert!(
        matches!(err.kind(), std::io::ErrorKind::InvalidInput),
        "got: {err:?}"
    );
    let mut bad = record(&cwd_a, "../escape", 30_000);
    let err = session::save(&bad).unwrap_err();
    assert!(
        matches!(err.kind(), std::io::ErrorKind::InvalidInput),
        "got: {err:?}"
    );

    bad.meta.id = "mismatch".into();
    let dir = session::sessions_dir_for(&cwd_a);
    std::fs::write(
        dir.join("actual-file.json"),
        serde_json::to_string(&bad).unwrap(),
    )
    .unwrap();
    let listed = session::list(&cwd_a);
    assert!(
        listed.iter().all(|m| m.id != "mismatch"),
        "mismatched session id should be skipped: {listed:#?}"
    );

    // --- Anthropic reasoning blocks survive a save/load roundtrip ------------
    // thinking/signature/redacted_thinking were #[serde(skip)], so they were
    // dropped from the persisted history; a resumed Anthropic turn then replayed
    // a tool_use without its required signed thinking block (provider 400). They
    // must persist now. Use a separate cwd so the list counts above are intact.
    let cwd_c = PathBuf::from("/tmp/tomte-test-proj-c");
    let mut reasoning_rec = record(&cwd_c, "reasoning", 40_000);
    reasoning_rec.history = vec![
        InputItem::Reasoning {
            id: String::new(),
            summary: Vec::new(),
            thinking: Some("deep thought".into()),
            signature: Some("sig-abc".into()),
            redacted_thinking: None,
        },
        InputItem::Reasoning {
            id: String::new(),
            summary: Vec::new(),
            thinking: None,
            signature: None,
            redacted_thinking: Some("redacted-xyz".into()),
        },
    ];
    session::save(&reasoning_rec).expect("save reasoning");
    let loaded_reasoning = session::load(&cwd_c, "reasoning").expect("load reasoning");
    match &loaded_reasoning.history[0] {
        InputItem::Reasoning {
            thinking,
            signature,
            ..
        } => {
            assert_eq!(thinking.as_deref(), Some("deep thought"));
            assert_eq!(signature.as_deref(), Some("sig-abc"));
        }
        other => panic!("expected reasoning item, got {other:?}"),
    }
    match &loaded_reasoning.history[1] {
        InputItem::Reasoning {
            redacted_thinking, ..
        } => {
            assert_eq!(redacted_thinking.as_deref(), Some("redacted-xyz"));
        }
        other => panic!("expected reasoning item, got {other:?}"),
    }
}
