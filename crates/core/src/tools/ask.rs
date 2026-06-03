use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

pub struct AskUserQuestion;

#[derive(Deserialize)]
struct Args {
    questions: Vec<Question>,
}

#[derive(Deserialize)]
struct Question {
    #[serde(alias = "prompt", alias = "text")]
    question: String,
    #[serde(default, alias = "title")]
    header: Option<String>,
    #[serde(alias = "choices")]
    options: Vec<Opt>,
    #[serde(
        default,
        alias = "multiSelect",
        deserialize_with = "super::deserialize_optional_bool"
    )]
    multi_select: Option<bool>,
}

struct Opt {
    label: String,
    description: String,
}

impl<'de> Deserialize<'de> for Opt {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(s) => {
                let label = s.trim().to_string();
                if label.is_empty() {
                    return Err(serde::de::Error::custom("option label must not be empty"));
                }
                Ok(Self {
                    description: label.clone(),
                    label,
                })
            }
            Value::Object(obj) => {
                let label = first_string(&obj, &["label", "value", "name", "title"])
                    .or_else(|| first_string(&obj, &["description", "detail", "details", "text"]))
                    .ok_or_else(|| serde::de::Error::custom("option label is required"))?;
                let description = first_string(&obj, &["description", "detail", "details", "text"])
                    .unwrap_or_else(|| label.clone());
                Ok(Self { label, description })
            }
            _ => Err(serde::de::Error::custom("expected option string or object")),
        }
    }
}

fn first_string(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| obj.get(*key)?.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

#[async_trait]
impl BuiltinTool for AskUserQuestion {
    fn name(&self) -> &'static str {
        "ask_user_question"
    }
    fn description(&self) -> &'static str {
        "Ask the user one or more multiple-choice questions when a decision is needed and you can't make it yourself. The CLI renders each question with its options; the user's selections come back in the next turn as a normal user message.\n\
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
Output: a JSON envelope the CLI can render. Do not reformat the response — just emit the structure and stop.\n\
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
        let args = normalize_args(args);
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
                "header": q.header.as_deref().unwrap_or("Choice"),
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

fn normalize_args(args: Value) -> Value {
    let Some(obj) = args.as_object() else {
        return args;
    };
    if obj.get("questions").is_some() {
        return args;
    }
    let has_question = obj
        .get("question")
        .or_else(|| obj.get("prompt"))
        .or_else(|| obj.get("text"))
        .is_some();
    let has_options = obj.get("options").or_else(|| obj.get("choices")).is_some();
    if has_question && has_options {
        json!({ "questions": [args] })
    } else {
        args
    }
}

pub fn render_ask_envelope(output: &str) -> Option<String> {
    let v: Value = serde_json::from_str(output).ok()?;
    if v.get("kind").and_then(|k| k.as_str()) != Some("ask_user_question") {
        return None;
    }
    let questions = v.get("questions")?.as_array()?;
    let mut out = String::from("User input needed:\n");
    for (qi, q) in questions.iter().enumerate() {
        let question = q.get("question").and_then(|v| v.as_str()).unwrap_or("");
        let header = q.get("header").and_then(|v| v.as_str()).unwrap_or("");
        if qi > 0 {
            out.push('\n');
        }
        if header.is_empty() {
            out.push_str(&format!("\n{}. {question}\n", qi + 1));
        } else {
            out.push_str(&format!("\n{}. [{header}] {question}\n", qi + 1));
        }
        if let Some(options) = q.get("options").and_then(|v| v.as_array()) {
            for (oi, opt) in options.iter().enumerate() {
                let label = opt.get("label").and_then(|v| v.as_str()).unwrap_or("");
                let description = opt
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                out.push_str(&format!("   {}. {}", oi + 1, label));
                if !description.is_empty() {
                    out.push_str(&format!(" - {description}"));
                }
                out.push('\n');
            }
        }
    }
    out.push_str("\nReply with the option label(s), or write a short free-form answer.");
    Some(out)
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
            require_approval: false,
            auto_approve_edits: false,
            non_interactive: false,
            session: Arc::new(Mutex::new(SessionState::default())),
            config: crate::config::Config::default(),
            cwd_override: Arc::new(Mutex::new(None)),
            events: None,
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
    async fn accepts_camel_case_multi_select_alias() {
        let out = AskUserQuestion
            .execute(
                json!({
                    "questions": [{
                        "question": "Which options?",
                        "header": "Opts",
                        "options": [
                            {"label": "A", "description": "do A"},
                            {"label": "B", "description": "do B"}
                        ],
                        "multiSelect": "true"
                    }]
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["questions"][0]["multi_select"], true);
    }

    #[tokio::test]
    async fn accepts_top_level_question_and_choice_aliases() {
        let out = AskUserQuestion
            .execute(
                json!({
                    "prompt": "Replace active goal?",
                    "title": "Goal",
                    "choices": [
                        "Replace",
                        {"value": "Keep", "details": "Keep the current goal running"}
                    ],
                    "multiSelect": "false"
                }),
                &ctx(),
            )
            .await
            .unwrap();

        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["questions"][0]["question"], "Replace active goal?");
        assert_eq!(v["questions"][0]["header"], "Goal");
        assert_eq!(v["questions"][0]["options"][0]["label"], "Replace");
        assert_eq!(v["questions"][0]["options"][0]["description"], "Replace");
        assert_eq!(v["questions"][0]["options"][1]["label"], "Keep");
        assert_eq!(
            v["questions"][0]["options"][1]["description"],
            "Keep the current goal running"
        );
        assert_eq!(v["questions"][0]["multi_select"], false);
    }

    #[tokio::test]
    async fn defaults_missing_header_for_single_question_alias() {
        let out = AskUserQuestion
            .execute(
                json!({
                    "question": "Which branch?",
                    "options": ["main", "feature"],
                    "multi_select": false
                }),
                &ctx(),
            )
            .await
            .unwrap();

        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["questions"][0]["header"], "Choice");
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
            .map(|i| {
                json!({
                    "question": format!("q{i}?"),
                    "header": "h",
                    "options": [
                        {"label": "a", "description": "a"},
                        {"label": "b", "description": "b"}
                    ],
                    "multi_select": false
                })
            })
            .collect();
        let err = AskUserQuestion
            .execute(json!({ "questions": qs }), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("1..=4 questions"), "got: {err}");
    }

    #[test]
    fn render_envelope_formats_questions_and_options() {
        let rendered = render_ask_envelope(
            r#"{"kind":"ask_user_question","questions":[{"question":"Which approach?","header":"Choice","options":[{"label":"A","description":"do A"},{"label":"B","description":"do B"}],"multi_select":false}]}"#,
        )
        .unwrap();
        assert!(rendered.contains("[Choice] Which approach?"), "{rendered}");
        assert!(rendered.contains("1. A - do A"), "{rendered}");
        assert!(rendered.contains("2. B - do B"), "{rendered}");
    }
}
