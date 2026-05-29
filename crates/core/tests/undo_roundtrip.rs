//! Integration tests for the in-session undo stack: write a file, edit it,
//! then verify `UndoLastEdit` rolls each step back in LIFO order.

use opencli_core::tools::{
    fs::{EditFile, MultiEdit, UndoLastEdit, WriteFile},
    ApprovalMode, BuiltinTool, SessionState, ToolContext,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;

fn ctx(cwd: std::path::PathBuf) -> ToolContext {
    ToolContext {
        cwd,
        approval: ApprovalMode::Auto,
        session: Arc::new(Mutex::new(SessionState::default())),
    }
}

#[tokio::test]
async fn undo_restores_overwritten_content() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());

    tokio::fs::write(tmp.path().join("a.txt"), b"original\n")
        .await
        .unwrap();

    WriteFile
        .execute(json!({"path": "a.txt", "content": "overwritten\n"}), &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "overwritten\n"
    );

    let msg = UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert!(msg.contains("Restored"), "got: {msg}");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn undo_removes_newly_created_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());

    WriteFile
        .execute(json!({"path": "new.txt", "content": "fresh\n"}), &ctx)
        .await
        .unwrap();
    assert!(tmp.path().join("new.txt").exists());

    let msg = UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert!(msg.contains("Removed"), "got: {msg}");
    assert!(!tmp.path().join("new.txt").exists());
}

#[tokio::test]
async fn undo_unwinds_in_lifo_order() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());

    tokio::fs::write(tmp.path().join("a.txt"), b"a0")
        .await
        .unwrap();
    tokio::fs::write(tmp.path().join("b.txt"), b"b0")
        .await
        .unwrap();

    WriteFile
        .execute(json!({"path": "a.txt", "content": "a1"}), &ctx)
        .await
        .unwrap();
    WriteFile
        .execute(json!({"path": "b.txt", "content": "b1"}), &ctx)
        .await
        .unwrap();

    UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("b.txt")).unwrap(),
        "b0"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "a1"
    );

    UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "a0"
    );
}

#[tokio::test]
async fn undo_with_empty_stack_errors_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    let err = UndoLastEdit.execute(json!({}), &ctx).await.unwrap_err();
    assert!(err.to_string().contains("no edits to undo"), "got: {err}");
}

#[tokio::test]
async fn undo_reverts_edit_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    tokio::fs::write(tmp.path().join("c.txt"), b"foo bar baz")
        .await
        .unwrap();

    EditFile
        .execute(
            json!({
                "path": "c.txt",
                "old_string": "bar",
                "new_string": "BAR",
                "replace_all": false,
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("c.txt")).unwrap(),
        "foo BAR baz"
    );

    UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("c.txt")).unwrap(),
        "foo bar baz"
    );
}

#[tokio::test]
async fn undo_reverts_multi_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    tokio::fs::write(tmp.path().join("d.txt"), b"alpha beta gamma")
        .await
        .unwrap();

    MultiEdit
        .execute(
            json!({
                "path": "d.txt",
                "edits": [
                    {"old_string": "alpha", "new_string": "A", "replace_all": false},
                    {"old_string": "gamma", "new_string": "G", "replace_all": false}
                ]
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("d.txt")).unwrap(),
        "A beta G"
    );

    UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("d.txt")).unwrap(),
        "alpha beta gamma"
    );
}

#[tokio::test]
async fn undo_restores_overwritten_binary_file() {
    // Regression: overwriting an existing NON-UTF-8 (binary) file then undoing
    // must restore the original bytes — not delete the file. The undo snapshot
    // previously used read_to_string().ok(), which returned None for binary
    // content, so undo mistook it for a newly-created file and remove_file'd it.
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());

    let original: &[u8] = &[0x00, 0xFF, 0xFE, 0x42, 0x00]; // invalid UTF-8
    tokio::fs::write(tmp.path().join("img.bin"), original)
        .await
        .unwrap();

    WriteFile
        .execute(json!({"path": "img.bin", "content": "overwritten"}), &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(tmp.path().join("img.bin")).unwrap(),
        b"overwritten"
    );

    let msg = UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert!(
        msg.contains("Restored"),
        "must restore, not remove; got: {msg}"
    );
    assert!(
        tmp.path().join("img.bin").exists(),
        "binary file must still exist after undo"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("img.bin")).unwrap(),
        original,
        "original binary bytes must be restored exactly"
    );
}
