use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use super::app::{todo_completion_key, App, Block, SPINNER_FRAMES, TODO_RECENT_COMPLETED_TTL};
use crate::tui::palette;
use tomte_core::auth::AuthMode;
use tomte_core::tools::{TodoItem, TodoStatus};

mod body;
mod chat;
mod markdown;
mod panels;
mod status;
mod tools;
mod util;

use body::*;
use chat::*;
use markdown::*;
use panels::*;
use status::*;
use tools::*;
use util::*;

#[cfg(test)]
mod tests;

pub fn render(f: &mut Frame, app: &mut App) {
    // The same one-row slot shows the turn spinner OR the compaction progress
    // bar — they never run at once (compaction only starts once a turn ends).
    let spinner_h: u16 = if app.busy || app.compacting { 1 } else { 0 };
    let queue_h: u16 = queued_height(app);
    let fleet_h: u16 = fleet_height(app);
    let todos_h: u16 = todos_height(app);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),                    // chat            [0]
            Constraint::Length(spinner_h),         // spinner         [1]
            Constraint::Length(queue_h),           // queued messages [2]
            Constraint::Length(fleet_h),           // sub-agent fleet [3]
            Constraint::Length(todos_h),           // todos           [4]
            Constraint::Length(input_height(app)), // input           [5]
            Constraint::Length(1),                 // status line     [6]
        ])
        .split(f.area());

    render_chat(f, layout[0], app);
    // render_chat reconciles auto_scroll (it flips back on once the user scrolls
    // to the tail). Only when the user is parked above the tail do we offer the
    // clickable jump-to-bottom bar; otherwise clear its hit-test rect.
    if !app.auto_scroll && layout[0].height > 0 {
        app.jump_to_bottom_hint = Some(render_jump_to_bottom(f, layout[0]));
    } else {
        app.jump_to_bottom_hint = None;
    }
    if app.busy {
        render_spinner(f, layout[1], app);
    } else if app.compacting {
        render_compact_progress(f, layout[1], app);
    }
    if queue_h > 0 {
        render_queue(f, layout[2], app);
    }
    if fleet_h > 0 {
        render_fleet(f, layout[3], app);
    } else {
        app.subagent_rows.clear();
    }
    if todos_h > 0 {
        render_todos(f, layout[4], app);
    }
    render_input(f, layout[5], app);
    render_status(f, layout[6], app);

    // Paint the left-drag text selection over the rendered content (below the
    // buddy / overlay drawn next, which should stay legible on top).
    if let Some(sel) = app.selection {
        let area = f.area();
        super::selection::highlight(f.buffer_mut(), &sel, area);
    }

    // Buddy companion: the hatch animation takes over the chat area; otherwise
    // the adopted pet tucks into the bottom-right corner.
    if app.hatch.is_some() {
        render_hatch(f, layout[0], app);
    } else if let (Some(pet), false) = (app.buddy_pet, app.buddy_hidden) {
        render_corner_buddy(f, layout[0], pet);
    }

    // Overlay popup — drawn above the input.
    if let Some((_, picker)) = &app.overlay {
        super::picker::render(f, layout[5], picker);
    }
    if app.pending_approval.is_some() {
        render_approval(f, layout[5], app);
    }
}
