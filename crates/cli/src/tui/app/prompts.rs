//! Git/goal prompt builders, todo-completion bookkeeping, markdown export. Split out of `app`; logic unchanged.

use super::*;

/// Shared git safety protocol injected into the `/commit` family so the agent
/// can't freelance into a destructive operation.
pub const GIT_SAFETY_PROTOCOL: &str = "Git safety protocol — follow exactly:\n\
- NEVER `git commit --amend`, `git rebase`, or `git reset --hard` unless the user explicitly asked.\n\
- NEVER pass `--no-verify` (do not skip hooks).\n\
- NEVER force-push, and never push to the `main`/`master` branch directly.\n\
- Do not commit unrelated changes; if the working tree mixes concerns, stage only the relevant files.\n\
- Do not add `git add -A` blindly — review `git status` first and stage deliberately.\n\
- Write the commit message yourself from the actual diff; do not invent changes you didn't make.";

pub fn commit_prompt(extra: &str) -> String {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("\n\nAdditional instructions from the user: {extra}")
    };
    format!(
        "Commit the current changes.\n\
Steps:\n\
1. Run `git status` and `git diff` (use the run_shell tool) to see exactly what changed.\n\
2. Stage the appropriate files.\n\
3. Commit with a concise Conventional-Commits message (e.g. `fix:`, `feat:`, `refactor:`) whose subject summarizes the change and whose body explains the why when non-obvious.\n\
4. Show the resulting `git log -1 --stat` so the user can confirm.\n\n\
{GIT_SAFETY_PROTOCOL}{extra}"
    )
}

pub fn commit_push_pr_prompt(extra: &str) -> String {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("\n\nAdditional instructions from the user: {extra}")
    };
    format!(
        "Commit the current changes, push a branch, and open a pull request.\n\
Steps:\n\
1. Run `git status` and `git diff` to see what changed; determine the current branch.\n\
2. If on `main`/`master`, create a new descriptively-named branch first.\n\
3. Stage the appropriate files and commit with a concise Conventional-Commits message.\n\
4. Push the branch with `git push -u origin <branch>`.\n\
5. Open a PR with `gh pr create` (fill in a clear title and body summarizing the change). If `gh` is not installed, stop after pushing and print the branch name plus the URL the user can use to open the PR manually.\n\n\
{GIT_SAFETY_PROTOCOL}{extra}"
    )
}

pub fn goal_start_prompt(objective: &str) -> String {
    format!(
        "{GOAL_START_PREFIX}\n\n\
Active goal:\n{objective}\n\n\
You are now in /goal mode. Work autonomously until this objective is genuinely complete.\n\n\
Rules:\n\
- Read the relevant codebase/context before changing code.\n\
- Use tools to inspect, edit, and verify. Do not stop at planning or partial work.\n\
- If the goal has 3+ concrete steps, maintain a todo list with todo_write and update it after each meaningful step.\n\
- Before marking complete, run the most relevant tests/build/checks available, or state exactly why a check cannot run.\n\
- Call goal_update with status \"complete\" only when the full objective is done and verified.\n\
- Call goal_update with status \"blocked\" only when no meaningful progress is possible without user input or an external state change.\n\
- If work remains at the end of a turn, call goal_update with status \"in_progress\" and summarize the next concrete action.\n\
- Keep going; the host will continue automatically until complete or blocked."
    )
}

pub fn goal_continuation_prompt(goal: &ActiveGoal) -> String {
    let last = goal
        .last_summary
        .as_deref()
        .unwrap_or("no explicit progress update yet");
    format!(
        "{GOAL_CONTINUATION_PREFIX}\n\n\
Continue the active /goal until it is genuinely complete.\n\n\
Goal:\n{}\n\n\
Last goal_update summary:\n{last}\n\n\
Keep working from the current repository state and conversation context. Do not ask whether to continue. \
Use tools, make the remaining changes, and verify them. Call goal_update with status \"complete\" only after the goal is fully done and verified; call \"blocked\" only if you cannot make meaningful progress without user input or an external state change.",
        goal.objective
    )
}

pub fn remove_pending_goal_continuations(queue: &mut Vec<String>) {
    queue.retain(|item| {
        !item.starts_with(GOAL_START_PREFIX) && !item.starts_with(GOAL_CONTINUATION_PREFIX)
    });
}

pub fn start_goal(app: &mut App, objective: String, replaced: bool) {
    app.pending_goal_replacement = None;
    remove_pending_goal_continuations(&mut app.message_queue);
    app.active_goal = Some(ActiveGoal::new(objective.clone()));
    app.pending_session_save = true;
    app.message_queue.push(goal_start_prompt(&objective));
    let prefix = if replaced {
        "Replaced active goal"
    } else {
        "Started goal"
    };
    app.blocks
        .push(Block::System(format!("{prefix}: {objective}")));
    app.auto_scroll = true;
}

pub fn push_visible_user_block(app: &mut App, text: &str) {
    if text.starts_with(GOAL_START_PREFIX) {
        if let Some(goal) = &app.active_goal {
            app.blocks
                .push(Block::System(format!("Goal running: {}", goal.objective)));
        } else {
            app.blocks.push(Block::System("Goal running.".into()));
        }
    } else if text.starts_with(GOAL_CONTINUATION_PREFIX) {
        if let Some(goal) = &app.active_goal {
            app.blocks.push(Block::System(format!(
                "Continuing goal: {} (turn {})",
                goal.objective,
                goal.turns_completed.saturating_add(1)
            )));
        } else {
            app.blocks.push(Block::System("Continuing goal.".into()));
        }
    } else if text.starts_with(PLAN_APPROVED_PREFIX) {
        app.blocks
            .push(Block::System("Implementing approved plan.".into()));
    } else if text.starts_with(PLAN_REJECTED_PREFIX) {
        app.blocks
            .push(Block::System("Revising rejected plan.".into()));
    } else {
        app.blocks.push(Block::User(text.to_string()));
    }
}

pub fn queue_goal_continuation(app: &mut App) {
    let Some(goal) = app.active_goal.as_ref() else {
        return;
    };
    if goal.waiting_for_user {
        app.status_line = "(goal paused for user input)".into();
        return;
    }
    if !app.message_queue.is_empty() {
        return;
    }
    let prompt = goal_continuation_prompt(goal);
    app.message_queue.push(prompt);
    app.status_line = "(continuing active goal...)".into();
}

pub fn schedule_goal_continuation(app: &mut App) {
    let Some(goal) = app.active_goal.as_mut() else {
        return;
    };
    goal.turns_completed = goal.turns_completed.saturating_add(1);
    app.pending_session_save = true;
    queue_goal_continuation(app);
}

pub fn update_todo_completion_timestamps(app: &mut App, next_todos: &[TodoItem]) {
    let next_all_completed = all_todos_completed(next_todos);
    let previous_completed: HashSet<String> = app
        .session_todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::Completed))
        .map(todo_completion_key)
        .collect();
    let current_completed: HashSet<String> = next_todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::Completed))
        .map(todo_completion_key)
        .collect();
    app.todo_completed_at
        .retain(|key, _| current_completed.contains(key));
    let now = std::time::Instant::now();
    for key in current_completed {
        if !previous_completed.contains(&key)
            || (next_all_completed && !app.todo_completed_at.contains_key(&key))
        {
            app.todo_completed_at.insert(key, now);
        }
    }
}

pub fn all_todos_completed(todos: &[TodoItem]) -> bool {
    !todos.is_empty()
        && todos
            .iter()
            .all(|todo| matches!(todo.status, TodoStatus::Completed))
}

pub fn has_recent_completed_todo(app: &App) -> bool {
    let now = std::time::Instant::now();
    app.session_todos.iter().any(|todo| {
        app.todo_completed_at
            .get(&todo_completion_key(todo))
            .is_some_and(|completed_at| {
                now.duration_since(*completed_at) < TODO_RECENT_COMPLETED_TTL
            })
    })
}

pub fn should_keep_recent_completed_todos_for_empty_snapshot(app: &App) -> bool {
    all_todos_completed(&app.session_todos) && has_recent_completed_todo(app)
}

pub fn prune_expired_completed_todos(app: &mut App) {
    if !all_todos_completed(&app.session_todos) || has_recent_completed_todo(app) {
        return;
    }
    app.session_todos.clear();
    app.todo_completed_at.clear();
}

/// `/usage` report: the active provider's real quota/rate-limit status (5h +
/// weekly for subscriptions, token/request budgets for API keys), captured from
/// the last turn's response. Falls back to a hint when no turn has run yet.
pub fn render_usage_report(app: &App) -> String {
    let current = Provider::from_model(&app.config.model);
    match &app.last_quota {
        Some(snapshot) => {
            // Label the report with the snapshot's OWN provider, not the active
            // model's: after a mid-session `/model` switch the cached quota can
            // belong to a different provider, and labeling it "current" would
            // print one provider's name over another's windows.
            let snap_provider = snapshot.provider.unwrap_or(current);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let mut msg = format!(
                "Usage — provider: {}, model: {}\n",
                snap_provider.display_name(),
                app.config.model
            );
            if snap_provider != current {
                msg.push_str(&format!(
                    "  (last captured {} quota; the active model now uses {} — send a message to refresh)\n",
                    snap_provider.display_name(),
                    current.display_name()
                ));
            }
            msg.push_str(&snapshot.render(now));
            msg
        }
        None => format!(
            "Usage — provider: {}, model: {}\n  No live quota data yet — send a message, then run /usage.\n  \
             (quota is read from the provider's response to your turns)",
            current.display_name(),
            app.config.model
        ),
    }
}

/// Render the visible chat blocks as a portable Markdown transcript for
/// `/export`. Reasoning bodies and tool args/outputs are included so the
/// export captures the same shape the user saw on screen.
pub fn markdown_fence_for(content: &str) -> String {
    let mut current_run = 0usize;
    let mut max_run = 0usize;
    for c in content.chars() {
        if c == '`' {
            current_run += 1;
            max_run = max_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    "`".repeat((max_run + 1).max(3))
}

pub fn push_markdown_fenced(out: &mut String, content: &str, language: Option<&str>) {
    let fence = markdown_fence_for(content);
    out.push_str(&fence);
    if let Some(language) = language {
        out.push_str(language);
    }
    out.push('\n');
    out.push_str(content);
    if !content.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push_str("\n\n");
}

pub fn render_blocks_as_markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    out.push_str("# tomte conversation\n\n");
    for b in blocks {
        match b {
            Block::Welcome => {}
            Block::User(text) => {
                out.push_str("## 🧑 user\n\n");
                out.push_str(text);
                out.push_str("\n\n");
            }
            Block::Assistant {
                text,
                reasoning,
                thought_for_secs,
                ..
            } => {
                out.push_str("## 🤖 assistant\n\n");
                if let Some(secs) = thought_for_secs {
                    out.push_str(&format!("_thought for {secs}s_\n\n"));
                }
                if !reasoning.is_empty() {
                    out.push_str("<details><summary>reasoning</summary>\n\n");
                    push_markdown_fenced(&mut out, reasoning, None);
                    out.push_str("</details>\n\n");
                }
                if !text.is_empty() {
                    out.push_str(text);
                    out.push_str("\n\n");
                }
            }
            Block::Tool {
                name,
                args,
                output,
                error,
                ..
            } => {
                let marker = if *error { "❌" } else { "🔧" };
                out.push_str(&format!("### {marker} tool: `{name}`\n\n"));
                if !args.is_empty() {
                    out.push_str("**args:**\n\n");
                    push_markdown_fenced(&mut out, args, Some("json"));
                }
                if let Some(o) = output {
                    out.push_str("**output:**\n\n");
                    push_markdown_fenced(&mut out, o, None);
                }
            }
            Block::System(s) => {
                out.push_str("> ");
                out.push_str(&s.replace('\n', "\n> "));
                out.push_str("\n\n");
            }
            // Live UI widgets (e.g. `/context`) aren't conversation content.
            Block::Rich(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_fence_outgrows_internal_backtick_runs() {
        // The `/export` fence must be longer than the longest backtick run inside
        // the content, so an embedded code block can't close the export's fence
        // early (the safe-fence invariant) — minimum three backticks.
        assert_eq!(markdown_fence_for("plain text"), "```");
        assert_eq!(markdown_fence_for("use `x` inline"), "```");
        assert_eq!(markdown_fence_for("```rust\ncode\n```"), "````");
        assert_eq!(markdown_fence_for("a ````` b"), "``````");
    }

    #[test]
    fn commit_prompts_carry_the_safety_protocol_and_optional_extra() {
        let bare = commit_prompt("");
        assert!(bare.contains(GIT_SAFETY_PROTOCOL));
        assert!(!bare.contains("Additional instructions"));

        let with_extra = commit_prompt("stage only src/");
        assert!(with_extra.contains("Additional instructions from the user: stage only src/"));

        let pr = commit_push_pr_prompt("");
        assert!(pr.contains(GIT_SAFETY_PROTOCOL));
        assert!(pr.contains("gh pr create"));
    }

    #[test]
    fn all_todos_completed_requires_nonempty_and_every_item_done() {
        use tomte_core::tools::{TodoItem, TodoStatus};
        let item = |status: TodoStatus| TodoItem {
            content: "x".into(),
            status,
            active_form: "x".into(),
            id: None,
            blocked_by: Vec::new(),
        };
        assert!(!all_todos_completed(&[]));
        assert!(!all_todos_completed(&[
            item(TodoStatus::Completed),
            item(TodoStatus::Pending)
        ]));
        assert!(all_todos_completed(&[
            item(TodoStatus::Completed),
            item(TodoStatus::Completed)
        ]));
    }
}
