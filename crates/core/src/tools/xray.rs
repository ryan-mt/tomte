//! The `why_context` tool: the Repo Twin's Context X-Ray, in the agent's own
//! hands. Given a seed (a file, a stack-trace `file:line`, or a symbol) it
//! returns the files a maintainer would pull into context — each claim grounded
//! in a real import edge, symbol definition, test edge, or commit — and the
//! nearby files deliberately left out, with the reason. The same engine behind
//! `tomte why-context` and `/why-context`; this makes the map something the
//! model consults on its own instead of a card only the user can ask for.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

pub struct WhyContext;

#[derive(Deserialize)]
struct Args {
    seed: String,
}

#[async_trait]
impl BuiltinTool for WhyContext {
    fn name(&self) -> &'static str {
        "why_context"
    }

    fn is_read_only(&self) -> bool {
        // It never touches the working tree; the twin's index cache lives under
        // tomte's own config dir, beside the memory/decision stores.
        true
    }

    fn description(&self) -> &'static str {
        "Ask the Repo Twin which files belong in context for a seed, BEFORE reading around a large or unfamiliar codebase. The seed is a file path (\"src/auth/session.rs\"), a stack-trace location (\"src/x.rs:88\"), or a symbol name (\"createSession\"). Returns the files a maintainer would pull in — each with the index it came from (import / symbol / test / git / decision) — plus the nearby files deliberately left out, with the reason they're unreachable.\n\
\n\
When to use:\n\
- At the start of a task that names a file, an error location, or a symbol: one call replaces several exploratory greps and tells you which test covers the code you're about to change.\n\
- NOT for keyword search (use grep) and NOT for listing directories (use list_dir).\n\
\n\
Every claim is grounded in a real edge of the index — nothing is guessed. The index builds on first use and re-uses a cache until the tree changes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "seed": {
                    "type": "string",
                    "description": "A file path, a `file:line` stack-trace location, or a symbol name."
                }
            },
            "required": ["seed"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: Args = super::parse_args("why_context", args)?;
        let seed = a.seed.trim().to_string();
        if seed.is_empty() {
            anyhow::bail!(
                "why_context requires a non-empty `seed` (a file, `file:line`, or symbol)."
            );
        }
        // The first build walks the whole tree; keep it off the async runtime.
        let cwd = ctx.cwd.clone();
        let card = tokio::task::spawn_blocking(move || {
            let twin = crate::repo_twin::load_or_build(&cwd)?;
            let sel = crate::repo_twin::select::why_context(&twin, &cwd, &seed);
            anyhow::Ok(crate::repo_twin::select::render(&sel))
        })
        .await??;
        Ok(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_in(cwd: std::path::PathBuf) -> ToolContext {
        ToolContext::new(cwd, crate::tools::ApprovalMode::Auto)
    }

    #[tokio::test]
    async fn resolves_a_file_seed_and_cites_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub mod util;\npub fn run() { util::help(); }\n",
        )
        .unwrap();
        std::fs::write(root.join("src/util.rs"), "pub fn help() {}\n").unwrap();

        let out = WhyContext
            .execute(json!({"seed": "src/lib.rs"}), &ctx_in(root.to_path_buf()))
            .await
            .unwrap();
        assert!(out.contains("Context X-Ray for `src/lib.rs`"), "{out}");
        // The `mod util;` edge pulls util.rs in, cited as an import.
        assert!(out.contains("src/util.rs"), "{out}");
        assert!(out.contains("[import]"), "{out}");
    }

    #[tokio::test]
    async fn empty_seed_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let err = WhyContext
            .execute(json!({"seed": "  "}), &ctx_in(tmp.path().to_path_buf()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("seed"));
    }

    #[tokio::test]
    async fn unknown_seed_reports_missing_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}\n").unwrap();
        let out = WhyContext
            .execute(
                json!({"seed": "noSuchSymbolAnywhere"}),
                &ctx_in(tmp.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(out.contains("Could not resolve"), "{out}");
    }
}
