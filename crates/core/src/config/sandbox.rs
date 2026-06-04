//! On-disk sandbox settings. Split out of `config.rs` to keep that file under
//! the line budget; the type is re-exported as `crate::config::SandboxConfig`.

use serde::{Deserialize, Serialize};

/// Environment variables that override the stored sandbox settings at runtime.
/// Read fresh on every [`SandboxConfig::effective_mode`]/[`effective_network`]
/// call so they apply everywhere (TUI, `chat`, `doctor`) without being written
/// back to `config.json`.
const SANDBOX_MODE_ENV: &str = "TOMTE_SANDBOX_MODE";
const SANDBOX_NETWORK_ENV: &str = "TOMTE_SANDBOX_NETWORK";

/// Sandbox settings for `run_shell`. Stored as plain values and parsed into the
/// runtime policy at the call site (mirrors `default_permission_mode`). The
/// default — `workspace-write` with network OFF — lets autonomous commands read
/// anywhere but write only inside the workspace, and blocks outbound network so
/// a prompt-injected command can't exfiltrate. Set `mode` to `read-only` to
/// forbid writes entirely, or `danger-full-access` to disable the sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// `read-only` | `workspace-write` | `danger-full-access`.
    #[serde(default = "default_sandbox_mode")]
    pub mode: String,
    /// Allow outbound network from sandboxed commands (only meaningful in
    /// `workspace-write`). Off by default; enable per project for builds that
    /// fetch dependencies (`cargo`, `npm`, `pip`, …).
    #[serde(default)]
    pub network: bool,
    /// Extra absolute directories sandboxed commands may write to, beyond the
    /// workspace and the system temp dir.
    #[serde(default)]
    pub writable_roots: Vec<String>,
    /// Runtime-only mode override from the `--sandbox` CLI flag. `serde(skip)`
    /// keeps it out of `config.json` in both directions, so a one-off `chat`/`run`
    /// override can never leak into the persisted config (the TUI saves the whole
    /// `Config`). Highest precedence — see [`effective_mode`](Self::effective_mode).
    #[serde(skip)]
    pub mode_override: Option<String>,
    /// Runtime-only network override from `--sandbox-allow-net`. Same
    /// non-persisted semantics as [`mode_override`](Self::mode_override).
    #[serde(skip)]
    pub network_override: Option<bool>,
}

fn default_sandbox_mode() -> String {
    "workspace-write".to_string()
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: default_sandbox_mode(),
            network: false,
            writable_roots: Vec::new(),
            mode_override: None,
            network_override: None,
        }
    }
}

impl SandboxConfig {
    /// Effective enforcement mode after applying overrides, in precedence order:
    /// the `--sandbox` CLI flag (`mode_override`) > the `TOMTE_SANDBOX_MODE` env
    /// var > the stored `mode`. The result is interpreted by
    /// `SandboxMode::from_config_str` (unknown values fall back to the safe
    /// `workspace-write`); an unrecognized env value is ignored with a warning so
    /// the stored mode stands rather than being silently downgraded.
    pub fn effective_mode(&self) -> String {
        if let Some(m) = &self.mode_override {
            return m.clone();
        }
        if let Ok(raw) = std::env::var(SANDBOX_MODE_ENV) {
            match parse_mode_override(Some(&raw)) {
                Some(m) => return m,
                None if !raw.trim().is_empty() => {
                    tracing::warn!("ignoring unrecognized {SANDBOX_MODE_ENV}={raw:?}");
                }
                None => {}
            }
        }
        self.mode.clone()
    }

    /// Effective network flag, in precedence order: `--sandbox-allow-net`
    /// (`network_override`) > `TOMTE_SANDBOX_NETWORK` env > the stored `network`.
    pub fn effective_network(&self) -> bool {
        if let Some(n) = self.network_override {
            return n;
        }
        if let Ok(raw) = std::env::var(SANDBOX_NETWORK_ENV) {
            match parse_network_override(Some(&raw)) {
                Some(n) => return n,
                None if !raw.trim().is_empty() => {
                    tracing::warn!("ignoring unrecognized {SANDBOX_NETWORK_ENV}={raw:?}");
                }
                None => {}
            }
        }
        self.network
    }
}

/// Parse a sandbox-mode override (CLI flag or env var) into its canonical
/// lowercase form. Pure (no logging) so it is trivially testable; `None` for an
/// absent, empty, or unrecognized value. The accepted synonyms mirror
/// `SandboxMode::from_config_str`.
pub(crate) fn parse_mode_override(raw: Option<&str>) -> Option<String> {
    let v = raw?.trim().to_ascii_lowercase();
    match v.as_str() {
        "read-only" | "readonly" | "workspace-write" | "danger-full-access" | "danger" | "off" => {
            Some(v)
        }
        _ => None,
    }
}

/// Parse a boolean network override (CLI flag or env var). Accepts common
/// truthy/falsey spellings; `None` for an absent, empty, or unrecognized value.
pub(crate) fn parse_network_override(raw: Option<&str>) -> Option<bool> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn mode_override_parses_known_and_rejects_unknown() {
        assert_eq!(
            parse_mode_override(Some("read-only")).as_deref(),
            Some("read-only")
        );
        assert_eq!(
            parse_mode_override(Some("  Workspace-Write ")).as_deref(),
            Some("workspace-write")
        );
        assert_eq!(
            parse_mode_override(Some("danger")).as_deref(),
            Some("danger")
        );
        assert_eq!(parse_mode_override(Some("off")).as_deref(), Some("off"));
        assert_eq!(parse_mode_override(Some("bogus")), None);
        assert_eq!(parse_mode_override(Some("   ")), None);
        assert_eq!(parse_mode_override(Some("")), None);
        assert_eq!(parse_mode_override(None), None);
    }

    #[test]
    fn network_override_parses_truthy_and_falsey() {
        for t in ["1", "true", "ON", "yes"] {
            assert_eq!(parse_network_override(Some(t)), Some(true), "{t}");
        }
        for f in ["0", "false", "Off", "no"] {
            assert_eq!(parse_network_override(Some(f)), Some(false), "{f}");
        }
        assert_eq!(parse_network_override(Some("maybe")), None);
        assert_eq!(parse_network_override(Some("")), None);
        assert_eq!(parse_network_override(None), None);
    }

    #[test]
    fn cli_mode_override_beats_stored() {
        // The CLI override wins regardless of env, so this is deterministic even
        // if TOMTE_SANDBOX_MODE happens to be set in the runner's environment.
        let mut cfg = SandboxConfig::default();
        cfg.mode = "read-only".to_string();
        cfg.mode_override = Some("danger-full-access".to_string());
        assert_eq!(cfg.effective_mode(), "danger-full-access");
    }

    #[test]
    fn cli_network_override_beats_stored() {
        let mut cfg = SandboxConfig::default();
        cfg.network_override = Some(true);
        assert!(cfg.effective_network());
        cfg.network_override = Some(false);
        assert!(!cfg.effective_network());
    }
}
