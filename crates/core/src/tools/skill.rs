//! The `skill` tool: load a curated playbook's full instructions on demand.
//!
//! This is the second half of progressive disclosure (see `crate::skill`).
//! The system prompt lists every installed skill by `name: description`; when
//! a task matches one, the model calls this tool with the skill's name and
//! gets back the full `SKILL.md` body to follow. Bodies are never injected
//! speculatively, so owning hundreds of skills costs ~one manifest line each.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

pub struct LoadSkill;

#[derive(Deserialize)]
struct Args {
    name: String,
}

#[async_trait]
impl BuiltinTool for LoadSkill {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "Load a curated skill (playbook) by name and return its full instructions to follow.\n\
\n\
When to use:\n\
- The user's task clearly matches one of the skills listed under \"# Available skills\" in your system prompt — load it, then follow its guidance for the rest of the task.\n\
- You recognise the work as a category another agent has already written a playbook for (a framework, a workflow, a domain): load the matching skill instead of improvising.\n\
\n\
When NOT to use:\n\
- Speculatively, or for a skill whose description does not clearly fit the task — loading wastes context.\n\
- For a skill you already loaded this session; its instructions are already in context.\n\
\n\
Parameters:\n\
- `name`: The exact skill name as shown in the \"# Available skills\" manifest (e.g. `git-workflow`).\n\
\n\
Behaviour:\n\
- Returns the skill's markdown body plus the skill's directory, so any files the skill references (scripts, templates) can be read relative to it.\n\
- If the name is not found, the error lists the available skill names so you can retry with a correct one."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Exact skill name from the \"# Available skills\" manifest."
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: Args = super::parse_args("skill", args)?;
        match crate::skill::load_body(&ctx.cwd, &a.name) {
            Ok((dir, body)) => Ok(format!(
                "# Skill: {}\n(skill directory: {})\n\n{}",
                a.name,
                dir.display(),
                body.trim()
            )),
            Err(_) => {
                let available: Vec<String> = crate::skill::discover(&ctx.cwd)
                    .into_iter()
                    .map(|e| e.name)
                    .collect();
                if available.is_empty() {
                    Err(anyhow!(
                        "skill `{}` not found — no skills are installed. Install skills under ~/.config/opencli/skills/<name>/SKILL.md or ~/.claude/skills/.",
                        a.name
                    ))
                } else {
                    Err(anyhow!(
                        "skill `{}` not found. Available skills: {}",
                        a.name,
                        available.join(", ")
                    ))
                }
            }
        }
    }
}
