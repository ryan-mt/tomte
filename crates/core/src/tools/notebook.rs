//! `notebook_edit` — the Claude Code `NotebookEdit` analogue. Replace, insert,
//! or delete a single cell in a Jupyter `.ipynb` notebook while leaving the
//! rest of the document untouched.
//!
//! Follows nbformat 4: each cell carries `cell_type` (`code` | `markdown`),
//! `source` (stored here as an array of line-strings), `metadata`, and — for
//! code cells — `outputs` and `execution_count`. Editing a code cell's source
//! invalidates its previous run, so outputs are cleared and `execution_count`
//! reset on every replace.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::fs::resolve;
use super::{BuiltinTool, ToolContext, UndoEntry};

pub struct NotebookEdit;

#[derive(Deserialize)]
struct Args {
    notebook_path: String,
    new_source: String,
    #[serde(default)]
    cell_id: Option<String>,
    #[serde(default)]
    cell_type: Option<String>,
    #[serde(default)]
    edit_mode: Option<String>,
}

#[async_trait]
impl BuiltinTool for NotebookEdit {
    fn name(&self) -> &'static str {
        "notebook_edit"
    }
    fn description(&self) -> &'static str {
        "Replace, insert, or delete a single cell in a Jupyter notebook (`.ipynb`), preserving everything else in the document.\n\
\n\
When to use:\n\
- Editing notebook cells. A `.ipynb` is JSON, not plain text — `edit_file` would corrupt its structure, so use this tool instead.\n\
\n\
Edit modes (`edit_mode`):\n\
- `replace` (default): overwrite the source of the cell identified by `cell_id`. For a code cell this also clears stale `outputs` and resets `execution_count`.\n\
- `insert`: add a NEW cell with `new_source`. `cell_type` is required. The cell is inserted AFTER `cell_id`, or at the very top when `cell_id` is null/empty.\n\
- `delete`: remove the cell identified by `cell_id`.\n\
\n\
Identifying a cell:\n\
- `cell_id` matches a cell's `id` field. As a fallback it is parsed as a 0-based index, so `\"0\"` targets the first cell.\n\
- Read the notebook first (`read_file`) to see cell ids and contents.\n\
\n\
Parameters:\n\
- `notebook_path`: Relative path to the `.ipynb` file inside the working directory.\n\
- `new_source`: The new cell source (ignored for `delete`).\n\
- `cell_id`: Target cell id (or numeric index). Required for `replace`/`delete`; for `insert` it's the cell to insert after (`null` = insert at top).\n\
- `cell_type`: `code` or `markdown`. Required for `insert`; for `replace` it changes the cell's type when supplied.\n\
- `edit_mode`: `replace` (default), `insert`, or `delete`."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "notebook_path": {"type": "string", "description": "Relative path to the .ipynb file inside the working directory."},
                "new_source": {"type": "string", "description": "New source for the cell (ignored for delete)."},
                "cell_id": {"type": ["string", "null"], "description": "Target cell id or 0-based index; null inserts at the top."},
                "cell_type": {"type": ["string", "null"], "enum": ["code", "markdown", null], "description": "Cell type; required for insert."},
                "edit_mode": {"type": ["string", "null"], "enum": ["replace", "insert", "delete", null], "description": "replace (default), insert, or delete."}
            },
            "required": ["notebook_path", "new_source", "cell_id", "cell_type", "edit_mode"],
            "additionalProperties": false
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: Args = super::parse_args("notebook_edit", args)?;
        if !a.notebook_path.ends_with(".ipynb") {
            return Err(anyhow!("notebook_path must point to a .ipynb file"));
        }
        let path = resolve(&ctx.cwd, &a.notebook_path)?;
        let original = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let mut nb: Value = serde_json::from_str(&original)
            .with_context(|| format!("parse notebook JSON: {}", path.display()))?;
        let cells = nb
            .get_mut("cells")
            .and_then(|c| c.as_array_mut())
            .ok_or_else(|| anyhow!("notebook has no `cells` array"))?;

        let mode = a.edit_mode.as_deref().unwrap_or("replace");
        let msg = match mode {
            "replace" => {
                let cid = a
                    .cell_id
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("edit_mode `replace` requires cell_id"))?;
                let idx = find_cell_index(cells, cid)
                    .ok_or_else(|| anyhow!("cell `{cid}` not found in notebook"))?;
                let cell = cells[idx]
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("cell {idx} is not an object"))?;
                if let Some(ct) = &a.cell_type {
                    validate_cell_type(ct)?;
                    cell.insert("cell_type".into(), json!(ct));
                }
                cell.insert("source".into(), to_source_lines(&a.new_source));
                let is_code = cell.get("cell_type").and_then(|v| v.as_str()) == Some("code");
                if is_code {
                    cell.insert("outputs".into(), json!([]));
                    cell.insert("execution_count".into(), Value::Null);
                } else {
                    cell.remove("outputs");
                    cell.remove("execution_count");
                }
                format!("Replaced cell `{cid}` (index {idx}) in {}", a.notebook_path)
            }
            "insert" => {
                let ct = a
                    .cell_type
                    .as_deref()
                    .ok_or_else(|| anyhow!("edit_mode `insert` requires cell_type"))?;
                validate_cell_type(ct)?;
                let at = match a.cell_id.as_deref().filter(|s| !s.is_empty()) {
                    None => 0,
                    Some(cid) => find_cell_index(cells, cid)
                        .map(|i| i + 1)
                        .ok_or_else(|| anyhow!("cell `{cid}` not found in notebook"))?,
                };
                cells.insert(at, make_cell(ct, &a.new_source));
                format!("Inserted {ct} cell at index {at} in {}", a.notebook_path)
            }
            "delete" => {
                let cid = a
                    .cell_id
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("edit_mode `delete` requires cell_id"))?;
                let idx = find_cell_index(cells, cid)
                    .ok_or_else(|| anyhow!("cell `{cid}` not found in notebook"))?;
                cells.remove(idx);
                format!(
                    "Deleted cell `{cid}` (index {idx}) from {}",
                    a.notebook_path
                )
            }
            other => {
                return Err(anyhow!(
                    "invalid edit_mode `{other}` (expected replace|insert|delete)"
                ))
            }
        };

        // Serialize and write atomically (temp + rename) so a crash mid-write
        // can't leave a half-written, unparseable notebook on disk.
        let mut new_content = serde_json::to_string_pretty(&nb)?;
        new_content.push('\n');
        let tmp = path.with_extension(format!("nbedit-{}.tmp", gen_id()));
        tokio::fs::write(&tmp, new_content.as_bytes())
            .await
            .with_context(|| format!("write temp {}", tmp.display()))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;

        let (post_edit_mtime, post_edit_size) = super::fs::snapshot_meta(&path);
        ctx.session.lock().await.push_undo_entry(UndoEntry {
            path: path.clone(),
            original_content: Some(original.into_bytes()),
            post_edit_mtime,
            post_edit_size,
        });
        Ok(msg)
    }
}

/// Find a cell by its `id` field, falling back to a 0-based numeric index.
fn find_cell_index(cells: &[Value], cell_id: &str) -> Option<usize> {
    if let Some(i) = cells
        .iter()
        .position(|c| c.get("id").and_then(|v| v.as_str()) == Some(cell_id))
    {
        return Some(i);
    }
    cell_id.parse::<usize>().ok().filter(|&i| i < cells.len())
}

/// nbformat stores `source` as an array of line-strings, each keeping its
/// trailing newline. An empty source becomes an empty array.
fn to_source_lines(s: &str) -> Value {
    if s.is_empty() {
        return json!([]);
    }
    let lines: Vec<Value> = s
        .split_inclusive('\n')
        .map(|l| Value::String(l.to_string()))
        .collect();
    Value::Array(lines)
}

fn make_cell(cell_type: &str, source: &str) -> Value {
    let id = gen_id();
    if cell_type == "code" {
        json!({
            "cell_type": "code",
            "id": id,
            "metadata": {},
            "source": to_source_lines(source),
            "outputs": [],
            "execution_count": Value::Null,
        })
    } else {
        json!({
            "cell_type": cell_type,
            "id": id,
            "metadata": {},
            "source": to_source_lines(source),
        })
    }
}

fn validate_cell_type(cell_type: &str) -> Result<()> {
    if cell_type == "code" || cell_type == "markdown" {
        Ok(())
    } else {
        Err(anyhow!("cell_type must be `code` or `markdown`"))
    }
}

fn gen_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut b);
    format!("{:08x}", u32::from_be_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ApprovalMode, SessionState};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn ctx(cwd: std::path::PathBuf) -> ToolContext {
        ToolContext {
            cwd,
            approval: ApprovalMode::Auto,
            session: Arc::new(Mutex::new(SessionState::default())),
            config: crate::config::Config::default(),
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

    #[tokio::test]
    async fn replace_updates_source_and_clears_outputs() {
        let dir = tempfile::tempdir().unwrap();
        write_nb(dir.path()).await;
        let out = NotebookEdit
            .execute(
                json!({"notebook_path": "nb.ipynb", "new_source": "print(42)\n", "cell_id": "aaa", "cell_type": null, "edit_mode": "replace"}),
                &ctx(dir.path().to_path_buf()),
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
                &ctx(dir.path().to_path_buf()),
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
                &ctx(dir.path().to_path_buf()),
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
                &ctx(dir.path().to_path_buf()),
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

    #[tokio::test]
    async fn rejects_non_ipynb_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = NotebookEdit
            .execute(
                json!({"notebook_path": "nb.txt", "new_source": "x", "cell_id": "0", "cell_type": null, "edit_mode": "replace"}),
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains(".ipynb"), "got: {err}");
    }

    #[tokio::test]
    async fn replace_by_numeric_index_fallback() {
        let dir = tempfile::tempdir().unwrap();
        write_nb(dir.path()).await;
        NotebookEdit
            .execute(
                json!({"notebook_path": "nb.ipynb", "new_source": "y = 2\n", "cell_id": "0", "cell_type": null, "edit_mode": "replace"}),
                &ctx(dir.path().to_path_buf()),
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
                &ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cell_type must be"), "got: {err}");

        let nb: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb.ipynb")).unwrap())
                .unwrap();
        assert_eq!(nb["cells"][0]["cell_type"], "code");
    }
}
