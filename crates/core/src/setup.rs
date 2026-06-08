//! First-run environment setup hints.
//!
//! Detects the host OS and an available package manager, and for any *important*
//! external tool missing from PATH produces a copy-pasteable install command for
//! THIS machine. Deliberately read-only: it only tells the user what to run — it
//! never runs an installer (that stays the user's explicit choice).
//!
//! Only `git` is treated as important: worktree isolation and repo-root memory
//! shell out to it. Ripgrep is intentionally NOT surfaced here — the search
//! fallback is gitignore-aware now, so `rg` is a minor speedup, not a
//! requirement, and nagging about it would be noise.

use crate::doctor::binary_on_path;

/// One missing tool: why it matters and how to install it on this OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupItem {
    pub tool: &'static str,
    pub why: &'static str,
    pub install: String,
}

/// Important external tools missing from PATH, each with a per-OS install
/// command. Empty when the environment is ready (the common case) — the TUI
/// shows a setup card only when this is non-empty.
pub fn missing_important_tools() -> Vec<SetupItem> {
    let mut out = Vec::new();
    if !binary_on_path("git") {
        out.push(SetupItem {
            tool: "git",
            why: "worktree isolation and repo-root memory need it",
            install: install_command("git", std::env::consts::OS, detect_package_manager()),
        });
    }
    out
}

/// First known package manager found on PATH for this OS, or `None` to fall back
/// to a download URL. Order = preferred first.
fn detect_package_manager() -> Option<&'static str> {
    let candidates: &[&str] = match std::env::consts::OS {
        "windows" => &["winget", "scoop", "choco"],
        "macos" => &["brew", "port"],
        _ => &["apt-get", "dnf", "pacman", "zypper", "apk"],
    };
    candidates.iter().copied().find(|c| binary_on_path(c))
}

/// Pure: the install command for `tool` given the OS id (`std::env::consts::OS`)
/// and a detected package manager. Pure so the per-OS/per-manager matrix is
/// unit-tested without touching the real PATH. No detected manager → a download
/// hint instead of a guessed command.
fn install_command(tool: &str, os: &str, pm: Option<&str>) -> String {
    match pm {
        Some("winget") => format!("winget install --id {} -e", winget_id(tool)),
        Some("scoop") => format!("scoop install {tool}"),
        Some("choco") => format!("choco install {tool} -y"),
        Some("brew") => format!("brew install {tool}"),
        Some("port") => format!("sudo port install {tool}"),
        Some("apt-get") => format!("sudo apt-get install -y {tool}"),
        Some("dnf") => format!("sudo dnf install -y {tool}"),
        Some("pacman") => format!("sudo pacman -S --noconfirm {tool}"),
        Some("zypper") => format!("sudo zypper install -y {tool}"),
        Some("apk") => format!("sudo apk add {tool}"),
        _ => download_hint(tool, os),
    }
}

/// winget keys on vendor package ids, not bare tool names.
fn winget_id(tool: &str) -> &str {
    match tool {
        "git" => "Git.Git",
        "rg" => "BurntSushi.ripgrep.MSVC",
        other => other,
    }
}

/// Last-resort hint when no package manager is detected.
fn download_hint(tool: &str, os: &str) -> String {
    match (tool, os) {
        ("git", "windows") => "download Git from https://git-scm.com/download/win".into(),
        ("git", "macos") => {
            "run `xcode-select --install` (installs git), or see https://git-scm.com/download/mac"
                .into()
        }
        ("git", _) => "install git from https://git-scm.com/download/linux".into(),
        (t, _) => format!("install `{t}` with your OS package manager"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_install_command_per_package_manager() {
        assert_eq!(
            install_command("git", "windows", Some("winget")),
            "winget install --id Git.Git -e"
        );
        assert_eq!(
            install_command("git", "windows", Some("scoop")),
            "scoop install git"
        );
        assert_eq!(
            install_command("git", "macos", Some("brew")),
            "brew install git"
        );
        assert_eq!(
            install_command("git", "linux", Some("apt-get")),
            "sudo apt-get install -y git"
        );
        assert_eq!(
            install_command("git", "linux", Some("pacman")),
            "sudo pacman -S --noconfirm git"
        );
    }

    #[test]
    fn falls_back_to_a_download_hint_without_a_package_manager() {
        assert!(install_command("git", "windows", None).contains("git-scm.com"));
        assert!(install_command("git", "macos", None).contains("xcode-select"));
        assert!(install_command("git", "linux", None).contains("git-scm.com"));
    }
}
