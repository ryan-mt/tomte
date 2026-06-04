//! macOS sandbox enforcement via `sandbox-exec` and an SBPL profile.
//!
//! `sandbox-exec` is deprecated by Apple but still ships and works; it is the
//! same mechanism Codex uses. We wrap the command in a `(version 1)` profile
//! that allows everything by default, then denies file writes (re-allowing only
//! the writable roots plus harmless device nodes) and — unless network is
//! allowed — denies all networking.

use tokio::process::Command;

use super::SandboxPolicy;

pub(super) fn wrap(command: &str, policy: &SandboxPolicy) -> Option<Command> {
    let profile = build_profile(policy);
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p")
        .arg(profile)
        .arg("/bin/sh")
        .arg("-c")
        .arg(command);
    Some(cmd)
}

/// Build an SBPL `(version 1)` profile. Read access stays open because only
/// `file-write*` and `network*` are denied. Writable roots must be real paths
/// (the caller canonicalizes, resolving `/tmp` → `/private/tmp`).
fn build_profile(policy: &SandboxPolicy) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n");
    if !policy.network {
        p.push_str("(deny network*)\n");
    }
    p.push_str("(deny file-write*)\n");
    // Always re-allow harmless device nodes (so `> /dev/null` works in every
    // mode), then the policy's writable roots.
    p.push_str("(allow file-write*\n");
    p.push_str("    (literal \"/dev/null\")\n");
    p.push_str("    (literal \"/dev/zero\")\n");
    p.push_str("    (literal \"/dev/tty\")\n");
    p.push_str("    (subpath \"/dev/fd\")\n");
    for root in &policy.writable_roots {
        // SBPL string literal: escape backslashes and quotes.
        let escaped = root
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        p.push_str("    (subpath \"");
        p.push_str(&escaped);
        p.push_str("\")\n");
    }
    p.push_str(")\n");
    p
}
