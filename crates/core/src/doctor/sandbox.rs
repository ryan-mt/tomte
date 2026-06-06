//! Sandbox diagnostics for `doctor` — reports the configured mode and whether
//! this platform can actually enforce it, so a user can confirm at a glance that
//! autonomous `run_shell` commands are confined.

use super::{Check, Section};
use crate::config;

pub(super) fn section() -> Section {
    let cfg = config::load();
    // Report the EFFECTIVE mode so `doctor` reflects a `TOMTE_SANDBOX_MODE` env
    // override (the CLI flag is per-run and not visible here).
    let mode = cfg.sandbox.effective_mode().trim().to_ascii_lowercase();
    let mechanism = mechanism();

    let check = match mode.as_str() {
        "danger-full-access" | "danger" | "off" => Check::warn(
            "mode danger-full-access — sandbox OFF; run_shell commands run with full privileges",
        ),
        "read-only" | "readonly" => match &mechanism {
            Mechanism::Active(m) => Check::ok(format!(
                "mode read-only — {m}; commands can read but not write or reach the network"
            )),
            Mechanism::Inactive(m) => Check::warn(format!(
                "mode read-only, but {m} is not active on this kernel — run_shell commands are refused, not run unconfined"
            )),
            Mechanism::Unsupported => Check::warn(
                "mode read-only, but no OS sandbox on this platform — commands run unsandboxed",
            ),
        },
        // workspace-write (the default) and any unknown value.
        _ => {
            let net = if cfg.sandbox.effective_network() {
                "network allowed"
            } else {
                "network blocked"
            };
            match &mechanism {
                Mechanism::Active(m) => Check::ok(format!(
                    "mode workspace-write — {m}; writes confined to the workspace, {net}"
                )),
                Mechanism::Inactive(m) => Check::warn(format!(
                    "mode workspace-write, but {m} is not active on this kernel — run_shell commands are refused, not run unconfined"
                )),
                Mechanism::Unsupported => Check::warn(
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

/// Enforcement mechanism for this platform: compiled in and active, compiled in
/// but inactive at runtime (e.g. Linux without active Landlock — the helper then
/// fails closed and refuses commands), or no OS sandbox at all. Each variant is
/// constructed on only some platforms, so suppress the per-target dead-code lint.
#[allow(dead_code)]
enum Mechanism {
    Active(&'static str),
    Inactive(&'static str),
    Unsupported,
}

fn mechanism() -> Mechanism {
    #[cfg(target_os = "linux")]
    {
        if landlock_active() {
            Mechanism::Active("Landlock + seccomp")
        } else {
            Mechanism::Inactive("Landlock + seccomp")
        }
    }
    #[cfg(target_os = "macos")]
    {
        Mechanism::Active("sandbox-exec")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Mechanism::Unsupported
    }
}

/// Best-effort check that Landlock is actually active on this kernel: the LSM
/// list at `/sys/kernel/security/lsm` includes `landlock` when it is. When it's
/// absent, the sandbox helper's `restrict_self()` degrades to "not enforced" and
/// fails closed, so report that rather than claiming filesystem confinement.
#[cfg(target_os = "linux")]
fn landlock_active() -> bool {
    std::fs::read_to_string("/sys/kernel/security/lsm")
        .map(|lsm| lsm.split(',').any(|m| m.trim() == "landlock"))
        .unwrap_or(false)
}
