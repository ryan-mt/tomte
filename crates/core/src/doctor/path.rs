use super::*;

/// Best-effort `which`: is `name` runnable? A bare name is searched on `$PATH`
/// (honouring `PATHEXT` on Windows); a name that already contains a path
/// separator is checked directly. Avoids spawning anything, so it can't hang.
///
/// On Windows a runnable command is often a `.cmd`/`.bat`/`.ps1` shim — `npx`,
/// `prettier`, `pnpm` — not a `.exe`, and the runtime spawns those via
/// PATH×PATHEXT (`mcp::resolve_windows_program`, and hooks via `cmd /C`). The
/// doctor must search the SAME extension set, or it falsely reports a valid MCP
/// server / hook command as "not found on PATH". On Unix there is no PATHEXT, so
/// the bare name (with its executable bit) is the command.
pub(crate) fn binary_on_path(name: &str) -> bool {
    let p = Path::new(name);
    if p.is_absolute() || p.components().count() > 1 {
        return p.is_file();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    binary_in_paths(name, &paths, &pathext_candidates())
}

/// Executable extensions (each with a leading dot) to try for a bare command on
/// this platform: `PATHEXT` on Windows (defaulted like cmd.exe), empty on Unix.
pub(super) fn pathext_candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(|e| {
                if e.starts_with('.') {
                    e.to_string()
                } else {
                    format!(".{e}")
                }
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Pure PATH×ext search, so the resolution is testable without mutating the
/// process environment: is `name` present as a file in one of `paths`, either
/// bare or with one of `exts` appended?
pub(super) fn binary_in_paths(name: &str, paths: &std::ffi::OsStr, exts: &[String]) -> bool {
    for dir in std::env::split_paths(paths) {
        if dir.join(name).is_file() {
            return true;
        }
        if exts
            .iter()
            .any(|ext| dir.join(format!("{name}{ext}")).is_file())
        {
            return true;
        }
    }
    false
}
