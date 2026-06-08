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
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use futures_util::{FutureExt, StreamExt};
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

/// Sibling of [`ApprovalHandle`] for the three-valued conscience-conflict card
/// (Pillar 5 A2). Cloned from `Agent.pending_conscience` at turn start so the
/// abort/supersede/edit-anyway choice can be delivered without the agent lock.
pub type ConscienceHandle = std::sync::Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<
            String,
            tokio::sync::oneshot::Sender<tomte_core::agent::ConscienceChoice>,
        >,
    >,
>;

/// Neutral window title shown before the first prompt and after `/clear`.
pub(super) const BASE_WINDOW_TITLE: &str = "tomte";

/// Longest visible task portion of the window title. Terminals clip long titles
/// anyway; a short cap keeps the tab label readable.
const WINDOW_TITLE_MAX: usize = 60;

/// Set the OS terminal window/tab title. crossterm picks the right mechanism
/// per platform (`SetConsoleTitle` on Windows, the OSC escape on Unix and
/// Windows Terminal), so this is the cross-platform path — never hand-write the
/// escape. Best-effort: a terminal that ignores the request is harmless, and a
/// write failure (e.g. a redirected stdout) is intentionally swallowed.
pub(super) fn set_terminal_title(title: &str) {
    let _ = execute!(io::stdout(), SetTitle(title));
}

/// Build the window title for a freshly submitted prompt: `tomte — <task>`,
/// where `<task>` is the prompt's first non-empty line with control characters
/// stripped (so a crafted prompt can't inject its own terminal escape),
/// whitespace collapsed, and the result capped to [`WINDOW_TITLE_MAX`] chars.
/// A blank prompt (e.g. an image-only message) yields just [`BASE_WINDOW_TITLE`].
pub(super) fn window_title_from_prompt(prompt: &str) -> String {
    let first = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let cleaned: String = first.chars().filter(|c| !c.is_control()).collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return BASE_WINDOW_TITLE.to_string();
    }
    let task = if collapsed.chars().count() > WINDOW_TITLE_MAX {
        // Reserve one char for the ellipsis so the whole title stays within cap.
        let kept: String = collapsed.chars().take(WINDOW_TITLE_MAX - 1).collect();
        format!("{kept}…")
    } else {
        collapsed
    };
    format!("{BASE_WINDOW_TITLE} — {task}")
}

#[cfg(test)]
mod window_title_tests {
    use super::{window_title_from_prompt, BASE_WINDOW_TITLE, WINDOW_TITLE_MAX};

    #[test]
    fn titles_after_first_line_collapsed_and_capped() {
        assert_eq!(
            window_title_from_prompt("fix the auth bug"),
            "tomte — fix the auth bug"
        );
        // Only the first non-empty line, whitespace collapsed.
        assert_eq!(
            window_title_from_prompt("  \n\n  add   retry   logic\nmore detail"),
            "tomte — add retry logic"
        );
    }

    #[test]
    fn blank_or_image_only_prompt_falls_back_to_base() {
        assert_eq!(window_title_from_prompt("   \n  "), BASE_WINDOW_TITLE);
        assert_eq!(window_title_from_prompt(""), BASE_WINDOW_TITLE);
    }

    #[test]
    fn long_prompt_is_capped_with_ellipsis() {
        let long = "x".repeat(200);
        let title = window_title_from_prompt(&long);
        let task = title.strip_prefix("tomte — ").unwrap();
        assert_eq!(task.chars().count(), WINDOW_TITLE_MAX);
        assert!(task.ends_with('…'));
    }

    #[test]
    fn strips_control_chars_so_a_prompt_cannot_inject_an_escape() {
        // A BEL/ESC in the prompt must not reach the terminal's title escape.
        let title = window_title_from_prompt("hi\x07\x1b]2;evil\x07there");
        assert!(
            !title.contains('\x07') && !title.contains('\x1b'),
            "{title:?}"
        );
        assert!(title.starts_with("tomte — "));
    }
}

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
mod slash_meta;
mod slash_ops;
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
pub use slash_meta::*;
pub use slash_ops::*;
pub use summary::*;
pub use turn::*;
pub use types::*;

#[cfg(test)]
mod app_behavior_tests;
#[cfg(test)]
mod app_goal_todo_tests;
#[cfg(test)]
mod app_plan_mode_tests;
#[cfg(test)]
mod app_test_support;
