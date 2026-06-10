use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use super::app::{
    fleet_idle_verb, spinner_word_index, todo_completion_key, App, Block, PreFlight,
    SPINNER_FRAMES, TODO_RECENT_COMPLETED_TTL,
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

// Shared with sibling tui modules (picker) so every truncation in the TUI
// goes through the one display-width-aware helper.
pub(super) use util::truncate_to_width;

#[cfg(test)]
mod tests;

/// The seven-slot vertical layout both renderers share. The same one-row slot
/// shows the turn spinner OR the compaction progress bar — they never run at
/// once (compaction only starts once a turn ends). Only the chat slot's
/// minimum differs: the alt screen reserves 5 rows for the transcript, the
/// inline viewport lets the live tail shrink to nothing.
fn split_frame(f: &Frame, app: &App, chat_min: u16) -> std::rc::Rc<[Rect]> {
    let spinner_h: u16 = if app.busy || app.compacting { 1 } else { 0 };
    let input_h = input_height(app);
    // The optional panels must never squeeze the input or status rows out of
    // the frame: a busy multi-agent turn wants spinner + queue + fleet + todos
    // ≈ 13+ rows on its own, more than the whole 13–16-row inline viewport,
    // and the unbudgeted layout answered by clipping the input box and status
    // line (the app then LOOKS frozen — typing echoes nowhere). Reserve the
    // always-on rows first; queue → fleet → todos share what's left, each
    // clipped to the remaining budget.
    let reserved = chat_min
        .saturating_add(spinner_h)
        .saturating_add(input_h)
        .saturating_add(1); // status line
    let mut budget = f.area().height.saturating_sub(reserved);
    let queued_h = queued_height(app).min(budget);
    budget = budget.saturating_sub(queued_h);
    let fleet_h = fleet_height(app).min(budget);
    budget = budget.saturating_sub(fleet_h);
    let todos_h = todos_height(app).min(budget);
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(chat_min),     // chat            [0]
            Constraint::Length(spinner_h), // spinner         [1]
            Constraint::Length(queued_h),  // queued messages [2]
            Constraint::Length(fleet_h),   // sub-agent fleet [3]
            Constraint::Length(todos_h),   // todos           [4]
            Constraint::Length(input_h),   // input           [5]
            Constraint::Length(1),         // status line     [6]
        ])
        .split(f.area())
}

/// Everything below the chat slot — spinner/compaction row, queued messages,
/// fleet, todos, input, status. Identical in both renderers; the chat slot
/// itself (and anything painted over it) is the caller's.
fn render_panels(f: &mut Frame, layout: &[Rect], app: &mut App) {
    if app.busy {
        render_spinner(f, layout[1], app);
    } else if app.compacting {
        render_compact_progress(f, layout[1], app);
    }
    if layout[2].height > 0 {
        render_queue(f, layout[2], app);
    }
    if layout[3].height > 0 {
        render_fleet(f, layout[3], app);
    } else {
        app.subagent_rows.clear();
    }
    if layout[4].height > 0 {
        render_todos(f, layout[4], app);
    }
    render_input(f, layout[5], app);
    render_status(f, layout[6], app);
}

/// The top layer both renderers share, drawn last so it stays legible over
/// everything: the hatch animation / corner buddy over the chat area, then the
/// overlay picker and the approval/conscience modals anchored above the input.
fn render_overlays(f: &mut Frame, layout: &[Rect], app: &mut App) {
    // Buddy companion: the hatch animation centers over the WHOLE frame — the
    // inline renderer's chat slot can be 0 rows tall while panels fill the
    // viewport, which used to play the entire animation invisibly. The adopted
    // pet still tucks into the chat area's bottom-right corner (and simply
    // stays hidden when that slot is too small to host it).
    if app.hatch.is_some() {
        render_hatch(f, f.area(), app);
    } else if let (Some(pet), false) = (app.buddy_pet, app.buddy_hidden) {
        render_corner_buddy(f, layout[0], pet);
    }
    if let Some((_, picker)) = &app.overlay {
        super::picker::render(f, layout[5], picker);
    }
    if app.pending_approval.is_some() {
        render_approval(f, layout[5], app);
    }
    if app.pending_conscience.is_some() {
        render_conscience(f, layout[5], app);
    }
}

pub fn render(f: &mut Frame, app: &mut App) {
    let layout = split_frame(f, app, 5);

    render_chat(f, layout[0], app);
    // render_chat reconciles auto_scroll (it flips back on once the user scrolls
    // to the tail). Only when the user is parked above the tail do we offer the
    // clickable jump-to-bottom bar; otherwise clear its hit-test rect.
    if !app.auto_scroll && layout[0].height > 0 {
        app.jump_to_bottom_hint = Some(render_jump_to_bottom(f, layout[0]));
    } else {
        app.jump_to_bottom_hint = None;
    }
    render_panels(f, &layout, app);

    // Paint the left-drag text selection over the rendered content (below the
    // buddy / overlay drawn next, which should stay legible on top).
    if let Some(sel) = app.selection {
        let area = f.area();
        super::selection::highlight(f.buffer_mut(), &sel, area);
    }

    render_overlays(f, &layout, app);
}

/// Inline-viewport render (SOUL Pillar 4 — the calm, tidy terminal). Unlike
/// [`render`], which paints the whole transcript into an alternate-screen
/// layout, this paints only the LIVE part of the session: the active
/// (uncommitted) turn, the live panels, the input, and the status line.
/// Finished turns are pushed to the terminal's native scrollback via
/// `insert_before` (see `app::commit_finished_blocks`), so they are not redrawn
/// here — the terminal's own scroll and copy keep working on that history.
pub fn render_inline(f: &mut Frame, app: &mut App) {
    let layout = split_frame(f, app, 0);
    render_inline_tail(f, layout[0], app);
    render_panels(f, &layout, app);
    render_overlays(f, &layout, app);
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
    if total <= visible {
        // Bottom-anchor short content so the live tail (and the first-screen
        // welcome card) hugs the input instead of floating at the top of the
        // viewport with a big empty gap below it.
        let used = total as u16;
        let sub = Rect {
            x: area.x,
            y: area.y + (area.height - used),
            width: area.width,
            height: used,
        };
        f.render_widget(Paragraph::new(lines), sub);
    } else {
        let scroll = (total - visible) as u16;
        f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
    }
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
    inline_blocks_to_lines_marked(blocks, inner_width, expanded, app).0
}

/// Like [`inline_blocks_to_lines`] but also returns `(line_offset, block_index)`
/// for each collapsed "Thought for Xs" line — the click-to-expand targets.
/// Offsets are relative to the returned `Vec`/`blocks`; callers add their base.
pub fn inline_blocks_to_lines_marked(
    blocks: &[Block],
    inner_width: usize,
    expanded: bool,
    app: &App,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut marks: Vec<(usize, usize)> = Vec::new();
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
            Block::Assistant {
                thought_for_secs, ..
            } => {
                // A collapsed thought renders its "Thought for Xs" line first (the
                // live-reasoning branch is gated off once collapsed), so the click
                // target sits at the current line offset.
                if thought_for_secs.is_some() {
                    marks.push((lines.len(), i));
                }
                push_assistant_lines(
                    &mut lines,
                    &blocks[i],
                    inner_width,
                    app.config.show_thinking,
                )
            }
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
    (lines, marks)
}
