pub(super) fn git_checkout_discards_worktree(tokens: &[&str]) -> bool {
    tokens
        .iter()
        .any(|t| matches!(*t, "-f" | "--force") || short_flag_has(t, 'f'))
        || git_has_broad_restore_target(tokens)
}

pub(super) fn git_restore_discards_worktree(tokens: &[&str]) -> bool {
    git_has_broad_restore_target(tokens)
}

pub(super) fn git_has_broad_restore_target(tokens: &[&str]) -> bool {
    tokens
        .iter()
        .skip_while(|t| **t != "checkout" && **t != "restore")
        .skip(1)
        .filter(|t| !t.starts_with('-'))
        .any(|t| is_broad_git_target(t))
}

pub(super) fn short_flag_has(token: &str, flag: char) -> bool {
    token.starts_with('-') && !token.starts_with("--") && token.chars().skip(1).any(|ch| ch == flag)
}

pub(super) fn git_push_is_destructive(tokens: &[&str]) -> bool {
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

pub(super) fn git_branch_force_deletes(tokens: &[&str], orig: &[&str]) -> bool {
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

pub(super) fn is_broad_git_target(token: &str) -> bool {
    let token = token.trim_end_matches([';', '&', '|']);
    let literal = token.trim_matches(|c| matches!(c, '"' | '\''));
    matches!(
        literal,
        "." | "./" | "./*" | ":/" | ":/*" | "*" | ":(top)" | ":(top)/*"
    )
}
