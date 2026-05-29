//! Integration test for session persistence: save records to a scratch
//! `XDG_CONFIG_HOME`, then exercise list + load + the not-found path.
//!
//! Cargo runs integration tests in parallel by default. `set_var` is process-
//! global, so each `#[test]` racing on `XDG_CONFIG_HOME` would corrupt every
//! other test's view of the sessions directory. We collapse the coverage into
//! a single function so the env override is set once and used serially.

use opencli_core::openai::{InputItem, MessageContent};
use opencli_core::session::{self, SessionMeta, SessionRecord};
use std::path::{Path, PathBuf};

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
        history: sample_history(id),
    }
}

#[test]
fn session_save_load_list_and_missing_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::env::set_var("XDG_CONFIG_HOME", tmp.path());

    let cwd_a = PathBuf::from("/tmp/opencli-test-proj-a");
    let cwd_b = PathBuf::from("/tmp/opencli-test-proj-b");

    // --- save/load roundtrip ---------------------------------------------
    let r = record(&cwd_a, "alpha", 1_000);
    session::save(&r).expect("save alpha");
    let loaded = session::load(&cwd_a, "alpha").expect("load alpha");
    assert_eq!(loaded.meta.id, "alpha");
    assert_eq!(loaded.meta.cwd, cwd_a);
    assert_eq!(loaded.meta.message_count, 2);
    assert_eq!(loaded.history.len(), 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let session_file = session::sessions_dir_for(&cwd_a).join("alpha.json");
        let mode = std::fs::metadata(session_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "session files must not be group/world-readable"
        );
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

    // --- cwd isolation ---------------------------------------------------
    session::save(&record(&cwd_b, "delta", 5_000)).expect("save delta");
    let la = session::list(&cwd_a);
    let lb = session::list(&cwd_b);
    assert_eq!(la.len(), 3);
    assert_eq!(lb.len(), 1);
    assert_eq!(lb[0].id, "delta");

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
}
