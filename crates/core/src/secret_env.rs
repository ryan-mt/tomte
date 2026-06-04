//! Secret-bearing environment-variable scrubbing, shared by every child process
//! the agent spawns: `run_shell`, MCP servers, and lifecycle hooks. Keeping it in
//! one place means a token can't be exfiltrated through whichever spawner forgot
//! to scrub. Moved here from `tools::shell::support` so non-shell spawners can
//! reuse it.

/// Env vars whose names contain any of these substrings are scrubbed from a
/// child's environment. Prevents the LLM (or an untrusted MCP server / hook)
/// from exfiltrating tokens via `env | curl …`. Substring match
/// (case-insensitive) catches the long tail of `*_TOKEN`, `*_KEY`, `*_SECRET`.
const ENV_DENYLIST_SUBSTRINGS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "_PWD", // *_PWD (e.g. MYSQL_PWD); not bare PWD/OLDPWD (no `_PWD` substring)
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "PRIVATE_KEY",
    "_KEY", // the long tail of *_KEY the comment promised (FOO_KEY, STRIPE_KEY)
    "CREDENTIALS",
    "DATABASE_URL", // routinely embeds inline creds (postgres://user:pass@host/db)
    "_DSN",         // SENTRY_DSN and friends carry a secret
    "WEBHOOK",      // webhook URLs are bearer-secret endpoints
    "OPENAI",
    "ANTHROPIC",
    "AWS_",
    "GOOGLE_",
    "GITHUB_",
    "GH_",
    "SUPABASE",
    "PASSPHRASE",  // GPG/SSH key passphrases
    "KUBECONFIG",  // path to a kube credentials file
    "DOCKER_AUTH", // DOCKER_AUTH_CONFIG embeds registry creds
    "NETRC",       // points at ~/.netrc (login:password pairs)
    "SSH_AUTH",    // SSH_AUTH_SOCK — live ssh-agent socket (auth without a key)
    "SSH_AGENT",   // SSH_AGENT_PID and friends
    "GPG_AGENT",   // GPG_AGENT_INFO — live gpg-agent socket
];

/// Whether an env var name looks secret enough to scrub before spawning a child
/// process. Case-insensitive substring match over [`ENV_DENYLIST_SUBSTRINGS`].
pub(crate) fn is_secret_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ENV_DENYLIST_SUBSTRINGS.iter().any(|p| upper.contains(p))
}

/// Remove inherited secret-bearing env vars from a child command before spawn,
/// so a spawned shell, MCP server, or hook can't read or echo the API keys,
/// tokens, and live agent sockets the agent process itself holds. A caller that
/// must pass an intended secret (e.g. an MCP server's configured `env`) should
/// re-apply it *after* this scrub.
pub(crate) fn scrub_secret_env(cmd: &mut tokio::process::Command) {
    for (k, _) in std::env::vars() {
        if is_secret_env_name(&k) {
            cmd.env_remove(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_env_names_are_scrubbed_without_eating_benign_ones() {
        for name in [
            "GITHUB_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "OPENAI_API_KEY",
            "DATABASE_URL",
            "STRIPE_KEY",
            "FOO_KEY",
            "MYSQL_PWD",
            "PGPASSWORD",
            "MY_WEBHOOK_URL",
            "SENTRY_DSN",
            "KUBECONFIG",
            "DOCKER_AUTH_CONFIG",
            "GPG_PASSPHRASE",
            "NETRC",
            "SSH_AUTH_SOCK",
            "SSH_AGENT_PID",
        ] {
            assert!(is_secret_env_name(name), "should scrub {name}");
        }
        // Benign session/info vars must survive (a child may legitimately use
        // them); only the agent socket itself is stripped.
        for name in [
            "PATH",
            "HOME",
            "LANG",
            "PWD",
            "OLDPWD",
            "SHELL",
            "TERM",
            "SSH_CONNECTION",
            "SSH_CLIENT",
        ] {
            assert!(!is_secret_env_name(name), "should NOT scrub {name}");
        }
    }
}
