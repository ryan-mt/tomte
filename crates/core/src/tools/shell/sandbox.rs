//! OS-level sandbox for `run_shell` child processes.
//!
//! The permission layer decides *whether* a command may run; this layer decides
//! *what it can touch once running*. They are orthogonal: even an allowed,
//! bypass-mode, or `--dangerously-skip-permissions` command executes inside the
//! sandbox, so a prompt-injected `curl … | sh` or `rm -rf ~` cannot reach the
//! network or write outside the workspace.
//!
//! Mechanism per OS:
//!   - **Linux**: a re-exec helper (`<exe> __sandbox --policy <json> -- sh -c
//!     <cmd>`) applies Landlock (filesystem) + seccomp (block `AF_INET`/
//!     `AF_INET6` sockets) to itself, single-threaded, then execs the real
//!     shell. Restrictions are inherited across `execve` and by every
//!     descendant. See [`linux`].
//!   - **macOS**: wraps the command with `sandbox-exec -p <SBPL profile>`.
//!   - **Windows**: a re-exec helper runs the command inside a Job Object with
//!     `KILL_ON_JOB_CLOSE` — best-effort process-tree cleanup only. Filesystem
//!     and network are NOT confined (no cheap Windows equivalent); `doctor`
//!     still reports the platform as unsandboxed. See [`windows`].
//!   - **Other platforms**: no enforcement — the command runs unsandboxed and
//!     `doctor` reports the gap.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::config::Config;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// Enforcement level. Vocabulary mirrors Codex for familiarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Read anywhere, write nothing, no network.
    ReadOnly,
    /// Read anywhere, write only inside the workspace (+ temp + configured
    /// roots). Network governed by the policy's `network` flag.
    WorkspaceWrite,
    /// No enforcement — the command runs with the process's full privileges.
    DangerFullAccess,
}

impl SandboxMode {
    pub fn from_config_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "read-only" | "readonly" => Self::ReadOnly,
            "danger-full-access" | "danger" | "off" => Self::DangerFullAccess,
            // Unknown values fall back to the safe default rather than wedging.
            _ => Self::WorkspaceWrite,
        }
    }
}

/// A resolved policy handed to the platform enforcer (and serialized to the
/// Linux helper). Read access is always "everywhere"; writes are confined to
/// `writable_roots` (empty ⇒ read-only); `network` toggles outbound sockets.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SandboxPolicy {
    writable_roots: Vec<PathBuf>,
    network: bool,
}

/// Resolve the effective policy for a command. `None` means "no sandbox"
/// (`danger-full-access`), which runs the command with full privileges.
fn resolve(config: &Config, cwd: &Path) -> Option<SandboxPolicy> {
    // `effective_mode`/`effective_network` fold in the `--sandbox` CLI flag and
    // the `TOMTE_SANDBOX_*` env vars over the stored config (CLI > env > file).
    match SandboxMode::from_config_str(&config.sandbox.effective_mode()) {
        SandboxMode::DangerFullAccess => None,
        SandboxMode::ReadOnly => Some(SandboxPolicy {
            writable_roots: Vec::new(),
            network: false,
        }),
        SandboxMode::WorkspaceWrite => {
            let mut roots = Vec::new();
            push_canonical(&mut roots, cwd);
            push_canonical(&mut roots, &std::env::temp_dir());
            for extra in &config.sandbox.writable_roots {
                push_canonical(&mut roots, Path::new(extra));
            }
            Some(SandboxPolicy {
                writable_roots: roots,
                network: config.sandbox.effective_network(),
            })
        }
    }
}

/// Canonicalize and de-duplicate a writable root. Canonicalizing resolves
/// symlinks (e.g. macOS `/tmp` → `/private/tmp`) so the enforcer's real-path
/// rules match. A path that can't be resolved is kept as-is (best effort).
fn push_canonical(roots: &mut Vec<PathBuf>, path: &Path) {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !roots.contains(&resolved) {
        roots.push(resolved);
    }
}

/// Build the `Command` that runs `command` under the OS sandbox configured in
/// `config`, working in `cwd`. Falls back to a plain shell when the sandbox is
/// disabled (`danger-full-access`) or unsupported on this platform.
pub fn shell_command(command: &str, config: &Config, cwd: &Path) -> Command {
    if let Some(policy) = resolve(config, cwd) {
        if let Some(cmd) = wrap_for_platform(command, &policy) {
            return cmd;
        }
        tracing::warn!(
            "sandbox is enabled but unsupported on this platform; running run_shell unsandboxed"
        );
    }
    plain_shell(command)
}

fn plain_shell(command: &str) -> Command {
    let mut cmd = Command::new(super::support::platform_shell_name());
    super::support::configure_platform_shell(&mut cmd, command);
    cmd
}

#[cfg(target_os = "linux")]
fn wrap_for_platform(command: &str, policy: &SandboxPolicy) -> Option<Command> {
    linux::wrap(command, policy)
}

#[cfg(target_os = "macos")]
fn wrap_for_platform(command: &str, policy: &SandboxPolicy) -> Option<Command> {
    macos::wrap(command, policy)
}

#[cfg(windows)]
fn wrap_for_platform(command: &str, policy: &SandboxPolicy) -> Option<Command> {
    windows::wrap(command, policy)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn wrap_for_platform(_command: &str, _policy: &SandboxPolicy) -> Option<Command> {
    None
}

/// If this process was launched as the sandbox helper (`<exe> __sandbox …`),
/// enforce the policy and exec the target — this never returns on success. A
/// normal launch returns immediately. Call FIRST in `main`, before arg parsing,
/// so the helper stays single-threaded when it restricts itself.
pub fn maybe_exec_helper() {
    let mut args = std::env::args_os();
    let _bin = args.next();
    let Some(sub) = args.next() else {
        return;
    };
    if sub.to_str() != Some("__sandbox") {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::exit(linux::run_helper(args));
    }
    #[cfg(windows)]
    {
        std::process::exit(windows::run_helper(args));
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = args;
        eprintln!("tomte: the sandbox helper is not supported on this platform");
        std::process::exit(70);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing_defaults_to_workspace_write() {
        assert_eq!(
            SandboxMode::from_config_str("read-only"),
            SandboxMode::ReadOnly
        );
        assert_eq!(
            SandboxMode::from_config_str("workspace-write"),
            SandboxMode::WorkspaceWrite
        );
        assert_eq!(
            SandboxMode::from_config_str("danger-full-access"),
            SandboxMode::DangerFullAccess
        );
        // Unknown / empty fall back to the safe default.
        assert_eq!(
            SandboxMode::from_config_str("nonsense"),
            SandboxMode::WorkspaceWrite
        );
        assert_eq!(
            SandboxMode::from_config_str(""),
            SandboxMode::WorkspaceWrite
        );
    }

    #[test]
    fn danger_full_access_resolves_to_no_sandbox() {
        let mut config = Config::default();
        config.sandbox.mode = "danger-full-access".to_string();
        assert!(resolve(&config, Path::new("/")).is_none());
    }

    #[test]
    fn workspace_write_confines_writes_and_blocks_network_by_default() {
        let config = Config::default(); // workspace-write, network off
        let policy = resolve(&config, Path::new(".")).expect("policy");
        assert!(!policy.network, "network must be off by default");
        assert!(
            !policy.writable_roots.is_empty(),
            "workspace-write must grant at least the cwd"
        );
    }

    #[test]
    fn read_only_grants_no_writable_roots() {
        let mut config = Config::default();
        config.sandbox.mode = "read-only".to_string();
        let policy = resolve(&config, Path::new(".")).expect("policy");
        assert!(policy.writable_roots.is_empty());
        assert!(!policy.network);
    }

    /// End-to-end of the wiring the helper-only integration tests skip: that
    /// `shell_command` (resolve → wrap) actually re-execs us as the `__sandbox`
    /// helper with the right policy for a confined mode.
    #[cfg(target_os = "linux")]
    #[test]
    fn workspace_write_wraps_as_sandbox_helper() {
        let config = Config::default(); // workspace-write, network off
        let cmd = shell_command("echo hi", &config, Path::new("."));
        let std_cmd = cmd.as_std();
        assert_eq!(
            std_cmd.get_program(),
            std::env::current_exe().unwrap().as_os_str(),
            "should re-exec our own binary as the sandbox helper"
        );
        let args: Vec<String> = std_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // __sandbox --policy <json> -- sh -c "echo hi"
        assert_eq!(args.first().map(String::as_str), Some("__sandbox"));
        assert_eq!(args.get(1).map(String::as_str), Some("--policy"));
        assert!(
            args[2].contains("\"network\":false"),
            "policy json: {}",
            args[2]
        );
        assert_eq!(&args[args.len() - 4..], ["--", "sh", "-c", "echo hi"]);
    }

    /// read-only must wrap with no writable roots (so the helper denies writes).
    #[cfg(target_os = "linux")]
    #[test]
    fn read_only_wraps_with_empty_writable_roots() {
        let mut config = Config::default();
        config.sandbox.mode = "read-only".to_string();
        let cmd = shell_command("ls", &config, Path::new("."));
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args[2].contains("\"writable_roots\":[]"),
            "policy json: {}",
            args[2]
        );
    }

    /// danger-full-access bypasses the helper entirely and runs a plain shell.
    #[test]
    fn danger_full_access_runs_plain_shell_not_helper() {
        let mut config = Config::default();
        config.sandbox.mode = "danger-full-access".to_string();
        let cmd = shell_command("echo hi", &config, Path::new("."));
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|a| a == "__sandbox"),
            "danger mode must not use the helper"
        );
        assert!(args.iter().any(|a| a == "echo hi"));
    }
}
