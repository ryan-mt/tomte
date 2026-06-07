//! Best-effort destructive-command classification (`classify_danger`) and
//! its helpers. Split out of `shell`; logic unchanged.

pub fn classify_danger(command: &str) -> Option<&'static str> {
    let lower = command.to_ascii_lowercase();
    let scan = normalize_shell_scan(&lower);
    let tokens: Vec<&str> = scan.split_whitespace().collect();
    let raw_tokens: Vec<&str> = lower.split_whitespace().collect();
    let command_names: Vec<String> = tokens.iter().map(|t| shell_token_command_name(t)).collect();
    let raw_command_names: Vec<String> = raw_tokens
        .iter()
        .map(|t| shell_token_command_name(t))
        .collect();
    let has = |t: &str| command_names.iter().any(|name| name == t);
    // Case-preserving tokens for the few flags whose case matters (`git branch
    // -D` force-delete vs the benign `-d`), since `lower`/`tokens` are lowercased.
    let orig_tokens: Vec<&str> = command.split_whitespace().collect();
    let stripped: String = scan.chars().filter(|c| !c.is_whitespace()).collect();
    if stripped.contains(":(){:|:&};:") {
        return Some("fork bomb pattern detected");
    }
    if detects_recursive_dangerous_rm(&tokens, &command_names)
        || detects_recursive_dangerous_rm(&raw_tokens, &raw_command_names)
    {
        return Some("recursive rm targeting root, home, or glob");
    }
    if command_names
        .iter()
        .any(|t| t == "mkswap" || t == "mkfs" || t.starts_with("mkfs."))
    {
        return Some("filesystem format command");
    }
    if has("dd") {
        // Share the redirect guard's device families instead of a bespoke list,
        // which had drifted: it missed `/dev/vd*` (virtio — the default disk on
        // KVM/QEMU/cloud VMs) and matched `/dev/disk` only exactly, letting
        // `/dev/disk2` (macOS) through.
        let writes_block_device = tokens
            .iter()
            .any(|t| is_raw_block_device(t.trim_start_matches("of=")));
        if writes_block_device {
            return Some("dd writing to a raw block device");
        }
    }
    // A redirect to a raw block device, whether the operator is glued to the
    // target (`>/dev/sda`, `x>>/dev/nvme0`, `>|/dev/sda`) or a separate token
    // (`> /dev/sda`, `&> /dev/sda`, `2> /dev/sda`). Only segments that follow a
    // redirect operator count, so a plain `/dev/sda` argument (e.g. `cat
    // /dev/sda`, a read) is not flagged.
    let redirect_to_block_device = tokens
        .iter()
        .any(|t| t.split(['>', '|']).skip(1).any(is_raw_block_device))
        || tokens
            .windows(2)
            .any(|w| is_redirect_op(w[0]) && is_raw_block_device(w[1]));
    if redirect_to_block_device {
        return Some("redirecting output to a raw block device");
    }
    // Tools that overwrite/destroy whatever path they are handed: a raw
    // block-device argument means wiping a disk. (`dd` is handled above with its
    // own richer device list; `cp` only writes its final argument.)
    if command_names
        .iter()
        .any(|n| matches!(n.as_str(), "shred" | "wipefs" | "tee" | "truncate"))
        && tokens.iter().any(|t| is_raw_block_device(t))
    {
        return Some("command writing to a raw block device");
    }
    if has("cp") {
        if let Some(last) = tokens.iter().rev().find(|t| !t.starts_with('-')) {
            if is_raw_block_device(last) {
                return Some("cp overwriting a raw block device");
            }
        }
    }
    // `find … -delete` / `-exec rm` recursively deletes everything it matches;
    // under a `find:*` allow-rule or bypass mode it would otherwise run unseen.
    if has("find")
        && (tokens.contains(&"-delete")
            || (tokens.iter().any(|t| matches!(*t, "-exec" | "-execdir")) && has("rm")))
    {
        return Some("find deletes matched files (-delete / -exec rm)");
    }
    if (has("chmod") || has("chown"))
        && tokens
            .iter()
            .any(|t| matches!(*t, "-R" | "-r" | "--recursive") || short_flag_has(t, 'r'))
        && tokens
            .iter()
            .any(|t| !t.starts_with('-') && is_dangerous_chmod_target(t))
    {
        return Some("recursive chmod/chown on a system, home, or root path");
    }
    if has("git") && has("push") && git_push_is_destructive(&tokens) {
        return Some("git push can rewrite or delete remote history");
    }
    if has("git")
        && has("reset")
        && tokens
            .iter()
            .any(|t| matches!(*t, "--hard" | "--merge" | "--keep"))
    {
        return Some("git reset --hard/--merge/--keep discards uncommitted work");
    }
    if has("git")
        && has("rm")
        && tokens.iter().any(|t| {
            matches!(*t, "-r" | "-rf" | "-fr" | "--force")
                || short_flag_has(t, 'r')
                || short_flag_has(t, 'f')
        })
    {
        return Some("git rm -r/-f deletes tracked files from the worktree");
    }
    if has("git") && has("clean") {
        // Plain `-f`/`--force` already deletes untracked files; the extra `d`/`x`
        // (directories / ignored files) only widens the blast radius.
        let forced = tokens
            .iter()
            .any(|t| *t == "--force" || short_flag_has(t, 'f'));
        if forced {
            return Some("git clean removes untracked files");
        }
    }
    if has("git") && has("checkout") && git_checkout_discards_worktree(&tokens) {
        return Some("git checkout can discard worktree changes");
    }
    if has("git") && has("restore") && git_restore_discards_worktree(&tokens) {
        return Some("git restore can discard worktree changes");
    }
    if has("git") && has("branch") && git_branch_force_deletes(&tokens, &orig_tokens) {
        return Some("git branch -D force-deletes an unmerged branch");
    }
    if has("git") && has("update-ref") && tokens.contains(&"-d") {
        return Some("git update-ref -d deletes a ref");
    }
    if has("git") && has("reflog") && tokens.contains(&"expire") {
        return Some("git reflog expire destroys commit-recovery history");
    }
    if has("git")
        && has("gc")
        && tokens
            .iter()
            .any(|t| matches!(*t, "--prune=now" | "--prune=all"))
    {
        return Some("git gc --prune drops unreachable objects");
    }
    if has("git") && has("stash") && tokens.iter().any(|t| matches!(*t, "clear" | "drop")) {
        return Some("git stash clear/drop discards stashed work");
    }
    if has("git") && has("filter-branch") {
        return Some("git filter-branch rewrites history destructively");
    }
    if has("git")
        && has("worktree")
        && tokens.contains(&"remove")
        && tokens
            .iter()
            .any(|t| *t == "--force" || short_flag_has(t, 'f'))
    {
        return Some("git worktree remove --force discards a worktree with changes");
    }
    const PIPE_INTERPRETERS: &[&str] = &["sh", "bash", "zsh", "dash", "python", "perl"];
    // Any pipeline stage after the first that feeds bytes into a shell
    // interpreter runs attacker-controlled output as code. Gating this on a
    // curl/wget *source* missed every other producer (`cat x | sh`, `base64 -d
    // | bash`, `xargs sh`), so the source is no longer required.
    let pipes_into_interpreter = lower
        .split('|')
        .skip(1)
        .any(|stage| pipe_stage_runs_interpreter(stage, PIPE_INTERPRETERS));
    if pipes_into_interpreter {
        return Some("piping output into a shell interpreter");
    }
    // `eval`/`iex` assemble and run a command at runtime, so no literal
    // destructive token is present at classify time. Only a command word (the
    // start of a `;`/`|`/`&` segment) counts, so a benign `grep eval` argument
    // is left alone.
    for kw in ["eval", "iex", "invoke-expression"] {
        if command_runs_keyword(&scan, kw) {
            return Some("eval/iex runs a dynamically-assembled command");
        }
    }
    // A non-shell interpreter handed an inline program (`python -c …`, `node -e
    // …`, `perl -e …`, `php -r …`, `awk 'BEGIN{system(…)}'`, PowerShell
    // `-EncodedCommand`) runs code this classifier can't read: the payload is in
    // the interpreter's own language, so a destructive `python -c
    // 'shutil.rmtree("/")'` carries no shell token the scan above can match. Like
    // `eval`, it's opaque at classify time, so it must clear the override prompt
    // rather than auto-run under a `run_shell(python:*)` grant or bypass mode.
    // Shell interpreters (`sh`/`bash -c "rm -rf /"`) are deliberately absent here:
    // their argument *is* shell and was already flattened and scanned above.
    if runs_inline_interpreter_code(&scan) {
        return Some("interpreter runs inline code from the command line");
    }
    // Windows cmd / PowerShell destructive verbs. The classifier runs on every
    // platform and the Unix vocabulary above never matches these; on Windows the
    // sandbox does not confine the filesystem, so this gate matters most there.
    if (has("del") || has("erase") || has("rd") || has("rmdir")) && tokens.contains(&"/s") {
        return Some("del/rd /s recursively deletes a directory tree");
    }
    if has("format") && tokens.iter().any(|t| t.len() == 2 && t.ends_with(':')) {
        return Some("format wipes a drive");
    }
    if (has("remove-item") || has("ri") || has("rm"))
        && tokens.iter().any(|t| t.starts_with("-rec"))
        && tokens.iter().any(|t| t.starts_with("-for") || *t == "-f")
    {
        return Some("Remove-Item -Recurse -Force deletes a directory tree");
    }
    None
}

/// Known command-wrappers that re-launch a following command word, so the real
/// interpreter hides behind them (`curl … | sudo sh`, `… | xargs sh`).
const PIPE_WRAPPERS: &[&str] = &[
    "sudo", "doas", "xargs", "env", "command", "exec", "nice", "nohup", "time", "stdbuf", "setsid",
    "ionice",
];

/// True when the command that reads a pipeline stage is an interpreter — either
/// directly (`| sh`) or behind a known wrapper (`| sudo sh`, `| env FOO=bar sh`).
/// The scan is bounded to the stage's first command segment (it stops at `;`/`&`)
/// so an unrelated later `sh` isn't blamed; and a benign `grep sh` / `sed
/// 's/sh/zsh/'` stays unflagged because those tools are not wrappers, so their
/// interpreter-named *arguments* are never inspected.
fn pipe_stage_runs_interpreter(stage: &str, interpreters: &[&str]) -> bool {
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

/// Strip surrounding shell grouping punctuation from a word so the real command
/// behind `{ … }` / `( … )` is what gets classified (`(sh)` -> `sh`). A free fn
/// (not a closure) so the borrowed return is tied to the input's lifetime.
fn degroup(w: &str) -> &str {
    w.trim_matches(['(', ')', '{', '}'])
}

/// A `VAR=val` shell environment assignment (a leading `env FOO=bar` form or a
/// bare prefix assignment), which precedes the real command word in a stage.
fn is_env_assignment(token: &str) -> bool {
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
fn command_runs_keyword(scan: &str, keyword: &str) -> bool {
    scan.split([';', '&', '|', '\n']).any(|seg| {
        seg.split_whitespace()
            .map(degroup)
            .find(|w| !w.is_empty() && !w.starts_with('-') && !is_env_assignment(w))
            .is_some_and(|w| shell_token_command_name(w) == keyword)
    })
}

/// The inline-code flag(s) for a non-shell interpreter, or `None` if `name` is
/// not one. Deliberately per-interpreter (not one shared set): `-r` is inline
/// code for PHP but "require a library" for Ruby/Perl, and `-c` is inline code
/// for Python but a syntax-check-only for Node — a shared set would over-flag
/// those benign forms. PowerShell and awk are handled by their own arms in the
/// caller because their flag/positional shapes don't fit this table.
fn inline_code_flags(name: &str) -> Option<&'static [&'static str]> {
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

/// A PowerShell `-Command` / `-EncodedCommand` flag or any unambiguous
/// abbreviation (`-c`, `-com…`, `-e`, `-en…`, `-ec`). The argument is an inline
/// PowerShell program (often base64 for `-EncodedCommand`), opaque here.
fn is_powershell_code_flag(tok: &str) -> bool {
    matches!(tok, "-c" | "-e" | "-ec") || tok.starts_with("-com") || tok.starts_with("-enc")
}

/// True when a segment invokes a non-shell interpreter with an inline program.
/// Mirrors [`command_runs_keyword`]'s segment/command-word logic so a benign
/// interpreter-named *argument* (`time python`, `echo node`) isn't blamed.
fn runs_inline_interpreter_code(scan: &str) -> bool {
    scan.split([';', '&', '|', '\n']).any(|seg| {
        let words: Vec<&str> = seg.split_whitespace().collect();
        let Some(cmd) = words
            .iter()
            .copied()
            .map(degroup)
            .find(|w| !w.is_empty() && !w.starts_with('-') && !is_env_assignment(w))
        else {
            return false;
        };
        let name = shell_token_command_name(cmd);
        if let Some(flags) = inline_code_flags(&name) {
            if words
                .iter()
                .copied()
                .map(degroup)
                .any(|w| flags.contains(&w))
            {
                return true;
            }
        }
        if (is_versioned_name(&name, "pwsh") || is_versioned_name(&name, "powershell"))
            && words
                .iter()
                .copied()
                .map(degroup)
                .any(is_powershell_code_flag)
        {
            return true;
        }
        // awk's program is a positional arg, not a flag; `system("…")` is the only
        // way it shells out, and after quote-stripping the inner words carry no
        // clean shell token, so the scan above can't see it.
        if matches!(name.as_str(), "awk" | "gawk" | "mawk" | "nawk") && seg.contains("system(") {
            return true;
        }
        false
    })
}

/// A raw block-device path that a write/redirect could corrupt. Kept to the
/// disk families the redirect guard has always covered (`sd`/`nvme`/`hd`).
fn is_raw_block_device(target: &str) -> bool {
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
fn is_redirect_op(tok: &str) -> bool {
    let rest = tok.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.strip_prefix('&').unwrap_or(rest);
    matches!(rest, ">" | ">>" | ">|")
}

/// True when `name` is `base` or `base` followed only by a version suffix
/// (digits and dots): `python3`, `python3.11`, `bash5` all match — but
/// `bashful` does not. Without this, `curl … | python3` (the default name on
/// modern systems) silently bypassed the curl-pipe-shell guard.
fn is_versioned_name(name: &str, base: &str) -> bool {
    name == base
        || name.strip_prefix(base).is_some_and(|rest| {
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
}

fn detects_recursive_dangerous_rm(tokens: &[&str], command_names: &[String]) -> bool {
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

fn shell_token_command_name(token: &str) -> String {
    let token = token.trim_end_matches([';', '&', '|']);
    let normalized = normalize_shell_scan(token);
    let literal = normalized.split_whitespace().next().unwrap_or("");
    let literal: String = literal
        .chars()
        .filter(|c| !matches!(c, '"' | '\''))
        .collect();
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

fn git_checkout_discards_worktree(tokens: &[&str]) -> bool {
    tokens
        .iter()
        .any(|t| matches!(*t, "-f" | "--force") || short_flag_has(t, 'f'))
        || git_has_broad_restore_target(tokens)
}

fn git_restore_discards_worktree(tokens: &[&str]) -> bool {
    git_has_broad_restore_target(tokens)
}

fn git_has_broad_restore_target(tokens: &[&str]) -> bool {
    tokens
        .iter()
        .skip_while(|t| **t != "checkout" && **t != "restore")
        .skip(1)
        .filter(|t| !t.starts_with('-'))
        .any(|t| is_broad_git_target(t))
}

fn short_flag_has(token: &str, flag: char) -> bool {
    token.starts_with('-') && !token.starts_with("--") && token.chars().skip(1).any(|ch| ch == flag)
}

fn git_push_is_destructive(tokens: &[&str]) -> bool {
    if tokens.iter().any(|t| {
        matches!(
            *t,
            "--force" | "-f" | "--force-with-lease" | "--mirror" | "--delete" | "-d" | "--prune"
        ) || short_flag_has(t, 'f')
    }) {
        return true;
    }
    // A refspec argument to `push` that starts with `+` (forced update) or `:`
    // (delete the remote ref) rewrites/deletes remote history with no flag.
    tokens
        .iter()
        .skip_while(|t| **t != "push")
        .skip(1)
        .map(|t| t.trim_matches(|c: char| matches!(c, '"' | '\'')))
        .any(|t| !t.starts_with('-') && (t.starts_with('+') || t.starts_with(':')))
}

fn git_branch_force_deletes(tokens: &[&str], orig: &[&str]) -> bool {
    // `-D` is case-sensitive (force-delete an unmerged branch); the lowercased
    // `tokens` can't tell it from the benign `-d` (delete-if-merged), so read the
    // case-preserving `orig`. `--delete --force` is the long-form equivalent.
    orig.iter()
        .any(|t| t.starts_with('-') && !t.starts_with("--") && t.contains('D'))
        || (tokens.contains(&"--delete")
            && tokens
                .iter()
                .any(|t| *t == "--force" || short_flag_has(t, 'f')))
}

fn is_broad_git_target(token: &str) -> bool {
    let token = token.trim_end_matches([';', '&', '|']);
    let literal = token.trim_matches(|c| matches!(c, '"' | '\''));
    matches!(
        literal,
        "." | "./" | "./*" | ":/" | ":/*" | "*" | ":(top)" | ":(top)/*"
    )
}

/// A recursive-`chmod`/`chown` target whose blast radius is the OS, a home dir,
/// or the filesystem root. Unlike [`is_dangerous_rm_target`] it deliberately
/// ignores relative targets (`.`, `*`) — a recursive mode change under the
/// project tree is common and not worth an override prompt.
fn is_dangerous_chmod_target(token: &str) -> bool {
    let token = token.trim_end_matches([';', '&', '|']);
    let literal = token.trim_matches(|c| matches!(c, '"' | '\''));
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

fn is_dangerous_rm_target(token: &str) -> bool {
    let token = token.trim_end_matches([';', '&', '|']);
    let literal = token.trim_matches(|c| matches!(c, '"' | '\''));
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
fn is_root_equivalent(literal: &str) -> bool {
    literal.starts_with('/') && literal.split('/').all(|s| matches!(s, "" | "." | ".."))
}

/// Best-effort detection of absolute paths whose recursive deletion would
/// devastate the OS or wipe a user's home. `classify_danger` is defense-in-depth
/// (it refuses pending an explicit override), not a sandbox, so erring toward
/// flagging is acceptable; children of non-OS roots like `/var/tmp` are left
/// alone to avoid drowning legitimate cleanups in override prompts.
fn is_critical_system_path(literal: &str) -> bool {
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

fn has_path_prefix(target: &str, prefix: &str) -> bool {
    target
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn has_shell_var_path_prefix(target: &str, var: &str) -> bool {
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

#[cfg(test)]
mod tests;
