//! `/memory edit` — open the project `CLAUDE.md` in the user's own editor.
//!
//! The TUI suspends around the child process: raw mode and bracketed paste go
//! off (and the alternate screen is left, when the app runs there) so the
//! editor owns a normal terminal, then everything is re-enabled and the main
//! loop repaints the live viewport from scratch (`terminal.clear()`) — the
//! same recovery contract the Ctrl+O pager uses. While suspended, crossterm's
//! event stream only *peeks* at input availability, so a terminal editor gets
//! every keystroke.

use std::path::Path;

use super::*;

/// Resolve the editor command: `$VISUAL` wins over `$EDITOR` (the long-standing
/// convention — VISUAL is "my full-screen editor", EDITOR the line-mode
/// fallback), with a per-OS default that exists everywhere: `notepad` on
/// Windows, `vi` elsewhere.
fn resolve_editor() -> (String, Vec<String>) {
    let raw = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
    editor_command_from(raw.as_deref())
}

/// Pure split of an editor value into program + leading args, falling back to
/// the platform default when unset or unparseable.
pub(super) fn editor_command_from(raw: Option<&str>) -> (String, Vec<String>) {
    if let Some((prog, args)) = raw.and_then(split_editor_command) {
        return (prog, args);
    }
    if cfg!(windows) {
        ("notepad".to_string(), Vec::new())
    } else {
        ("vi".to_string(), Vec::new())
    }
}

/// Split an `$EDITOR` value into program + args. Handles the two shapes that
/// occur in practice: a bare command with flags (`code --wait`) and a
/// double-quoted program path containing spaces
/// (`"C:\Program Files\Vim\vim.exe" -f`). Whitespace-splitting the args is the
/// same convention git and most CLIs apply to `$EDITOR`.
fn split_editor_command(raw: &str) -> Option<(String, Vec<String>)> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('"') {
        let (prog, tail) = rest.split_once('"')?;
        if prog.is_empty() {
            return None;
        }
        Some((
            prog.to_string(),
            tail.split_whitespace().map(str::to_string).collect(),
        ))
    } else {
        let mut it = raw.split_whitespace();
        let prog = it.next()?.to_string();
        Some((prog, it.map(str::to_string).collect()))
    }
}

/// Suspend the TUI, run the editor on `path`, and take the terminal back —
/// even when the editor failed to spawn. The caller must `terminal.clear()`
/// afterwards: ratatui's cached buffer can't be trusted across the excursion.
pub async fn edit_file_in_editor(
    mode: RenderMode,
    path: &Path,
) -> Result<std::process::ExitStatus> {
    let (prog, args) = resolve_editor();
    // Hand the terminal over: cooked mode, no paste markers, cursor visible.
    // Inline mode never entered the alternate screen or captured the mouse,
    // so those undo steps are alt-screen-only.
    disable_raw_mode()?;
    {
        let mut out = io::stdout();
        if mode == RenderMode::AltScreen {
            let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
        }
        let _ = execute!(out, DisableBracketedPaste, crossterm::cursor::Show);
    }
    let result = tokio::process::Command::new(&prog)
        .args(&args)
        .arg(path)
        .status()
        .await;
    {
        let mut out = io::stdout();
        if mode == RenderMode::AltScreen {
            let _ = execute!(out, EnterAlternateScreen, EnableMouseCapture);
        }
        let _ = execute!(out, EnableBracketedPaste);
    }
    enable_raw_mode()?;
    result.map_err(|e| anyhow::anyhow!("launch `{prog}`: {e} (set $VISUAL or $EDITOR)"))
}

#[cfg(test)]
mod tests {
    use super::editor_command_from;

    #[test]
    fn editor_value_splits_program_and_flags() {
        assert_eq!(
            editor_command_from(Some("code --wait")),
            ("code".to_string(), vec!["--wait".to_string()])
        );
        assert_eq!(
            editor_command_from(Some("vim")),
            ("vim".to_string(), Vec::new())
        );
    }

    #[test]
    fn quoted_program_path_with_spaces_stays_one_token() {
        assert_eq!(
            editor_command_from(Some(r#""C:\Program Files\Vim\vim.exe" -f"#)),
            (
                r"C:\Program Files\Vim\vim.exe".to_string(),
                vec!["-f".to_string()]
            )
        );
    }

    #[test]
    fn unset_or_blank_editor_falls_back_to_the_platform_default() {
        let expected = if cfg!(windows) { "notepad" } else { "vi" };
        assert_eq!(editor_command_from(None).0, expected);
        assert_eq!(editor_command_from(Some("   ")).0, expected);
        // An unterminated quote is unparseable → default, not a panic.
        assert_eq!(editor_command_from(Some("\"broken")).0, expected);
    }
}
