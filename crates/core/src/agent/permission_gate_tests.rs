//! Agent tests (permission_gate_tests), split out of `agent`.

#[cfg(unix)]
use super::post_approval_tool_gate;
use super::{
    approval_outcome, effective_approval_for_tool, effective_tool_read_only,
    instructions_for_approval, preflight_tool_call, project_permission_decision, ApprovalOutcome,
    ToolPreflight,
};
#[cfg(unix)]
use crate::hooks::{HookEntry, HookSet, HooksConfig};
use crate::permissions::{Decision, ProjectPermissions};
use crate::tools::{ApprovalMode, Registry};
use serde_json::json;

#[test]
fn deny_rules_apply_to_read_only_tools() {
    let perms = ProjectPermissions {
        allow: vec![],
        deny: vec!["read_file(.env*)".into()],
    };

    assert_eq!(
        project_permission_decision(
            &perms,
            "read_file",
            &json!({"file_path": ".env.local"}),
            true
        ),
        Decision::Deny
    );
}

#[test]
fn allow_rules_do_not_change_read_only_gating() {
    let perms = ProjectPermissions {
        allow: vec!["read_file(src/**)".into()],
        deny: vec![],
    };

    assert_eq!(
        project_permission_decision(&perms, "read_file", &json!({"path": "src/main.rs"}), true),
        Decision::Ask
    );
}

#[cfg(unix)]
fn sh_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[test]
fn deny_rules_block_before_hook_phase() {
    let perms = ProjectPermissions {
        allow: vec![],
        deny: vec!["read_file(.env*)".into()],
    };

    let outcome = preflight_tool_call(
        &perms,
        "read_file",
        &json!({"file_path": ".env.local"}),
        ApprovalMode::OnRequest,
        true,
    );

    match outcome {
        ToolPreflight::Block(reason) => assert!(reason.contains("deny rule"), "{reason}"),
        ToolPreflight::Proceed { .. } => panic!("expected deny preflight block"),
    }
}

#[test]
fn plan_mode_allows_session_only_todo_write() {
    let perms = ProjectPermissions::default();
    let registry = Registry::standard();
    let todo_write = registry.find("todo_write").expect("todo_write");

    let outcome = preflight_tool_call(
        &perms,
        "todo_write",
        &json!({"todos": []}),
        ApprovalMode::Plan,
        todo_write.is_read_only(),
    );

    assert!(
        matches!(outcome, ToolPreflight::Proceed { .. }),
        "todo_write should remain available in plan mode"
    );
}

#[test]
fn plan_mode_blocks_external_mutating_tools() {
    let perms = ProjectPermissions::default();
    let registry = Registry::standard();
    let run_shell = registry.find("run_shell").expect("run_shell");

    let outcome = preflight_tool_call(
        &perms,
        "run_shell",
        &json!({"command": "cargo test"}),
        ApprovalMode::Plan,
        run_shell.is_read_only(),
    );

    match outcome {
        ToolPreflight::Block(reason) => assert!(reason.contains("plan mode"), "{reason}"),
        ToolPreflight::Proceed { .. } => panic!("run_shell should be blocked in plan mode"),
    }
}

#[test]
fn plan_mode_allows_only_plan_required_dispatch_agent() {
    let perms = ProjectPermissions::default();
    let registry = Registry::standard();
    let dispatch = registry.find("dispatch_agent").expect("dispatch_agent");
    let read_only_args = json!({
        "subagentType": "code-explorer",
        "prompt": "Inspect the repo",
        "planModeRequired": "yes"
    });

    let outcome = preflight_tool_call(
        &perms,
        "dispatch_agent",
        &read_only_args,
        ApprovalMode::Plan,
        effective_tool_read_only("dispatch_agent", &read_only_args, dispatch.is_read_only()),
    );

    assert!(
        matches!(outcome, ToolPreflight::Proceed { .. }),
        "plan-required dispatch_agent should remain available in plan mode"
    );

    let read_only_mode_args = json!({
        "agentType": "code-explorer",
        "instructions": "Inspect the repo",
        "mode": "plan"
    });
    let outcome = preflight_tool_call(
        &perms,
        "dispatch_agent",
        &read_only_mode_args,
        ApprovalMode::Plan,
        effective_tool_read_only(
            "dispatch_agent",
            &read_only_mode_args,
            dispatch.is_read_only(),
        ),
    );

    assert!(
        matches!(outcome, ToolPreflight::Proceed { .. }),
        "mode=plan dispatch_agent should also be treated as read-only"
    );

    let mutating_args = json!({
        "subagent_type": "code-editor",
        "prompt": "Patch the repo"
    });
    let outcome = preflight_tool_call(
        &perms,
        "dispatch_agent",
        &mutating_args,
        ApprovalMode::Plan,
        effective_tool_read_only("dispatch_agent", &mutating_args, dispatch.is_read_only()),
    );

    match outcome {
        ToolPreflight::Block(reason) => assert!(reason.contains("plan mode"), "{reason}"),
        ToolPreflight::Proceed { .. } => {
            panic!("dispatch_agent without plan_mode_required should be blocked in plan mode")
        }
    }
}

#[test]
fn enter_plan_mode_batch_forces_other_tools_through_plan_preflight() {
    assert_eq!(
        effective_approval_for_tool(ApprovalMode::OnRequest, true, "write_file"),
        ApprovalMode::Plan
    );
    assert_eq!(
        effective_approval_for_tool(ApprovalMode::OnRequest, true, "enter_plan_mode"),
        ApprovalMode::OnRequest
    );
    assert_eq!(
        effective_approval_for_tool(ApprovalMode::OnRequest, false, "write_file"),
        ApprovalMode::OnRequest
    );
}

#[test]
fn plan_mode_instructions_include_active_runtime_reminder() {
    let base = "base prompt";
    let plan = instructions_for_approval(base, ApprovalMode::Plan);
    let normal = instructions_for_approval(base, ApprovalMode::OnRequest);

    assert!(plan.contains("Plan mode is currently active"));
    assert!(plan.contains("exit_plan_mode"));
    assert_eq!(normal, base);
}

#[cfg(unix)]
#[tokio::test]
async fn user_denial_skips_pretooluse_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("hook-ran-after-denial");
    let hooks = HookSet {
        config: HooksConfig {
            pre_tool_use: vec![HookEntry {
                matcher: "run_shell".into(),
                command: format!("printf ran > {}", sh_quote(&marker)),
            }],
            ..HooksConfig::default()
        },
    };

    let err = post_approval_tool_gate(
        &hooks,
        "run_shell",
        &json!({"command": "cargo test"}),
        false,
    )
    .await
    .unwrap_err();

    assert_eq!(err, "Error: tool call denied by user");
    assert!(
        !marker.exists(),
        "PreToolUse hook ran even though the user denied approval"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn approved_calls_run_pretooluse_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("hook-ran-after-approval");
    let hooks = HookSet {
        config: HooksConfig {
            pre_tool_use: vec![HookEntry {
                matcher: "run_shell".into(),
                command: format!("printf ran > {}", sh_quote(&marker)),
            }],
            ..HooksConfig::default()
        },
    };

    post_approval_tool_gate(&hooks, "run_shell", &json!({"command": "cargo test"}), true)
        .await
        .unwrap();

    assert!(marker.exists(), "approved call did not run PreToolUse hook");
}

#[test]
fn approval_outcome_non_interactive_fails_closed_even_on_allow_rule() {
    // Headless/unattended: a side-effecting tool (base_gate=true) is denied
    // regardless of a persisted allow rule — an unattended run stays
    // read-only unless the operator passes --dangerously-skip-permissions
    // (which clears require_approval, making base_gate false → AutoRun).
    assert_eq!(
        approval_outcome(true, true, Decision::Allow),
        ApprovalOutcome::Deny
    );
    assert_eq!(
        approval_outcome(true, true, Decision::Ask),
        ApprovalOutcome::Deny
    );
    // Read-only / auto / skip-permissions (base_gate=false) still runs.
    assert_eq!(
        approval_outcome(true, false, Decision::Ask),
        ApprovalOutcome::AutoRun
    );
}

#[test]
fn approval_outcome_interactive_prompts_unless_allowed() {
    // Interactive: a persisted allow rule auto-runs; otherwise prompt the
    // human. A read-only tool (base_gate=false) always auto-runs.
    assert_eq!(
        approval_outcome(false, true, Decision::Allow),
        ApprovalOutcome::AutoRun
    );
    assert_eq!(
        approval_outcome(false, true, Decision::Ask),
        ApprovalOutcome::Prompt
    );
    assert_eq!(
        approval_outcome(false, false, Decision::Ask),
        ApprovalOutcome::AutoRun
    );
}

#[test]
fn non_interactive_block_message_steers_model_to_read_only_tools() {
    // Verified live: gpt-5.5 reads this and retries with `list_dir` instead
    // of dead-ending. Guard the actionable content against regressions.
    let m = super::non_interactive_blocked_message("run_shell");
    assert!(m.contains("run_shell"), "names the blocked tool: {m}");
    for t in ["read_file", "list_dir", "grep", "glob"] {
        assert!(m.contains(t), "should point the model at `{t}`: {m}");
    }
    assert!(
        m.contains("--dangerously-skip-permissions"),
        "tells the operator how to allow side effects: {m}"
    );
}
