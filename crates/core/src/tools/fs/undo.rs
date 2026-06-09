//! The `undo_last_edit` tool. Split out of `fs`; logic unchanged.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tools::{BuiltinTool, ToolContext};

use super::common::{atomic_write_preserving_permissions, rand_suffix, snapshot_meta};

pub struct UndoLastEdit;

#[async_trait]
impl BuiltinTool for UndoLastEdit {
    fn name(&self) -> &'static str {
        "undo_last_edit"
    }
    fn description(&self) -> &'static str {
        "Roll back the most recent file edit you made via `edit_file`, `write_file`, or `multi_edit`. Undo is a stack: each call reverts one edit, most-recent first. If the edit created a new file, undo deletes it; otherwise it restores the previous contents.\n\
\n\
Refuses to undo when the file changed since your edit (the user, their editor, or another tool touched it) so it can't silently destroy work you didn't make — restore manually in that case.\n\
\n\
Use it to recover from a bad edit right after you notice. It does not undo `run_shell` side effects or anything outside the file-edit stack.\n\
\n\
Parameters: none."
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false })
    }
    async fn execute(&self, _args: Value, ctx: &ToolContext) -> Result<String> {
        let mut session = ctx.session.lock().await;
        let entry = session
            .undo_stack
            .back()
            .cloned()
            .ok_or_else(|| anyhow!("no edits to undo"))?;
        // TOCTOU guard: refuse to restore if the file has been touched since
        // the edit. Without this, an `undo_last_edit` after the user manually
        // edits the file (in their editor, another shell, etc.) would
        // silently nuke those changes.
        if let Some(expected) = entry.post_edit_mtime {
            let (current_mtime, current_size) = snapshot_meta(&entry.path);
            if current_mtime != Some(expected) || current_size != entry.post_edit_size {
                return Err(anyhow!(
                    "refusing to undo {}: file has been modified since the edit; restore manually if intended",
                    entry.path.display()
                ));
            }
        }
        let was_content_restore = entry.original_content.is_some();
        let message = match entry.original_content {
            Some(content) => {
                // Atomic restore (temp + rename), matching the edit/write tools,
                // so a crash mid-restore can't leave a half-written file.
                let tmp = entry
                    .path
                    .with_extension(format!("undo-{}.tmp", rand_suffix()));
                atomic_write_preserving_permissions(&entry.path, &tmp, &content).await?;
                format!("Restored {}", entry.path.display())
            }
            None => {
                tokio::fs::remove_file(&entry.path)
                    .await
                    .with_context(|| format!("remove {}", entry.path.display()))?;
                format!("Removed (was a new file): {}", entry.path.display())
            }
        };
        session.pop_undo_entry();
        if was_content_restore {
            // Our restore rewrote the file with a fresh mtime; refresh the next
            // same-file entry so a follow-up undo doesn't read it as external.
            session.refresh_top_snapshot_for(&entry.path);
        }
        Ok(message)
    }
}
