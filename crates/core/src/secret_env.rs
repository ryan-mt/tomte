//! Secret-bearing environment-variable scrubbing, shared by child process
//! spawners. Keeping it in one place means a token can't be exfiltrated through
//! whichever helper path forgot to scrub.

/// Env vars whose names contain any of these substrings are scrubbed from a
/// child's environment. Prevents the LLM (or an untrusted MCP server / hook)
/// from exfiltrating tokens via `env | curl …`. Substring match
/// (case-insensitive) catches the long tail of `*_TOKEN`, `*_KEY`, `*_SECRET`.
const ENV_DENYLIST_SUBSTRINGS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "_PASS", // *_PASS (DB_PASS, SMTP_PASS); the `_` spares BYPASS/COMPASS-style names
    "_PWD",  // *_PWD (e.g. MYSQL_PWD); not bare PWD/OLDPWD (no `_PWD` substring)
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "PRIVATE_KEY",
    "_KEY", // the long tail of *_KEY the comment promised (FOO_KEY, STRIPE_KEY)
    "CREDENTIALS",
    "DATABASE_URL", // routinely embeds inline creds (postgres://user:pass@host/db)
    // Sibling connection strings that embed inline `user:pass@host` creds, matched
    // by specific name (not a blanket `_URL`/`_URI`) so non-secret `REDIS_HOST` /
    // `MONGO_HOST` style vars are not over-scrubbed.
    "REDIS_URL",
    "MONGODB_URI",
    "MONGODB_URL",
    "AMQP_URL",
    "RABBITMQ_URL",
    "CELERY_BROKER_URL",
    "_DSN",    // SENTRY_DSN and friends carry a secret
    "WEBHOOK", // webhook URLs are bearer-secret endpoints
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
    // Secret material that does NOT follow the *_KEY / *_TOKEN / *_SECRET
    // convention the catch-alls above already cover. Each substring below is
    // chosen to avoid colliding with benign vars (PATH/HOME/USER/LANG/TERM/…).
    "JWT",        // raw JSON Web Tokens
    "BEARER",     // bearer credentials
    "OAUTH",      // OAuth client material
    "_SID",       // service IDs used as secrets (e.g. TWILIO_ACCOUNT_SID)
    "SIGNING",    // signing keys/secrets
    "ENCRYPTION", // encryption keys
    "MNEMONIC",   // wallet seed phrases
    "STRIPE",     // Stripe keys (sk_live_…, often unsuffixed)
    "TWILIO",     // Twilio auth token / SID
    "SENDGRID",   // SendGrid API credentials
    "DOPPLER",    // Doppler secrets-manager token
    // Secret-store / provider names whose auth material does not always follow
    // the *_TOKEN / *_KEY / *_SECRET convention the catch-alls cover (e.g.
    // VAULT_ROLE_ID). Each is chosen to avoid benign collisions (DEFAULT has no
    // "VAULT"; "_PAT" was rejected because it is a substring of "_PATH").
    "VAULT",        // HashiCorp Vault (VAULT_ROLE_ID / VAULT_SECRET_ID / token)
    "GITLAB",       // GitLab CI job/deploy credentials
    "CLOUDFLARE",   // Cloudflare API credentials
    "HEROKU",       // Heroku platform API auth
    "DIGITALOCEAN", // DigitalOcean API auth
    "SLACK",        // Slack bot/app credentials
    "DISCORD",      // Discord bot credentials
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
pub fn scrub_secret_env(cmd: &mut tokio::process::Command) {
    for (k, _) in std::env::vars() {
        if is_secret_env_name(&k) {
            cmd.env_remove(&k);
        }
    }
}

pub fn scrub_secret_env_std(cmd: &mut std::process::Command) {
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
            "DB_PASS",
            "SMTP_PASS",
            "PGPASSWORD",
            "MY_WEBHOOK_URL",
            "SENTRY_DSN",
            "KUBECONFIG",
            "DOCKER_AUTH_CONFIG",
            "GPG_PASSPHRASE",
            "NETRC",
            "SSH_AUTH_SOCK",
            "SSH_AGENT_PID",
            "JWT",
            "MY_BEARER",
            "GOOGLE_OAUTH",
            "TWILIO_ACCOUNT_SID",
            "REQUEST_SIGNING",
            "DATA_ENCRYPTION",
            "WALLET_MNEMONIC",
            "STRIPE",
            "TWILIO_AUTH",
            "SENDGRID",
            "DOPPLER",
            "VAULT_ROLE_ID",
            "VAULT_ADDR",
            "GITLAB_DEPLOY_TOKEN",
            "CLOUDFLARE_API_TOKEN",
            "HEROKU_API_KEY",
            "DIGITALOCEAN_ACCESS_TOKEN",
            "SLACK_BOT_TOKEN",
            "DISCORD_BOT_TOKEN",
            // Connection strings that embed inline user:pass@host creds.
            "REDIS_URL",
            "MONGODB_URI",
            "AMQP_URL",
            "CELERY_BROKER_URL",
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
            // Must survive the newly added provider substrings (collision guard):
            // DEFAULT/FAULT contain no "VAULT".
            "DEFAULT_TIMEOUT",
            "FAULT_TOLERANCE",
            // Non-secret host/port siblings of the connection-string URLs must
            // survive: the URL entries are matched by specific name, not a bare
            // REDIS/MONGO prefix.
            "REDIS_HOST",
            "REDIS_PORT",
            "MONGO_HOST",
            // `_PASS` requires the underscore: BYPASS/COMPASS-style names survive.
            "CACHE_BYPASS",
            "COMPASS_DIR",
        ] {
            assert!(!is_secret_env_name(name), "should NOT scrub {name}");
        }
    }
}
