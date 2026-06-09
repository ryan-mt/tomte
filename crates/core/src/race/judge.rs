//! The deterministic judge's pure parsers: diff size, whether a regression test
//! was added, and how many risky shell commands a contestant ran. These take
//! plain strings (git output, the JSON event stream) so they're unit-tested
//! without a repo or a live agent — the evidence, not the model's word, decides.

use crate::tools::shell::classify_danger;

/// Parse `git diff --numstat` into `(files_changed, insertions, deletions)`. Each
/// line is `<ins>\t<del>\t<path>`; a binary file shows `-\t-` and counts as a
/// changed file with zero line deltas.
pub fn parse_numstat(numstat: &str) -> (usize, u64, u64) {
    let mut files = 0usize;
    let mut ins = 0u64;
    let mut del = 0u64;
    for line in numstat.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let a = cols.next().unwrap_or("");
        let b = cols.next().unwrap_or("");
        let path = cols.next().unwrap_or("");
        if path.is_empty() {
            continue;
        }
        files += 1;
        ins += a.parse::<u64>().unwrap_or(0);
        del += b.parse::<u64>().unwrap_or(0);
    }
    (files, ins, del)
}

/// The `<path>` column of every `git diff --numstat` line — the files a
/// contestant changed.
pub fn changed_paths(numstat: &str) -> Vec<String> {
    numstat
        .lines()
        .filter_map(|line| line.split('\t').nth(2))
        .filter(|p| !p.trim().is_empty())
        .map(|p| crate::repo_twin::normalize(p.trim()))
        .collect()
}

/// Whether a contestant added regression coverage: any changed file is a test by
/// path convention, or the diff *adds* a recognizable test definition. The
/// "coverage" term of the score — rewarding the contestant that didn't just make
/// the symptom go away but locked it with a test.
pub fn detect_added_test(diff: &str, changed: &[String]) -> bool {
    if changed
        .iter()
        .any(|p| crate::repo_twin::testmap::is_test_path(p))
    {
        return true;
    }
    const MARKERS: &[&str] = &[
        "#[test]",
        "#[tokio::test]",
        "def test_",
        "func Test",
        "it(",
        "describe(",
        "test(",
        "@Test",
    ];
    diff.lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .any(|l| {
            let body = l[1..].trim_start();
            MARKERS.iter().any(|m| body.contains(m))
        })
}

/// Count the risky shell commands a contestant executed, read from its
/// `--output-format json` event stream and classified by the SAME
/// [`classify_danger`] guard the live agent uses — so the race penalizes exactly
/// what tomte itself would flag. Tolerant of malformed lines: a line that
/// doesn't parse is skipped, so a stream hiccup can only *under*-count, never
/// crash the judge.
pub fn count_risky_commands(events_jsonl: &str) -> u32 {
    use std::collections::HashSet;
    let mut shell_calls: HashSet<String> = HashSet::new();
    let mut risky = 0u32;
    for line in events_jsonl.lines() {
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match ev.get("kind").and_then(|k| k.as_str()) {
            Some("ToolCallStarted") => {
                // The event stream carries the model's raw spelling; execution
                // resolves aliases (`bash`, `shell`, …) through the registry,
                // so the judge must canonicalise the same way or it undercounts.
                let name = ev.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if crate::tools::canonical_tool_name(name) == Some("run_shell") {
                    if let Some(id) = ev.get("call_id").and_then(|c| c.as_str()) {
                        shell_calls.insert(id.to_string());
                    }
                }
            }
            Some("ToolCallArgsDone") => {
                let id = ev.get("call_id").and_then(|c| c.as_str()).unwrap_or("");
                if !shell_calls.contains(id) {
                    continue;
                }
                let args = ev.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                if let Some(cmd) = command_of(args) {
                    if classify_danger(&cmd).is_some() {
                        risky += 1;
                    }
                }
            }
            _ => {}
        }
    }
    risky
}

/// Extract the `command` string from a run_shell arguments JSON blob, through
/// the same tolerance the executing agent applies (empty → `{}`, double-encoded
/// payload unwrapped) and the same `cmd` alias run_shell's own args accept —
/// the judge must count what actually ran, not just what parsed cleanly.
fn command_of(arguments: &str) -> Option<String> {
    let v = crate::agent::parse_tool_call_arguments(arguments).ok()?;
    v.get("command")
        .or_else(|| v.get("cmd"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_sums_lines_and_counts_files() {
        let s = "10\t2\tsrc/a.rs\n0\t5\tsrc/b.rs\n-\t-\tlogo.png\n";
        let (files, ins, del) = parse_numstat(s);
        assert_eq!(files, 3);
        assert_eq!(ins, 10);
        assert_eq!(del, 7);
        assert_eq!(changed_paths(s), vec!["src/a.rs", "src/b.rs", "logo.png"]);
    }

    #[test]
    fn added_test_detected_by_path_or_marker() {
        // By path convention.
        assert!(detect_added_test("", &["tests/auth.rs".to_string()]));
        assert!(detect_added_test("", &["src/x.test.ts".to_string()]));
        // By an added marker in the diff, even when no file is named like a test.
        let diff = "+++ b/src/x.rs\n+#[test]\n+fn checks() {}\n";
        assert!(detect_added_test(diff, &["src/x.rs".to_string()]));
        // A removed test line (`-`) doesn't count as *adding* coverage.
        let removed = "--- a/src/x.rs\n-#[test]\n";
        assert!(!detect_added_test(removed, &["src/x.rs".to_string()]));
        // Plain change, no test anywhere.
        assert!(!detect_added_test(
            "+let x = 1;\n",
            &["src/x.rs".to_string()]
        ));
    }

    #[test]
    fn risky_commands_counted_from_event_stream() {
        let stream = concat!(
            r#"{"kind":"ToolCallStarted","name":"run_shell","call_id":"1"}"#,
            "\n",
            r#"{"kind":"ToolCallArgsDone","call_id":"1","arguments":"{\"command\":\"rm -rf /\"}"}"#,
            "\n",
            r#"{"kind":"ToolCallStarted","name":"run_shell","call_id":"2"}"#,
            "\n",
            r#"{"kind":"ToolCallArgsDone","call_id":"2","arguments":"{\"command\":\"cargo test\"}"}"#,
            "\n",
        );
        // Only `rm -rf /` is destructive.
        assert_eq!(count_risky_commands(stream), 1);
    }

    #[test]
    fn risky_count_survives_alias_names_and_double_encoded_args() {
        // `bash` resolves to run_shell at execution, and the arguments arrive
        // double-encoded (a JSON string holding the object) under the `cmd`
        // alias — the contestant's agent runs `rm -rf /` all the same, so the
        // judge must count it.
        let inner = serde_json::json!({"cmd": "rm -rf /"}).to_string();
        let ev1 = serde_json::json!({
            "kind": "ToolCallStarted",
            "name": "bash",
            "call_id": "1",
        });
        let ev2 = serde_json::json!({
            "kind": "ToolCallArgsDone",
            "call_id": "1",
            // The raw arguments TEXT is itself a JSON string — double-encoded.
            "arguments": serde_json::to_string(&inner).unwrap(),
        });
        let stream = format!("{ev1}\n{ev2}\n");
        assert_eq!(count_risky_commands(&stream), 1);
    }

    #[test]
    fn non_shell_tool_args_are_ignored_and_garbage_does_not_panic() {
        let stream = concat!(
            r#"{"kind":"ToolCallStarted","name":"edit_file","call_id":"9"}"#,
            "\n",
            r#"{"kind":"ToolCallArgsDone","call_id":"9","arguments":"{\"command\":\"rm -rf /\"}"}"#,
            "\n",
            "not json at all\n",
        );
        // The destructive-looking string belongs to edit_file, not run_shell.
        assert_eq!(count_risky_commands(stream), 0);
    }
}
