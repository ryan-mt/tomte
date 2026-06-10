use super::*;

#[cfg(unix)]
pub(super) fn isolate_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(not(unix))]
pub(super) fn isolate_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
pub(super) fn kill_process_group(pid: Option<u32>) {
    const SIGKILL: i32 = 9;
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) else {
        return;
    };
    unsafe {
        let _ = kill(-pid, SIGKILL);
    }
}

#[cfg(not(unix))]
pub(super) fn kill_process_group(_pid: Option<u32>) {}

/// Resolve a bare program name (e.g. `npx`) to a concrete executable on Windows
/// by searching PATH × PATHEXT, the way a shell does. `CreateProcessW` (which
/// Rust's `Command` uses) only appends `.exe`, so `npx`/`pnpm`/`node`-style
/// shims that live as `.cmd`/`.bat` are otherwise unspawnable. Returns `None`
/// for a command that already carries a path or extension (used verbatim) or
/// that can't be found (caller falls back to the original name and its error).
#[cfg(windows)]
pub(super) fn resolve_windows_program(command: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    resolve_program_in(command, &path, &pathext)
}

/// PATH×PATHEXT resolution as a pure function so it can be tested without
/// mutating the process environment.
#[cfg(windows)]
pub(super) fn resolve_program_in(
    command: &str,
    path: &std::ffi::OsStr,
    pathext: &str,
) -> Option<std::path::PathBuf> {
    // A command that already names a path or an extension is used as-is.
    if command.contains(['/', '\\']) || std::path::Path::new(command).extension().is_some() {
        return None;
    }
    for dir in std::env::split_paths(path) {
        for ext in pathext.split(';').filter(|e| !e.is_empty()) {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
