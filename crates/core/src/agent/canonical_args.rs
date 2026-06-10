//! Canonicalize a tool call's arguments into one stable, provider-agnostic
//! shape for the conversation history and the approval gate — folding the many
//! aliases a model might emit (`filePath`/`file_path`/`path`, …) onto tomte's
//! canonical field names. The per-field/per-item helper primitives live in
//! `canonical_helpers`.

use super::*;

pub(super) fn history_tool_arguments(tool_name: &str, args: &Value) -> String {
    let value = canonical_history_arguments(tool_name, args).unwrap_or_else(|| args.clone());
    serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
}

/// Arguments shown on the approval card, in the SAME canonical shape the
/// history records — a user must never approve `cmd=…` and have history record
/// `command=…`. Null placeholders the canonical fold inserts for absent fields
/// are stripped so the card shows only what the call actually carries; a tool
/// without a canonical mapping passes its args through unchanged.
pub(super) fn approval_args_json(tool_name: &str, args: &Value) -> String {
    let value = canonical_history_arguments(tool_name, args)
        .map(|mut v| {
            if let Value::Object(map) = &mut v {
                map.retain(|_, val| !val.is_null());
            }
            v
        })
        .unwrap_or_else(|| args.clone());
    serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
}

/// The target path of a file-mutating call, under any alias the executing
/// tool's deserializer accepts (`path`/`file_path`/`filePath`). Consumers that
/// surface per-file context (house rules) must look up the same file the tool
/// will actually write — a call spelled `filePath` executes fine, so it must
/// not silently skip the lookup.
pub(super) fn edit_path_argument(args: &Value) -> Option<&str> {
    let obj = args.as_object()?;
    ["path", "file_path", "filePath"]
        .iter()
        .find_map(|k| obj.get(*k)?.as_str())
}

pub(super) fn canonical_history_arguments(tool_name: &str, args: &Value) -> Option<Value> {
    let obj = args.as_object()?;
    let mut out = serde_json::Map::new();
    match tool_name {
        "read_file" => {
            insert_first(&mut out, obj, "path", &["path", "file_path", "filePath"]);
            insert_number_or_null(&mut out, obj, "offset", &["offset"]);
            insert_number_or_null(&mut out, obj, "limit", &["limit"]);
        }
        "write_file" => {
            insert_first(&mut out, obj, "path", &["path", "file_path", "filePath"]);
            insert_first(&mut out, obj, "content", &["content", "contents", "text"]);
        }
        "edit_file" => {
            insert_first(&mut out, obj, "path", &["path", "file_path", "filePath"]);
            insert_first(
                &mut out,
                obj,
                "old_string",
                &["old_string", "oldString", "old_text", "oldText"],
            );
            insert_first(
                &mut out,
                obj,
                "new_string",
                &["new_string", "newString", "new_text", "newText"],
            );
            insert_bool_or_default(
                &mut out,
                obj,
                "replace_all",
                &["replace_all", "replaceAll"],
                false,
            );
        }
        "multi_edit" => {
            insert_first(&mut out, obj, "path", &["path", "file_path", "filePath"]);
            let edits = obj
                .get("edits")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| canonical_edit_item(item).unwrap_or_else(|| item.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            out.insert("edits".to_string(), Value::Array(edits));
        }
        "list_dir" => {
            insert_first(
                &mut out,
                obj,
                "path",
                &[
                    "path",
                    "file_path",
                    "filePath",
                    "directory",
                    "dir",
                    "folder",
                ],
            );
        }
        "glob" => {
            insert_first(&mut out, obj, "pattern", &["pattern"]);
            insert_or_null(&mut out, obj, "path", &["path"]);
            out.insert(
                "sort".to_string(),
                first_value(obj, &["sort"])
                    .and_then(normalized_glob_sort)
                    .unwrap_or(Value::Null),
            );
            insert_number_or_null(&mut out, obj, "limit", &["limit"]);
        }
        "run_shell" => {
            insert_first(&mut out, obj, "command", &["command", "cmd"]);
            insert_number_or_null(
                &mut out,
                obj,
                "timeout_ms",
                &["timeout_ms", "timeoutMs", "timeout"],
            );
            insert_bool_or_null(
                &mut out,
                obj,
                "run_in_background",
                &["run_in_background", "runInBackground"],
            );
            // Do not treat Claude's `dangerouslyDisableSandbox` as permission
            // to bypass tomte's destructive-command guard. Only the explicit
            // tomte field is preserved.
            insert_bool_or_null(
                &mut out,
                obj,
                "dangerous_override",
                &["dangerous_override", "dangerousOverride"],
            );
        }
        "grep" => {
            insert_first(&mut out, obj, "pattern", &["pattern"]);
            insert_or_null(&mut out, obj, "path", &["path"]);
            insert_or_null(&mut out, obj, "glob", &["glob"]);
            insert_bool_or_default(
                &mut out,
                obj,
                "case_insensitive",
                &[
                    "case_insensitive",
                    "caseInsensitive",
                    "ignore_case",
                    "ignoreCase",
                    "-i",
                ],
                false,
            );
            out.insert(
                "output_mode".to_string(),
                first_value(obj, &["output_mode", "outputMode"])
                    .and_then(normalized_grep_output_mode)
                    .unwrap_or(Value::Null),
            );
            insert_number_or_null(&mut out, obj, "head_limit", &["head_limit", "headLimit"]);
            insert_number_or_null(&mut out, obj, "offset", &["offset", "skip"]);
            insert_number_or_null(
                &mut out,
                obj,
                "context_after",
                &["context_after", "contextAfter", "-A", "-C", "contextLines"],
            );
            insert_number_or_null(
                &mut out,
                obj,
                "context_before",
                &[
                    "context_before",
                    "contextBefore",
                    "-B",
                    "-C",
                    "contextLines",
                ],
            );
            insert_bool_or_null(&mut out, obj, "multiline", &["multiline", "multiLine"]);
            insert_or_null(
                &mut out,
                obj,
                "file_type",
                &["file_type", "fileType", "type"],
            );
        }
        "todo_write" => {
            let todos = obj
                .get("todos")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| canonical_todo_item(item).unwrap_or_else(|| item.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            out.insert("todos".to_string(), Value::Array(todos));
        }
        "goal_update" => {
            if let Some(status) =
                first_value(obj, &["status", "state", "goal_status", "goalStatus"])
            {
                out.insert(
                    "status".to_string(),
                    normalized_goal_status(status).unwrap_or_else(|| status.clone()),
                );
            }
            insert_first(
                &mut out,
                obj,
                "summary",
                &["summary", "message", "details", "note"],
            );
        }
        "exit_plan_mode" => {
            insert_first(&mut out, obj, "plan", &["plan", "summary", "proposal"]);
        }
        "enter_plan_mode" => {}
        "web_fetch" => {
            insert_first(&mut out, obj, "url", &["url", "uri", "link"]);
            insert_number_or_null(&mut out, obj, "max_bytes", &["max_bytes", "maxBytes"]);
        }
        "web_search" => {
            insert_first(
                &mut out,
                obj,
                "query",
                &["query", "q", "search_query", "searchQuery"],
            );
            insert_number_or_null(
                &mut out,
                obj,
                "max_results",
                &[
                    "max_results",
                    "maxResults",
                    "num_results",
                    "numResults",
                    "limit",
                ],
            );
            insert_string_vec_or_null(
                &mut out,
                obj,
                "allowed_domains",
                &["allowed_domains", "allowedDomains"],
            );
            insert_string_vec_or_null(
                &mut out,
                obj,
                "blocked_domains",
                &["blocked_domains", "blockedDomains"],
            );
        }
        "notebook_edit" => {
            insert_first(
                &mut out,
                obj,
                "notebook_path",
                &[
                    "notebook_path",
                    "notebookPath",
                    "path",
                    "file_path",
                    "filePath",
                ],
            );
            insert_source_if_present(
                &mut out,
                obj,
                "new_source",
                &["new_source", "newSource", "source", "content", "text"],
            );
            insert_string_or_null(
                &mut out,
                obj,
                "cell_id",
                &[
                    "cell_id",
                    "cellId",
                    "cellID",
                    "id",
                    "index",
                    "cell_index",
                    "cellIndex",
                ],
            );
            insert_or_null(
                &mut out,
                obj,
                "cell_type",
                &["cell_type", "cellType", "type"],
            );
            insert_or_null(
                &mut out,
                obj,
                "edit_mode",
                &["edit_mode", "editMode", "mode", "action"],
            );
        }
        "skill" => {
            insert_first(&mut out, obj, "name", &["name"]);
        }
        "ask_user_question" => {
            let questions = canonical_question_items(obj);
            out.insert("questions".to_string(), Value::Array(questions));
        }
        "dispatch_agent" => {
            insert_dispatch_subagent_type(&mut out, obj);
            insert_first(
                &mut out,
                obj,
                "prompt",
                &[
                    "prompt",
                    "task",
                    "instructions",
                    "instruction",
                    "input",
                    "message",
                ],
            );
            insert_first(&mut out, obj, "description", &["description"]);
            insert_first(&mut out, obj, "model", &["model"]);
            insert_first(
                &mut out,
                obj,
                "cwd",
                &["cwd", "working_dir", "workingDir", "directory", "dir"],
            );
            insert_dispatch_plan_mode_required(&mut out, obj);
        }
        "bash_output" | "kill_shell" => {
            insert_first(
                &mut out,
                obj,
                "bash_id",
                &["bash_id", "bashId", "id", "shell_id", "shellId"],
            );
        }
        _ => return None,
    }
    Some(Value::Object(out))
}
