//! Split out of `agent`; logic unchanged.

use super::*;

pub(super) async fn apply_cwd_override(ctx: &mut ToolContext) {
    let next = ctx.cwd_override.lock().await.take();
    let Some(cwd) = next else {
        return;
    };
    ctx.cwd = cwd;
}

/// Control signal from `Agent::apply_stream_event` back to the `run_turn_inner`
/// recv loop, so the per-event handling lives in its own method while the loop
/// keeps the labeled `continue`/`break` and overflow recovery.
pub(super) enum EventFlow {
    Continue,
    Break,
    Failed(String),
    Errored(String),
}

/// Control signal from `Agent::run_tool_phase` back to the `run_turn_inner`
/// `'turn` loop: keep looping to send tool outputs, or finish the turn.
pub(super) enum TurnFlow {
    Continue,
    Done,
}

pub(super) struct PendingCall {
    pub(super) call_id: String,
    pub(super) item_id: String,
    pub(super) name: String,
    pub(super) args: ToolArgsBuffer,
    /// Whether a `ToolCallArgsDone` has already been emitted for this call.
    /// OpenAI sends both `function_call_arguments.done` and
    /// `output_item.done` carrying the full args, so without this guard the
    /// event fires twice (e.g. `chat` text mode prints the `args:` line twice).
    pub(super) args_done_emitted: bool,
}

pub(super) async fn skip_incomplete_tool_calls_after_truncation(
    pending_calls: &mut Vec<PendingCall>,
    tx: &mpsc::Sender<AgentEvent>,
    error: &str,
) {
    let mut idx = 0;
    while idx < pending_calls.len() {
        if pending_calls[idx].args_done_emitted {
            idx += 1;
            continue;
        }
        let pc = pending_calls.remove(idx);
        tracing::warn!(
            call_id = %pc.call_id,
            name = %pc.name,
            "dropping incomplete tool call after stream truncation"
        );
        let _ = tx
            .send(AgentEvent::ToolResult {
                call_id: pc.call_id,
                output: format!(
                    "Error: stream ended before tool `{}` finished sending arguments; skipped this incomplete tool call instead of executing partial input. Upstream error: {error}",
                    display_tool_name(&pc.name)
                ),
                error: true,
            })
            .await;
    }
}

pub(super) type RunnableToolCall<'a> = (String, Value, &'a dyn crate::tools::BuiltinTool);

pub(super) async fn execute_parallel_tool_batch(
    batch: Vec<RunnableToolCall<'_>>,
    ctx: ToolContext,
    tx: mpsc::Sender<AgentEvent>,
    hooks: Arc<crate::hooks::HookSet>,
) -> Vec<(String, String, bool)> {
    let mut results = Vec::with_capacity(batch.len());
    let mut iter = batch.into_iter();
    loop {
        let chunk: Vec<_> = iter.by_ref().take(MAX_PARALLEL_TOOL_CALLS).collect();
        if chunk.is_empty() {
            break;
        }
        let futures = chunk.into_iter().map(|(call_id, args, tool)| {
            execute_builtin_tool_call(call_id, args, tool, ctx.clone(), tx.clone(), hooks.clone())
        });
        results.extend(futures::future::join_all(futures).await);
    }
    results
}

pub(super) async fn execute_builtin_tool_call(
    call_id: String,
    args: Value,
    tool: &dyn crate::tools::BuiltinTool,
    ctx: ToolContext,
    tx: mpsc::Sender<AgentEvent>,
    hooks: Arc<crate::hooks::HookSet>,
) -> (String, String, bool) {
    let started = std::time::Instant::now();
    let tool_name = tool.name().to_string();
    tracing::info!(
        tool = %tool_name,
        call_id = %call_id,
        "tool.start"
    );
    let post_args = args.clone();
    let timeout = tool.timeout(&args);
    let res = tokio::time::timeout(timeout, tool.execute(args, &ctx)).await;
    let (output, is_err) = match res {
        Ok(Ok(s)) => (s, false),
        Ok(Err(e)) => {
            // An argument-schema mismatch gets a compact summary of the tool's
            // expected arguments appended, so the model can fix the shape and
            // retry within the same turn instead of guessing. Runtime errors
            // (e.g. "file not found") are passed through unchanged.
            let msg = if e.downcast_ref::<crate::tools::ArgSchemaError>().is_some() {
                let hint = crate::tools::schema_hint(&tool_name, &tool.parameters_schema());
                if hint.is_empty() {
                    format!("Error: {e}")
                } else {
                    format!("Error: {e}\n{hint}\nFix the arguments and call `{tool_name}` again.")
                }
            } else {
                format!("Error: {e}")
            };
            (msg, true)
        }
        Err(_) => (
            format!(
                "Error: tool `{tool_name}` exceeded the {}s hard timeout and was aborted",
                timeout.as_secs()
            ),
            true,
        ),
    };
    let (output, was_capped) = cap_tool_output(output);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if is_err {
        tracing::warn!(
            tool = %tool_name,
            call_id = %call_id,
            elapsed_ms,
            bytes = output.len(),
            was_capped,
            "tool.error"
        );
    } else {
        tracing::info!(
            tool = %tool_name,
            call_id = %call_id,
            elapsed_ms,
            bytes = output.len(),
            was_capped,
            "tool.ok"
        );
    }
    let _ = tx
        .send(AgentEvent::ToolResult {
            call_id: call_id.clone(),
            output: output.clone(),
            error: is_err,
        })
        .await;
    // Best-effort PostToolUse hook. We do not propagate failures from here —
    // the call already happened.
    hooks
        .fire_post(&tool_name, &post_args, &output, is_err)
        .await;
    (call_id, output, is_err)
}

pub(super) fn cap_tool_output(output: String) -> (String, bool) {
    if output.len() <= TOOL_RESULT_MAX_BYTES {
        return (output, false);
    }
    let mut cut = TOOL_RESULT_MAX_BYTES;
    while cut > 0 && !output.is_char_boundary(cut) {
        cut -= 1;
    }
    let omitted = output.len().saturating_sub(cut);
    let body = &output[..cut];
    (
        format!(
            "<system-reminder>Tool result truncated: showing the first {cut} byte(s), omitted {omitted} byte(s). Re-run the tool with narrower arguments, offsets, limits, or redirects if you need the omitted content.</system-reminder>\n{body}"
        ),
        true,
    )
}

pub(super) fn cap_precomputed_outputs(precomputed: &mut [(String, String, bool)]) {
    for (_, output, _) in precomputed {
        *output = cap_tool_output(std::mem::take(output)).0;
    }
}

/// Message for a side-effecting tool blocked in a non-interactive (read-only)
/// run. Steers the *model* to a read-only tool it can actually run — the common
/// case is it reached for `run_shell` on a task `read_file`/`grep`/`glob` can do
/// — and tells the *operator* how to allow side effects, so a headless run
/// recovers a read-only goal instead of dead-ending on "denied". Verified live:
/// `gpt-5.5` retries with `list_dir` after reading this.
pub(super) fn non_interactive_blocked_message(tool_name: &str) -> String {
    format!(
        "Error: `{tool_name}` is a side-effecting tool and is blocked in this non-interactive (read-only) run. \
If your goal is read-only, use a read-only tool instead — `read_file`, `list_dir`, `grep`, or `glob`. \
(To allow side-effecting tools, the operator must re-run with `--dangerously-skip-permissions`.)"
    )
}

/// `"\n<schema hint>"` for a tool the registry can resolve, else `""`. Appended
/// to an argument-level error raised before dispatch (invalid JSON, non-object
/// args) so the model sees the tool's expected shape next to the parse failure.
pub(super) fn schema_hint_suffix(registry: &crate::tools::Registry, tool_name: &str) -> String {
    registry
        .find(tool_name)
        .map(|t| crate::tools::schema_hint(t.name(), &t.parameters_schema()))
        .filter(|h| !h.is_empty())
        .map(|h| format!("\n{h}"))
        .unwrap_or_default()
}

pub(super) fn is_parallel_safe_tool_call(
    tool: &dyn crate::tools::BuiltinTool,
    args: &Value,
) -> bool {
    let name = tool.name();
    let is_effectively_read_only = effective_tool_read_only(name, args, tool.is_read_only());
    is_parallel_safe_tool_name(name, is_effectively_read_only)
        || (name == "dispatch_agent" && is_effectively_read_only)
}

pub(super) fn is_parallel_safe_tool_name(name: &str, is_read_only: bool) -> bool {
    is_read_only
        && matches!(
            name,
            "read_file"
                | "list_dir"
                | "grep"
                | "glob"
                | "web_fetch"
                | "web_search"
                | "skill"
                | "lsp"
        )
}

pub(super) fn effective_tool_read_only(name: &str, args: &Value, declared_read_only: bool) -> bool {
    declared_read_only || plan_required_dispatch_args(name, args)
}

pub(super) fn plan_required_dispatch_args(name: &str, args: &Value) -> bool {
    if name != "dispatch_agent" {
        return false;
    }
    let Some(obj) = args.as_object() else {
        return false;
    };
    first_value(
        obj,
        &[
            "plan_mode_required",
            "planModeRequired",
            "plan_required",
            "planRequired",
        ],
    )
    .and_then(normalized_bool)
    .or_else(|| {
        first_value(
            obj,
            &["mode", "permission_mode", "permissionMode", "spawnMode"],
        )
        .and_then(normalized_dispatch_plan_mode)
    })
    .and_then(|v| v.as_bool())
    .unwrap_or(false)
}

pub(super) async fn request_tool_approval(
    pending_approvals: &Arc<
        Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    >,
    tx: &mpsc::Sender<AgentEvent>,
    call_id: &str,
    tool_name: &str,
    args_json: String,
    diff_preview: Option<String>,
    timeout: Duration,
) -> bool {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<bool>();
    pending_approvals
        .lock()
        .await
        .insert(call_id.to_string(), resp_tx);
    if tx
        .send(AgentEvent::ApprovalRequest {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            args_json,
            diff_preview,
        })
        .await
        .is_err()
    {
        pending_approvals.lock().await.remove(call_id);
        return false;
    }

    let granted = match tokio::time::timeout(timeout, resp_rx).await {
        Ok(Ok(g)) => g,
        _ => false,
    };
    pending_approvals.lock().await.remove(call_id);
    let _ = tx
        .send(if granted {
            AgentEvent::ApprovalGranted {
                call_id: call_id.to_string(),
            }
        } else {
            AgentEvent::ApprovalDenied {
                call_id: call_id.to_string(),
            }
        })
        .await;
    granted
}

pub(super) enum ToolPreflight {
    Block(String),
    Proceed {
        decision: crate::permissions::Decision,
    },
}

pub(super) fn effective_approval_for_tool(
    current: ApprovalMode,
    batch_enters_plan_mode: bool,
    tool_name: &str,
) -> ApprovalMode {
    if batch_enters_plan_mode && tool_name != "enter_plan_mode" {
        ApprovalMode::Plan
    } else {
        current
    }
}

pub(super) fn preflight_tool_call(
    perms: &crate::permissions::ProjectPermissions,
    tool_name: &str,
    args: &Value,
    approval: ApprovalMode,
    is_read_only: bool,
    cwd: &std::path::Path,
) -> ToolPreflight {
    // Plan mode is read-only for external side effects: file writes, shell, and
    // non-plan sub-agent dispatches are rejected up-front so the model can
    // adjust its plan instead of attempting the call and producing a confusing
    // failure.
    if approval == ApprovalMode::Plan && !is_read_only {
        return ToolPreflight::Block(format!(
            "Error: tool `{tool_name}` is blocked in plan mode (read-only). Switch out of plan mode to execute writes/shell."
        ));
    }

    // Deny rules are hard stops and must run before PreToolUse hooks. Hooks are
    // shell commands and may have side effects; a denied tool call should not
    // execute any user-configured hook first.
    let decision = project_permission_decision(perms, tool_name, args, is_read_only);
    if matches!(decision, crate::permissions::Decision::Deny) {
        return ToolPreflight::Block(format!(
            "Error: `{tool_name}` is blocked by a deny rule in .opencli/permissions.json"
        ));
    }

    // The raw decision above matches the spelled path. Re-check `deny` rules
    // against the symlink-resolved real path so an in-repo symlink (or a
    // case-variant on a case-insensitive FS) can't launder a denied target.
    if crate::permissions::deny_matches_resolved(perms, tool_name, args, cwd) {
        return ToolPreflight::Block(format!(
            "Error: `{tool_name}` is blocked by a deny rule in .opencli/permissions.json (the path resolves to a denied target)"
        ));
    }

    ToolPreflight::Proceed { decision }
}

/// What the approval gate decides for one tool call, given the precomputed
/// `base_gate` (true when the tool is side-effecting and the session would
/// normally prompt) and the project-permission `decision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApprovalOutcome {
    /// Run without prompting (read-only/auto tool, or an interactive allow rule).
    AutoRun,
    /// Refuse without running. Only happens in a non-interactive run, where no
    /// human can approve and a persisted allow rule is not an unattended grant.
    Deny,
    /// Ask the human (interactive sessions only).
    Prompt,
}

/// Decide a tool call's fate at the approval gate. Pure so it is unit-testable.
///
/// In a non-interactive run (`opencli chat`/`run` without
/// `--dangerously-skip-permissions`) any side-effecting tool fails closed —
/// even when a persisted `allow` rule matches. The allow store is populated by
/// interactive "allow in this project" choices; honoring it with no human
/// present is a separate trust decision, so an unattended run stays read-only
/// unless the operator explicitly opts in (which clears `require_approval`, so
/// `base_gate` is false here and the tool runs).
pub(super) fn approval_outcome(
    non_interactive: bool,
    base_gate: bool,
    decision: crate::permissions::Decision,
    dangerous: bool,
) -> ApprovalOutcome {
    // A classifier-flagged destructive command must never auto-run: a human has
    // to see and approve THIS exact command (interactive), or it is refused
    // (headless) — even under a persisted allow rule, Auto mode, or a bypass.
    // Without this, a `run_shell(<prog>:*)` grant silently auto-runs a
    // destructive command (force-push, `rm -rf`) the user never saw.
    if dangerous {
        return if non_interactive {
            ApprovalOutcome::Deny
        } else {
            ApprovalOutcome::Prompt
        };
    }
    if !base_gate {
        ApprovalOutcome::AutoRun
    } else if non_interactive {
        ApprovalOutcome::Deny
    } else if matches!(decision, crate::permissions::Decision::Allow) {
        ApprovalOutcome::AutoRun
    } else {
        ApprovalOutcome::Prompt
    }
}

pub(super) async fn post_approval_tool_gate(
    hooks: &crate::hooks::HookSet,
    tool_name: &str,
    args: &Value,
    approved: bool,
) -> std::result::Result<(), String> {
    if !approved {
        return Err("Error: tool call denied by user".to_string());
    }

    // PreToolUse hooks (from settings.json) can block a tool call by exiting 2.
    // We surface the hook's stdout as the error so the model sees a useful
    // reason. Hooks run only after user approval has been granted (or no prompt
    // was needed), so a denied approval cannot trigger hook side effects.
    match hooks.fire_pre(tool_name, args).await {
        crate::hooks::HookDecision::Block(reason) => {
            Err(format!("Error: blocked by PreToolUse hook: {reason}"))
        }
        crate::hooks::HookDecision::Allow => Ok(()),
    }
}

pub(super) fn project_permission_decision(
    perms: &crate::permissions::ProjectPermissions,
    tool_name: &str,
    args: &Value,
    is_read_only: bool,
) -> crate::permissions::Decision {
    let decision = crate::permissions::decide(perms, tool_name, args);
    if is_read_only && matches!(decision, crate::permissions::Decision::Allow) {
        crate::permissions::Decision::Ask
    } else {
        decision
    }
}
