//! Setup diagnostics for the `tomte doctor` subcommand and the `/doctor`
//! slash command.
//!
//! [`diagnose`] inspects the environment — credentials, config, MCP servers,
//! discovered skills/subagents/hooks, and the external binaries tomte shells
//! out to — and returns a structured [`Report`]. It is deliberately **read-only
//! and side-effect-free**: it never mutates state, never writes files, and never
//! spawns an MCP server (a health check must be fast and safe to run when
//! something is already broken). The same report backs both the headless CLI
//! command and the in-TUI slash command, so the wording and pass/fail logic stay
//! in one place.

use std::collections::HashMap;
use std::path::Path;

use crate::auth::{self, AuthMode, CredentialCoverage, CredentialPresence};
use crate::config::{self, ProviderConfig};
use crate::provider::Provider;

/// Outcome of a single check. `Info` is neutral context (counts, paths) that
/// never counts toward the warning/error tally; only `Warn` and `Error` signal
/// something the user may want to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Info,
    Warn,
    Error,
}

impl Status {
    /// Single-column glyph shown before each line. ASCII-fallback-friendly but
    /// uses the same check/warn marks the rest of the TUI already renders.
    pub fn glyph(self) -> &'static str {
        match self {
            Status::Ok => "✓",
            Status::Info => "·",
            Status::Warn => "⚠",
            Status::Error => "✗",
        }
    }
}

/// One diagnostic line: a status plus a fully-formed human message.
#[derive(Debug, Clone)]
pub struct Check {
    pub status: Status,
    pub message: String,
}

impl Check {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            status: Status::Ok,
            message: message.into(),
        }
    }
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            status: Status::Info,
            message: message.into(),
        }
    }
    pub fn warn(message: impl Into<String>) -> Self {
        Self {
            status: Status::Warn,
            message: message.into(),
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: Status::Error,
            message: message.into(),
        }
    }
}

/// A titled group of related checks (e.g. "Authentication").
#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub checks: Vec<Check>,
}

/// Tally of non-neutral checks, used for the summary line and the process exit
/// code of `tomte doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub ok: usize,
    pub warn: usize,
    pub error: usize,
}

/// A full diagnostic run: every section, in display order.
#[derive(Debug, Clone)]
pub struct Report {
    pub sections: Vec<Section>,
}

impl Report {
    /// Tally Ok/Warn/Error across every section. `Info` lines are ignored so a
    /// clean setup reports zero warnings even though it has many info lines.
    pub fn counts(&self) -> Counts {
        let mut c = Counts {
            ok: 0,
            warn: 0,
            error: 0,
        };
        for section in &self.sections {
            for check in &section.checks {
                match check.status {
                    Status::Ok => c.ok += 1,
                    Status::Warn => c.warn += 1,
                    Status::Error => c.error += 1,
                    Status::Info => {}
                }
            }
        }
        c
    }

    /// True if any check failed hard. `tomte doctor` exits non-zero on this so
    /// it can gate a setup script or CI step.
    pub fn has_errors(&self) -> bool {
        self.counts().error > 0
    }

    /// Plain-text render shared by the CLI (stdout) and the TUI (a system
    /// block). Ends with a one-line summary.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, section) in self.sections.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&section.title);
            out.push('\n');
            for check in &section.checks {
                out.push_str(&format!("  {} {}\n", check.status.glyph(), check.message));
            }
        }
        let c = self.counts();
        out.push_str(&format!(
            "\nSummary: {} ok · {} warning{} · {} error{}",
            c.ok,
            c.warn,
            plural(c.warn),
            c.error,
            plural(c.error),
        ));
        out
    }
}

/// Run every check against the given working directory and collect the report.
///
/// `cwd` scopes the project-local lookups — `.tomte/config.json`, and the
/// recursively-discovered skills/subagents — so the report reflects the
/// directory the user is actually working in.
pub fn diagnose(cwd: &Path) -> Report {
    Report {
        sections: vec![
            runtime_section(),
            auth_section(),
            config_section(cwd),
            sandbox::section(),
            model_routing_section(cwd),
            mcp_section(),
            discovery_section(cwd),
            tools_section(),
        ],
    }
}

fn runtime_section() -> Section {
    Section {
        title: "tomte".to_string(),
        checks: vec![Check::ok(format!("version {}", env!("CARGO_PKG_VERSION")))],
    }
}

fn auth_section() -> Section {
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

fn config_section(cwd: &Path) -> Section {
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

fn model_routing_section(cwd: &Path) -> Section {
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
fn model_routing_check(
    model: &str,
    coverage: &CredentialCoverage,
    providers: &HashMap<String, ProviderConfig>,
) -> Check {
    // A `<id>/<model>` spec whose prefix names a configured custom provider
    // routes through that endpoint, not the built-in OpenAI/Anthropic paths.
    if let Some((prefix, _rest)) = model.split_once('/') {
        if let Some(pc) = providers.get(prefix) {
            return if pc.resolve_api_key().is_empty() {
                Check::warn(format!(
                    "{model} → custom provider '{prefix}' has no API key (set providers.{prefix}.api_key or api_key_env)"
                ))
            } else {
                Check::ok(format!(
                    "{model} → custom provider '{prefix}' ({})",
                    pc.base_url
                ))
            };
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

fn mcp_section() -> Section {
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

fn discovery_section(cwd: &Path) -> Section {
    let skills = crate::skill::discover(cwd).len();
    let subagents = crate::subagent::load_all(cwd).len();
    let hooks = crate::hooks::load().config.pre_tool_use.len();
    Section {
        title: "Skills, subagents & hooks".to_string(),
        checks: vec![
            Check::info(format!("{skills} skill{} discovered", plural(skills))),
            Check::info(format!(
                "{subagents} subagent{} discovered",
                plural(subagents)
            )),
            Check::info(format!(
                "{hooks} PreToolUse hook{} configured",
                plural(hooks)
            )),
        ],
    }
}

fn tools_section() -> Section {
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
        Check::warn("ripgrep (rg) not found — the grep tool falls back to slower `grep -rn`")
    });
    // grep is the fallback search backend. Missing grep only matters when rg is
    // also missing — then the grep tool has nothing to run.
    checks.push(if grep {
        Check::ok("grep")
    } else if rg {
        Check::info("grep not found (ripgrep present, so search still works)")
    } else {
        Check::error("neither ripgrep (rg) nor grep found — the grep tool cannot run")
    });
    Section {
        title: "External tools".to_string(),
        checks,
    }
}

/// `#[cfg(unix)]` permission check: tomte writes `auth.json` as `0600`, so a
/// looser mode means the credential file is exposed and is worth flagging.
#[cfg(unix)]
fn auth_file_permission_check(path: &Path) -> Option<Check> {
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
fn auth_file_permission_check(_path: &Path) -> Option<Check> {
    None
}

/// Best-effort `which`: is `name` runnable? A bare name is searched on `$PATH`
/// (honouring `EXE_EXTENSION` on Windows); a name that already contains a path
/// separator is checked directly. Avoids spawning anything, so it can't hang.
fn binary_on_path(name: &str) -> bool {
    let p = Path::new(name);
    if p.is_absolute() || p.components().count() > 1 {
        return p.is_file();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let exe_ext = std::env::consts::EXE_EXTENSION;
    for dir in std::env::split_paths(&paths) {
        if dir.join(name).is_file() {
            return true;
        }
        if !exe_ext.is_empty() && dir.join(format!("{name}.{exe_ext}")).is_file() {
            return true;
        }
    }
    false
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

mod sandbox;

#[cfg(test)]
mod tests;
