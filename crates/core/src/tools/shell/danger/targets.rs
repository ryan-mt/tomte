/// A Windows drive-root wildcard (`c:\*`, `c:\*.*`, `c:/*`, or a bare `\*` on the
/// current drive) — `del`/`erase` against it wipes the root's files, the cmd.exe
/// analog of `rm -rf /*`. A plain `del *` (current dir) is deliberately excluded,
/// matching the Unix side leaving `rm *` (no recurse) unflagged. `target` is
/// lowercase and comes from `raw_tokens` so the `\` root anchor survives.
pub(super) fn is_windows_drive_root_glob(target: &str) -> bool {
    let rest = match target.split_once(':') {
        Some((drive, rest))
            if drive.len() == 1 && drive.chars().all(|c| c.is_ascii_alphabetic()) =>
        {
            rest
        }
        Some(_) => return false,
        None => target,
    };
    matches!(rest, "\\*" | "\\*.*" | "/*" | "/*.*")
}

/// A recursive (optionally force) delete flag: `-rf`, `-r`, `-R`, `--recursive`,
/// or any short cluster containing both `r` and `f`.
pub(super) fn is_recursive_delete_flag(tok: &str) -> bool {
    matches!(tok, "-rf" | "-fr" | "-r" | "-R" | "--recursive")
        || (tok.starts_with('-')
            && !tok.starts_with("--")
            && tok.contains('r')
            && tok.contains('f'))
}

/// A raw block-device path that a write/redirect could corrupt. Kept to the
/// disk families the redirect guard has always covered (`sd`/`nvme`/`hd`).
pub(super) fn is_raw_block_device(target: &str) -> bool {
    let target = collapse_leading_slashes(target);
    target.starts_with("/dev/sd")
        || target.starts_with("/dev/nvme")
        || target.starts_with("/dev/hd")
        || target.starts_with("/dev/vd")
        || target.starts_with("/dev/mmcblk")
        || target.starts_with("/dev/disk")
}

/// A redirect operator token whose write target is the *next* whitespace-
/// separated word (`> /dev/sda`, `>> …`, `>| …`, `&> …`, `2> …`). Accepts an
/// optional leading fd number, then an optional `&`, then `>`, `>>`, or the
/// POSIX clobber `>|`. The glued forms (`>/dev/sda`, `2>>/dev/sda`,
/// `>|/dev/sda`) are caught separately by splitting the token on `>`/`|`.
pub(super) fn is_redirect_op(tok: &str) -> bool {
    let rest = tok.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.strip_prefix('&').unwrap_or(rest);
    matches!(rest, ">" | ">>" | ">|")
}

/// True when `name` is `base` or `base` followed only by a version suffix
/// (digits and dots): `python3`, `python3.11`, `bash5` all match — but
/// `bashful` does not. Without this, `curl … | python3` (the default name on
/// modern systems) silently bypassed the curl-pipe-shell guard.
pub(super) fn is_versioned_name(name: &str, base: &str) -> bool {
    name == base
        || name.strip_prefix(base).is_some_and(|rest| {
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
}

pub(super) fn detects_recursive_dangerous_rm(tokens: &[&str], command_names: &[String]) -> bool {
    let Some(rm_idx) = command_names.iter().position(|n| n == "rm") else {
        return false;
    };
    let is_recursive = tokens.iter().any(|t| {
        matches!(*t, "-rf" | "-fr" | "-r" | "-R" | "--recursive")
            || (t.starts_with('-') && !t.starts_with("--") && t.contains('r') && t.contains('f'))
    });
    is_recursive
        && tokens
            .iter()
            .skip(rm_idx + 1)
            .any(|t| !t.starts_with('-') && is_dangerous_rm_target(t))
}

/// A recursive-`chmod`/`chown` target whose blast radius is the OS, a home dir,
/// or the filesystem root. Unlike [`is_dangerous_rm_target`] it deliberately
/// ignores relative targets (`.`, `*`) — a recursive mode change under the
/// project tree is common and not worth an override prompt.
pub(super) fn is_dangerous_chmod_target(token: &str) -> bool {
    let token = token.trim_end_matches([';', '&', '|']);
    let literal = token.trim_matches(|c| matches!(c, '"' | '\''));
    let literal = collapse_leading_slashes(literal);
    if literal == "/"
        || is_root_equivalent(literal)
        || literal.starts_with("/*")
        || is_critical_system_path(literal)
    {
        return true;
    }
    let is_unquoted = !token.contains('"') && !token.contains('\'');
    if is_unquoted && (literal.starts_with('~') || literal.starts_with('$')) {
        return true;
    }
    token.contains('`') || token.contains("$(")
}

pub(super) fn is_dangerous_rm_target(token: &str) -> bool {
    let token = token.trim_end_matches([';', '&', '|']);
    let literal = token.trim_matches(|c| matches!(c, '"' | '\''));
    let literal = collapse_leading_slashes(literal);
    // Command substitution hides the real path at classify time (`rm -rf $(…)`,
    // `rm -rf `…``) — err toward flagging.
    if token.contains('`') || token.contains("$(") {
        return true;
    }
    if matches!(
        literal,
        "/" | "/*" | "." | "./" | "./*" | "./.*" | ".." | "../*" | ".*" | "*"
    ) {
        return true;
    }
    // POSIX root-equivalents: `//`, `/.`, `/..`, `/./../` all resolve to `/`
    // (`..` from root stays at root) and delete the filesystem root just like
    // `/`, which the literal list above only catches in its canonical spelling.
    if is_root_equivalent(literal) {
        return true;
    }
    // Any glob rooted at `/` (`/*`, `/*/`, `/*/*`, …) hits top-level entries.
    if literal.starts_with("/*") {
        return true;
    }
    if is_critical_system_path(literal) {
        return true;
    }

    let is_unquoted = !token.contains('"') && !token.contains('\'');
    // An unquoted target beginning with `~` (home, incl. `~user`) or a shell
    // variable (`$X`, `${X}`, `$HOME/*`) is a recursive-delete root whose
    // expansion is invisible at classify time — err toward flagging.
    if is_unquoted && (literal.starts_with('~') || literal.starts_with('$')) {
        return true;
    }

    let double_unquoted: String = token.chars().filter(|c| *c != '"').collect();
    has_shell_var_path_prefix(&double_unquoted, "home")
        || has_shell_var_path_prefix(&double_unquoted, "pwd")
}

/// True for an absolute path that collapses to `/` under POSIX resolution —
/// every segment after the leading slash is empty, `.`, or `..`. Catches the
/// root-equivalent spellings (`//`, `/.`, `/..`, `/./../`) that the literal
/// match in `is_dangerous_rm_target` would otherwise miss; never fires on a
/// relative path (it requires the leading `/`) so it adds no false positives.
pub(super) fn is_root_equivalent(literal: &str) -> bool {
    literal.starts_with('/') && literal.split('/').all(|s| matches!(s, "" | "." | ".."))
}

/// Collapse a run of leading `/` to a single `/`. Linux and macOS resolve
/// `//etc` and `///dev/sda` to `/etc` and `/dev/sda` — the same OS targets as
/// the single-slash spellings — but the prefix matches below (`/etc`, `/dev/sd`,
/// the `/*` glob test) would miss the doubled form, so `rm -rf //etc` and
/// `dd of=//dev/sda` slipped the classifier that flags their single-slash twins.
/// Normalizing the leading run closes that bypass; mid-path `//` already resolves
/// and matches, so it's left alone. `/` is ASCII, so byte-slicing is boundary-safe.
pub(super) fn collapse_leading_slashes(s: &str) -> &str {
    let trimmed = s.trim_start_matches('/');
    // Zero or one leading slash: nothing to collapse.
    if trimmed.len() + 1 >= s.len() {
        return s;
    }
    &s[s.len() - trimmed.len() - 1..]
}

/// Best-effort detection of absolute paths whose recursive deletion would
/// devastate the OS or wipe a user's home. `classify_danger` is defense-in-depth
/// (it refuses pending an explicit override), not a sandbox, so erring toward
/// flagging is acceptable; children of non-OS roots like `/var/tmp` are left
/// alone to avoid drowning legitimate cleanups in override prompts.
pub(super) fn is_critical_system_path(literal: &str) -> bool {
    let path = literal.trim_end_matches("/*").trim_end_matches('/');
    if path.is_empty() {
        return false;
    }
    // NOTE: `classify_danger` lowercases the command before tokenizing, so every
    // entry here must be lowercase (incl. the macOS roots) or it can never match.
    // Deleting any of these directories *themselves* is catastrophic.
    const ROOTS: &[&str] = &[
        "/etc",
        "/usr",
        "/var",
        "/bin",
        "/sbin",
        "/lib",
        "/lib32",
        "/lib64",
        "/boot",
        "/sys",
        "/proc",
        "/dev",
        "/root",
        "/opt",
        "/home",
        "/srv",
        "/run",
        "/mnt",
        "/media",
        "/data",
        "/system",
        "/library",
        "/applications",
        "/users",
        "/private",
        "/volumes",
    ];
    if ROOTS.contains(&path) {
        return true;
    }
    // For OS-owned and home roots, any descendant is also essentially never a
    // legitimate recursive-delete target (e.g. `/etc/x`, `/usr/lib`,
    // `/home/<user>/.ssh`, `/root/...`).
    const RECURSIVE_ROOTS: &[&str] = &[
        "/etc", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/boot", "/sys", "/proc", "/dev",
        "/usr", "/root", "/home", "/users", "/system", "/library",
    ];
    RECURSIVE_ROOTS.iter().any(|root| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

pub(super) fn has_path_prefix(target: &str, prefix: &str) -> bool {
    target
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

pub(super) fn has_shell_var_path_prefix(target: &str, var: &str) -> bool {
    if has_path_prefix(target, &format!("${var}")) {
        return true;
    }

    let Some(rest) = target.strip_prefix(&format!("${{{var}")) else {
        return false;
    };
    let Some(first) = rest.chars().next() else {
        return false;
    };
    if first != '}'
        && !matches!(
            first,
            ':' | '?' | '+' | '-' | '#' | '%' | '/' | ',' | '^' | '='
        )
    {
        return false;
    }
    let Some(close_idx) = rest.find('}') else {
        return false;
    };
    let after = &rest[close_idx + 1..];
    after.is_empty() || after.starts_with('/')
}
