
use super::*;

#[test]
fn system_prompt_carries_the_senior_workflow() {
    // The end-to-end working discipline (plan -> test-first -> finish ->
    // verify -> loop until green) must stay in the base prompt: it is what
    // makes the agent see a task through instead of handing back partial
    // work. Guards the section against an accidental deletion.
    let p = default_system_prompt();
    assert!(
        p.contains("# Seeing a task through"),
        "workflow section header"
    );
    for marker in [
        "TEST-FIRST",
        "WORK TO COMPLETION",
        "DEFINITION OF DONE",
        "LOOP ON FAILURE",
    ] {
        assert!(p.contains(marker), "missing discipline marker: {marker}");
    }
}

#[test]
fn system_prompt_teaches_the_why_context_xray() {
    // The Repo Twin X-ray only pays off if the model reaches for it at the
    // right moment (task start, seeded by a file/line/symbol) — guard the
    // tool-discipline line that teaches that timing.
    let p = default_system_prompt();
    assert!(
        p.contains("`why_context`"),
        "tool discipline must list why_context"
    );
    assert!(
        p.contains("call it FIRST"),
        "the timing guidance must survive"
    );
}

#[test]
fn system_prompt_lists_builtin_subagent_types() {
    // The model must see which `subagent_type` values exist, or it guesses
    // names like `code-explorer` and the dispatch fails. Keep the advertised
    // roster in sync with `builtin_subagents()`.
    let p = default_system_prompt();
    for name in crate::subagent::builtin_subagent_names() {
        assert!(
            p.contains(name),
            "system prompt must advertise built-in subagent `{name}`"
        );
    }
}

#[test]
fn system_prompt_carries_git_and_security_discipline() {
    // Git state belongs to the user (no unasked commits, no force-push, no
    // --no-verify) and the security stance must survive edits — both are
    // stability rails, not flavor.
    let p = default_system_prompt();
    assert!(p.contains("# Git & version control"));
    assert!(p.contains("Never commit, push, tag, amend, or rebase unless the user asked"));
    assert!(p.contains("Never force-push"));
    assert!(p.contains("`--no-verify`"));
    assert!(p.contains("# Security"));
    assert!(p.contains("DENIED was a decision"), "denied-call rule");
}

#[test]
fn environment_block_states_the_facts_models_guess_wrong() {
    // cwd + git standing, platform, the shell behind run_shell, and today's
    // date — each absent fact costs real turns (bash syntax to cmd.exe,
    // stale "latest version" reasoning).
    let tmp = tempfile::tempdir().unwrap();
    let block = environment_block(tmp.path());
    assert!(block.contains("# Environment"));
    assert!(block.contains("not a git repository"));
    assert!(block.contains(std::env::consts::OS));
    if cfg!(windows) {
        assert!(block.contains("`cmd /C`"));
    } else {
        assert!(block.contains("`sh -c`"));
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert!(block.contains(&today), "today's date must be stated");
}

#[test]
fn apply_environment_block_replaces_in_place_and_keeps_later_blocks() {
    // The env block sits BEFORE the memory/trail/skill blocks, so a
    // re-apply must splice in place — a truncate-style strip would destroy
    // everything appended after it (the bug class this guards against).
    let tmp = tempfile::tempdir().unwrap();
    let mut prompt = String::from("BASE");
    apply_environment_block(&mut prompt, tmp.path());
    prompt.push_str("\n\nLATER-BLOCKS-SENTINEL");
    apply_environment_block(&mut prompt, tmp.path());

    assert_eq!(
        prompt.matches(ENV_BLOCK_BEGIN).count(),
        1,
        "exactly one env block after re-apply"
    );
    assert!(
        prompt.contains("LATER-BLOCKS-SENTINEL"),
        "blocks after the env block must survive a re-apply"
    );
    assert!(
        prompt.find(ENV_BLOCK_BEGIN).unwrap() < prompt.find("LATER-BLOCKS-SENTINEL").unwrap(),
        "env block stays in place, before later blocks"
    );
    assert!(prompt.starts_with("BASE"));
}
