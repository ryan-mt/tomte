//! Integration tests for `/rewind`: `Agent::rewind_to` must revert the file
//! edits made since a checkpoint (newest-first, skipping externally-changed
//! files), truncate the conversation back to it, and report what it did.

use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;
use tomte_core::agent::Agent;
use tomte_core::client::LlmClient;
use tomte_core::config::{Config, ProviderConfig};
use tomte_core::openai::{InputItem, MessageContent};
use tomte_core::tools::{
    fs::{EditFile, WriteFile},
    ApprovalMode, BuiltinTool, ToolContext,
};

/// An `Agent` wired to an offline `local/` provider (no network until a request,
/// which these tests never make) and rooted at `cwd`.
async fn test_agent(cwd: &Path) -> Agent {
    let config = Config {
        model: "local/test-model".to_string(),
        providers: HashMap::from([(
            "local".to_string(),
            ProviderConfig {
                base_url: "http://localhost/v1".to_string(),
                api_key: Some("sk-test".to_string()),
                api_key_env: None,
                context_limit: None,
                forward_reasoning_effort: false,
            },
        )]),
        ..Config::default()
    };
    let client = LlmClient::for_config(&config).await.unwrap();
    let mut agent = Agent::new(client, config);
    agent.cwd = cwd.to_path_buf();
    agent
}

/// A `ToolContext` whose session is the agent's, so edits push onto the same undo
/// stack `rewind_to` reverts.
fn ctx_for(agent: &Agent) -> ToolContext {
    ToolContext {
        cwd: agent.cwd.clone(),
        approval: ApprovalMode::Auto,
        require_approval: false,
        auto_approve_edits: false,
        non_interactive: false,
        session: agent.session.clone(),
        config: Config::default(),
        cwd_override: Arc::new(Mutex::new(None)),
        events: None,
    }
}

async fn mark_read(ctx: &ToolContext, rel: &str) {
    let p = std::fs::canonicalize(ctx.cwd.join(rel)).expect("file exists to mark read");
    let mut session = ctx.session.lock().await;
    session.read_files.insert(p.clone());
    session.fully_read_files.insert(p);
}

async fn write_file(ctx: &ToolContext, rel: &str, content: &str) {
    WriteFile
        .execute(json!({ "path": rel, "content": content }), ctx)
        .await
        .unwrap();
}

async fn edit_file(ctx: &ToolContext, rel: &str, old: &str, new: &str) {
    EditFile
        .execute(
            json!({ "path": rel, "old_string": old, "new_string": new, "replace_all": false }),
            ctx,
        )
        .await
        .unwrap();
}

fn read(cwd: &Path, rel: &str) -> String {
    std::fs::read_to_string(cwd.join(rel)).unwrap()
}

fn assistant(text: &str) -> InputItem {
    InputItem::Message {
        role: "assistant".to_string(),
        content: vec![MessageContent::text(text)],
    }
}

async fn force_mtime_change(path: &Path, expected: Option<SystemTime>) {
    for i in 0..20 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        tokio::fs::write(path, format!("manual change {i}"))
            .await
            .unwrap();
        if std::fs::metadata(path).and_then(|m| m.modified()).ok() != expected {
            return;
        }
    }
    panic!("mtime did not change after repeated writes");
}

#[tokio::test]
async fn rewind_reverts_edits_and_truncates_history_across_turns() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    let mut agent = test_agent(cwd).await;
    let ctx = ctx_for(&agent);

    tokio::fs::write(cwd.join("a.txt"), b"a0").await.unwrap();
    tokio::fs::write(cwd.join("b.txt"), b"b0").await.unwrap();
    mark_read(&ctx, "a.txt").await;
    mark_read(&ctx, "b.txt").await;

    // Turn 1: edits a.txt and b.txt.
    agent.record_checkpoint("first turn").await;
    agent.push_user_message("first turn");
    write_file(&ctx, "a.txt", "a1").await;
    write_file(&ctx, "b.txt", "b1").await;
    agent.history.push(assistant("did turn 1"));

    // Turn 2: edits a.txt again.
    agent.record_checkpoint("second turn").await;
    agent.push_user_message("second turn");
    write_file(&ctx, "a.txt", "a2").await;
    agent.history.push(assistant("did turn 2"));

    assert_eq!(agent.checkpoints.len(), 2);
    let history_after_turn1 = agent.checkpoints[1].history_index; // = [user1, assistant1].len()

    // Rewind to the start of turn 2: only a.txt's turn-2 edit is reverted.
    let out = agent.rewind_to(1).await.unwrap();
    assert_eq!(out.files_reverted, 1, "only the turn-2 edit reverts");
    assert_eq!(out.turns_dropped, 1);
    assert_eq!(read(cwd, "a.txt"), "a1", "a.txt back to its turn-1 value");
    assert_eq!(
        read(cwd, "b.txt"),
        "b1",
        "b.txt untouched by a turn-2 rewind"
    );
    assert_eq!(agent.history.len(), history_after_turn1);
    assert_eq!(agent.checkpoints.len(), 1);

    // Rewind to the start of turn 1: every edit reverts, conversation empties.
    let out0 = agent.rewind_to(0).await.unwrap();
    assert_eq!(out0.files_reverted, 2);
    assert_eq!(read(cwd, "a.txt"), "a0");
    assert_eq!(read(cwd, "b.txt"), "b0");
    assert!(
        agent.history.is_empty(),
        "history truncated to before turn 1"
    );
    assert!(agent.checkpoints.is_empty());
    assert_eq!(agent.session.lock().await.undo_stack.len(), 0);
}

#[tokio::test]
async fn rewind_unwinds_stacked_same_file_edits_to_pre_checkpoint() {
    // Several edits to ONE file in a turn must revert to the file's pre-turn
    // content — newest-first restore, not an intermediate version.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    let mut agent = test_agent(cwd).await;
    let ctx = ctx_for(&agent);

    tokio::fs::write(cwd.join("c.txt"), b"v0").await.unwrap();
    mark_read(&ctx, "c.txt").await;

    agent.record_checkpoint("turn").await;
    agent.push_user_message("turn");
    edit_file(&ctx, "c.txt", "v0", "v1").await;
    edit_file(&ctx, "c.txt", "v1", "v2").await;
    assert_eq!(read(cwd, "c.txt"), "v2");

    let out = agent.rewind_to(0).await.unwrap();
    // One FILE reverted (both stacked edits collapse to a single restore of the
    // pre-checkpoint content), not one-per-edit.
    assert_eq!(out.files_reverted, 1);
    assert_eq!(
        read(cwd, "c.txt"),
        "v0",
        "back to the pre-checkpoint content"
    );
}

#[tokio::test]
async fn rewind_skips_a_file_changed_outside_tomte() {
    // A file the user edited in their own editor between turns must be reported
    // and left as-is, never clobbered back to tomte's version.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    let mut agent = test_agent(cwd).await;
    let ctx = ctx_for(&agent);

    tokio::fs::write(cwd.join("e.txt"), b"orig").await.unwrap();
    mark_read(&ctx, "e.txt").await;

    agent.record_checkpoint("turn").await;
    agent.push_user_message("turn");
    edit_file(&ctx, "e.txt", "orig", "edited").await;

    let expected = agent
        .session
        .lock()
        .await
        .undo_stack
        .back()
        .unwrap()
        .post_edit_mtime;
    force_mtime_change(&cwd.join("e.txt"), expected).await;

    let out = agent.rewind_to(0).await.unwrap();
    assert_eq!(out.files_reverted, 0);
    assert_eq!(out.files_skipped.len(), 1, "the modified file is reported");
    assert!(
        read(cwd, "e.txt").starts_with("manual change"),
        "the user's manual change must survive the rewind"
    );
    // The skipped entry is consumed, so a repeat rewind doesn't re-attempt it.
    assert_eq!(agent.session.lock().await.undo_stack.len(), 0);
}

#[tokio::test]
async fn rewind_counts_shell_effects_it_cannot_undo() {
    // `run_shell` side effects in the dropped turns can't be reverted; the
    // outcome counts them (and only them) so the summary can be honest.
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = test_agent(tmp.path()).await;

    agent.record_checkpoint("turn").await;
    agent.push_user_message("turn");
    agent.history.push(InputItem::FunctionCall {
        call_id: "c1".into(),
        name: "run_shell".into(),
        arguments: "{}".into(),
    });
    agent.history.push(InputItem::FunctionCall {
        call_id: "c2".into(),
        name: "read_file".into(),
        arguments: "{}".into(),
    });
    agent.history.push(InputItem::FunctionCall {
        call_id: "c3".into(),
        name: "run_shell".into(),
        arguments: "{}".into(),
    });

    let out = agent.rewind_to(0).await.unwrap();
    assert_eq!(
        out.shell_effects, 2,
        "two run_shell calls, the read isn't one"
    );
    assert!(agent.history.is_empty());
}

#[tokio::test]
async fn rewind_to_an_invalid_ordinal_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = test_agent(tmp.path()).await;
    let err = agent.rewind_to(0).await.unwrap_err();
    assert!(
        err.to_string().contains("no such rewind point"),
        "got: {err}"
    );
}
