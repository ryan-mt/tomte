use super::*;

/// Split a command string into command/argument words, treating the shell
/// control operators `;`, `&`, `|`, newline, and `(`/`)` as word boundaries
/// even when glued to an adjacent word, then splitting the pieces on whitespace.
/// `ls;rm`, `true&&rm`, `dir&del`, and `(rm` all yield a bare `rm`/`del` word,
/// so the command-name guards in `classify_danger` see the fused command. The
/// returned slices borrow from `s`. Empty pieces (from `&&`, `||`, doubled
/// separators) drop out via `split_whitespace`.
pub(super) fn operator_split(s: &str) -> Vec<&str> {
    s.split([';', '&', '|', '\n', '(', ')'])
        .flat_map(str::split_whitespace)
        .collect()
}

/// True when a cmd.exe switch (`/s`, `/q`, …) carrying `sw` is present, including
/// the glued cluster form cmd.exe accepts (`/s/q`). `operator_split` breaks only
/// on whitespace and shell operators — not `/` — so `del /s/q dir` arrives as the
/// single token `/s/q`; an exact `== "/s"` test missed it. A token counts as a
/// switch cluster only when every `/`-separated piece is a single letter, so a
/// real path like `/usr/s` (segment `usr` is multi-letter) never false-matches.
pub(super) fn has_cmd_switch(tokens: &[&str], sw: char) -> bool {
    tokens.iter().any(|t| {
        if !t.starts_with('/') {
            return false;
        }
        let segments = || t.split('/').filter(|s| !s.is_empty());
        segments().all(|s| s.len() == 1) && segments().any(|s| s.chars().eq([sw]))
    })
}

/// A PowerShell parameter written as `-Name` or any prefix abbreviation down to
/// `-` + one letter (PowerShell resolves `-Recurse` from `-r`/`-rec`/`-recurse`
/// when unambiguous). `tok` and `full` are lowercase; `full` includes the dash.
pub(super) fn is_ps_param(tok: &str, full: &str) -> bool {
    tok.len() >= 2 && full.starts_with(tok)
}

/// Strip surrounding shell grouping punctuation from a word so the real command
/// behind `{ … }` / `( … )` is what gets classified (`(sh)` -> `sh`). A free fn
/// (not a closure) so the borrowed return is tied to the input's lifetime.
pub(super) fn degroup(w: &str) -> &str {
    w.trim_matches(['(', ')', '{', '}'])
}

/// A `VAR=val` shell environment assignment (a leading `env FOO=bar` form or a
/// bare prefix assignment), which precedes the real command word in a stage.
pub(super) fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
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

/// True when `keyword` is the command word at the start of any `;`/`|`/`&`
/// separated segment (`eval …`, `x; eval …`, `… | iex`). An argument
/// occurrence (`grep eval`) is ignored, so it doesn't over-flag.
pub(super) fn command_runs_keyword(scan: &str, keyword: &str) -> bool {
    scan.split([';', '&', '|', '\n']).any(|seg| {
        seg.split_whitespace()
            .map(degroup)
            .find(|w| !w.is_empty() && !w.starts_with('-') && !is_env_assignment(w))
            .is_some_and(|w| shell_token_command_name(w) == keyword)
    })
}

/// The command name(s) to classify for one `;`/`&`/`|`-separated segment: the
/// first real command word's name, and — when that word is a known
/// command-wrapper (`env`, `sudo`, `xargs`, …) — every word's name, because the
/// real interpreter hides behind the wrapper (`env python3 -c …`, `sudo bash
/// <(…)`). Mirrors the pipe-into-interpreter guard, which already de-sugars
/// wrappers this way: we can't reliably parse each wrapper's own flags, so we err
/// toward flagging (a flag here only adds an override prompt, never auto-runs).
/// Empty when the segment has no command word.
pub(super) fn segment_command_candidates(words: &[&str]) -> Vec<String> {
    let Some(first) = words
        .iter()
        .copied()
        .map(degroup)
        .find(|w| !w.is_empty() && !w.starts_with('-') && !is_env_assignment(w))
    else {
        return Vec::new();
    };
    let first_name = shell_token_command_name(first);
    if PIPE_WRAPPERS
        .iter()
        .any(|w| is_versioned_name(&first_name, w))
    {
        return words
            .iter()
            .copied()
            .map(degroup)
            .map(shell_token_command_name)
            .collect();
    }
    vec![first_name]
}

/// True when the command WORD (not an argument) of any segment is built by
/// command substitution — a backtick or `$(…)` opening the segment's first real
/// word. The substituted text becomes the command name, so what runs is hidden
/// at classify time (`` `echo rm` -rf / ``, `$(printf rm) -rf /`).
pub(super) fn command_word_uses_substitution(lower: &str) -> bool {
    lower.split([';', '&', '|', '\n']).any(|seg| {
        seg.split_whitespace()
            .map(degroup)
            .find(|w| !w.is_empty() && !w.starts_with('-') && !is_env_assignment(w))
            .is_some_and(|w| w.starts_with('`') || w.starts_with("$("))
    })
}

/// True when a shell interpreter is handed a process substitution (`bash
/// <(curl …)`, `sh <(echo rm -rf /)`): the inner command's output becomes a
/// script the shell runs — the `<(…)` analog of piping into a shell.
pub(super) fn shell_runs_process_substitution(scan: &str) -> bool {
    const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];
    scan.split([';', '&', '|', '\n']).any(|seg| {
        let words: Vec<&str> = seg.split_whitespace().collect();
        segment_command_candidates(&words)
            .iter()
            .any(|name| SHELLS.iter().any(|s| is_versioned_name(name, s)))
            && words.iter().any(|w| w.contains("<(") || w.contains(">("))
    })
}

pub(super) fn shell_token_command_name(token: &str) -> String {
    let token = token.trim_end_matches([';', '&', '|']);
    let normalized = normalize_shell_scan(token);
    let literal = normalized.split_whitespace().next().unwrap_or("");
    let literal: String = literal
        .chars()
        .filter(|c| !matches!(c, '"' | '\''))
        .collect();
    literal.rsplit('/').next().unwrap_or("").to_string()
}

pub(super) fn normalize_shell_scan(input: &str) -> String {
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
                // A single-quoted span is a shell literal: the delimiters must be
                // dropped (so prefix/exact path matchers see the real token, e.g.
                // `'/dev/sda'` -> `/dev/sda`, `/'etc'` -> `/etc`, `'/'` -> `/`),
                // and `$`/`~` inside it are inert (no expansion), so they are
                // dropped too — otherwise the unquoted-expansion heuristics would
                // misfire on a literal like `'$HOME'` (a file named `$HOME`, not
                // the home dir). Double quotes are handled below and DO expand `$`.
                i += 1;
                while let Some(ch) = chars.get(i) {
                    i += 1;
                    if *ch == '\'' {
                        break;
                    }
                    if *ch != '$' && *ch != '~' {
                        out.push(*ch);
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

pub(super) fn push_shell_param(chars: &[char], dollar: usize, out: &mut String) -> usize {
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
