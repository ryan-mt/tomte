use super::*;

pub(super) fn runtime_section() -> Section {
    Section {
        title: "tomte".to_string(),
        checks: vec![Check::ok(format!("version {}", env!("CARGO_PKG_VERSION")))],
    }
}

pub(super) fn auth_section() -> Section {
    let record = auth::load_auth().unwrap_or_default();
    let mode = auth::effective_mode_with_env(&record);
    let mut checks = vec![match mode {
        AuthMode::None => Check::error("not signed in — run `tomte login`"),
        AuthMode::OpenaiApiKey => Check::ok("signed in with OpenAI API key"),
        AuthMode::OpenaiOauth => Check::ok("signed in with ChatGPT (OpenAI OAuth)"),
        AuthMode::AnthropicApiKey => Check::ok("signed in with Anthropic API key"),
        AuthMode::AnthropicOauth => Check::ok("signed in with Claude Pro/Max OAuth"),
    }];

    // auth.json lives next to config.json. Reconstructed here rather than read
    // from the (private) storage helper, but the location is stable.
    let auth_path = config::config_dir().join("auth.json");
    if auth_path.is_file() {
        checks.push(Check::ok(format!("auth.json  {}", auth_path.display())));
        if let Some(c) = auth_file_permission_check(&auth_path) {
            checks.push(c);
        }
    } else if !matches!(mode, AuthMode::None) {
        checks.push(Check::info(
            "no auth.json — using credentials from the environment",
        ));
    }

    let cov = auth::credential_coverage();
    checks.push(Check::info(format!(
        "credentials — OpenAI OAuth: {} · OpenAI key: {} · Anthropic OAuth: {} · Anthropic key: {}",
        cov.openai_oauth.label(),
        cov.openai_api_key.label(),
        cov.anthropic_oauth.label(),
        cov.anthropic_api_key.label(),
    )));

    Section {
        title: "Authentication".to_string(),
        checks,
    }
}

pub(super) fn config_section(cwd: &Path) -> Section {
    let cfg = config::load_for_cwd(cwd);
    let mut checks = Vec::new();

    let cfg_file = config::config_file();
    if cfg_file.is_file() {
        checks.push(Check::ok(format!("config file  {}", cfg_file.display())));
    } else {
        checks.push(Check::info("no config file yet — using defaults"));
    }
    let project_overlay = config::project_config_file(cwd);
    if project_overlay.is_file() {
        checks.push(Check::info(format!(
            "project overlay  {}",
            project_overlay.display()
        )));
    }

    checks.push(
        match config::normalize_reasoning_effort(&cfg.reasoning_effort) {
            Some(_) => Check::ok(format!("reasoning_effort  {}", cfg.reasoning_effort)),
            None => Check::warn(format!(
                "reasoning_effort `{}` is not one of: {}",
                cfg.reasoning_effort,
                config::VALID_REASONING_EFFORTS.join(", ")
            )),
        },
    );
    checks.push(match config::normalize_verbosity(&cfg.verbosity) {
        Some(_) => Check::ok(format!("verbosity  {}", cfg.verbosity)),
        None => Check::warn(format!(
            "verbosity `{}` is not one of: {}",
            cfg.verbosity,
            config::VALID_VERBOSITIES.join(", ")
        )),
    });

    // Custom OpenAI-compatible providers, sorted for stable output. A provider
    // with no resolvable key can't authenticate, so flag it.
    let mut ids: Vec<&String> = cfg.providers.keys().collect();
    ids.sort();
    for id in ids {
        let pc = &cfg.providers[id];
        if pc.resolve_api_key().is_empty() {
            checks.push(Check::warn(format!(
                "provider '{id}' ({}) has no API key — set providers.{id}.api_key or api_key_env",
                pc.base_url
            )));
        } else {
            checks.push(Check::ok(format!("provider '{id}'  {}", pc.base_url)));
        }
    }

    Section {
        title: "Configuration".to_string(),
        checks,
    }
}

pub(super) fn model_routing_section(cwd: &Path) -> Section {
    let cfg = config::load_for_cwd(cwd);
    let cov = auth::credential_coverage();
    Section {
        title: "Model routing".to_string(),
        checks: vec![model_routing_check(&cfg.model, &cov, &cfg.providers)],
    }
}

/// Cross-check the configured model against available credentials — the single
/// most useful diagnostic, since a model pointing at a provider you aren't
/// signed in to fails every turn with an opaque auth error.
///
/// Pure so it can be unit-tested across the credential/provider matrix.
pub(super) fn model_routing_check(
    model: &str,
    coverage: &CredentialCoverage,
    providers: &HashMap<String, ProviderConfig>,
) -> Check {
    // A `<id>/<model>` spec routes through a custom provider entry OR a built-in
    // preset (groq, openrouter, …, plus the keyless local ollama/lmstudio), not
    // the built-in OpenAI/Anthropic paths. Mirror the real routing in
    // `LlmClient::for_config` / `Config::effective_context_limit`, which both
    // fall back to `builtin_provider`; without that fallback the check misroutes
    // a valid preset model to the OpenAI/Anthropic credential check and reports a
    // false Error (or a false OK), exactly when a user runs `doctor` to confirm
    // their setup.
    if let Some((prefix, _rest)) = model.split_once('/') {
        if let Some(pc) = providers.get(prefix) {
            return provider_routing_check(model, prefix, pc, false);
        }
        if let Some(pc) = crate::config::builtin_provider(prefix) {
            return provider_routing_check(model, prefix, &pc, true);
        }
    }

    // Built-in routing. `parse_model` honours an explicit `anthropic/` or
    // `openai/` prefix and otherwise falls back to the model-name heuristic.
    let (provider, _bare) = Provider::parse_model(model);
    let present = match provider {
        Provider::Anthropic => {
            coverage.anthropic_oauth != CredentialPresence::Missing
                || coverage.anthropic_api_key != CredentialPresence::Missing
        }
        Provider::OpenAi => {
            coverage.openai_oauth != CredentialPresence::Missing
                || coverage.openai_api_key != CredentialPresence::Missing
        }
    };
    let name = provider.display_name();
    if present {
        Check::ok(format!("{model} → {name} (credentials present)"))
    } else {
        Check::error(format!(
            "{model} → {name}, but no {name} credentials are configured — run `tomte login`"
        ))
    }
}

/// Verdict for a `<id>/<model>` routed through a custom or built-in provider.
/// `keyless_ok` lets a built-in *local* preset (Ollama / LM Studio, which declare
/// no key env and need none) pass without a key; any other provider with an
/// empty resolved key still warns.
pub(super) fn provider_routing_check(
    model: &str,
    prefix: &str,
    pc: &ProviderConfig,
    keyless_ok: bool,
) -> Check {
    let keyless = keyless_ok && pc.api_key.is_none() && pc.api_key_env.is_none();
    if !keyless && pc.resolve_api_key().is_empty() {
        Check::warn(format!(
            "{model} → provider '{prefix}' has no API key — set its API-key env var or providers.{prefix}.api_key"
        ))
    } else {
        Check::ok(format!("{model} → provider '{prefix}' ({})", pc.base_url))
    }
}

pub(super) fn mcp_section() -> Section {
    let servers = crate::mcp::load_servers_config();
    let checks = if servers.is_empty() {
        vec![Check::info("no MCP servers configured")]
    } else {
        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let cfg = &servers[n];
                if binary_on_path(&cfg.command) {
                    let line = format!("{n}  {} {}", cfg.command, cfg.args.join(" "));
                    Check::ok(line.trim_end().to_string())
                } else {
                    Check::warn(format!(
                        "{n}: command '{}' not found on PATH — server will fail to start",
                        cfg.command
                    ))
                }
            })
            .collect()
    };
    Section {
        title: "MCP servers".to_string(),
        checks,
    }
}

pub(super) fn discovery_section(cwd: &Path) -> Section {
    let skills = crate::skill::discover(cwd).len();
    let subagents = crate::subagent::load_all(cwd).len();
    let hc = crate::hooks::load().config;
    let hooks = hc.pre_tool_use.len()
        + hc.post_tool_use.len()
        + hc.user_prompt_submit.len()
        + hc.session_start.len()
        + hc.stop.len();
    Section {
        title: "Skills, subagents & hooks".to_string(),
        checks: vec![
            Check::info(format!("{skills} skill{} discovered", plural(skills))),
            Check::info(format!(
                "{subagents} subagent{} discovered",
                plural(subagents)
            )),
            Check::info(format!("{hooks} hook{} configured", plural(hooks))),
        ],
    }
}

/// Per-hook health: which shell runs hooks on this OS, and for each configured
/// hook whether its command's program resolves on PATH — so a missing tool
/// shows up here instead of silently failing the first time the hook fires.
pub(super) fn hooks_section() -> Section {
    let cfg = crate::hooks::load().config;
    let mut checks = vec![Check::info(format!(
        "hook shell on this OS: {}",
        crate::hooks::hook_shell_label()
    ))];

    let mut entries: Vec<(&str, &crate::hooks::HookEntry)> = Vec::new();
    for h in &cfg.pre_tool_use {
        entries.push(("PreToolUse", h));
    }
    for h in &cfg.post_tool_use {
        entries.push(("PostToolUse", h));
    }
    for h in &cfg.user_prompt_submit {
        entries.push(("UserPromptSubmit", h));
    }
    for h in &cfg.session_start {
        entries.push(("SessionStart", h));
    }
    for h in &cfg.stop {
        entries.push(("Stop", h));
    }

    if entries.is_empty() {
        checks.push(Check::info(
            "no hooks configured — enable a preset with `tomte hooks enable <id>`",
        ));
    } else {
        for (event, entry) in entries {
            checks.push(hook_check(event, entry));
        }
    }

    Section {
        title: "Hooks".to_string(),
        checks,
    }
}

/// Validate one configured hook: does its command's program resolve on PATH? A
/// missing program is a warning (the hook would fail at runtime), softened
/// because it may legitimately be a shell builtin or alias.
pub(super) fn hook_check(event: &str, entry: &crate::hooks::HookEntry) -> Check {
    match hook_program(&entry.command) {
        Some(prog) if !binary_on_path(prog) => Check::warn(format!(
            "{event} {} → `{prog}` not found on PATH (ok if a shell builtin or alias)",
            entry.matcher
        )),
        _ => Check::ok(format!("{event} {} → {}", entry.matcher, entry.command)),
    }
}

/// The program a hook command would execute: the first whitespace token,
/// skipping leading `VAR=value` environment assignments. Good enough to flag a
/// missing binary; deliberately not a full shell parser.
pub(super) fn hook_program(command: &str) -> Option<&str> {
    for tok in command.split_whitespace() {
        if is_env_assignment(tok) {
            continue;
        }
        return Some(tok.trim_matches(|c| c == '"' || c == '\''));
    }
    None
}

/// Is `tok` a leading `KEY=value` env assignment (so the program is the next
/// token)? Requires a non-empty, identifier-like key before the first `=`.
pub(super) fn is_env_assignment(tok: &str) -> bool {
    let Some((key, _)) = tok.split_once('=') else {
        return false;
    };
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(super) fn tools_section() -> Section {
    let git = binary_on_path("git");
    let rg = binary_on_path("rg");
    let grep = binary_on_path("grep");
    let mut checks = vec![if git {
        Check::ok("git")
    } else {
        Check::warn("git not found — worktree isolation and repo-root memory degrade")
    }];
    checks.push(if rg {
        Check::ok("ripgrep (rg)")
    } else {
        Check::warn(
            "ripgrep (rg) not found — optional; search uses grep, then a built-in gitignore-aware walk (skips node_modules/target, a bit slower than rg)",
        )
    });
    // grep is the middle search backend; below it, glob/grep use a native,
    // dependency-free fallback (recursive walk + in-process regex), so search
    // still works with neither installed — just slower and without .gitignore
    // awareness. A missing grep is therefore never a hard error.
    checks.push(if grep {
        Check::ok("grep")
    } else if rg {
        Check::info("grep not found (ripgrep present, so search still works)")
    } else {
        Check::warn(
            "neither ripgrep (rg) nor grep found — grep/glob use a built-in gitignore-aware fallback (skips node_modules/target, a bit slower than rg)",
        )
    });
    Section {
        title: "External tools".to_string(),
        checks,
    }
}

/// `#[cfg(unix)]` permission check: tomte writes `auth.json` as `0600`, so a
/// looser mode means the credential file is exposed and is worth flagging.
#[cfg(unix)]
pub(super) fn auth_file_permission_check(path: &Path) -> Option<Check> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    Some(if mode == 0o600 {
        Check::ok(format!("auth.json permissions {mode:o}"))
    } else {
        Check::warn(format!(
            "auth.json permissions {mode:o} (expected 600; run `chmod 600 {}`)",
            path.display()
        ))
    })
}

/// Non-Unix platforms don't carry POSIX mode bits, so there's nothing to check.
#[cfg(not(unix))]
pub(super) fn auth_file_permission_check(_path: &Path) -> Option<Check> {
    None
}
