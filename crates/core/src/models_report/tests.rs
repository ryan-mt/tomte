use super::*;

fn coverage(
    openai_oauth: CredentialPresence,
    openai_api_key: CredentialPresence,
    anthropic_oauth: CredentialPresence,
    anthropic_api_key: CredentialPresence,
) -> CredentialCoverage {
    CredentialCoverage {
        openai_oauth,
        openai_api_key,
        anthropic_oauth,
        anthropic_api_key,
    }
}

fn signed_in_everywhere() -> CredentialCoverage {
    coverage(
        CredentialPresence::Stored,
        CredentialPresence::Env,
        CredentialPresence::Stored,
        CredentialPresence::Stored,
    )
}

// The card marks exactly the active model, carries the catalog facts for every
// row, and flags the OpenAI ids the ChatGPT-subscription backend rejects.
#[test]
fn collect_marks_active_and_catalog_facts() {
    let cfg = Config {
        model: "claude-fable-5".into(),
        ..Config::default()
    };
    let r = collect(&cfg, &signed_in_everywhere());
    assert_eq!(r.active_provider, "anthropic");

    let actives: Vec<&str> = r
        .providers
        .iter()
        .flat_map(|p| &p.models)
        .filter(|m| m.active)
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(actives, vec!["claude-fable-5"], "exactly one active row");

    let anthropic = r
        .providers
        .iter()
        .find(|p| p.provider == "anthropic")
        .unwrap();
    let fable = anthropic
        .models
        .iter()
        .find(|m| m.id == "claude-fable-5")
        .unwrap();
    assert_eq!(fable.context_limit, 1_000_000);
    assert_eq!(fable.thinking, "adaptive thinking");
    assert!(fable.xhigh);
    let haiku = anthropic
        .models
        .iter()
        .find(|m| m.id == "claude-haiku-4-5")
        .unwrap();
    assert_eq!(haiku.thinking, "extended thinking");
    assert!(!haiku.xhigh);

    let openai = r.providers.iter().find(|p| p.provider == "openai").unwrap();
    let mini = openai
        .models
        .iter()
        .find(|m| m.id == "gpt-5.4-mini")
        .unwrap();
    assert!(mini.api_key_only, "subscription OAuth rejects mini ids");
    let full = openai.models.iter().find(|m| m.id == "gpt-5.5").unwrap();
    assert!(!full.api_key_only);
}

// The failover footer reports exactly what an overload would walk: the
// configured list when present, the built-in ladder when not, and the honest
// off/none states otherwise.
#[test]
fn failover_source_covers_all_four_states() {
    let cov = signed_in_everywhere();

    let auto = collect(
        &Config {
            model: "claude-fable-5".into(),
            ..Config::default()
        },
        &cov,
    );
    assert_eq!(auto.failover_source, "auto");
    assert_eq!(
        auto.failover,
        vec!["claude-opus-4-8".to_string(), "claude-sonnet-4-6".into()]
    );

    let configured = collect(
        &Config {
            model: "claude-fable-5".into(),
            fallback_models: vec!["groq/llama-3.3-70b".into()],
            ..Config::default()
        },
        &cov,
    );
    assert_eq!(configured.failover_source, "configured");
    assert_eq!(configured.failover, vec!["groq/llama-3.3-70b".to_string()]);

    let off = collect(
        &Config {
            model: "claude-fable-5".into(),
            auto_fallback: false,
            ..Config::default()
        },
        &cov,
    );
    assert_eq!(off.failover_source, "off");
    assert!(off.failover.is_empty());

    let none = collect(
        &Config {
            model: "local/primary".into(),
            ..Config::default()
        },
        &cov,
    );
    assert_eq!(none.failover_source, "none");
    assert!(none.failover.is_empty());
}

#[test]
fn render_carries_credentials_active_marker_and_failover() {
    let cfg = Config {
        model: "claude-fable-5".into(),
        ..Config::default()
    };
    let cov = coverage(
        CredentialPresence::Missing,
        CredentialPresence::Missing,
        CredentialPresence::Stored,
        CredentialPresence::Missing,
    );
    let out = render(&collect(&cfg, &cov));
    assert!(
        out.contains("active: claude-fable-5 (anthropic)"),
        "got: {out}"
    );
    assert!(
        out.contains("OpenAI — not signed in — `tomte login`"),
        "got: {out}"
    );
    assert!(
        out.contains("Anthropic — OAuth: stored · API key: not configured"),
        "got: {out}"
    );
    assert!(out.contains("▸ claude-fable-5"), "got: {out}");
    assert!(
        out.contains("failover: built-in ladder — claude-opus-4-8 → claude-sonnet-4-6"),
        "got: {out}"
    );
    assert!(out.contains("API key only"), "got: {out}");
}

#[test]
fn context_sizes_render_human_readably() {
    assert_eq!(fmt_ctx(1_000_000), "1M");
    assert_eq!(fmt_ctx(1_050_000), "1.05M");
    assert_eq!(fmt_ctx(400_000), "400K");
    assert_eq!(fmt_ctx(200_000), "200K");
}
