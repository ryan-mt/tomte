use super::*;

#[test]
fn args_accept_subagent_type_camel_alias() {
    let args: DispatchArgs = super::super::parse_args(
        "dispatch_agent",
        json!({
            "subagentType": "code-explorer",
            "prompt": "Inspect the repo"
        }),
    )
    .unwrap();

    assert_eq!(args.subagent_type(), "code-explorer");
    assert_eq!(args.prompt, "Inspect the repo");
}

#[test]
fn args_accept_claude_task_aliases() {
    let args: DispatchArgs = super::super::parse_args(
        "dispatch_agent",
        json!({
            "agent_type": "code-explorer",
            "instructions": "Inspect the repo",
            "description": "repo scan",
            "mode": "plan",
            "model": "sonnet",
            "directory": "."
        }),
    )
    .unwrap();

    assert_eq!(args.subagent_type(), "code-explorer");
    assert_eq!(args.prompt, "Inspect the repo");
    assert_eq!(args.description.as_deref(), Some("repo scan"));
    assert_eq!(args.model.as_deref(), Some("sonnet"));
    assert!(args.requires_plan_mode());
}

#[test]
fn args_default_missing_subagent_to_general_purpose() {
    let args: DispatchArgs = super::super::parse_args(
        "dispatch_agent",
        json!({
            "prompt": "Inspect the repo"
        }),
    )
    .unwrap();

    assert_eq!(args.subagent_type(), DEFAULT_SUBAGENT_TYPE);
}

#[test]
fn args_accept_plan_mode_required_alias() {
    let args: DispatchArgs = super::super::parse_args(
        "dispatch_agent",
        json!({
            "subagentType": "code-explorer",
            "prompt": "Inspect the repo",
            "planModeRequired": "yes"
        }),
    )
    .unwrap();

    assert!(args.plan_mode_required);
}

#[test]
fn child_cwd_accepts_paths_inside_parent() {
    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let args = DispatchArgs {
        subagent_type: None,
        prompt: "inspect".into(),
        description: None,
        model: None,
        cwd: Some("child".into()),
        mode: None,
        plan_mode_required: false,
    };

    assert_eq!(
        args.child_cwd(dir.path()).unwrap(),
        child.canonicalize().unwrap()
    );

    let args = DispatchArgs {
        cwd: Some(child.to_string_lossy().to_string()),
        ..args
    };
    assert_eq!(
        args.child_cwd(dir.path()).unwrap(),
        child.canonicalize().unwrap()
    );
}

#[test]
fn child_cwd_rejects_paths_outside_parent() {
    let parent = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().canonicalize().unwrap();
    let base = DispatchArgs {
        subagent_type: None,
        prompt: "inspect".into(),
        description: None,
        model: None,
        cwd: Some(outside_path.to_string_lossy().to_string()),
        mode: None,
        plan_mode_required: false,
    };

    let err = base.child_cwd(parent.path()).unwrap_err().to_string();
    assert!(err.contains("escapes the parent cwd"), "got: {err}");

    let err = DispatchArgs {
        cwd: Some("..".into()),
        ..base
    }
    .child_cwd(parent.path())
    .unwrap_err()
    .to_string();
    assert!(err.contains("escapes the parent cwd"), "got: {err}");
}

#[test]
fn child_policy_forces_plan_when_parent_would_need_approval() {
    let mut ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);
    ctx.require_approval = true;
    assert!(child_requires_plan_mode(&ctx, false));

    ctx.auto_approve_edits = true;
    assert!(child_requires_plan_mode(&ctx, false));

    ctx.require_approval = false;
    ctx.auto_approve_edits = false;
    assert!(!child_requires_plan_mode(&ctx, false));

    ctx.approval = ApprovalMode::Auto;
    ctx.require_approval = true;
    assert!(!child_requires_plan_mode(&ctx, false));

    assert!(child_requires_plan_mode(&ctx, true));
}
