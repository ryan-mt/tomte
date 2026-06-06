use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use super::app::{
    todo_completion_key, App, Block, PreFlight, SPINNER_FRAMES, TODO_RECENT_COMPLETED_TTL,
};
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

/// Inline-viewport render (SOUL Pillar 4 — the calm, tidy terminal). Unlike
/// [`render`], which paints the whole transcript into an alternate-screen
/// layout, this paints only the LIVE part of the session: the active
/// (uncommitted) turn, the live panels, the input, and the status line.
/// Finished turns are pushed to the terminal's native scrollback via
/// `insert_before` (see `app::commit_finished_blocks`), so they are not redrawn
/// here — the terminal's own scroll and copy keep working on that history.
pub fn render_inline(f: &mut Frame, app: &mut App) {
    let spinner_h: u16 = if app.busy || app.compacting { 1 } else { 0 };
    let queue_h = queued_height(app);
    let fleet_h = fleet_height(app);
    let todos_h = todos_height(app);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),                    // live turn tail  [0]
            Constraint::Length(spinner_h),         // spinner         [1]
            Constraint::Length(queue_h),           // queued messages [2]
            Constraint::Length(fleet_h),           // sub-agent fleet [3]
            Constraint::Length(todos_h),           // todos           [4]
            Constraint::Length(input_height(app)), // input           [5]
            Constraint::Length(1),                 // status line     [6]
        ])
        .split(f.area());

    render_inline_tail(f, layout[0], app);
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

    // Hatch animation / corner buddy share the live area, same as full mode.
    if app.hatch.is_some() {
        render_hatch(f, layout[0], app);
    } else if let (Some(pet), false) = (app.buddy_pet, app.buddy_hidden) {
        render_corner_buddy(f, layout[0], pet);
    }

    // Overlay popup + approval modal anchor above the input, same as full mode.
    if let Some((_, picker)) = &app.overlay {
        super::picker::render(f, layout[5], picker);
    }
    if app.pending_approval.is_some() {
        render_approval(f, layout[5], app);
    }
}

/// Render the live (uncommitted) tail — usually just the streaming block —
/// bottom-aligned so the most recent output stays visible in the slim viewport.
fn render_inline_tail(f: &mut Frame, area: Rect, app: &mut App) {
    if area.height == 0 {
        return;
    }
    let inner_width = area.width.saturating_sub(2) as usize;
    let start = app.committed_blocks.min(app.blocks.len());
    let lines = inline_blocks_to_lines(&app.blocks[start..], inner_width, app.expanded_tools, app);
    let total = lines.len();
    let visible = area.height as usize;
    let scroll = total.saturating_sub(visible) as u16;
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
}

/// Render a run of blocks to wrapped lines, reusing the same leaf renderers as
/// the alt-screen transcript so both modes look identical. Used for the live
/// tail above and for the scrollback commit (`insert_before`) in `mainloop`.
pub fn inline_blocks_to_lines(
    blocks: &[Block],
    inner_width: usize,
    expanded: bool,
    app: &App,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut i = 0;
    while i < blocks.len() {
        // Group consecutive read_file calls into one stanza, mirroring render_chat.
        if matches!(&blocks[i], Block::Tool { name, .. } if name == "read_file") {
            let mut j = i;
            while j < blocks.len()
                && matches!(&blocks[j], Block::Tool { name, .. } if name == "read_file")
            {
                j += 1;
            }
            render_read_group(&mut lines, &blocks[i..j], expanded);
            i = j;
            continue;
        }
        match &blocks[i] {
            Block::Welcome => render_welcome(&mut lines, app),
            Block::User(text) => push_user_lines(&mut lines, text, inner_width),
            Block::Assistant { .. } => push_assistant_lines(&mut lines, &blocks[i], inner_width),
            Block::Tool {
                name,
                args,
                output,
                error,
                preflight,
                ..
            } => render_tool(
                &mut lines,
                name,
                args,
                output.as_deref(),
                *error,
                preflight.as_ref(),
                inner_width,
                expanded,
            ),
            Block::System(text) => {
                for l in wrap(text, inner_width.saturating_sub(2)) {
                    lines.push(Line::from(Span::styled(
                        format!("  {l}"),
                        Style::default().fg(palette::TEXT_MUTED),
                    )));
                }
                lines.push(Line::raw(""));
            }
            Block::Rich(rich_lines) => {
                for l in rich_lines {
                    lines.push(l.clone());
                }
                lines.push(Line::raw(""));
            }
        }
        i += 1;
    }
    lines
}
