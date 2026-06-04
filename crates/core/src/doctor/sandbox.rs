//! Sandbox diagnostics for `doctor` — reports the configured mode and whether
//! this platform can actually enforce it, so a user can confirm at a glance that
//! autonomous `run_shell` commands are confined.

use super::{Check, Section};
use crate::config;

pub(super) fn section() -> Section {
    let cfg = config::load();
    let mode = cfg.sandbox.mode.trim().to_ascii_lowercase();
    let mechanism = mechanism();

    let check = match mode.as_str() {
        "danger-full-access" | "danger" | "off" => Check::warn(
            "mode danger-full-access — sandbox OFF; run_shell commands run with full privileges",
        ),
        "read-only" | "readonly" => match mechanism {
            Some(m) => Check::ok(format!(
                "mode read-only — {m}; commands can read but not write or reach the network"
            )),
            None => Check::warn(
                "mode read-only, but no OS sandbox on this platform — commands run unsandboxed",
            ),
        },
        // workspace-write (the default) and any unknown value.
        _ => {
            let net = if cfg.sandbox.network {
                "network allowed"
            } else {
                "network blocked"
            };
            match mechanism {
                Some(m) => Check::ok(format!(
                    "mode workspace-write — {m}; writes confined to the workspace, {net}"
                )),
                None => Check::warn(
                    "mode workspace-write, but no OS sandbox on this platform — commands run unsandboxed",
                ),
            }
        }
    };

    Section {
        title: "Sandbox".to_string(),
        checks: vec![check],
    }
}

/// The enforcement mechanism compiled in for this platform, or `None` where no
/// OS sandbox is available (commands then run unsandboxed).
fn mechanism() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        Some("Landlock + seccomp")
    }
    #[cfg(target_os = "macos")]
    {
        Some("sandbox-exec")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}
