use super::*;

pub(super) const CONFIG_DIR_NAME: &str = "tomte";

pub(super) static SAVE_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn config_dir() -> PathBuf {
    // An explicit `TOMTE_CONFIG_DIR` relocates the whole config tree (config,
    // auth, sessions, logs). Used by power users to keep state off the default
    // OS location, and by tests to isolate onto a scratch dir on every platform
    // — `dirs::config_dir()` only honors `XDG_CONFIG_HOME` on Unix, so a Windows
    // test that set that alone would silently write to the real `%APPDATA%`.
    if let Some(dir) = std::env::var_os("TOMTE_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    // Never fall back to the current working directory: that risks writing
    // `auth.json` (OAuth tokens) into a project checkout that then gets
    // git-committed. Prefer the OS config dir, then `~/.config`, then a temp
    // dir — anything but the cwd.
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join(CONFIG_DIR_NAME)
}

/// Create `dir` (recursively) restricted to the owner. The config dir holds
/// `auth.json` (mode 0o600) and `config.json`; the directory itself must be
/// 0o700 too, or with the usual umask it lands at 0o755 and other local users
/// can list it and stat the files (leaking login/refresh timestamps).
pub fn create_dir_secure(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        // Repair an existing dir created before this (or under a looser umask).
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Tighten a secret-bearing file to the current user — the cross-platform
/// `chmod 600`: Unix file mode, Windows owner-only ACL via `icacls`. Public so
/// CLI-side writers of credential-adjacent files (e.g. settings.json, whose MCP
/// server `env` may carry tokens) can apply the same enforcement `auth.json`
/// and `config.json` get. Best-effort, matching `save_auth`'s Windows posture.
pub fn restrict_file_to_owner(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(windows)]
    crate::auth::storage::restrict_to_owner_windows(path);
    #[cfg(not(any(unix, windows)))]
    let _ = path;
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

/// Strip a leading UTF-8 BOM before JSON parsing. Editors on Windows commonly
/// write one, and `serde_json` rejects it — which silently turned a valid
/// user-edited file (config.json, settings.json, a project's package.json)
/// into "unparseable", falling back to defaults / empty as if the file weren't
/// there. npm itself tolerates a BOM'd package.json, so tomte should too.
/// `pub` so the CLI's settings.json read-modify-write paths share the same
/// tolerance as the core loaders.
pub fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

pub fn load() -> Config {
    let path = config_file();
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<Config>(strip_bom(&s)) {
            Ok(cfg) => cfg,
            Err(e) => {
                // Silently resetting to defaults on a corrupt file used to make
                // model/effort changes appear to vanish — log loudly so the
                // user sees something is wrong instead of debugging mystery
                // setting resets.
                tracing::warn!(
                    config = %path.display(),
                    error = %e,
                    "config.json parse failed; falling back to defaults"
                );
                Config::default()
            }
        },
        Err(_) => Config::default(),
    };
    // Normalise the configured model: accept an explicit built-in
    // `provider/model` spec, preserve custom provider specs, then auto-upgrade
    // legacy placeholder names from earlier tomte builds.
    let normalized = normalize_model_name(&cfg.model);
    if normalized != cfg.model {
        tracing::info!(
            old = %cfg.model,
            new = %normalized,
            "normalizing model name in config.json"
        );
        cfg.model = normalized;
    }
    cfg
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    save_to_path(&config_file(), cfg)
}

pub(super) fn save_to_path(path: &Path, cfg: &Config) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        create_dir_secure(dir)?;
        // Windows: config.json can hold a literal provider `api_key` (a real
        // credential), so it deserves the same owner-only ACL `auth.json` gets —
        // Unix already writes it `0o600` below. Harden the dir with inheritance
        // first so the temp file is owner-only from birth, mirroring `save_auth`.
        #[cfg(windows)]
        crate::auth::storage::restrict_dir_to_owner_windows(dir);
    }
    let persistable = persist_view(cfg);
    let text = serde_json::to_string_pretty(&persistable).unwrap();
    // Atomic write: a SIGKILL between truncate and write previously left
    // config.json empty, silently resetting all settings on next launch.
    let tmp = unique_tmp_path(path);
    write_config_file(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

#[cfg(unix)]
pub(crate) fn write_config_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn write_config_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    // Owner-only ACL parity with auth.json and the Unix 0o600 path: config.json
    // may carry a literal provider api_key. No-op on a non-Windows, non-Unix
    // target (which can't enforce owner-only perms anyway).
    #[cfg(windows)]
    crate::auth::storage::restrict_to_owner_windows(path);
    Ok(())
}

pub fn redacted_view(cfg: &Config) -> Config {
    let mut out = cfg.clone();
    for provider in out.providers.values_mut() {
        if provider.api_key.as_ref().is_some_and(|key| !key.is_empty()) {
            provider.api_key = Some("<redacted>".to_string());
        }
    }
    out
}

/// `max` is the heaviest adaptive-thinking tier on Anthropic and is
/// deliberately session-only — relaunching the CLI should not silently
/// re-engage the heaviest spend tier. Persist it as `xhigh` (next step
/// down). OpenAI models are untouched.
pub(super) fn persist_view(cfg: &Config) -> Config {
    let mut out = cfg.clone();
    out.model = normalize_model_name(&out.model);
    if out.reasoning_effort == "max"
        && crate::provider::Provider::from_model(&out.model) == crate::provider::Provider::Anthropic
    {
        out.reasoning_effort = "xhigh".to_string();
    }
    out
}
