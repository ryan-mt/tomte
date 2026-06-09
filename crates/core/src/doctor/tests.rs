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
fn builtin_local_preset_routes_to_preset_not_openai() {
    // A built-in preset id with no `providers` entry must route through the
    // preset (mirroring LlmClient::for_config), not the OpenAI credential check.
    // Ollama is a keyless local server, so it's OK with no key and no creds —
    // deterministic, no env var needed. Before the builtin_provider fallback,
    // this misrouted to OpenAI and reported a false Error.
    let c = model_routing_check(
        "ollama/llama3",
        &coverage(MISS, MISS, MISS, MISS),
        &HashMap::new(),
    );
    assert_eq!(c.status, Status::Ok);
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
    let dir = std::env::temp_dir().join(format!("tomte-doctor-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("tomte-fake-bin");
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

    // Absolute path: checked directly.
    assert!(binary_on_path(bin.to_str().unwrap()));
    assert!(!binary_on_path(&dir.join("nope").to_string_lossy()));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn binary_in_paths_honors_pathext_extensions() {
    // The Windows `.cmd`/`.bat` shim case (npx, prettier, pnpm): the runtime
    // spawns these via PATH×PATHEXT, so the doctor's `which` must find a `.cmd`
    // shim when the ext list includes `.cmd` — and must NOT when only `.exe` is
    // searched (the old behavior, which falsely reported a valid command missing).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("npx.cmd"), b"@echo off").unwrap();
    let paths = std::env::join_paths([dir.path()]).unwrap();
    let exe_only = vec![".exe".to_string()];
    let with_cmd = vec![".exe".to_string(), ".cmd".to_string()];

    assert!(
        !binary_in_paths("npx", &paths, &exe_only),
        "a .cmd shim must be missed when only .exe is searched"
    );
    assert!(
        binary_in_paths("npx", &paths, &with_cmd),
        "a .cmd shim must be found when .cmd is in the ext list"
    );

    // A bare (extensionless) executable is found regardless of the ext list.
    std::fs::write(dir.path().join("tool"), b"#!/bin/sh").unwrap();
    assert!(binary_in_paths("tool", &paths, &exe_only));
    // An absent command is not found.
    assert!(!binary_in_paths("definitely-absent-xyz", &paths, &with_cmd));
}

#[test]
fn hook_program_extracts_first_program_skipping_env() {
    assert_eq!(hook_program("cargo fmt"), Some("cargo"));
    assert_eq!(
        hook_program("npx --no-install prettier --write ."),
        Some("npx")
    );
    assert_eq!(hook_program("RUST_LOG=info cargo fmt"), Some("cargo"));
    assert_eq!(hook_program("  gofmt -w .  "), Some("gofmt"));
    assert_eq!(hook_program(""), None);
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
