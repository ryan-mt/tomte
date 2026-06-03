//! App entry points and terminal setup/teardown. Split out of `app`; logic unchanged.

use super::*;

pub async fn run() -> Result<()> {
    run_with(false, false).await
}

pub async fn run_plan_mode_required() -> Result<()> {
    run_with(false, true).await
}

/// Same as [`run`] but opens the resume-session picker on first frame so
/// `opencli resume` lands the user directly on the session list.
pub async fn run_resume() -> Result<()> {
    run_with(true, false).await
}

pub async fn run_resume_plan_mode_required() -> Result<()> {
    run_with(true, true).await
}

pub async fn run_with(start_with_resume_picker: bool, plan_mode_required: bool) -> Result<()> {
    // Install a panic hook that restores the terminal before unwinding, so a
    // panic inside main_loop (or any library it pulls in) doesn't leave the
    // user's shell stuck in raw mode + alternate screen.
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

    let mut terminal = setup_terminal()?;
    // SessionStart hook (best-effort, once per interactive session). Spawned so a
    // slow hook can't delay the first frame; its output/exit code is ignored.
    tokio::spawn(async { opencli_core::hooks::load().fire_session_start().await });
    let res = main_loop(&mut terminal, start_with_resume_picker, plan_mode_required).await;
    restore_terminal(&mut terminal)?;
    res
}

pub fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // EnableBracketedPaste makes the terminal wrap pasted text in escape
    // markers and deliver it as one `Event::Paste(String)` instead of a stream
    // of individual key presses. Without it, a multi-line paste arrives as
    // KeyCode::Char + KeyCode::Enter events, and the first Enter submits the
    // (partial) message — the "long paste auto-sends" bug.
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(out);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore_terminal(t: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        t.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    t.show_cursor()?;
    Ok(())
}
