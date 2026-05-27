use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

pub struct AskUserQuestion;

#[derive(Deserialize)]
struct Args {
    questions: Vec<Question>,
}

#[derive(Deserialize)]
struct Question {
    question: String,
    header: String,
    options: Vec<Opt>,
    #[serde(default)]
    multi_select: Option<bool>,
}

#[derive(Deserialize)]
struct Opt {
    label: String,
    description: String,
}

#[async_trait]
impl BuiltinTool for AskUserQuestion {
    fn name(&self) -> &'static str {
        "ask_user_question"
    }
    fn description(&self) -> &'static str {
        "Ask the user one or more multiple-choice questions when a decision is needed and you can't make it yourself. The CLI / Web UI renders each question with its options; the user's selections come back in the next turn as a normal user message.\n\
\n\
When to use:\n\
- You need a real decision the user must own (which approach, which file, which trade-off).\n\
- Two or more reasonable interpretations exist and picking silently would be wrong.\n\
- You want consent before a hard-to-reverse action (drop a table, force push, delete a branch).\n\
\n\
When NOT to use:\n\
- The answer is derivable by reading code or running a command — do that instead.\n\
- Trivial follow-ups (\"continue?\") — just continue.\n\
- A free-text answer is more natural — ask in plain text, not via this tool.\n\
\n\
Mechanics:\n\
- 1–4 questions per call. Each question has 2–4 options. Each option has a short `label` and a longer `description`.\n\
- Set `multi_select: true` when the user can pick more than one option; default is single-select.\n\
- Keep options mutually exclusive (single-select) or non-overlapping (multi-select). Never include an \"Other\" — the UI provides it automatically.\n\
- After you call this tool, STOP and wait for the user's reply. Do not pre-emptively assume an answer in the same turn.\n\
\n\
Output: a JSON envelope the CLI/Web UI can render. Do not reformat the response — just emit the structure and stop.\n\
\n\
Parameters:\n\
- `questions`: Array of 1–4 questions. Each `{question, header, options, multi_select}`.\n\
  - `question`: Full question text ending in `?`.\n\
  - `header`: Short chip label (max ~12 chars).\n\
  - `options`: 2–4 entries; each `{label, description}`.\n\
  - `multi_select`: When true, user can pick multiple; default false."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 4,
                    "description": "1–4 questions to ask.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {"type": "string", "description": "Full question text."},
                            "header": {"type": "string", "description": "Short chip label."},
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 4,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {"type": "string"},
                                        "description": {"type": "string"}
                                    },
                                    "required": ["label", "description"],
                                    "additionalProperties": false
                                }
                            },
                            "multi_select": {"type": ["boolean", "null"], "description": "Allow multiple selections; default false."}
                        },
                        "required": ["question", "header", "options", "multi_select"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["questions"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let a: Args = super::parse_args("ask_user_question", args)?;
        if a.questions.is_empty() || a.questions.len() > 4 {
            return Err(anyhow!("must supply 1..=4 questions"));
        }
        for (i, q) in a.questions.iter().enumerate() {
            if q.options.len() < 2 || q.options.len() > 4 {
                return Err(anyhow!(
                    "question #{}: needs 2..=4 options (got {})",
                    i + 1,
                    q.options.len()
                ));
            }
        }
        let envelope = json!({
            "kind": "ask_user_question",
            "questions": a.questions.iter().map(|q| json!({
                "question": q.question,
                "header": q.header,
                "options": q.options.iter().map(|o| json!({
                    "label": o.label,
                    "description": o.description,
                })).collect::<Vec<_>>(),
                "multi_select": q.multi_select.unwrap_or(false),
            })).collect::<Vec<_>>(),
        });
        Ok(serde_json::to_string(&envelope)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ApprovalMode, SessionState};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn ctx() -> ToolContext {
        ToolContext {
            cwd: std::env::current_dir().unwrap(),
            approval: ApprovalMode::Auto,
            session: Arc::new(Mutex::new(SessionState::default())),
        }
    }

    #[tokio::test]
    async fn emits_envelope_with_kind() {
        let out = AskUserQuestion
            .execute(
                json!({
                    "questions": [{
                        "question": "Which approach?",
                        "header": "Approach",
                        "options": [
                            {"label": "A", "description": "do A"},
                            {"label": "B", "description": "do B"}
                        ],
                        "multi_select": false
                    }]
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["kind"], "ask_user_question");
        assert_eq!(v["questions"][0]["header"], "Approach");
        assert_eq!(v["questions"][0]["options"][1]["label"], "B");
        assert_eq!(v["questions"][0]["multi_select"], false);
    }

    #[tokio::test]
    async fn rejects_too_few_options() {
        let err = AskUserQuestion
            .execute(
                json!({
                    "questions": [{
                        "question": "?",
                        "header": "h",
                        "options": [{"label": "x", "description": "y"}],
                        "multi_select": false
                    }]
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("2..=4 options"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_too_many_questions() {
        let qs: Vec<Value> = (0..5)
            .map(|i| json!({
                "question": format!("q{i}?"),
                "header": "h",
                "options": [
                    {"label": "a", "description": "a"},
                    {"label": "b", "description": "b"}
                ],
                "multi_select": false
            }))
            .collect();
        let err = AskUserQuestion
            .execute(json!({ "questions": qs }), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("1..=4 questions"), "got: {err}");
    }
}
