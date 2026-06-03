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
    #[serde(
        alias = "path",
        alias = "file_path",
        alias = "filePath",
        alias = "notebookPath"
    )]
    notebook_path: String,
    #[serde(
        default,
        alias = "newSource",
        alias = "source",
        alias = "content",
        alias = "text",
        deserialize_with = "deserialize_optional_source"
    )]
    new_source: Option<String>,
    #[serde(
        default,
        alias = "cellId",
        alias = "cellID",
        alias = "id",
        alias = "index",
        alias = "cell_index",
        alias = "cellIndex",
        deserialize_with = "deserialize_optional_stringish"
    )]
    cell_id: Option<String>,
    #[serde(default, alias = "cellType", alias = "type")]
    cell_type: Option<String>,
    #[serde(default, alias = "editMode", alias = "mode", alias = "action")]
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
        // Read-before-edit safety, same as edit_file/write_file: the edit may
        // only proceed if the model actually read this notebook this session and
        // it hasn't changed on disk since — otherwise an edit/delete could clobber
        // cells the model never saw.
        {
            let session = ctx.session.lock().await;
            if !session.read_files.contains(&path) {
                return Err(anyhow!(
                    "notebook_edit requires reading {} first so the edit targets cells you've seen. Call read_file on it.",
                    path.display()
                ));
            }
            super::fs::ensure_not_stale(&session, &path, "notebook_edit")?;
        }
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
                let source = a
                    .new_source
                    .as_deref()
                    .ok_or_else(|| anyhow!("edit_mode `replace` requires new_source"))?;
                let cid = a
                    .cell_id
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("edit_mode `replace` requires cell_id"))?;
                let (idx, by_id) = find_cell_index(cells, cid)
                    .ok_or_else(|| anyhow!("cell `{cid}` not found in notebook"))?;
                let cell = cells[idx]
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("cell {idx} is not an object"))?;
                if let Some(ct) = &a.cell_type {
                    validate_cell_type(ct)?;
                    cell.insert("cell_type".into(), json!(ct));
                }
                cell.insert("source".into(), to_source_lines(source));
                let is_code = cell.get("cell_type").and_then(|v| v.as_str()) == Some("code");
                if is_code {
                    cell.insert("outputs".into(), json!([]));
                    cell.insert("execution_count".into(), Value::Null);
                } else {
                    cell.remove("outputs");
                    cell.remove("execution_count");
                }
                format!(
                    "Replaced cell `{cid}` (index {idx}){} in {}",
                    index_fallback_note(by_id),
                    a.notebook_path
                )
            }
            "insert" => {
                let source = a
                    .new_source
                    .as_deref()
                    .ok_or_else(|| anyhow!("edit_mode `insert` requires new_source"))?;
                let ct = a
                    .cell_type
                    .as_deref()
                    .ok_or_else(|| anyhow!("edit_mode `insert` requires cell_type"))?;
                validate_cell_type(ct)?;
                let at = match a.cell_id.as_deref().filter(|s| !s.is_empty()) {
                    None => 0,
                    Some(cid) => find_cell_index(cells, cid)
                        .map(|(i, _)| i + 1)
                        .ok_or_else(|| anyhow!("cell `{cid}` not found in notebook"))?,
                };
                cells.insert(at, make_cell(ct, source));
                format!("Inserted {ct} cell at index {at} in {}", a.notebook_path)
            }
            "delete" => {
                let cid = a
                    .cell_id
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("edit_mode `delete` requires cell_id"))?;
                let (idx, by_id) = find_cell_index(cells, cid)
                    .ok_or_else(|| anyhow!("cell `{cid}` not found in notebook"))?;
                // Delete is destructive and irreversible within the notebook, so
                // never fall back to treating a numeric `cell_id` as a position —
                // that could remove a cell the model didn't mean. Require a real id.
                if !by_id {
                    return Err(anyhow!(
                        "refusing to delete cell `{cid}` by positional index — no cell has that id. Pass the cell's real `id`."
                    ));
                }
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
        super::fs::atomic_write_preserving_permissions(&path, &tmp, new_content.as_bytes()).await?;

        let (post_edit_mtime, post_edit_size) = super::fs::snapshot_meta(&path);
        {
            let mut session = ctx.session.lock().await;
            // Refresh the read snapshot to the bytes we just wrote so a follow-up
            // notebook_edit isn't flagged stale by our own change, and record the
            // path as read so the read-before-edit guard above passes next time.
            session
                .read_file_meta
                .insert(path.clone(), (post_edit_mtime, post_edit_size));
            session.read_files.insert(path.clone());
            session.push_undo_entry(UndoEntry {
                path: path.clone(),
                original_content: Some(original.into_bytes()),
                post_edit_mtime,
                post_edit_size,
            });
        }
        Ok(msg)
    }
}

fn deserialize_optional_source<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                match item {
                    Value::String(s) => out.push_str(&s),
                    _ => {
                        return Err(serde::de::Error::custom(
                            "expected source string or string array",
                        ))
                    }
                }
            }
            Ok(Some(out))
        }
        _ => Err(serde::de::Error::custom(
            "expected source string, string array, or null",
        )),
    }
}

fn deserialize_optional_stringish<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        Value::Number(n) => Ok(Some(n.to_string())),
        _ => Err(serde::de::Error::custom("expected string, number, or null")),
    }
}

/// Find a cell by its `id` field, falling back to a 0-based numeric index.
/// Returns the index plus whether the match was by `id` (`true`) or via the
/// numeric-index fallback (`false`); callers flag the fallback in their result
/// so an index match on what the model meant as an id can't silently edit or
/// delete the wrong cell while still reporting success.
fn find_cell_index(cells: &[Value], cell_id: &str) -> Option<(usize, bool)> {
    if let Some(i) = cells
        .iter()
        .position(|c| c.get("id").and_then(|v| v.as_str()) == Some(cell_id))
    {
        return Some((i, true));
    }
    cell_id
        .parse::<usize>()
        .ok()
        .filter(|&i| i < cells.len())
        .map(|i| (i, false))
}

/// Suffix appended to a result message when a cell was located via the
/// numeric-index fallback rather than a real `id` match.
fn index_fallback_note(by_id: bool) -> &'static str {
    if by_id {
        ""
    } else {
        " — matched by index, no cell has that id"
    }
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
mod tests;
