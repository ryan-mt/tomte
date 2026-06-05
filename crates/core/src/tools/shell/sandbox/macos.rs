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
        let lossy = root.to_string_lossy();
        // A newline (or other control char) in a path could terminate the
        // `(subpath "...")` literal early and inject top-level SBPL directives
        // (e.g. re-allowing writes/network), defeating the confinement. Such a
        // path is never a legitimate writable root, so drop it — fail closed.
        if lossy.chars().any(|c| c.is_control()) {
            continue;
        }
        // SBPL string literal: escape backslashes and quotes.
        let escaped = lossy.replace('\\', "\\\\").replace('"', "\\\"");
        p.push_str("    (subpath \"");
        p.push_str(&escaped);
        p.push_str("\")\n");
    }
    p.push_str(")\n");
    p
}

#[cfg(test)]
mod tests {
    use super::{build_profile, SandboxPolicy};
    use std::path::PathBuf;

    #[test]
    fn profile_denies_writes_and_network_by_default() {
        let policy = SandboxPolicy {
            writable_roots: vec![PathBuf::from("/work/ws")],
            network: false,
        };
        let p = build_profile(&policy);
        assert!(p.contains("(version 1)"));
        assert!(p.contains("(deny network*)"), "profile: {p}");
        assert!(p.contains("(deny file-write*)"), "profile: {p}");
        assert!(p.contains("(subpath \"/work/ws\")"), "profile: {p}");
        // Harmless device nodes stay writable in every mode.
        assert!(p.contains("/dev/null"), "profile: {p}");
    }

    #[test]
    fn profile_allows_network_when_enabled() {
        let policy = SandboxPolicy {
            writable_roots: Vec::new(),
            network: true,
        };
        let p = build_profile(&policy);
        assert!(
            !p.contains("(deny network*)"),
            "network must not be denied when enabled: {p}"
        );
        assert!(p.contains("(deny file-write*)"), "profile: {p}");
    }

    #[test]
    fn profile_escapes_quotes_in_paths() {
        let policy = SandboxPolicy {
            writable_roots: vec![PathBuf::from("/weird\"dir")],
            network: false,
        };
        let p = build_profile(&policy);
        // The `"` must be backslash-escaped inside the SBPL string literal.
        assert!(p.contains("/weird\\\"dir"), "profile: {p}");
    }

    #[test]
    fn profile_drops_paths_with_control_chars() {
        // A newline-bearing root must be dropped, not emitted, so it can't
        // inject a top-level directive into the profile.
        let policy = SandboxPolicy {
            writable_roots: vec![PathBuf::from("/work\n(allow file-write*)")],
            network: false,
        };
        let p = build_profile(&policy);
        assert!(!p.contains("/work"), "poisoned root must be dropped: {p}");
        // Exactly the one intended deny remains; no injected allow.
        assert_eq!(p.matches("(allow file-write*").count(), 1, "profile: {p}");
    }
}
