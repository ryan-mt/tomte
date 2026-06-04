//! Function-call-id tests (fcid_c), split out of `agent`.

use super::*;

#[test]
fn history_tool_arguments_canonicalize_common_camel_case_aliases() {
    let write_raw = history_tool_arguments(
        "write_file",
        &json!({
            "filePath": "src/lib.rs",
            "contents": "hello"
        }),
    );
    let write_value: Value = serde_json::from_str(&write_raw).unwrap();
    assert_eq!(write_value["path"], "src/lib.rs");
    assert_eq!(write_value["content"], "hello");
    assert!(write_value.get("filePath").is_none());
    assert!(write_value.get("contents").is_none());

    let edit_raw = history_tool_arguments(
        "edit_file",
        &json!({
            "filePath": "src/lib.rs",
            "oldText": "old",
            "newText": "new",
            "replaceAll": "false"
        }),
    );
    let edit_value: Value = serde_json::from_str(&edit_raw).unwrap();
    assert_eq!(edit_value["path"], "src/lib.rs");
    assert_eq!(edit_value["old_string"], "old");
    assert_eq!(edit_value["new_string"], "new");
    assert_eq!(edit_value["replace_all"], false);
    assert!(edit_value.get("oldText").is_none());
    assert!(edit_value.get("newText").is_none());

    let file_raw = history_tool_arguments(
        "multi_edit",
        &json!({
            "filePath": "src/lib.rs",
            "edits": [{
                "old_text": "old",
                "new_text": "new",
                "replaceAll": "true"
            }]
        }),
    );
    let file_value: Value = serde_json::from_str(&file_raw).unwrap();
    assert_eq!(file_value["path"], "src/lib.rs");
    assert_eq!(file_value["edits"][0]["old_string"], "old");
    assert_eq!(file_value["edits"][0]["new_string"], "new");
    assert_eq!(file_value["edits"][0]["replace_all"], true);
    assert!(file_value.get("filePath").is_none());
    assert!(file_value["edits"][0].get("old_text").is_none());
    assert!(file_value["edits"][0].get("new_text").is_none());

    let shell_raw = history_tool_arguments(
        "run_shell",
        &json!({
            "command": "cargo test",
            "timeoutMs": "1000",
            "runInBackground": "false",
            "dangerousOverride": "false"
        }),
    );
    let shell_value: Value = serde_json::from_str(&shell_raw).unwrap();
    assert_eq!(shell_value["timeout_ms"], 1000);
    assert_eq!(shell_value["run_in_background"], false);
    assert_eq!(shell_value["dangerous_override"], false);

    let list_raw = history_tool_arguments(
        "list_dir",
        &json!({
            "directory": "src"
        }),
    );
    let list_value: Value = serde_json::from_str(&list_raw).unwrap();
    assert_eq!(list_value["path"], "src");
    assert!(list_value.get("directory").is_none());

    let grep_raw = history_tool_arguments(
        "grep",
        &json!({
            "pattern": "needle",
            "caseInsensitive": "yes",
            "outputMode": "paths",
            "headLimit": "5",
            "offset": "2",
            "contextLines": 2,
            "multiLine": "true",
            "fileType": "rust"
        }),
    );
    let grep_value: Value = serde_json::from_str(&grep_raw).unwrap();
    assert_eq!(grep_value["case_insensitive"], true);
    assert_eq!(grep_value["output_mode"], "files_with_matches");
    assert_eq!(grep_value["head_limit"], 5);
    assert_eq!(grep_value["offset"], 2);
    assert_eq!(grep_value["context_after"], 2);
    assert_eq!(grep_value["context_before"], 2);
    assert_eq!(grep_value["multiline"], true);
    assert_eq!(grep_value["file_type"], "rust");

    let web_raw = history_tool_arguments(
        "web_search",
        &json!({
            "query": "rust",
            "maxResults": "3",
            "allowedDomains": "doc.rust-lang.org crates.io",
            "blockedDomains": ["ads.example"]
        }),
    );
    let web_value: Value = serde_json::from_str(&web_raw).unwrap();
    assert_eq!(web_value["max_results"], 3);
    assert_eq!(
        web_value["allowed_domains"],
        json!(["doc.rust-lang.org", "crates.io"])
    );
    assert_eq!(web_value["blocked_domains"], json!(["ads.example"]));

    let notebook_raw = history_tool_arguments(
        "notebook_edit",
        &json!({
            "notebookPath": "nb.ipynb",
            "newSource": "print(42)\n",
            "cellId": "aaa",
            "cellType": null,
            "editMode": "replace"
        }),
    );
    let notebook_value: Value = serde_json::from_str(&notebook_raw).unwrap();
    assert_eq!(notebook_value["notebook_path"], "nb.ipynb");
    assert_eq!(notebook_value["new_source"], "print(42)\n");
    assert_eq!(notebook_value["cell_id"], "aaa");
    assert_eq!(notebook_value["edit_mode"], "replace");

    let dispatch_raw = history_tool_arguments(
        "dispatch_agent",
        &json!({
            "agentType": "code-explorer",
            "instructions": "Inspect the repo",
            "description": "repo scan",
            "model": "sonnet",
            "workingDir": "src",
            "mode": "plan"
        }),
    );
    let dispatch_value: Value = serde_json::from_str(&dispatch_raw).unwrap();
    assert_eq!(dispatch_value["subagent_type"], "code-explorer");
    assert_eq!(dispatch_value["prompt"], "Inspect the repo");
    assert_eq!(dispatch_value["description"], "repo scan");
    assert_eq!(dispatch_value["model"], "sonnet");
    assert_eq!(dispatch_value["cwd"], "src");
    assert_eq!(dispatch_value["plan_mode_required"], true);

    let ask_raw = history_tool_arguments(
        "ask_user_question",
        &json!({
            "questions": [{
                "question": "Pick?",
                "header": "Choice",
                "options": [
                    {"label": "A", "description": "Do A"},
                    {"label": "B", "description": "Do B"}
                ],
                "multiSelect": "true"
            }]
        }),
    );
    let ask_value: Value = serde_json::from_str(&ask_raw).unwrap();
    assert_eq!(ask_value["questions"][0]["multi_select"], true);

    let goal_raw = history_tool_arguments(
        "goal_update",
        &json!({
            "goalStatus": "completed",
            "message": "checks passed"
        }),
    );
    let goal_value: Value = serde_json::from_str(&goal_raw).unwrap();
    assert_eq!(goal_value["status"], "complete");
    assert_eq!(goal_value["summary"], "checks passed");

    let enter_plan_raw = history_tool_arguments(
        "enter_plan_mode",
        &json!({
            "reason": "inspect first",
            "unexpected": true
        }),
    );
    let enter_plan_value: Value = serde_json::from_str(&enter_plan_raw).unwrap();
    assert_eq!(enter_plan_value, json!({}));

    let plan_raw = history_tool_arguments(
        "exit_plan_mode",
        &json!({
            "proposal": "1. Patch\n2. Test"
        }),
    );
    let plan_value: Value = serde_json::from_str(&plan_raw).unwrap();
    assert_eq!(plan_value["plan"], "1. Patch\n2. Test");
}

#[test]
fn approval_args_json_uses_parsed_tool_arguments() {
    let raw = approval_args_json(&json!({
        "command": "cargo test",
        "timeout": "1000"
    }));
    let value: Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value["command"], "cargo test");
    assert_eq!(value["timeout"], "1000");
}

#[test]
fn approval_args_json_serializes_empty_arguments_as_object() {
    assert_eq!(approval_args_json(&json!({})), "{}");
}

#[test]
fn history_tool_arguments_canonicalize_claude_style_grep_args() {
    let raw = history_tool_arguments(
        "grep",
        &json!({
            "pattern": "needle",
            "-i": "true",
            "-C": 2,
            "type": "rust"
        }),
    );
    let value: Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value["pattern"], "needle");
    assert_eq!(value["case_insensitive"], true);
    assert_eq!(value["context_after"], 2);
    assert_eq!(value["context_before"], 2);
    assert_eq!(value["file_type"], "rust");
    assert!(value.get("-C").is_none());
    assert!(value.get("type").is_none());
}

#[test]
fn history_tool_arguments_prefers_claude_active_form_spelling() {
    let raw = history_tool_arguments(
        "todo_write",
        &json!({
            "todos": [{
                "content": "Implement feature",
                "status": "in_progress",
                "active_form": "Implementing feature"
            }]
        }),
    );
    let value: Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value["todos"][0]["activeForm"], "Implementing feature");
    assert_eq!(value["todos"][0]["status"], "in_progress");
    assert!(value["todos"][0].get("active_form").is_none());
}

#[test]
fn history_tool_arguments_canonicalize_todo_status_aliases() {
    let raw = history_tool_arguments(
        "todo_write",
        &json!({
            "todos": [
                {
                    "content": "Read code",
                    "status": "done",
                    "activeForm": "Reading code"
                },
                {
                    "content": "Run tests",
                    "status": "in progress",
                    "activeForm": "Running tests"
                }
            ]
        }),
    );
    let value: Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value["todos"][0]["status"], "completed");
    assert_eq!(value["todos"][1]["status"], "in_progress");
}

#[test]
fn history_tool_arguments_canonicalize_glob_and_web_scalars() {
    let glob_raw = history_tool_arguments(
        "glob",
        &json!({
            "pattern": "**/*.rs",
            "path": null,
            "sort": "recent",
            "limit": "5"
        }),
    );
    let glob: Value = serde_json::from_str(&glob_raw).unwrap();
    assert_eq!(glob["sort"], "mtime");
    assert_eq!(glob["limit"], 5);

    let web_raw = history_tool_arguments(
        "web_search",
        &json!({
            "q": "tomte",
            "limit": "7",
            "allowed_domains": "example.com, docs.rs",
            "blocked_domains": ""
        }),
    );
    let web: Value = serde_json::from_str(&web_raw).unwrap();
    assert_eq!(web["query"], "tomte");
    assert_eq!(web["max_results"], 7);
    assert_eq!(web["blocked_domains"], Value::Null);
    assert_eq!(web["allowed_domains"][0], "example.com");
    assert_eq!(web["allowed_domains"][1], "docs.rs");
    assert!(web.get("q").is_none());
    assert!(web.get("limit").is_none());

    let fetch_raw = history_tool_arguments(
        "web_fetch",
        &json!({
            "link": "https://example.com",
            "maxBytes": "4096"
        }),
    );
    let fetch: Value = serde_json::from_str(&fetch_raw).unwrap();
    assert_eq!(fetch["url"], "https://example.com");
    assert_eq!(fetch["max_bytes"], 4096);
    assert!(fetch.get("link").is_none());
}

#[test]
fn history_tool_arguments_canonicalize_ask_question_booleans() {
    let raw = history_tool_arguments(
        "ask_user_question",
        &json!({
            "prompt": "Which path?",
            "title": "Path",
            "choices": [
                "A",
                {"value": "B", "details": "Use B"}
            ],
            "multiSelect": "yes"
        }),
    );
    let value: Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value["questions"][0]["question"], "Which path?");
    assert_eq!(value["questions"][0]["header"], "Path");
    assert_eq!(value["questions"][0]["multi_select"], true);
    assert_eq!(value["questions"][0]["options"][0]["label"], "A");
    assert_eq!(value["questions"][0]["options"][0]["description"], "A");
    assert_eq!(value["questions"][0]["options"][1]["label"], "B");
    assert_eq!(value["questions"][0]["options"][1]["description"], "Use B");
    assert!(value.get("prompt").is_none());
    assert!(value.get("choices").is_none());
}

#[test]
fn unsupported_tool_result_history_becomes_safe_user_message() {
    let registry = crate::tools::Registry::standard();
    let mut history = Vec::new();

    append_tool_result_history(
        &mut history,
        &registry,
        "call_sleep",
        "Sleep",
        "Error: unknown tool: Sleep".to_string(),
        true,
        None,
    );

    assert_eq!(history.len(), 1);
    match &history[0] {
        InputItem::Message { role, content } => {
            assert_eq!(role, "user");
            match &content[0] {
                MessageContent::InputText { text } => {
                    assert!(text.contains("unknown tool: Sleep"), "got: {text}");
                    assert!(
                        text.contains("not recorded as a function_call"),
                        "got: {text}"
                    );
                }
                other => panic!("expected input text, got {other:?}"),
            }
        }
        other => panic!("expected safe message, got {other:?}"),
    }
}
