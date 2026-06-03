//! Agent tests (tool_result_tests), split out of `agent`.

use super::{
    cap_precomputed_outputs, cap_tool_output, execute_builtin_tool_call, AgentEvent,
    TOOL_RESULT_MAX_BYTES,
};
use crate::tools::{ApprovalMode, BuiltinTool, ToolContext};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

struct LargeTool;

#[async_trait]
impl BuiltinTool for LargeTool {
    fn name(&self) -> &'static str {
        "large_tool"
    }

    fn description(&self) -> &'static str {
        "returns large output for tests"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<String> {
        Ok("x".repeat(TOOL_RESULT_MAX_BYTES + 4096))
    }
}

#[test]
fn cap_tool_output_leaves_small_output_unchanged() {
    let (out, capped) = cap_tool_output("small".to_string());

    assert_eq!(out, "small");
    assert!(!capped);
}

#[test]
fn cap_tool_output_truncates_on_utf8_boundary() {
    let input = format!("{}étail", "x".repeat(TOOL_RESULT_MAX_BYTES - 1));
    let (out, capped) = cap_tool_output(input);

    assert!(capped);
    assert!(out.contains("Tool result truncated"), "got: {out}");
    assert!(out.is_char_boundary(out.len()));
    assert!(out.len() < TOOL_RESULT_MAX_BYTES + 512);
}

#[test]
fn cap_precomputed_outputs_preserves_error_flags() {
    let mut items = vec![(
        "call_bad".to_string(),
        "x".repeat(TOOL_RESULT_MAX_BYTES + 4096),
        true,
    )];

    cap_precomputed_outputs(&mut items);

    assert_eq!(items[0].0, "call_bad");
    assert!(items[0].1.contains("Tool result truncated"));
    assert!(items[0].1.len() < TOOL_RESULT_MAX_BYTES + 512);
    assert!(items[0].2);
}

#[tokio::test]
async fn execute_builtin_tool_call_caps_event_and_history_output() {
    let (tx, mut rx) = mpsc::channel(4);
    let ctx = ToolContext::new(std::env::current_dir().unwrap(), ApprovalMode::OnRequest);
    let (_, returned, is_err) = execute_builtin_tool_call(
        "call_large".to_string(),
        json!({}),
        &LargeTool,
        ctx,
        tx,
        Arc::new(crate::hooks::HookSet::default()),
    )
    .await;

    assert!(!is_err);
    assert!(returned.contains("Tool result truncated"));
    assert!(returned.len() < TOOL_RESULT_MAX_BYTES + 512);
    match rx.recv().await.unwrap() {
        AgentEvent::ToolResult { output, error, .. } => {
            assert!(!error);
            assert_eq!(output, returned);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

/// Fails its `parse_args` step so the agent should append a schema hint.
struct SchemaTool;

#[async_trait]
impl BuiltinTool for SchemaTool {
    fn name(&self) -> &'static str {
        "schema_tool"
    }
    fn description(&self) -> &'static str {
        "validates args for tests"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"path": {"type": "string", "description": "Where to look."}},
            "required": ["path"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct A {
            path: String,
        }
        let _: A = crate::tools::parse_args("schema_tool", args)?;
        Ok("ok".to_string())
    }
}

/// Always errors at runtime (after args parse), so no schema hint applies.
struct RuntimeErrTool;

#[async_trait]
impl BuiltinTool for RuntimeErrTool {
    fn name(&self) -> &'static str {
        "runtime_err"
    }
    fn description(&self) -> &'static str {
        "always fails at runtime"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {"x": {"type": "string"}}})
    }
    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<String> {
        Err(anyhow::anyhow!("file not found: /nope"))
    }
}

#[tokio::test]
async fn execute_builtin_tool_call_appends_schema_hint_on_arg_error() {
    // A schema mismatch must surface the original error AND a summary of the
    // tool's expected arguments + a retry nudge — the exact text the model
    // sees on the event channel, so it can self-correct within the turn.
    let (tx, mut rx) = mpsc::channel(4);
    let ctx = ToolContext::new(std::env::current_dir().unwrap(), ApprovalMode::OnRequest);
    let (_, output, is_err) = execute_builtin_tool_call(
        "call_bad".to_string(),
        json!({"path": 5}), // wrong type → ArgSchemaError
        &SchemaTool,
        ctx,
        tx,
        Arc::new(crate::hooks::HookSet::default()),
    )
    .await;

    assert!(is_err);
    assert!(output.contains("argument schema mismatch"), "got: {output}");
    assert!(
        output.contains("Expected arguments for `schema_tool`"),
        "got: {output}"
    );
    assert!(
        output.contains("path (string, required): Where to look."),
        "got: {output}"
    );
    assert!(
        output.contains("Fix the arguments and call `schema_tool` again."),
        "got: {output}"
    );
    match rx.recv().await.unwrap() {
        AgentEvent::ToolResult { output, error, .. } => {
            assert!(error);
            assert!(output.contains("Expected arguments for `schema_tool`"));
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_builtin_tool_call_leaves_runtime_error_unchanged() {
    // A non-argument (runtime) error must pass through verbatim — attaching a
    // schema hint there would be noise and could mislead the model.
    let (tx, _rx) = mpsc::channel(4);
    let ctx = ToolContext::new(std::env::current_dir().unwrap(), ApprovalMode::OnRequest);
    let (_, output, is_err) = execute_builtin_tool_call(
        "call_rt".to_string(),
        json!({}),
        &RuntimeErrTool,
        ctx,
        tx,
        Arc::new(crate::hooks::HookSet::default()),
    )
    .await;

    assert!(is_err);
    assert!(output.contains("file not found"), "got: {output}");
    assert!(
        !output.contains("Expected arguments"),
        "a runtime error must not get a schema hint; got: {output}"
    );
}
