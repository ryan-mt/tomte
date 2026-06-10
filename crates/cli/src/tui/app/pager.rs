//! Full-screen expanded-transcript viewer for inline mode (Ctrl+O at rest).
//!
//! Inline mode pushes finished turns into the terminal's native scrollback via
//! `insert_before`, which can never be repainted — so once a turn settles, an
//! expanded/collapsed flag has nothing left to redraw and the "(Ctrl+O for
//! more)" hints would point at a dead key. This pager honors them instead: it
//! re-renders the whole transcript with tool detail expanded inside the
//! alternate screen (native scrollback stays untouched underneath) and
//! restores the prompt on exit.

use super::*;

/// What a pager keystroke means. `Anchor(None)` sticks to the bottom (the
/// newest lines, where the hints point); `Anchor(Some(n))` parks `n` lines
/// from the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerStep {
    Exit,
    Anchor(Option<usize>),
    Ignore,
}

/// Pure key → scroll resolution, split out so both branches are unit-tested.
/// `cur` is the resolved scroll offset this frame, `page` the body height.
pub fn pager_step(
    code: KeyCode,
    ctrl: bool,
    cur: usize,
    page: usize,
    max_scroll: usize,
) -> PagerStep {
    // Landing on (or past) the last line re-arms stick-to-bottom, so a reader
    // who scrolls back down keeps following the tail like they never left.
    let anchor = |s: usize| {
        if s >= max_scroll {
            PagerStep::Anchor(None)
        } else {
            PagerStep::Anchor(Some(s))
        }
    };
    match code {
        KeyCode::Esc | KeyCode::Char('q') => PagerStep::Exit,
        KeyCode::Char('o' | 'c') if ctrl => PagerStep::Exit,
        KeyCode::Up => anchor(cur.saturating_sub(1)),
        KeyCode::Down => anchor(cur.saturating_add(1)),
        KeyCode::PageUp => anchor(cur.saturating_sub(page.max(1))),
        KeyCode::PageDown | KeyCode::Char(' ') => anchor(cur.saturating_add(page.max(1))),
        KeyCode::Home => anchor(0),
        KeyCode::End => PagerStep::Anchor(None),
        _ => PagerStep::Ignore,
    }
}

/// Run the modal pager: enter the alternate screen, draw the expanded
/// transcript, and pump the shared event stream until the user closes it.
/// Only opened while idle (`!app.busy`), so nothing streams behind the modal
/// — the agent channels simply wait.
pub async fn run_transcript_pager(app: &App, events: &mut EventStream) -> Result<()> {
    // Mouse capture is scoped to the pager: inline mode never captures the
    // mouse (native scrollback/copy stay intact), but inside the alternate
    // screen there is no native scrollback to protect, and the wheel should
    // scroll the transcript.
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let res = pager_loop(app, events).await;
    // Restore even when the loop errored: never leave the user stuck in the
    // alternate screen.
    let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    res
}

async fn pager_loop(app: &App, events: &mut EventStream) -> Result<()> {
    // A second, full-screen ratatui terminal over the same stdout. Safe: the
    // main loop is suspended for the pager's whole lifetime, and the inline
    // viewport repaints from scratch (`terminal.clear()`) after we return.
    let mut term = Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
    // None = stick to bottom: open at the newest lines, like `less +G`.
    let mut anchor: Option<usize> = None;
    // Wrapped lines are cached and only rebuilt when the width changes
    // (resize); the transcript itself cannot change while the pager is open.
    let mut lines: Option<Vec<ratatui::text::Line<'static>>> = None;
    loop {
        let size = term.size()?;
        let inner_width = (size.width.saturating_sub(2) as usize).max(1);
        let all = lines
            .get_or_insert_with(|| ui::inline_blocks_to_lines(&app.blocks, inner_width, true, app));
        let total = all.len();
        let page = size.height.saturating_sub(1) as usize; // one footer row
        let max_scroll = total.saturating_sub(page);
        let cur = anchor.map_or(max_scroll, |s| s.min(max_scroll));
        // Hand the draw a screenful slice instead of `Paragraph::scroll`: the
        // scroll offset is u16, which a long session's transcript overflows.
        let visible: Vec<ratatui::text::Line<'static>> = all[cur..(cur + page).min(total)].to_vec();
        let pct = (cur * 100)
            .checked_div(max_scroll)
            .map_or(100, |p| p.min(100));
        term.draw(move |f| {
            let area = f.area();
            if area.height == 0 {
                return;
            }
            let body = ratatui::layout::Rect {
                height: area.height - 1,
                ..area
            };
            f.render_widget(ratatui::widgets::Paragraph::new(visible), body);
            let bar = ratatui::layout::Rect {
                y: area.y + area.height - 1,
                height: 1,
                ..area
            };
            let label = format!(
                " transcript — full tool detail · ↑/↓ PgUp/PgDn scroll · Esc to close · {pct}%"
            );
            f.render_widget(
                ratatui::widgets::Paragraph::new(label).style(
                    ratatui::style::Style::default()
                        .add_modifier(ratatui::style::Modifier::REVERSED),
                ),
                bar,
            );
        })?;
        let Some(ev) = events.next().await else {
            return Ok(());
        };
        match ev? {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                match pager_step(k.code, ctrl, cur, page, max_scroll) {
                    PagerStep::Exit => return Ok(()),
                    PagerStep::Anchor(a) => anchor = a,
                    PagerStep::Ignore => {}
                }
            }
            Event::Mouse(m) => {
                use crossterm::event::MouseEventKind;
                match m.kind {
                    MouseEventKind::ScrollUp => anchor = Some(cur.saturating_sub(3)),
                    MouseEventKind::ScrollDown => {
                        let s = cur.saturating_add(3);
                        anchor = if s >= max_scroll { None } else { Some(s) };
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {
                lines = None; // rewrap to the new width on the next pass
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_keys_close_the_pager() {
        for (code, ctrl) in [
            (KeyCode::Esc, false),
            (KeyCode::Char('q'), false),
            (KeyCode::Char('o'), true),
            (KeyCode::Char('c'), true),
        ] {
            assert_eq!(
                pager_step(code, ctrl, 5, 10, 20),
                PagerStep::Exit,
                "{code:?}"
            );
        }
        // A plain 'o' (no ctrl) is not an exit key.
        assert_eq!(
            pager_step(KeyCode::Char('o'), false, 5, 10, 20),
            PagerStep::Ignore
        );
    }

    #[test]
    fn scrolling_clamps_and_resticks_to_bottom() {
        // Up from the bottom parks one line above it.
        assert_eq!(
            pager_step(KeyCode::Up, false, 20, 10, 20),
            PagerStep::Anchor(Some(19))
        );
        // Down at the last line re-arms stick-to-bottom.
        assert_eq!(
            pager_step(KeyCode::Down, false, 19, 10, 20),
            PagerStep::Anchor(None)
        );
        // Page movements use the body height and clamp at both ends.
        assert_eq!(
            pager_step(KeyCode::PageUp, false, 5, 10, 20),
            PagerStep::Anchor(Some(0))
        );
        assert_eq!(
            pager_step(KeyCode::PageDown, false, 15, 10, 20),
            PagerStep::Anchor(None)
        );
        // Home parks at the top; End follows the tail again.
        assert_eq!(
            pager_step(KeyCode::Home, false, 20, 10, 20),
            PagerStep::Anchor(Some(0))
        );
        assert_eq!(
            pager_step(KeyCode::End, false, 0, 10, 20),
            PagerStep::Anchor(None)
        );
    }

    #[test]
    fn short_transcript_always_sticks_to_bottom() {
        // Nothing to scroll (max_scroll == 0): every movement resolves to the
        // stick-to-bottom anchor instead of parking on a phantom offset.
        for code in [KeyCode::Up, KeyCode::Down, KeyCode::Home, KeyCode::PageUp] {
            assert_eq!(
                pager_step(code, false, 0, 10, 0),
                PagerStep::Anchor(None),
                "{code:?}"
            );
        }
    }
}
