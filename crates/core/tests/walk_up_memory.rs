//! Integration: Agent::apply_project_memory discovers repo-scoped memory
//! in ancestor-first order (git root → cwd).
use opencli_core::agent::Agent;
use opencli_core::auth::Credential;
use opencli_core::client::LlmClient;
use opencli_core::config::Config;
use opencli_core::memory::{self, MEMORY_BLOCK_BEGIN};
use opencli_core::provider::Provider;

fn init_git_repo(root: &std::path::Path) {
    let out = std::process::Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output()
        .expect("git init");
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn apply_memory(agent: &mut Agent) {
    memory::apply_to_system_prompt_with_globals(&mut agent.system_prompt, &agent.cwd, vec![]);
}

fn memory_suffix(agent: &Agent, baseline_len: usize) -> &str {
    let prompt = &agent.system_prompt[baseline_len..];
    assert!(
        prompt.starts_with(MEMORY_BLOCK_BEGIN),
        "expected memory marker block; got:\n{prompt}"
    );
    prompt
}

#[test]
fn walk_up_picks_ancestor_then_project_with_correct_ordering() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    init_git_repo(root);
    let pkg = root.join("packages").join("frontend");
    std::fs::create_dir_all(&pkg).unwrap();

    std::fs::write(root.join("CLAUDE.md"), "ROOT_RULES").unwrap();
    std::fs::write(root.join("packages").join("AGENTS.md"), "PACKAGES_RULES").unwrap();
    std::fs::write(pkg.join("CLAUDE.md"), "FRONTEND_RULES").unwrap();

    let client = LlmClient::new(Credential::ApiKey {
        provider: Provider::OpenAi,
        key: "sk-dummy".into(),
    })
    .unwrap();
    let mut agent = Agent::new(client, Config::default());
    agent.cwd = pkg.clone();

    let baseline_len = agent.system_prompt.len();
    apply_memory(&mut agent);
    let prompt = memory_suffix(&agent, baseline_len);

    assert!(
        prompt.contains("ROOT_RULES"),
        "missing ROOT_RULES; got:\n{prompt}"
    );
    assert!(
        prompt.contains("PACKAGES_RULES"),
        "missing PACKAGES_RULES; got:\n{prompt}"
    );
    assert!(
        prompt.contains("FRONTEND_RULES"),
        "missing FRONTEND_RULES; got:\n{prompt}"
    );

    let pos_root = prompt.find("ROOT_RULES").unwrap();
    let pos_pkgs = prompt.find("PACKAGES_RULES").unwrap();
    let pos_front = prompt.find("FRONTEND_RULES").unwrap();
    assert!(
        pos_root < pos_pkgs && pos_pkgs < pos_front,
        "expected root < packages < frontend; got positions {pos_root}/{pos_pkgs}/{pos_front}"
    );
}

#[test]
fn one_file_per_scope_prefers_agents_over_claude() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    init_git_repo(root);

    std::fs::write(root.join("AGENTS.md"), "AGENTS_RULES").unwrap();
    std::fs::write(root.join("CLAUDE.md"), "CLAUDE_RULES").unwrap();

    let client = LlmClient::new(Credential::ApiKey {
        provider: Provider::OpenAi,
        key: "sk-dummy".into(),
    })
    .unwrap();
    let mut agent = Agent::new(client, Config::default());
    agent.cwd = root.to_path_buf();

    let baseline_len = agent.system_prompt.len();
    apply_memory(&mut agent);
    let prompt = memory_suffix(&agent, baseline_len);

    assert!(prompt.contains("AGENTS_RULES"));
    assert!(
        !prompt.contains("CLAUDE_RULES"),
        "expected only AGENTS.md in this scope; got:\n{prompt}"
    );
}

#[test]
fn agents_override_wins_over_agents_and_claude() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    init_git_repo(root);

    std::fs::write(root.join("AGENTS.override.md"), "OVERRIDE_RULES").unwrap();
    std::fs::write(root.join("AGENTS.md"), "AGENTS_RULES").unwrap();
    std::fs::write(root.join("CLAUDE.md"), "CLAUDE_RULES").unwrap();

    let client = LlmClient::new(Credential::ApiKey {
        provider: Provider::OpenAi,
        key: "sk-dummy".into(),
    })
    .unwrap();
    let mut agent = Agent::new(client, Config::default());
    agent.cwd = root.to_path_buf();

    let baseline_len = agent.system_prompt.len();
    apply_memory(&mut agent);
    let prompt = memory_suffix(&agent, baseline_len);

    assert!(prompt.contains("OVERRIDE_RULES"));
    assert!(!prompt.contains("AGENTS_RULES"));
    assert!(!prompt.contains("CLAUDE_RULES"));
}

#[test]
fn memory_outside_git_root_is_not_loaded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path();
    let repo = parent.join("repo");
    let nested = repo.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    init_git_repo(&repo);

    std::fs::write(parent.join("AGENTS.md"), "PARENT_RULES").unwrap();
    std::fs::write(repo.join("AGENTS.md"), "REPO_RULES").unwrap();

    let client = LlmClient::new(Credential::ApiKey {
        provider: Provider::OpenAi,
        key: "sk-dummy".into(),
    })
    .unwrap();
    let mut agent = Agent::new(client, Config::default());
    agent.cwd = nested;

    let baseline_len = agent.system_prompt.len();
    apply_memory(&mut agent);
    let prompt = memory_suffix(&agent, baseline_len);

    assert!(prompt.contains("REPO_RULES"));
    assert!(
        !prompt.contains("PARENT_RULES"),
        "must not load memory above the git root; got:\n{prompt}"
    );
}

#[test]
fn apply_project_memory_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_git_repo(tmp.path());
    std::fs::write(tmp.path().join("AGENTS.md"), "RULES").unwrap();

    let client = LlmClient::new(Credential::ApiKey {
        provider: Provider::OpenAi,
        key: "sk-dummy".into(),
    })
    .unwrap();
    let mut agent = Agent::new(client, Config::default());
    agent.cwd = tmp.path().to_path_buf();

    apply_memory(&mut agent);
    let len = agent.system_prompt.len();
    apply_memory(&mut agent);
    assert_eq!(agent.system_prompt.len(), len);
    assert_eq!(agent.system_prompt.matches(MEMORY_BLOCK_BEGIN).count(), 1);
}

#[test]
fn project_memory_dirs_match_git_root_to_cwd() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let nested = repo.join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    init_git_repo(&repo);

    let dirs = memory::project_memory_dirs(&nested);
    assert_eq!(dirs.len(), 3);
    assert_eq!(dirs.last().unwrap(), &nested.canonicalize().unwrap());
}
