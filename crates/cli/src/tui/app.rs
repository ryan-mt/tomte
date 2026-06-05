use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tomte_core::agent::{Agent, AgentEvent};
use tomte_core::auth::{self, AuthMode};
use tomte_core::client::LlmClient;
use tomte_core::config::{self, Config};
use tomte_core::provider::Provider;
use tomte_core::session::{SessionGoalSnapshot, SessionRecord};
use tomte_core::tools::{ApprovalMode, TodoItem, TodoStatus};

use super::clipboard;
use super::composer::{self, BangResult};
use super::input::TextInput;
use super::login::{self, LoginScreen, Stage as LoginStage};
use super::picker::{self, Picker};
use super::selection;
use super::ui;

/// Shared handle into the Agent's in-flight approval map. Cloned from
/// `Agent.pending_approvals` BEFORE the long-lived `run_turn` lock is taken so
/// the TUI can deliver Y/N decisions without blocking on the outer agent mutex.
pub type ApprovalHandle = std::sync::Arc<
    tokio::sync::Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
>;

mod agentevent;
mod blocks;
mod consts;
mod entry;
mod helpers;
mod keys;
mod mainloop;
mod methods;
mod overlay;
mod prompts;
mod resume;
mod slash;
mod slash2;
mod slash3;
mod summary;
mod turn;
mod types;

pub use agentevent::*;
pub use blocks::*;
pub use consts::*;
pub use entry::*;
pub use helpers::*;
pub use keys::*;
pub use mainloop::*;
pub use overlay::*;
pub use prompts::*;
pub use resume::*;
pub use slash::*;
pub use slash2::*;
pub use slash3::*;
pub use summary::*;
pub use turn::*;
pub use types::*;

#[cfg(test)]
mod tests_a;
#[cfg(test)]
mod tests_b;
#[cfg(test)]
mod tests_c;
#[cfg(test)]
mod tests_common;
