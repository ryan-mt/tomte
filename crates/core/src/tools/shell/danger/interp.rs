use super::*;

/// Known command-wrappers that re-launch a following command word, so the real
/// interpreter hides behind them (`curl … | sudo sh`, `… | xargs sh`).
pub(super) const PIPE_WRAPPERS: &[&str] = &[
    "sudo", "doas", "xargs", "env", "command", "exec", "nice", "nohup", "time", "stdbuf", "setsid",
    "ionice",
];

/// True when the command that reads a pipeline stage is an interpreter — either
/// directly (`| sh`) or behind a known wrapper (`| sudo sh`, `| env FOO=bar sh`).
/// The scan is bounded to the stage's first command segment (it stops at `;`/`&`)
/// so an unrelated later `sh` isn't blamed; and a benign `grep sh` / `sed
/// 's/sh/zsh/'` stays unflagged because those tools are not wrappers, so their
/// interpreter-named *arguments* are never inspected.
pub(super) fn pipe_stage_runs_interpreter(stage: &str, interpreters: &[&str]) -> bool {
    let is_interp = |w: &str| {
        let name = shell_token_command_name(w);
        interpreters
            .iter()
            .any(|base| is_versioned_name(&name, base))
    };
    // Only the first command of the stage reads the pipe.
    let segment = stage.split([';', '&']).next().unwrap_or(stage);
    let words: Vec<&str> = segment.split_whitespace().collect();
    // The effective command is the first word that isn't grouping, a flag, or a
    // `VAR=val` env assignment. `degroup` strips surrounding grouping/`exec`
    // punctuation (`| { sh; }`, `| ( sh )`, `| (sh)`) so the real command word
    // behind it is the one classified.
    let Some(first) = words
        .iter()
        .copied()
        .map(degroup)
        .find(|w| !w.is_empty() && !w.starts_with('-') && !is_env_assignment(w))
    else {
        return false;
    };
    if is_interp(first) {
        return true;
    }
    let first_name = shell_token_command_name(first);
    if PIPE_WRAPPERS
        .iter()
        .any(|w| is_versioned_name(&first_name, w))
    {
        // Wrapper-led: we can't reliably parse each wrapper's own flags/args, so
        // scan the segment for an interpreter — erring toward flagging, as this
        // whole defense-in-depth gate does.
        return words.iter().copied().map(degroup).any(is_interp);
    }
    false
}

/// True when a pipeline stage runs `xargs`/`parallel` invoking `rm` with a
/// recursive/force flag. The paths to delete come from stdin, so no dangerous
/// target token is present — but a recursive force delete of upstream-supplied
/// paths is the destructive intent. Plain `xargs rm` (per-file, no `-r`/`-f`) is
/// left alone: it's a routine `find … | xargs rm` cleanup, like `rm file`.
pub(super) fn pipe_stage_xargs_recursive_rm(stage: &str) -> bool {
    let segment = stage.split([';', '&']).next().unwrap_or(stage);
    let words: Vec<&str> = segment.split_whitespace().map(degroup).collect();
    let Some(first) = words
        .iter()
        .copied()
        .find(|w| !w.is_empty() && !w.starts_with('-') && !is_env_assignment(w))
    else {
        return false;
    };
    let leader = shell_token_command_name(first);
    if leader != "xargs" && leader != "parallel" {
        return false;
    }
    // Only flags *after* `rm` are rm's own — `xargs -r rm file` has the `-r` on
    // xargs (--no-run-if-empty), not a recursive rm.
    let Some(rm_pos) = words
        .iter()
        .position(|w| shell_token_command_name(w) == "rm")
    else {
        return false;
    };
    words.iter().skip(rm_pos + 1).any(|w| {
        matches!(
            *w,
            "-rf" | "-fr" | "-r" | "-R" | "--recursive" | "-f" | "--force"
        ) || (w.starts_with('-') && !w.starts_with("--") && (w.contains('r') || w.contains('f')))
    })
}

/// The inline-code flag(s) for a non-shell interpreter, or `None` if `name` is
/// not one. Deliberately per-interpreter (not one shared set): `-r` is inline
/// code for PHP but "require a library" for Ruby/Perl, and `-c` is inline code
/// for Python but a syntax-check-only for Node — a shared set would over-flag
/// those benign forms. PowerShell and awk are handled by their own arms in the
/// caller because their flag/positional shapes don't fit this table.
pub(super) fn inline_code_flags(name: &str) -> Option<&'static [&'static str]> {
    if is_versioned_name(name, "python") {
        return Some(&["-c"]);
    }
    if is_versioned_name(name, "ruby") {
        return Some(&["-e"]);
    }
    if is_versioned_name(name, "perl") {
        return Some(&["-e", "-E"]);
    }
    if is_versioned_name(name, "php") {
        return Some(&["-r"]);
    }
    if is_versioned_name(name, "node")
        || is_versioned_name(name, "nodejs")
        || is_versioned_name(name, "bun")
    {
        return Some(&["-e", "--eval", "-p", "--print"]);
    }
    if is_versioned_name(name, "deno") {
        return Some(&["eval"]); // `deno eval "<code>"`
    }
    None
}

/// True when any segment's command candidate is `pwsh`/`powershell` — i.e. the
/// command actually invokes PowerShell (as a word, not as some other program's
/// argument). Gates the string-builder markers in
/// [`runs_inline_interpreter_code`].
pub(super) fn names_powershell(scan: &str) -> bool {
    scan.split([';', '&', '|', '\n']).any(|seg| {
        let words: Vec<&str> = seg.split_whitespace().collect();
        segment_command_candidates(&words)
            .iter()
            .any(|name| is_versioned_name(name, "pwsh") || is_versioned_name(name, "powershell"))
    })
}

/// A PowerShell `-EncodedCommand` flag or an abbreviation (`-e`, `-en…`, `-ec`).
/// Its base64 argument is opaque to the token scan. Deliberately NOT `-Command`:
/// that argument is plain PowerShell, which `normalize_shell_scan` exposes
/// (quotes stripped) to the destructive-token rules, so flagging every
/// `-Command` would refuse benign invocations like `-Command "Start-Sleep 30"`.
pub(super) fn is_powershell_code_flag(tok: &str) -> bool {
    matches!(tok, "-e" | "-ec") || tok.starts_with("-enc")
}

/// True when a segment invokes a non-shell interpreter with an inline program.
/// Uses [`segment_command_candidates`] so a benign interpreter-named *argument*
/// (`echo node`) isn't blamed, while an interpreter behind a command-wrapper
/// (`env python3 -c …`, `sudo node -e …`) — which a first-word-only check let
/// slip past — is still caught.
pub(super) fn runs_inline_interpreter_code(scan: &str) -> bool {
    // A `-Command` payload is normally plain PowerShell that the flattened scan
    // reads (see `is_powershell_code_flag`). But string-BUILT execution —
    // `[scriptblock]::Create(…)`, a `FromBase64String` decode, `[char]`-array
    // `-join` assembly — composes the real command at runtime where no token
    // rule can see it. When PowerShell is invoked anywhere in the command,
    // treat those builders as inline code. Checked over the whole scan (not
    // per segment) because the call operator `&` that runs the built block is
    // itself a segment separator here. A flag only adds an approval prompt.
    let lower = scan.to_ascii_lowercase();
    if (lower.contains("[scriptblock]::create")
        || lower.contains("frombase64string")
        || (lower.contains("[char") && lower.contains("-join")))
        && names_powershell(scan)
    {
        return true;
    }
    scan.split([';', '&', '|', '\n']).any(|seg| {
        let words: Vec<&str> = seg.split_whitespace().collect();
        segment_command_candidates(&words).iter().any(|name| {
            if let Some(flags) = inline_code_flags(name) {
                // Match the flag as its own word (`python -c 'code'`) OR glued to
                // its argument (`python -c'code'`, `node -e'…'`) — a single-letter
                // flag (`-c`/`-e`) can carry the program with no space, which a
                // plain equality test missed.
                if words.iter().copied().map(degroup).any(|w| {
                    flags
                        .iter()
                        .any(|f| w == *f || (f.len() == 2 && w.len() > 2 && w.starts_with(f)))
                }) {
                    return true;
                }
            }
            if (is_versioned_name(name, "pwsh") || is_versioned_name(name, "powershell"))
                && words
                    .iter()
                    .copied()
                    .map(degroup)
                    .any(is_powershell_code_flag)
            {
                return true;
            }
            // awk's program is a positional arg, not a flag; `system("…")` is the
            // only way it shells out, and after quote-stripping the inner words
            // carry no clean shell token, so the scan above can't see it.
            matches!(name.as_str(), "awk" | "gawk" | "mawk" | "nawk") && seg.contains("system(")
        })
    })
}
