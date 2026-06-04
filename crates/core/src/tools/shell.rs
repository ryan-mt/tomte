//! Shell tools: `run_shell` (foreground/background exec), `bash_output`,
//! and `kill_shell`, plus destructive-command classification. Each concern
//! lives in a submodule; the public tools and `classify_danger` are unchanged.

mod danger;
mod poll;
mod run;
pub mod sandbox;
mod support;

#[cfg(test)]
mod test_support;

pub use danger::classify_danger;
pub use poll::{BashOutput, KillShell};
pub use run::RunShell;
