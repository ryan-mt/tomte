//! Best-effort destructive-command classification (`classify_danger`) and
//! its helpers. Split out of `shell`; logic unchanged.

pub fn classify_danger(command: &str) -> Option<&'static str> {
    let lower = command.to_ascii_lowercase();
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    let command_names: Vec<String> = tokens.iter().map(|t| shell_token_command_name(t)).collect();
    let has = |t: &str| command_names.iter().any(|name| name == t);
    // Case-preserving tokens for the few flags whose case matters (`git branch
    // -D` force-delete vs the benign `-d`), since `lower`/`tokens` are lowercased.
    let orig_tokens: Vec<&str> = command.split_whitespace().collect();
    let stripped: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if stripped.contains(":(){:|:&};:") {
        return Some("fork bomb pattern detected");
    }
    if let Some(rm_idx) = command_names.iter().position(|n| n == "rm") {
        let is_recursive = tokens.iter().any(|t| {
            matches!(*t, "-rf" | "-fr" | "-r" | "-R" | "--recursive")
                || (t.starts_with('-')
                    && !t.starts_with("--")
                    && t.contains('r')
                    && t.contains('f'))
        });
        if is_recursive {
            // Deletion targets are the non-flag tokens AFTER the `rm` executable.
            // Command/wrapper words (`rm`, `/bin/rm`, `sudo …`) sit at or before
            // `rm_idx`, so an executable path like `/bin/rm` isn't mistaken for a
            // target under `/bin` — yet a genuine target like `/etc/sudo` (which
            // appears after rm) is still inspected.
            let dangerous_target = tokens
                .iter()
                .skip(rm_idx + 1)
                .any(|t| !t.starts_with('-') && is_dangerous_rm_target(t));
            if dangerous_target {
                return Some("recursive rm targeting root, home, or glob");
            }
        }
    }
    if command_names
        .iter()
        .any(|t| t == "mkswap" || t == "mkfs" || t.starts_with("mkfs."))
    {
        return Some("filesystem format command");
    }
    if has("dd") {
        let writes_block_device = tokens.iter().any(|t| {
            let t = t.trim_start_matches("of=");
            t.starts_with("/dev/sd")
                || t.starts_with("/dev/nvme")
                || t.starts_with("/dev/mmcblk")
                || t.starts_with("/dev/hd")
                || t == "/dev/disk"
        });
        if writes_block_device {
            return Some("dd writing to a raw block device");
        }
    }
    // A redirect to a raw block device, whether the `>`/`>>` is glued to the
    // target (`>/dev/sda`, `x>>/dev/nvme0`) or a separate token (`> /dev/sda`).
    // Only segments that follow a `>` count, so a plain `/dev/sda` argument
    // (e.g. `cat /dev/sda`, a read) is not flagged.
    let redirect_to_block_device = tokens
        .iter()
        .any(|t| t.split('>').skip(1).any(is_raw_block_device))
        || tokens
            .windows(2)
            .any(|w| (w[0] == ">" || w[0] == ">>") && is_raw_block_device(w[1]));
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
        && tokens.iter().any(|t| *t == "/" || *t == "/*")
    {
        return Some("recursive chmod/chown at filesystem root");
    }
    if has("git") && has("push") && git_push_is_destructive(&tokens) {
        return Some("git push can rewrite or delete remote history");
    }
    if has("git") && has("reset") && tokens.contains(&"--hard") {
        return Some("git reset --hard discards uncommitted work");
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
    const PIPE_INTERPRETERS: &[&str] = &["sh", "bash", "zsh", "dash", "python", "perl"];
    let pipes_into_interpreter = tokens.iter().any(|token| {
        token
            .rsplit_once('|')
            .is_some_and(|(_, rhs)| pipe_rhs_is_interpreter(rhs, PIPE_INTERPRETERS))
    }) || tokens.windows(2).any(|w| {
        let rhs = w[1].trim_start_matches('|');
        w[0] == "|" && pipe_rhs_is_interpreter(rhs, PIPE_INTERPRETERS)
            || w[0].ends_with('|') && pipe_rhs_is_interpreter(w[1], PIPE_INTERPRETERS)
            || w[1].starts_with('|') && pipe_rhs_is_interpreter(rhs, PIPE_INTERPRETERS)
    });
    if (has("curl") || has("wget")) && pipes_into_interpreter {
        return Some("piping curl/wget output into a shell");
    }
    None
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

fn pipe_rhs_is_interpreter(rhs: &str, interpreters: &[&str]) -> bool {
    let rhs = rhs.trim_start_matches('|');
    if rhs.is_empty() {
        return false;
    }
    let name = shell_token_command_name(rhs);
    interpreters
        .iter()
        .any(|base| is_versioned_name(&name, base))
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

fn shell_token_command_name(token: &str) -> String {
    let token = token.trim_end_matches([';', '&', '|']);
    // Shells let quotes appear anywhere inside a word (`r''m`, `"rm"`, `rm''`
    // all execute `rm`), so strip every quote char — not just the surrounding
    // ones — before taking the basename, otherwise `r''m -rf /` slips past.
    let literal: String = token.chars().filter(|c| !matches!(c, '"' | '\'')).collect();
    literal.rsplit(['/', '\\']).next().unwrap_or("").to_string()
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
            "--force" | "-f" | "--force-with-lease" | "--mirror" | "--delete" | "-d"
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

fn is_dangerous_rm_target(token: &str) -> bool {
    let token = token.trim_end_matches([';', '&', '|']);
    let literal = token.trim_matches(|c| matches!(c, '"' | '\''));
    if matches!(
        literal,
        "/" | "/*" | "." | "./" | "./*" | "./.*" | ".." | "../*" | ".*" | "*"
    ) {
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
