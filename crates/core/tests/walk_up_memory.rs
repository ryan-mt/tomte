//! Integration: Agent::apply_project_memory walks up the directory tree
//! and orders ancestor-first cwd-last.
use opencli_core::agent::Agent;
use opencli_core::auth::Credential;
use opencli_core::config::Config;
use opencli_core::openai::OpenAiClient;

#[test]
fn walk_up_picks_ancestor_then_project_with_correct_ordering() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let pkg = root.join("packages").join("frontend");
    std::fs::create_dir_all(&pkg).unwrap();

    std::fs::write(root.join("CLAUDE.md"), "ROOT_RULES").unwrap();
    std::fs::write(root.join("packages").join("AGENTS.md"), "PACKAGES_RULES").unwrap();
    std::fs::write(pkg.join("CLAUDE.md"), "FRONTEND_RULES").unwrap();

    let client = OpenAiClient::new(Credential::ApiKey("sk-dummy".into())).unwrap();
    let mut agent = Agent::new(client, Config::default());
    agent.cwd = pkg.clone();

    let baseline_len = agent.system_prompt.len();
    agent.apply_project_memory();
    let prompt = &agent.system_prompt[baseline_len..];

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
