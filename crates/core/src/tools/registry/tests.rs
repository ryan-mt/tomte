//! Registry tests, split out of `tools`.

use super::super::schema::schema_type_contains;
use super::super::{BuiltinTool, ToolContext};
use super::*;
use anyhow::Result;
use serde_json::Value;

fn names(reg: &Registry) -> Vec<&'static str> {
    reg.tools.iter().map(|t| t.name()).collect()
}

/// Function names actually advertised to the provider (post-deferral).
fn def_names(reg: &Registry) -> Vec<String> {
    reg.definitions()
        .into_iter()
        .filter_map(|t| match t {
            crate::openai::models::Tool::Function(f) => Some(f.name),
            _ => None,
        })
        .collect()
}

/// Minimal fake MCP tool so registry tests don't need a live server.
struct FakeMcp(&'static str, &'static str);
#[async_trait::async_trait]
impl BuiltinTool for FakeMcp {
    fn name(&self) -> &'static str {
        self.0
    }
    fn description(&self) -> &'static str {
        self.1
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
    }
    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<String> {
        Ok(String::new())
    }
}

#[test]
fn enable_tool_search_defers_mcp_until_activated() {
    let mut reg = Registry::standard();
    reg.add(Box::new(FakeMcp(
        "mcp__gh__create_issue",
        "Open a GitHub issue",
    )));
    reg.add(Box::new(FakeMcp(
        "mcp__gh__list_pulls",
        "List pull requests",
    )));
    reg.enable_tool_search();

    let defs = def_names(&reg);
    // Built-ins still advertised; tool_search added; MCP tools withheld.
    assert!(defs.contains(&"read_file".to_string()));
    assert!(defs.contains(&"tool_search".to_string()));
    assert!(!defs.contains(&"mcp__gh__create_issue".to_string()));
    assert!(!defs.contains(&"mcp__gh__list_pulls".to_string()));
    // But they are advertised in the manifest summaries.
    let summary_names: Vec<&str> = reg
        .deferred_summaries()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(summary_names.contains(&"mcp__gh__create_issue"));

    // Activating one (as `tool_search` would) surfaces only that one.
    reg.activated
        .lock()
        .unwrap()
        .insert("mcp__gh__create_issue".to_string());
    let defs = def_names(&reg);
    assert!(defs.contains(&"mcp__gh__create_issue".to_string()));
    assert!(!defs.contains(&"mcp__gh__list_pulls".to_string()));
}

#[test]
fn no_deferral_keeps_all_tools_callable() {
    // Without enable_tool_search, every added MCP tool is directly callable
    // and no tool_search appears.
    let mut reg = Registry::standard();
    reg.add(Box::new(FakeMcp("mcp__x__a", "a")));
    let defs = def_names(&reg);
    assert!(defs.contains(&"mcp__x__a".to_string()));
    assert!(!defs.contains(&"tool_search".to_string()));
    assert!(reg.deferred_summaries().is_empty());
}

#[test]
fn filtered_maps_claude_code_tool_names() {
    // A Claude Code agent whitelist: PascalCase names + `Task`/`Agent`.
    let reg = Registry::filtered(&[
        "Read".into(),
        "Grep".into(),
        "Bash".into(),
        "Task".into(),
        "Agent".into(),
    ]);
    let n = names(&reg);
    assert!(n.contains(&"read_file"));
    assert!(n.contains(&"grep"));
    assert!(n.contains(&"run_shell"));
    // Task/Agent -> dispatch_agent, which is always stripped.
    assert!(!n.contains(&"dispatch_agent"));
    assert_eq!(n.len(), 3);
}

#[test]
fn filtered_skips_unknown_names() {
    let reg = Registry::filtered(&["Read".into(), "TotallyMadeUp".into()]);
    assert_eq!(names(&reg), vec!["read_file"]);
}

#[test]
fn find_accepts_provider_aliases_for_builtin_tools() {
    let reg = Registry::standard();

    let cases = [
        (" Read ", "read_file"),
        ("ReadFile", "read_file"),
        ("Write", "write_file"),
        ("WriteFile", "write_file"),
        ("Edit", "edit_file"),
        ("EditFile", "edit_file"),
        ("MultiEdit", "multi_edit"),
        ("UndoLastEdit", "undo_last_edit"),
        ("ListDir", "list_dir"),
        ("ListDirectory", "list_dir"),
        ("Grep", "grep"),
        ("Glob", "glob"),
        ("Bash", "run_shell"),
        ("PowerShell", "run_shell"),
        ("pwsh", "run_shell"),
        ("RunShell", "run_shell"),
        ("BashOutput", "bash_output"),
        ("KillShell", "kill_shell"),
        ("TodoWrite", "todo_write"),
        ("GoalUpdate", "goal_update"),
        ("UpdateGoal", "goal_update"),
        ("functions.update_goal", "goal_update"),
        ("functions.GoalUpdate", "goal_update"),
        ("EnterPlanMode", "enter_plan_mode"),
        ("functions.EnterPlanMode", "enter_plan_mode"),
        ("ExitPlanMode", "exit_plan_mode"),
        ("functions.ExitPlanMode", "exit_plan_mode"),
        ("tool.BashOutput", "bash_output"),
        ("builtin.Agent", "dispatch_agent"),
        ("WebFetch", "web_fetch"),
        ("WebSearch", "web_search"),
        ("LSP", "lsp"),
        ("LspTool", "lsp"),
        ("NotebookEdit", "notebook_edit"),
        ("LoadSkill", "skill"),
        ("AskUserQuestion", "ask_user_question"),
        ("Agent", "dispatch_agent"),
        ("Task", "dispatch_agent"),
        ("DispatchAgent", "dispatch_agent"),
        ("goalupdate", "goal_update"),
        ("goalstatus", "goal_update"),
        ("enterplanmode", "enter_plan_mode"),
        ("exitplanmode", "exit_plan_mode"),
    ];

    for (alias, canonical) in cases {
        assert_eq!(reg.find(alias).unwrap().name(), canonical, "{alias}");
    }
}

#[test]
fn wildcard_includes_skill_but_not_dispatch() {
    let reg = Registry::filtered(&[]);
    let n = names(&reg);
    assert!(n.contains(&"skill"));
    assert!(!n.contains(&"dispatch_agent"));
    assert!(!n.contains(&"ask_user_question"));
    assert!(!n.contains(&"goal_update"));
    assert!(!n.contains(&"enter_plan_mode"));
    assert!(!n.contains(&"exit_plan_mode"));
}

#[test]
fn filtered_strips_user_prompt_tool_from_subagents() {
    let reg = Registry::filtered(&["ask_user_question".into(), "Read".into()]);
    assert_eq!(names(&reg), vec!["read_file"]);
}

#[test]
fn standard_includes_skill_and_dispatch() {
    let n = names(&Registry::standard());
    assert!(n.contains(&"skill"));
    assert!(n.contains(&"dispatch_agent"));
    assert!(n.contains(&"goal_update"));
    assert!(n.contains(&"enter_plan_mode"));
    assert!(n.contains(&"exit_plan_mode"));
}

#[test]
fn standard_tool_definitions_use_portable_function_names() {
    for def in Registry::standard().definitions() {
        let crate::openai::Tool::Function(f) = def else {
            continue;
        };
        assert!(
            !f.name.is_empty() && f.name.len() <= 64,
            "bad tool name length: {}",
            f.name
        );
        assert!(
            f.name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')),
            "non-portable tool name: {}",
            f.name
        );
        assert!(
            f.parameters.get("type").and_then(|v| v.as_str()) == Some("object"),
            "tool schema root must be object: {}",
            f.name
        );
        assert!(
            f.parameters.get("additionalProperties").is_some(),
            "tool schema must state additionalProperties: {}",
            f.name
        );
        assert_openai_strict_object_schema(&f.parameters, &f.name);
    }
}

#[test]
fn dispatch_schema_keeps_optional_fields_nullable_and_required() {
    let dispatch = Registry::standard()
        .find("dispatch_agent")
        .expect("dispatch_agent")
        .definition();
    let crate::openai::Tool::Function(f) = dispatch else {
        panic!("dispatch_agent must be a function tool");
    };
    let required = f.parameters["required"].as_array().expect("required array");
    for key in [
        "subagent_type",
        "prompt",
        "description",
        "model",
        "cwd",
        "plan_mode_required",
    ] {
        assert!(
            required.iter().any(|item| item == key),
            "dispatch_agent missing required key {key}"
        );
    }
    let props = f.parameters["properties"]
        .as_object()
        .expect("dispatch properties");
    assert_schema_type_contains(&props["cwd"], "null");
    assert_schema_type_contains(&props["model"], "null");
    assert_schema_type_contains(&props["description"], "null");
    assert_schema_type_contains(&props["plan_mode_required"], "null");
    assert_schema_type_contains(&props["plan_mode_required"], "boolean");
}

fn assert_openai_strict_object_schema(schema: &Value, label: &str) {
    if let Some(items) = schema.get("items") {
        assert_openai_strict_object_schema(items, &format!("{label}.items"));
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(values) = schema.get(key).and_then(Value::as_array) {
            for (idx, value) in values.iter().enumerate() {
                assert_openai_strict_object_schema(value, &format!("{label}.{key}[{idx}]"));
            }
        }
    }

    let is_object =
        schema_type_contains(schema.get("type"), "object") || schema.get("properties").is_some();
    if !is_object {
        return;
    }

    assert_eq!(
        schema.get("additionalProperties"),
        Some(&Value::Bool(false)),
        "object schema must disable additional properties: {label}"
    );
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("object schema must contain properties");
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("object schema must contain required");
    for key in properties.keys() {
        assert!(
            required.iter().any(|item| item == key),
            "object schema required must include {label}.{key}"
        );
        assert_openai_strict_object_schema(&properties[key], &format!("{label}.{key}"));
    }
}

fn assert_schema_type_contains(schema: &Value, expected: &str) {
    assert!(
        schema_type_contains(schema.get("type"), expected),
        "schema {schema:?} does not contain type {expected}"
    );
}
