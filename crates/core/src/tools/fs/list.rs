//! The `list_dir` tool. Split out of `fs`; logic unchanged.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tools::{BuiltinTool, ToolContext};

use super::common::resolve;

pub struct ListDir;

#[derive(Deserialize)]
struct ListArgs {
    #[serde(
        alias = "file_path",
        alias = "filePath",
        alias = "directory",
        alias = "dir",
        alias = "folder"
    )]
    path: String,
}

#[async_trait]
impl BuiltinTool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }
    fn description(&self) -> &'static str {
        "List the immediate entries of a directory. Directories are suffixed with `/` so you can distinguish them from files at a glance. Output is sorted lexicographically.\n\
\n\
When to use:\n\
- A snapshot of one directory's direct children — \"what's at the repo root?\", \"what's in src/?\".\n\
\n\
When NOT to use:\n\
- Recursive discovery — use `glob` with `**/*` patterns; it's faster and respects `.gitignore`.\n\
- Searching for files matching a pattern — use `glob` directly.\n\
- Searching file contents — use `grep`.\n\
\n\
Parameters:\n\
- `path`: Relative directory path inside the working directory."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative directory path inside the working directory."}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: ListArgs = crate::tools::parse_args("list_dir", args)?;
        let path = resolve(&ctx.cwd, &a.path)?;
        // Distinguish "not found" and "it's a file" from a raw read_dir OS error
        // so the model can self-correct (e.g. reach for read_file on a file).
        match tokio::fs::metadata(&path).await {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(anyhow!(
                    "{} is a file, not a directory — use read_file to read it",
                    a.path
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(anyhow!("directory not found: {}", a.path))
            }
            Err(e) => return Err(e).with_context(|| format!("list_dir {}", a.path)),
        }
        let mut entries = tokio::fs::read_dir(&path).await?;
        let mut items: Vec<String> = Vec::new();
        while let Some(e) = entries.next_entry().await? {
            let ft = e.file_type().await?;
            let name = e.file_name().to_string_lossy().to_string();
            items.push(if ft.is_dir() {
                format!("{name}/")
            } else {
                name
            });
        }
        items.sort();
        Ok(items.join("\n"))
    }
}
