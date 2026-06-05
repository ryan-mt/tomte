//! App entry points and terminal setup/teardown. Split out of `app`; logic unchanged.

use super::*;

pub async fn run() -> Result<()> {
    run_with(false, false).await
}

pub async fn run_plan_mode_required() -> Result<()> {
    run_with(false, true).await
}

/// Same as [`run`] but opens the resume-session picker on first frame so
/// `tomte resume` lands the user directly on the session list.
pub async fn run_resume() -> Result<()> {
    run_with(true, false).await
}

pub async fn run_resume_plan_mode_required() -> Result<()> {
    run_with(true, true).await
}

pub async fn run_with(start_with_resume_picker: bool, plan_mode_required: bool) -> Result<()> {
    let mode = RenderMode::from_env();
    // Install a panic hook that restores the terminal before unwinding, so a
    // panic inside main_loop (or any library it pulls in) doesn't leave the
    // user's shell stuck in raw mode + alternate screen. The alt-screen/mouse
    // disables are harmless no-ops when inline mode never enabled them.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        original_hook(info);
    }));

    let mut terminal = setup_terminal(mode)?;
    // SessionStart hook (best-effort, once per interactive session). Spawned so a
    // slow hook can't delay the first frame; its output/exit code is ignored.
    tokio::spawn(async { tomte_core::hooks::load().fire_session_start().await });
    let res = main_loop(&mut terminal, start_with_resume_picker, plan_mode_required).await;
    restore_terminal(&mut terminal, mode)?;
    res
}

pub fn setup_terminal(mode: RenderMode) -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // EnableBracketedPaste makes the terminal wrap pasted text in escape
    // markers and deliver it as one `Event::Paste(String)` instead of a stream
    // of individual key presses. Without it, a multi-line paste arrives as
    // KeyCode::Char + KeyCode::Enter events, and the first Enter submits the
    // (partial) message — the "long paste auto-sends" bug.
    match mode {
        RenderMode::AltScreen => {
            execute!(
                out,
                EnterAlternateScreen,
                EnableMouseCapture,
                EnableBracketedPaste
            )?;
            let backend = CrosstermBackend::new(out);
            Ok(Terminal::new(backend)?)
        }
        RenderMode::Inline => {
            // Pillar 4 — the custodian does not hijack the terminal: no
            // alternate screen (native scrollback survives) and no mouse
            // capture (native wheel-scroll + click-drag copy keep working; we
            // trade away the in-app drag-selection to honor that).
            execute!(out, EnableBracketedPaste)?;
            let backend = CrosstermBackend::new(out);
            Ok(Terminal::with_options(
                backend,
                ratatui::TerminalOptions {
                    viewport: ratatui::Viewport::Inline(inline_viewport_height()),
                },
            )?)
        }
    }
}

/// Live-viewport height for inline mode: tall enough for the active turn plus
/// the input and status rows, short enough that finished turns flow into the
/// terminal's own scrollback rather than dominating the screen.
fn inline_viewport_height() -> u16 {
    let rows = crossterm::terminal::size().map(|(_, r)| r).unwrap_or(24);
    (rows / 2).clamp(10, 20)
}

pub fn restore_terminal(
    t: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mode: RenderMode,
) -> Result<()> {
    disable_raw_mode()?;
    match mode {
        RenderMode::AltScreen => {
            execute!(
                t.backend_mut(),
                DisableBracketedPaste,
                LeaveAlternateScreen,
                DisableMouseCapture
            )?;
        }
        RenderMode::Inline => {
            // Never entered the alt screen or captured the mouse — just undo
            // bracketed paste. The viewport's final frame stays in scrollback,
            // which is the point: the session's tail remains visible.
            execute!(t.backend_mut(), DisableBracketedPaste)?;
        }
    }
    t.show_cursor()?;
    Ok(())
}
