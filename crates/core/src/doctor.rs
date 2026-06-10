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

mod path;
mod sections;

pub(crate) use path::*;
use sections::*;

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
            hooks_section(),
            tools_section(),
        ],
    }
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
