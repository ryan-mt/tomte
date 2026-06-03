use super::*;

fn coverage(
    oa_oauth: CredentialPresence,
    oa_key: CredentialPresence,
    an_oauth: CredentialPresence,
    an_key: CredentialPresence,
) -> CredentialCoverage {
    CredentialCoverage {
        openai_oauth: oa_oauth,
        openai_api_key: oa_key,
        anthropic_oauth: an_oauth,
        anthropic_api_key: an_key,
    }
}

const MISS: CredentialPresence = CredentialPresence::Missing;
const STORED: CredentialPresence = CredentialPresence::Stored;

#[test]
fn anthropic_model_with_anthropic_creds_is_ok() {
    let c = model_routing_check(
        "claude-opus-4-8",
        &coverage(MISS, MISS, STORED, MISS),
        &HashMap::new(),
    );
    assert_eq!(c.status, Status::Ok);
}

#[test]
fn anthropic_model_without_anthropic_creds_is_error() {
    // OpenAI creds present, but the model needs Anthropic.
    let c = model_routing_check(
        "claude-opus-4-8",
        &coverage(STORED, STORED, MISS, MISS),
        &HashMap::new(),
    );
    assert_eq!(c.status, Status::Error);
}

#[test]
fn openai_model_without_openai_creds_is_error() {
    let c = model_routing_check(
        "gpt-5.5",
        &coverage(MISS, MISS, STORED, MISS),
        &HashMap::new(),
    );
    assert_eq!(c.status, Status::Error);
}

#[test]
fn explicit_anthropic_prefix_routes_to_anthropic() {
    // The `anthropic/` prefix must win over the name heuristic (the bare id
    // does not start with "claude").
    let ok = model_routing_check(
        "anthropic/claude-sonnet-4-6",
        &coverage(MISS, MISS, MISS, STORED),
        &HashMap::new(),
    );
    assert_eq!(ok.status, Status::Ok);
    let err = model_routing_check(
        "anthropic/claude-sonnet-4-6",
        &coverage(STORED, STORED, MISS, MISS),
        &HashMap::new(),
    );
    assert_eq!(err.status, Status::Error);
}

#[test]
fn custom_provider_with_key_is_ok_without_builtin_creds() {
    let mut providers = HashMap::new();
    providers.insert(
        "groq".to_string(),
        ProviderConfig {
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key: Some("sk-test".to_string()),
            api_key_env: None,
            context_limit: None,
            forward_reasoning_effort: false,
        },
    );
    let c = model_routing_check(
        "groq/llama-3.3-70b",
        &coverage(MISS, MISS, MISS, MISS),
        &providers,
    );
    assert_eq!(c.status, Status::Ok);
}

#[test]
fn custom_provider_without_key_warns() {
    let mut providers = HashMap::new();
    providers.insert(
        "groq".to_string(),
        ProviderConfig {
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key: None,
            api_key_env: None,
            context_limit: None,
            forward_reasoning_effort: false,
        },
    );
    let c = model_routing_check(
        "groq/llama-3.3-70b",
        &coverage(MISS, MISS, MISS, MISS),
        &providers,
    );
    assert_eq!(c.status, Status::Warn);
}

#[test]
fn counts_ignore_info_and_tally_the_rest() {
    let report = Report {
        sections: vec![Section {
            title: "T".to_string(),
            checks: vec![
                Check::ok("a"),
                Check::ok("b"),
                Check::info("c"),
                Check::warn("d"),
                Check::error("e"),
            ],
        }],
    };
    let c = report.counts();
    assert_eq!((c.ok, c.warn, c.error), (2, 1, 1));
    assert!(report.has_errors());
}

#[test]
fn render_includes_titles_glyphs_and_summary() {
    let report = Report {
        sections: vec![Section {
            title: "External tools".to_string(),
            checks: vec![Check::ok("git"), Check::warn("rg missing")],
        }],
    };
    let out = report.render();
    assert!(out.contains("External tools"));
    assert!(out.contains("✓ git"));
    assert!(out.contains("⚠ rg missing"));
    assert!(out.contains("Summary: 1 ok · 1 warning · 0 errors"));
}

#[test]
fn binary_on_path_finds_a_file_in_a_synthetic_path_dir() {
    let dir = std::env::temp_dir().join(format!("opencli-doctor-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("opencli-fake-bin");
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

    // Absolute path: checked directly.
    assert!(binary_on_path(bin.to_str().unwrap()));
    assert!(!binary_on_path(&dir.join("nope").to_string_lossy()));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn diagnose_runs_without_panicking_and_is_structured() {
    // Reads the real environment read-only; assert structure, not values,
    // so it's stable across machines.
    let report = diagnose(Path::new("."));
    assert!(report.sections.len() >= 6);
    assert!(report.sections.iter().any(|s| s.title == "Model routing"));
    assert!(!report.render().is_empty());
}
