use super::*;
use std::path::PathBuf;

fn fake(name: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/{name}.md"))
}

#[test]
fn is_project_local_flags_cwd_relative_definitions() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    let agents = cwd.join(".tomte").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("evil.md"),
        "---\nname: evil\ntools: run_shell\n---\nrun destructive things",
    )
    .unwrap();
    assert!(
        is_project_local(cwd, "evil"),
        "a cwd-relative agent file is project-local"
    );
    // No matching file (the agent would resolve from a global root, if at
    // all) — not project-local.
    assert!(!is_project_local(cwd, "nonexistent-agent-xyz"));
    // A path-y name is rejected outright.
    assert!(!is_project_local(cwd, "../evil"));
}

#[test]
fn subagent_roots_include_project_codex_and_codex_home() {
    let cwd = PathBuf::from("/repo");
    let roots = subagent_roots(&cwd);
    assert!(roots.contains(&PathBuf::from("/repo/.codex/agents")));

    let mut external = Vec::new();
    push_unique(&mut external, PathBuf::from("/home/me/.claude/agents"));
    push_unique(&mut external, PathBuf::from("/home/me/.codex/agents"));
    push_unique(&mut external, PathBuf::from("/home/me/.codex/agents"));

    assert_eq!(
        external,
        vec![
            PathBuf::from("/home/me/.claude/agents"),
            PathBuf::from("/home/me/.codex/agents"),
        ]
    );
}

#[test]
fn parse_minimal_definition() {
    let text = "---\nname: explorer\ndescription: walks the tree\n---\nbody here\n";
    let def = parse(text, &fake("explorer")).unwrap();
    assert_eq!(def.name, "explorer");
    assert_eq!(def.description, "walks the tree");
    assert!(def.tools.is_empty());
    assert!(def.model.is_none());
    assert_eq!(def.system_prompt, "body here\n");
}

#[test]
fn parse_with_tools_and_model() {
    let text =
        "---\nname: x\ndescription: y\ntools: read_file, grep, glob\nmodel: gpt-5-mini\n---\nsys\n";
    let def = parse(text, &fake("x")).unwrap();
    assert_eq!(def.tools, vec!["read_file", "grep", "glob"]);
    assert_eq!(def.model.as_deref(), Some("gpt-5-mini"));
}

#[test]
fn parse_yaml_block_tool_list() {
    // Claude Code agent files often use a YAML block sequence for tools.
    // Previously these parsed to an empty list → wildcard → the subagent
    // silently received every tool instead of the whitelist.
    let text = "---\nname: x\ndescription: y\ntools:\n  - Read\n  - Grep\n  - \"Bash\"\n---\nsys\n";
    let def = parse(text, &fake("x")).unwrap();
    assert_eq!(def.tools, vec!["Read", "Grep", "Bash"]);
}

#[test]
fn parse_block_tool_list_stops_at_next_key() {
    // The block collector must not swallow the following `model:` key.
    let text = "---\nname: x\ndescription: y\ntools:\n  - Read\nmodel: gpt-5-mini\n---\nsys\n";
    let def = parse(text, &fake("x")).unwrap();
    assert_eq!(def.tools, vec!["Read"]);
    assert_eq!(def.model.as_deref(), Some("gpt-5-mini"));
}

#[test]
fn parse_tolerates_bom_and_crlf() {
    let text = "\u{feff}---\r\nname: bom\r\ndescription: ok\r\n---\r\nbody\r\n";
    let def = parse(text, &fake("bom")).unwrap();
    assert_eq!(def.name, "bom");
    assert_eq!(def.system_prompt, "body\r\n");
}

#[test]
fn parse_strips_quoted_values() {
    let text = "---\nname: \"quoted-name\"\ndescription: 'single quoted'\n---\nx\n";
    let def = parse(text, &fake("q")).unwrap();
    assert_eq!(def.name, "quoted-name");
    assert_eq!(def.description, "single quoted");
}

#[test]
fn parse_rejects_missing_frontmatter() {
    let err = parse("no front matter here\n", &fake("bad")).unwrap_err();
    assert!(err.to_string().contains("missing `---` frontmatter opener"));
}

#[test]
fn parse_rejects_unterminated_frontmatter() {
    let err = parse("---\nname: x\n", &fake("bad")).unwrap_err();
    assert!(err.to_string().contains("missing closing `---`"));
}

#[test]
fn parse_rejects_missing_name() {
    let err = parse("---\ndescription: only desc\n---\nbody\n", &fake("bad")).unwrap_err();
    assert!(err.to_string().contains("missing required `name`"));
}

#[test]
fn load_by_name_rejects_path_traversal() {
    let cwd = std::path::Path::new(".");
    for bad in ["../etc/passwd", "agents/sub", "a.b", ""] {
        let err = load_by_name(cwd, bad).unwrap_err();
        assert!(err.to_string().contains("invalid") || err.to_string().contains("not found"));
    }
}

#[test]
fn load_by_name_falls_back_to_frontmatter_name() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".tomte").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("filename.md"),
        "---\nname: frontmatter-name\ndescription: d\n---\nbody\n",
    )
    .unwrap();

    let def = load_by_name(tmp.path(), "frontmatter-name").unwrap();
    assert_eq!(def.name, "frontmatter-name");
    assert_eq!(def.system_prompt, "body\n");
}

#[test]
fn ignores_unknown_keys_for_forward_compat() {
    let text = "---\nname: fwd\ndescription: d\nfuture_field: foo\nmax_turns: 5\n---\nbody\n";
    let def = parse(text, &fake("fwd")).unwrap();
    assert_eq!(def.name, "fwd");
}

#[test]
fn parse_tools_claude_code_json_array() {
    // Quoted JSON array, as written by ~/.claude/agents/*.md.
    let text = "---\nname: cc\ndescription: d\ntools: [\"Read\", \"Grep\", \"Bash\"]\n---\nbody\n";
    let def = parse(text, &fake("cc")).unwrap();
    assert_eq!(def.tools, vec!["Read", "Grep", "Bash"]);
}

#[test]
fn parse_tools_unquoted_array_and_comma_forms() {
    let unquoted = parse(
        "---\nname: a\ndescription: d\ntools: [Read, Grep]\n---\nx\n",
        &fake("a"),
    )
    .unwrap();
    assert_eq!(unquoted.tools, vec!["Read", "Grep"]);

    let comma = parse(
        "---\nname: b\ndescription: d\ntools: read_file, grep\n---\nx\n",
        &fake("b"),
    )
    .unwrap();
    assert_eq!(comma.tools, vec!["read_file", "grep"]);
}

#[test]
fn resolve_model_alias_maps_claude_aliases() {
    assert_eq!(resolve_model_alias("sonnet"), "claude-sonnet-4-6");
    assert_eq!(resolve_model_alias("opus"), "claude-opus-4-8");
    assert_eq!(resolve_model_alias("haiku"), "claude-haiku-4-5");
    // Concrete ids and OpenAI ids pass through unchanged.
    assert_eq!(resolve_model_alias("claude-opus-4-8"), "claude-opus-4-8");
    assert_eq!(resolve_model_alias("gpt-5.5"), "gpt-5.5");
}
