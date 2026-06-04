//! On-disk sandbox settings. Split out of `config.rs` to keep that file under
//! the line budget; the type is re-exported as `crate::config::SandboxConfig`.

use serde::{Deserialize, Serialize};

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
        }
    }
}
