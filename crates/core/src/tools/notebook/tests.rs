use super::*;
use crate::tools::{ApprovalMode, SessionState};
use std::sync::Arc;
use tokio::sync::Mutex;

fn ctx(cwd: std::path::PathBuf) -> ToolContext {
    ToolContext {
        cwd,
        approval: ApprovalMode::Auto,
        require_approval: false,
        auto_approve_edits: false,
        non_interactive: false,
        session: Arc::new(Mutex::new(SessionState::default())),
        config: crate::config::Config::default(),
        cwd_override: Arc::new(Mutex::new(None)),
        events: None,
    }
}

fn sample_nb() -> String {
    json!({
        "cells": [
            {"cell_type": "code", "id": "aaa", "metadata": {}, "source": ["print(1)\n"], "outputs": [{"x": 1}], "execution_count": 3},
            {"cell_type": "markdown", "id": "bbb", "metadata": {}, "source": ["# Title\n"]}
        ],
        "metadata": {},
        "nbformat": 4,
        "nbformat_minor": 5
    })
    .to_string()
}

async fn write_nb(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("nb.ipynb");
    tokio::fs::write(&p, sample_nb()).await.unwrap();
    p
}

/// A `ctx` with the sample notebook already registered as read, so the
/// read-before-edit guard passes (mirrors the model having read it first).
async fn nb_ctx(dir: &std::path::Path) -> ToolContext {
    let c = ctx(dir.to_path_buf());
    if let Ok(p) = resolve(&c.cwd, "nb.ipynb") {
        c.session.lock().await.read_files.insert(p);
    }
    c
}

#[tokio::test]
async fn replace_updates_source_and_clears_outputs() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    let out = NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "print(42)\n", "cell_id": "aaa", "cell_type": null, "edit_mode": "replace"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();
    assert!(out.contains("Replaced cell `aaa`"), "got: {out}");
    let nb: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
            .unwrap();
    let cell = &nb["cells"][0];
    assert_eq!(cell["source"], json!(["print(42)\n"]));
    assert_eq!(cell["outputs"], json!([]));
    assert_eq!(cell["execution_count"], Value::Null);
    assert_eq!(nb["cells"][1]["id"], "bbb");
}

#[tokio::test]
async fn insert_adds_cell_after_target() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "x = 5\n", "cell_id": "aaa", "cell_type": "code", "edit_mode": "insert"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();
    let nb: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
            .unwrap();
    assert_eq!(nb["cells"].as_array().unwrap().len(), 3);
    assert_eq!(nb["cells"][1]["source"], json!(["x = 5\n"]));
    assert_eq!(nb["cells"][1]["cell_type"], "code");
}

#[tokio::test]
async fn insert_at_top_when_cell_id_null() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "# Intro\n", "cell_id": null, "cell_type": "markdown", "edit_mode": "insert"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();
    let nb: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
            .unwrap();
    assert_eq!(nb["cells"][0]["source"], json!(["# Intro\n"]));
    assert_eq!(nb["cells"][0]["cell_type"], "markdown");
}

#[tokio::test]
async fn delete_removes_cell() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "", "cell_id": "aaa", "cell_type": null, "edit_mode": "delete"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();
    let nb: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
            .unwrap();
    let cells = nb["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0]["id"], "bbb");
}

// Regression: notebook_edit must refuse to run if the notebook wasn't read
// this session, so it can't clobber cells the model never saw.
#[tokio::test]
async fn notebook_edit_requires_prior_read() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    let err = NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "x", "cell_id": "aaa", "cell_type": null, "edit_mode": "replace"}),
            // plain ctx — the notebook is NOT registered as read.
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("requires reading"), "got: {err}");
}

// Regression: delete is destructive, so a numeric cell_id that isn't a real
// cell id must be refused rather than removing a cell by position.
#[tokio::test]
async fn delete_refuses_numeric_index_fallback() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    let err = NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "", "cell_id": "0", "cell_type": null, "edit_mode": "delete"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("positional index"), "got: {err}");
    // Both cells survive.
    let nb: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
            .unwrap();
    assert_eq!(nb["cells"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn rejects_non_ipynb_path() {
    let dir = tempfile::tempdir().unwrap();
    let err = NotebookEdit
        .execute(
            json!({"notebook_path": "nb.txt", "new_source": "x", "cell_id": "0", "cell_type": null, "edit_mode": "replace"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains(".ipynb"), "got: {err}");
}

#[tokio::test]
async fn accepts_path_alias_for_notebook_path() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;

    let out = NotebookEdit
        .execute(
            json!({"path": "nb.ipynb", "new_source": "print(42)\n", "cell_id": "aaa", "cell_type": null, "edit_mode": "replace"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();

    assert!(out.contains("Replaced cell `aaa`"), "got: {out}");
}

#[tokio::test]
async fn accepts_camel_case_arg_aliases() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;

    let out = NotebookEdit
        .execute(
            json!({
                "notebookPath": "nb.ipynb",
                "newSource": "print(42)\n",
                "cellId": "aaa",
                "cellType": null,
                "editMode": "replace"
            }),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();

    assert!(out.contains("Replaced cell `aaa`"), "got: {out}");
}

#[tokio::test]
async fn accepts_source_mode_and_numeric_index_aliases() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;

    let out = NotebookEdit
        .execute(
            json!({
                "path": "nb.ipynb",
                "source": ["print(99)\n"],
                "index": 0,
                "type": null,
                "mode": "replace"
            }),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();

    assert!(out.contains("matched by index"), "got: {out}");
    let nb: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
            .unwrap();
    assert_eq!(nb["cells"][0]["source"], json!(["print(99)\n"]));
}

#[tokio::test]
async fn delete_allows_omitting_unused_new_source() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;

    NotebookEdit
        .execute(
            json!({
                "notebook_path": "nb.ipynb",
                "id": "aaa",
                "action": "delete"
            }),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();

    let nb: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
            .unwrap();
    assert_eq!(nb["cells"].as_array().unwrap().len(), 1);
    assert_eq!(nb["cells"][0]["id"], "bbb");
}

#[tokio::test]
async fn replace_by_numeric_index_fallback() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "y = 2\n", "cell_id": "0", "cell_type": null, "edit_mode": "replace"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();
    let nb: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
            .unwrap();
    assert_eq!(nb["cells"][0]["source"], json!(["y = 2\n"]));
}

#[tokio::test]
async fn replace_rejects_invalid_cell_type() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    let err = NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "text\n", "cell_id": "aaa", "cell_type": "raw", "edit_mode": "replace"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cell_type must be"), "got: {err}");

    let nb: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
            .unwrap();
    assert_eq!(nb["cells"][0]["cell_type"], "code");
}

#[tokio::test]
async fn numeric_index_fallback_is_flagged_in_message() {
    // cell_id "0" matches no cell `id`, so it resolves via the numeric
    // fallback. The result must say so, otherwise a wrong-cell edit looks
    // identical to a real id match.
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    let out = NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "y = 2\n", "cell_id": "0", "cell_type": null, "edit_mode": "replace"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();
    assert!(out.contains("matched by index"), "got: {out}");
}

#[tokio::test]
async fn id_match_has_no_index_note() {
    let dir = tempfile::tempdir().unwrap();
    write_nb(dir.path()).await;
    let out = NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "z\n", "cell_id": "aaa", "cell_type": null, "edit_mode": "replace"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();
    assert!(!out.contains("matched by index"), "got: {out}");
}

#[cfg(unix)]
#[tokio::test]
async fn notebook_edit_preserves_existing_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = write_nb(dir.path()).await;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();

    NotebookEdit
        .execute(
            json!({"notebook_path": "nb.ipynb", "new_source": "print(42)\n", "cell_id": "aaa", "cell_type": null, "edit_mode": "replace"}),
            &nb_ctx(dir.path()).await,
        )
        .await
        .unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o664);
}
