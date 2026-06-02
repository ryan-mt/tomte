use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const CONFIG_DIR_NAME: &str = "opencli";
static SAVE_TMP_SEQ: AtomicU64 = AtomicU64::new(0);
// `ultracode` is Claude Code's top effort-menu entry. Per the Anthropic docs it
// is not a distinct API effort level — it pairs `xhigh` thinking with standing
// permission to launch multi-agent workflows. opencli accepts it as a selectable
// effort and maps it onto `xhigh` on the wire (see translate.rs / openai client).
pub const VALID_REASONING_EFFORTS: &[&str] = &[
    "none",
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "ultracode",
    "max",
];
pub const VALID_VERBOSITIES: &[&str] = &["low", "medium", "high"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_reasoning_effort")]
    pub reasoning_effort: String,
    #[serde(default = "default_verbosity")]
    pub verbosity: String,
    /// Automatically compact the conversation when the context window nears
    /// its limit (~85%), replacing history with a model-generated summary so
    /// long sessions don't overflow or re-bill the full transcript each turn.
    #[serde(default = "default_auto_compact")]
    pub auto_compact: bool,
    #[serde(default)]
    pub auto_approve_read: bool,
    #[serde(default)]
    pub auto_approve_write: bool,
    /// Permission mode the TUI starts in, persisted across launches. One of
    /// `default`, `acceptEdits`, `plan`, `bypassPermissions`. Shift+Tab in the
    /// TUI cycles the mode and writes the new value here, so relaunching keeps
    /// the last-chosen mode. Mirrors Claude Code's `permissions.defaultMode`.
    #[serde(default = "default_permission_mode")]
    pub default_permission_mode: String,
    /// Extra OpenAI-compatible providers, keyed by the id used in the `model`
    /// field as `<id>/<model>` (e.g. `groq/llama-3.3-70b`). Optional and empty
    /// by default, so existing configs are unaffected.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// Ordered models to fall back to when the active model is rate-limited or
    /// its provider is overloaded. Each entry is a spec in the same form as
    /// [`model`](Config::model) (`gpt-5.5`, `claude-opus-4-8`, or
    /// `<provider-id>/<model>` for a configured endpoint). Empty by default, so
    /// existing and single-model setups are unaffected. The list is
    /// provider-agnostic: a fallback may target a different provider or a local
    /// endpoint. (Consumed by [`crate::fallback`]; not yet wired into the turn
    /// loop — see that module.)
    #[serde(default)]
    pub fallback_models: Vec<String>,
}

/// Configuration for one OpenAI-compatible (`/v1/chat/completions`) provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// API base URL, e.g. `https://api.groq.com/openai/v1`. The adapter appends
    /// `/chat/completions`.
    pub base_url: String,
    /// Literal API key. Prefer `api_key_env` so keys stay out of config.json.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Name of an environment variable to read the API key from (checked first).
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// True input context window (in tokens) of this endpoint's model. opencli
    /// cannot probe a custom OpenAI-compatible provider, so the catalog's
    /// guess-by-model-name is usually wrong (it may claim 1M for a 128K model),
    /// which makes auto-compaction fire too late and the provider reject the
    /// request with "input exceeds the context window". Set this to the model's
    /// real window so compaction triggers in time. Falls back to a conservative
    /// [`DEFAULT_PROVIDER_CONTEXT_LIMIT`] when unset.
    #[serde(default)]
    pub context_limit: Option<u64>,
    /// Forward the selected reasoning effort to this provider as the Chat
    /// Completions `reasoning_effort` field. Off by default because many
    /// OpenAI-compatible endpoints reject unknown fields with a 400; enable it
    /// only for reasoning models that accept `reasoning_effort` (OpenAI, Groq,
    /// DeepSeek, Together, …). `minimal`/`low`/`medium`/`high` pass through;
    /// `xhigh`/`max`/`ultracode` clamp to `high`; `none` and unknown levels have
    /// no standard value and are simply not forwarded.
    #[serde(default)]
    pub forward_reasoning_effort: bool,
}

/// Conservative input-window assumed for a configured provider that does not
/// declare `context_limit`. Most OpenAI-compatible models are 128K–256K, so
/// 200K leaves headroom while still being low enough that auto-compaction
/// fires before a real overflow. Users with a larger model raise it explicitly.
pub const DEFAULT_PROVIDER_CONTEXT_LIMIT: u64 = 200_000;

impl ProviderConfig {
    /// Resolve the API key: the env var named by `api_key_env` (if set and
    /// non-empty) wins, then the literal `api_key`, else empty (local servers
    /// such as Ollama/LM Studio accept no key).
    pub fn resolve_api_key(&self) -> String {
        if let Some(var) = &self.api_key_env {
            if let Ok(v) = std::env::var(var) {
                if !v.is_empty() {
                    return v;
                }
            }
        }
        self.api_key.clone().unwrap_or_default()
    }
}

impl Config {
    /// Effective input context window (tokens) for the active model — the value
    /// the warn/auto-compact thresholds and the status bar must use. A model
    /// routed through a configured provider (`<id>/<model>` whose `<id>` is in
    /// [`providers`](Config::providers)) takes that provider's declared
    /// `context_limit`, or [`DEFAULT_PROVIDER_CONTEXT_LIMIT`] when unset, because
    /// opencli can't infer a custom endpoint's real window from the model name.
    /// Built-in OpenAI/Anthropic models use the catalog value.
    pub fn effective_context_limit(&self) -> u64 {
        if let Some((prefix, _)) = self.model.split_once('/') {
            if let Some(pc) = self.providers.get(prefix) {
                return pc.context_limit.unwrap_or(DEFAULT_PROVIDER_CONTEXT_LIMIT);
            }
        }
        crate::catalog::context_limit(&self.model)
    }
}

fn default_model() -> String {
    "gpt-5.5".to_string()
}
fn default_reasoning_effort() -> String {
    "medium".to_string()
}
fn default_verbosity() -> String {
    "medium".to_string()
}
fn default_auto_compact() -> bool {
    true
}
fn default_permission_mode() -> String {
    "default".to_string()
}

/// Map model ids that opencli once surfaced but that don't resolve at the
/// OpenAI API onto the closest current model, so a returning user keeps a
/// working model after a catalog change. Idempotent.
///
/// NOTE: `gpt-5` and `gpt-5.2` are NOT remapped — they are real, currently
/// available OpenAI models (verified against the model docs, June 2026), so a
/// user who selects them keeps them rather than being silently forced onto the
/// default.
pub fn migrate_legacy_model_name(name: &str) -> String {
    match name {
        // gpt-5.1 / gpt-5.3 never resolved at the API; gpt-5-{pro,mini,nano}
        // were superseded by the gpt-5.4/5.5 generation. Map each onto its
        // closest current equivalent.
        "gpt-5.1" | "gpt-5.3" => "gpt-5.5".to_string(),
        "gpt-5-pro" => "gpt-5.5-pro".to_string(),
        "gpt-5-mini" => "gpt-5.4-mini".to_string(),
        "gpt-5-nano" => "gpt-5.4-nano".to_string(),
        other => other.to_string(),
    }
}

/// Normalize a model id accepted from config/CLI/UI. Built-in provider prefixes
/// (`openai/…`, `anthropic/…`) are stripped to the wire id; unknown prefixes are
/// preserved so custom OpenAI-compatible providers keep routing through
/// `Config.providers`.
pub fn normalize_model_name(name: &str) -> String {
    let (_, bare) = crate::provider::Provider::parse_model(name.trim());
    migrate_legacy_model_name(&bare)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            reasoning_effort: default_reasoning_effort(),
            verbosity: default_verbosity(),
            auto_compact: true,
            auto_approve_read: true,
            auto_approve_write: false,
            default_permission_mode: default_permission_mode(),
            providers: HashMap::new(),
            fallback_models: Vec::new(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
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

pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load() -> Config {
    let path = config_file();
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<Config>(&s) {
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
    // legacy placeholder names from earlier opencli builds.
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

/// `<cwd>/.opencli/config.json` — the optional project-local config overlay.
pub fn project_config_file(cwd: &Path) -> PathBuf {
    cwd.join(".opencli").join("config.json")
}

/// Top-level keys a project config may NOT override. A project `.opencli/`
/// ships in cloned repos, so letting one set these would let an untrusted repo
/// disable approval prompts, auto-approve writes, or redirect the model to an
/// arbitrary endpoint (leaking prompts / API keys). They stay global-only;
/// present keys are ignored with a warning.
const PROJECT_PROTECTED_KEYS: &[&str] = &[
    "default_permission_mode",
    "auto_approve_read",
    "auto_approve_write",
    "providers",
];

/// Safe, behavioral fields a project `.opencli/config.json` may override on top
/// of the global config. All optional; unknown keys (including the protected
/// ones) are dropped by serde and never applied here.
#[derive(Debug, Default, Deserialize)]
struct ProjectOverlay {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    verbosity: Option<String>,
    #[serde(default)]
    auto_compact: Option<bool>,
    #[serde(default)]
    fallback_models: Option<Vec<String>>,
}

/// Load the global config, then overlay a project-local `.opencli/config.json`
/// from `cwd` if present. Only safe behavioral fields are overlaid (see
/// [`PROJECT_PROTECTED_KEYS`]); a missing, oversized, or unparseable project
/// file is ignored and the global config stands.
pub fn load_for_cwd(cwd: &Path) -> Config {
    overlay_project_config(load(), cwd)
}

fn overlay_project_config(mut cfg: Config, cwd: &Path) -> Config {
    let path = project_config_file(cwd);
    let Some(text) = read_project_config(&path) else {
        return cfg;
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(config = %path.display(), error = %e, "project config.json parse failed; ignoring it");
            return cfg;
        }
    };
    if let Some(obj) = value.as_object() {
        for key in PROJECT_PROTECTED_KEYS {
            if obj.contains_key(*key) {
                tracing::warn!(
                    "ignoring project config override of `{key}` (global-only for safety)"
                );
            }
        }
    }
    match serde_json::from_value::<ProjectOverlay>(value) {
        Ok(overlay) => apply_project_overlay(&mut cfg, overlay),
        Err(e) => {
            tracing::warn!(config = %path.display(), error = %e, "project config overlay parse failed; ignoring it");
        }
    }
    cfg
}

/// Read a project config file, bounded to a sane size — it is attacker-influenced
/// (it ships in cloned repos) and a real config is tiny. Returns `None` when the
/// file is absent, not a regular file, too large, or unreadable.
fn read_project_config(path: &Path) -> Option<String> {
    const MAX_PROJECT_CONFIG_BYTES: u64 = 64 * 1024;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_PROJECT_CONFIG_BYTES {
        tracing::warn!(config = %path.display(), "project config.json too large; ignoring it");
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Apply a validated project overlay onto `cfg`. Model names are normalized and
/// effort/verbosity are validated; an invalid enum value is dropped rather than
/// overriding the global with garbage.
fn apply_project_overlay(cfg: &mut Config, overlay: ProjectOverlay) {
    if let Some(model) = overlay.model {
        cfg.model = normalize_model_name(&model);
    }
    if let Some(effort) = overlay.reasoning_effort {
        if let Some(v) = normalize_reasoning_effort(&effort) {
            cfg.reasoning_effort = v;
        }
    }
    if let Some(verbosity) = overlay.verbosity {
        if let Some(v) = normalize_verbosity(&verbosity) {
            cfg.verbosity = v;
        }
    }
    if let Some(auto_compact) = overlay.auto_compact {
        cfg.auto_compact = auto_compact;
    }
    if let Some(fallbacks) = overlay.fallback_models {
        cfg.fallback_models = fallbacks;
    }
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    save_to_path(&config_file(), cfg)
}

fn save_to_path(path: &Path, cfg: &Config) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        create_dir_secure(dir)?;
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
fn write_config_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
fn write_config_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
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

pub fn normalize_reasoning_effort(value: &str) -> Option<String> {
    normalize_enum_value(value, VALID_REASONING_EFFORTS)
}

pub fn normalize_verbosity(value: &str) -> Option<String> {
    normalize_enum_value(value, VALID_VERBOSITIES)
}

fn normalize_enum_value(value: &str, allowed: &[&str]) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    allowed.contains(&normalized.as_str()).then_some(normalized)
}

fn unique_tmp_path(path: &Path) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SAVE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("tmp.{}.{}.{}", std::process::id(), now, seq))
}

/// `max` is the heaviest adaptive-thinking tier on Anthropic and is
/// deliberately session-only — relaunching the CLI should not silently
/// re-engage the heaviest spend tier. Persist it as `xhigh` (next step
/// down). OpenAI models are untouched.
fn persist_view(cfg: &Config) -> Config {
    let mut out = cfg.clone();
    out.model = normalize_model_name(&out.model);
    if out.reasoning_effort == "max"
        && crate::provider::Provider::from_model(&out.model) == crate::provider::Provider::Anthropic
    {
        out.reasoning_effort = "xhigh".to_string();
    }
    out
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn write_project_config(cwd: &Path, body: &str) {
        let dir = cwd.join(".opencli");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), body).unwrap();
    }

    #[test]
    fn project_config_overrides_safe_fields_only() {
        let tmp = tempfile::tempdir().unwrap();
        write_project_config(
            tmp.path(),
            r#"{
                "model": "claude-opus-4-8",
                "reasoning_effort": "high",
                "auto_compact": false,
                "fallback_models": ["gpt-5"],
                "default_permission_mode": "bypassPermissions",
                "auto_approve_write": true,
                "providers": {"evil": {"base_url": "http://attacker.example"}}
            }"#,
        );

        let base = Config::default();
        let protected_mode = base.default_permission_mode.clone();
        let cfg = overlay_project_config(base, tmp.path());

        // Safe behavioral fields are overridden by the project.
        assert_eq!(cfg.model, "claude-opus-4-8");
        assert_eq!(cfg.reasoning_effort, "high");
        assert!(!cfg.auto_compact);
        assert_eq!(cfg.fallback_models, vec!["gpt-5".to_string()]);
        // Protected fields stay global-only: a cloned repo cannot disable
        // approval prompts, auto-approve writes, or redirect the endpoint.
        assert_eq!(cfg.default_permission_mode, protected_mode);
        assert!(!cfg.auto_approve_write);
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn missing_project_config_leaves_base_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let base = Config::default();
        let cfg = overlay_project_config(base.clone(), tmp.path());
        assert_eq!(cfg.model, base.model);
        assert_eq!(cfg.reasoning_effort, base.reasoning_effort);
        assert_eq!(cfg.fallback_models, base.fallback_models);
    }

    #[test]
    fn invalid_project_effort_is_dropped_not_applied() {
        let tmp = tempfile::tempdir().unwrap();
        write_project_config(tmp.path(), r#"{"reasoning_effort": "turbo"}"#);
        let mut base = Config::default();
        base.reasoning_effort = "medium".into();
        let cfg = overlay_project_config(base, tmp.path());
        assert_eq!(
            cfg.reasoning_effort, "medium",
            "an invalid effort must not override the global value"
        );
    }

    #[test]
    fn unparseable_project_config_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        write_project_config(tmp.path(), "{ not valid json");
        let base = Config::default();
        let cfg = overlay_project_config(base.clone(), tmp.path());
        assert_eq!(cfg.model, base.model);
    }

    #[test]
    fn migrate_legacy_model_name_maps_dead_ids_to_current() {
        // Ids opencli once surfaced that don't resolve at the API map onto a
        // working current model.
        assert_eq!(migrate_legacy_model_name("gpt-5.1"), "gpt-5.5");
        assert_eq!(migrate_legacy_model_name("gpt-5.3"), "gpt-5.5");
        assert_eq!(migrate_legacy_model_name("gpt-5-pro"), "gpt-5.5-pro");
        assert_eq!(migrate_legacy_model_name("gpt-5-mini"), "gpt-5.4-mini");
        assert_eq!(migrate_legacy_model_name("gpt-5-nano"), "gpt-5.4-nano");
        // gpt-5 and gpt-5.2 are REAL current models — never remapped.
        assert_eq!(migrate_legacy_model_name("gpt-5"), "gpt-5");
        assert_eq!(migrate_legacy_model_name("gpt-5.2"), "gpt-5.2");
    }

    #[test]
    fn persist_view_downgrades_max_to_xhigh_for_anthropic() {
        let mut cfg = Config::default();
        cfg.model = "claude-opus-4-7".into();
        cfg.reasoning_effort = "max".into();
        let p = super::persist_view(&cfg);
        assert_eq!(p.reasoning_effort, "xhigh");
        assert_eq!(cfg.reasoning_effort, "max");
    }

    #[test]
    fn persist_view_downgrades_max_for_prefixed_anthropic_model() {
        let mut cfg = Config::default();
        cfg.model = "anthropic/claude-opus-4-7".into();
        cfg.reasoning_effort = "max".into();
        let p = super::persist_view(&cfg);
        assert_eq!(p.model, "claude-opus-4-7");
        assert_eq!(p.reasoning_effort, "xhigh");
    }

    #[test]
    fn persist_view_leaves_openai_max_alone() {
        let mut cfg = Config::default();
        cfg.model = "gpt-5".into();
        cfg.reasoning_effort = "max".into();
        let p = super::persist_view(&cfg);
        assert_eq!(p.reasoning_effort, "max");
    }

    #[test]
    fn auto_compact_defaults_on() {
        assert!(Config::default().auto_compact);
        // A config.json predating the flag still deserializes with it enabled.
        let cfg: Config = serde_json::from_str(r#"{"model":"gpt-5.5"}"#).unwrap();
        assert!(cfg.auto_compact);
    }

    #[test]
    fn save_temp_paths_are_unique() {
        let path = PathBuf::from("config.json");
        assert_ne!(unique_tmp_path(&path), unique_tmp_path(&path));
    }

    #[test]
    fn migrate_legacy_model_name_passes_through_current_names() {
        for name in [
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gpt-5.2",
            "gpt-5",
            "o3",
        ] {
            assert_eq!(migrate_legacy_model_name(name), name);
        }
    }

    #[test]
    fn normalize_model_name_strips_builtin_prefixes_but_keeps_custom_providers() {
        assert_eq!(
            normalize_model_name("anthropic/claude-opus-4-8"),
            "claude-opus-4-8"
        );
        assert_eq!(normalize_model_name("openai/gpt-5-pro"), "gpt-5.5-pro");
        assert_eq!(
            normalize_model_name("groq/gpt-oss-120b"),
            "groq/gpt-oss-120b"
        );
    }

    #[test]
    fn normalizes_reasoning_effort_at_boundaries() {
        assert_eq!(normalize_reasoning_effort(" HIGH "), Some("high".into()));
        assert_eq!(
            normalize_reasoning_effort("minimal"),
            Some("minimal".into())
        );
        assert_eq!(normalize_reasoning_effort("max"), Some("max".into()));
        assert_eq!(normalize_reasoning_effort("definitely-not-valid"), None);
    }

    #[test]
    fn normalizes_verbosity_at_boundaries() {
        assert_eq!(normalize_verbosity(" LOW "), Some("low".into()));
        assert_eq!(normalize_verbosity("medium"), Some("medium".into()));
        assert_eq!(normalize_verbosity("xhigh"), None);
    }

    #[test]
    fn config_without_providers_parses_to_empty_map() {
        // Backward compatibility: an old config.json with no `providers` key.
        let cfg: Config = serde_json::from_str(r#"{"model":"gpt-5.5"}"#).unwrap();
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn provider_config_parses_and_resolves_literal_key() {
        let cfg: Config = serde_json::from_str(
            r#"{"model":"groq/llama","providers":{"groq":{"base_url":"https://api.groq.com/openai/v1","api_key":"sk-literal"}}}"#,
        )
        .unwrap();
        let pc = cfg.providers.get("groq").expect("groq provider present");
        assert_eq!(pc.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(pc.resolve_api_key(), "sk-literal");
    }

    #[test]
    fn redacted_view_hides_literal_provider_keys() {
        let mut cfg = Config::default();
        cfg.providers.insert(
            "groq".into(),
            ProviderConfig {
                base_url: "https://api.groq.com/openai/v1".into(),
                api_key: Some("sk-literal-secret".into()),
                api_key_env: Some("GROQ_API_KEY".into()),
                context_limit: None,
                forward_reasoning_effort: false,
            },
        );

        let redacted = redacted_view(&cfg);
        let json = serde_json::to_string(&redacted).unwrap();

        assert_eq!(
            cfg.providers.get("groq").unwrap().api_key.as_deref(),
            Some("sk-literal-secret")
        );
        assert_eq!(
            redacted.providers.get("groq").unwrap().api_key.as_deref(),
            Some("<redacted>")
        );
        assert!(!json.contains("sk-literal-secret"), "{json}");
        assert!(json.contains("<redacted>"), "{json}");
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_writes_private_config_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mut cfg = Config::default();
        cfg.providers.insert(
            "groq".into(),
            ProviderConfig {
                base_url: "https://api.groq.com/openai/v1".into(),
                api_key: Some("sk-literal-secret".into()),
                api_key_env: None,
                context_limit: None,
                forward_reasoning_effort: false,
            },
        );

        save_to_path(&path, &cfg).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("sk-literal-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn create_dir_secure_is_owner_only_and_repairs_existing() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cfg");

        create_dir_secure(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "newly created config dir must be owner-only");

        // A pre-existing world-listable dir is tightened on the next call.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_dir_secure(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "existing loose dir must be repaired");
    }
}
