//! Best-effort destructive-command classification (`classify_danger`) and
//! its helpers. Split out of `shell`; logic unchanged.

pub fn classify_danger(command: &str) -> Option<&'static str> {
    let lower = command.to_ascii_lowercase();
    let scan = normalize_shell_scan(&lower);
    // Tokenize on the shell control operators (`; & | newline ( )`) AS WELL AS
    // whitespace, so a destructive command word fused to an operator with no
    // surrounding space (`ls;rm -rf /`, `true&&rm -rf /`, `dir&del /s …`,
    // `(rm -rf /)`) is still recognized as its own command. Splitting on
    // whitespace alone hid that word inside one token (`ls;rm`), and every
    // `has(...)`-based guard below — which compares against bare command names —
    // silently missed it, letting the fused form slip past the override prompt.
    let tokens: Vec<&str> = operator_split(&scan);
    let raw_tokens: Vec<&str> = operator_split(&lower);
    // Whitespace-only tokens, used solely for the redirect-to-block-device scan
    // below, which parses glued `>`/`>|`/`|` inside a token (`>|/dev/sda`) and so
    // must NOT have `|` split out from under it.
    let ws_tokens: Vec<&str> = scan.split_whitespace().collect();
    let command_names: Vec<String> = tokens.iter().map(|t| shell_token_command_name(t)).collect();
    let raw_command_names: Vec<String> = raw_tokens
        .iter()
        .map(|t| shell_token_command_name(t))
        .collect();
    let has = |t: &str| command_names.iter().any(|name| name == t);
    // Case-preserving tokens for the few flags whose case matters (`git branch
    // -D` force-delete vs the benign `-d`), since `lower`/`tokens` are lowercased.
    let orig_tokens: Vec<&str> = operator_split(command);
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
    let redirect_to_block_device = ws_tokens
        .iter()
        .any(|t| t.split(['>', '|']).skip(1).any(is_raw_block_device))
        || ws_tokens
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
    // Partition editors and low-level disk writers that destroy a disk when
    // handed a raw block device (`blkdiscard /dev/sda`, `sgdisk --zap-all
    // /dev/sda`, `parted /dev/sda mklabel gpt`, `mke2fs /dev/sda1`, `tar cf
    // /dev/sda .`). Each needs a `/dev/sd*`-style target to do harm, so requiring
    // one keeps read-only forms without a device (`fdisk -l`) unflagged.
    const BLOCK_DEVICE_MUTATORS: &[&str] = &[
        "blkdiscard",
        "sgdisk",
        "gdisk",
        "sfdisk",
        "fdisk",
        "parted",
        "mke2fs",
        "mkntfs",
        "newfs",
        "tar",
        "cpio",
    ];
    if command_names
        .iter()
        .any(|n| BLOCK_DEVICE_MUTATORS.contains(&n.as_str()))
        && tokens.iter().any(|t| is_raw_block_device(t))
    {
        return Some("repartitions or overwrites a raw block device");
    }
    // ATA secure-erase wipes the whole drive irrecoverably.
    if has("hdparm") && tokens.iter().any(|t| t.starts_with("--security-erase")) {
        return Some("hdparm --security-erase wipes a drive");
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
    const PIPE_INTERPRETERS: &[&str] = &[
        // POSIX shells.
        "sh",
        "bash",
        "zsh",
        "dash",
        "ksh",
        "fish",
        // Script interpreters that execute bytes read from stdin just as readily
        // as a shell does (`curl … | node`, `… | ruby`, `… | pwsh -c -`). Leaving
        // these off let the curl-pipe-shell guard be sidestepped by any non-shell
        // interpreter; `is_versioned_name` covers `python3`, `ruby3.2`, etc.
        "python",
        "perl",
        "node",
        "nodejs",
        "ruby",
        "php",
        "pwsh",
        "powershell",
        "deno",
        "bun",
        "osascript",
        "rscript",
        "lua",
        "tclsh",
    ];
    // Any pipeline stage after the first that feeds bytes into an interpreter
    // runs attacker-controlled output as code. Gating this on a curl/wget
    // *source* missed every other producer (`cat x | sh`, `base64 -d | bash`,
    // `xargs sh`), so the source is no longer required.
    let pipes_into_interpreter = lower
        .split('|')
        .skip(1)
        .any(|stage| pipe_stage_runs_interpreter(stage, PIPE_INTERPRETERS));
    if pipes_into_interpreter {
        return Some("piping output into a command interpreter");
    }
    // `find … | xargs rm -rf` (or `… | parallel rm -rf`) recursively force-deletes
    // whatever paths the upstream stage emits. The deletion targets arrive on
    // stdin, so there is no dangerous target *token* for the rm guard above to
    // match — the same hidden-target case as `rm -rf $(…)`, which is flagged.
    if lower.split('|').skip(1).any(pipe_stage_xargs_recursive_rm) {
        return Some("xargs/parallel feeds stdin paths to a recursive rm");
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
    // Command substitution in the command-WORD position hides what actually runs
    // (`` `echo rm` -rf / ``, `$(printf rm) -rf /`). `is_dangerous_rm_target` only
    // covers substitution in the *target* position, so pair the hidden command
    // word with a recursive flag and a dangerous target to catch this form.
    if command_word_uses_substitution(&lower)
        && tokens.iter().any(|t| is_recursive_delete_flag(t))
        && tokens
            .iter()
            .any(|t| !t.starts_with('-') && is_dangerous_rm_target(t))
    {
        return Some("command substitution hides a recursive delete");
    }
    // `bash <(curl evil)` / `sh <(echo rm -rf /)`: a shell interpreter handed a
    // process substitution runs its inner command as a script — the same
    // download-and-run / hidden-command risk as `| bash`, reached via `<(…)`.
    if shell_runs_process_substitution(&scan) {
        return Some("shell runs a process-substitution script");
    }
    // Windows cmd / PowerShell destructive verbs. The classifier runs on every
    // platform and the Unix vocabulary above never matches these; on Windows the
    // sandbox does not confine the filesystem, so this gate matters most there.
    if (has("del") || has("erase") || has("rd") || has("rmdir")) && has_cmd_switch(&tokens, 's') {
        return Some("del/rd /s recursively deletes a directory tree");
    }
    // `del`/`erase` at a drive root (`del c:\* /q`, `del \*.*`) wipes the root's
    // files even without `/s` — the cmd.exe analog of `rm -rf /*`. Use the
    // pre-normalize `raw_tokens`: `normalize_shell_scan` eats the `\` (a Unix
    // escape), turning `c:\*` into `c:*` and hiding the root anchor.
    if (has("del") || has("erase"))
        && raw_tokens
            .iter()
            .any(|t| !t.starts_with('-') && is_windows_drive_root_glob(t))
    {
        return Some("del at a drive root deletes everything there");
    }
    if has("format") && tokens.iter().any(|t| t.len() == 2 && t.ends_with(':')) {
        return Some("format wipes a drive");
    }
    // PowerShell volume/disk wipes — the cmdlet analog of `format`/`mkfs`.
    if has("format-volume") || has("clear-disk") {
        return Some("Format-Volume/Clear-Disk erases a volume or disk");
    }
    // PowerShell tree delete: `Remove-Item -Recurse -Force` and any unambiguous
    // prefix abbreviation. The old `-rec`/`-for` substring test missed `-r`,
    // `-fo`, and the spelled-out `-recurse`/`-force`. `remove-item`/`ri` are
    // PowerShell-only, so any `-r…` + `-f…` flags it. The `rm` alias is shared
    // with Unix, where `rm -r -f node_modules` is a routine cleanup — so for `rm`
    // require a distinctive `-fo…` force flag (never the bare Unix `-f`) to avoid
    // an override prompt on every Unix recursive delete.
    let ps_recurse = tokens.iter().any(|t| is_ps_param(t, "-recurse"));
    if ps_recurse
        && (has("remove-item") || has("ri"))
        && tokens.iter().any(|t| is_ps_param(t, "-force"))
    {
        return Some("Remove-Item -Recurse -Force deletes a directory tree");
    }
    if ps_recurse
        && has("rm")
        && tokens
            .iter()
            .any(|t| t.len() >= 3 && is_ps_param(t, "-force"))
    {
        return Some("Remove-Item -Recurse -Force deletes a directory tree");
    }
    None
}

/// Split a command string into command/argument words, treating the shell
/// control operators `;`, `&`, `|`, newline, and `(`/`)` as word boundaries
/// even when glued to an adjacent word, then splitting the pieces on whitespace.
/// `ls;rm`, `true&&rm`, `dir&del`, and `(rm` all yield a bare `rm`/`del` word,
/// so the command-name guards in `classify_danger` see the fused command. The
/// returned slices borrow from `s`. Empty pieces (from `&&`, `||`, doubled
/// separators) drop out via `split_whitespace`.
mod git;
mod interp;
mod scan;
mod targets;

use git::*;
use interp::*;
use scan::*;
use targets::*;

#[cfg(test)]
mod tests;
