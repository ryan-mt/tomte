use super::*;

/// `<cwd>/.tomte/config.json` — the optional project-local config overlay.
pub fn project_config_file(cwd: &Path) -> PathBuf {
    cwd.join(".tomte").join("config.json")
}

/// Top-level keys a project config may NOT override. A project `.tomte/`
/// ships in cloned repos, so letting one set these would let an untrusted repo
/// disable approval prompts, auto-approve writes, or redirect the model to an
/// arbitrary endpoint (leaking prompts / API keys). They stay global-only;
/// present keys are ignored with a warning.
pub(super) const PROJECT_PROTECTED_KEYS: &[&str] = &[
    "default_permission_mode",
    "auto_approve_read",
    "auto_approve_write",
    "providers",
];

/// Safe, behavioral fields a project `.tomte/config.json` may override on top
/// of the global config. All optional; unknown keys (including the protected
/// ones) are dropped by serde and never applied here.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ProjectOverlay {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    verbosity: Option<String>,
    #[serde(default)]
    auto_compact: Option<bool>,
    #[serde(default)]
    auto_capture: Option<bool>,
    #[serde(default)]
    conscience: Option<String>,
    #[serde(default)]
    fallback_models: Option<Vec<String>>,
}

/// Load the global config, then overlay a project-local `.tomte/config.json`
/// from `cwd` if present. Only safe behavioral fields are overlaid (see
/// [`PROJECT_PROTECTED_KEYS`]); a missing, oversized, or unparseable project
/// file is ignored and the global config stands.
pub fn load_for_cwd(cwd: &Path) -> Config {
    overlay_project_config(load(), cwd)
}

pub(super) fn overlay_project_config(mut cfg: Config, cwd: &Path) -> Config {
    let path = project_config_file(cwd);
    let Some(text) = read_project_config(&path) else {
        return cfg;
    };
    let value: serde_json::Value = match serde_json::from_str(strip_bom(&text)) {
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

/// Read an attacker-influenceable text file bounded to `max_bytes`, restricted
/// to a non-symlink regular file. A planted symlink must not redirect a project
/// instruction/config path to local secrets, and a multi-GB planted file must
/// not exhaust memory in `read_to_string`. Returns `NotFound` when absent so
/// callers can distinguish "missing" from "rejected".
pub(crate) fn read_text_file_capped(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is a symlink", path.display()),
        ));
    }
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    if meta.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} exceeds the {max_bytes} byte cap", path.display()),
        ));
    }
    std::fs::read_to_string(path)
}

/// Read a project config file, bounded to a sane size — it is attacker-influenced
/// (it ships in cloned repos) and a real config is tiny. Returns `None` when the
/// file is absent, not a regular file, too large, or unreadable.
pub(super) fn read_project_config(path: &Path) -> Option<String> {
    const MAX_PROJECT_CONFIG_BYTES: u64 = 64 * 1024;
    match read_text_file_capped(path, MAX_PROJECT_CONFIG_BYTES) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(config = %path.display(), error = %e, "project config.json unreadable or too large; ignoring it");
            None
        }
    }
}

/// Apply a validated project overlay onto `cfg`. Model names are normalized and
/// effort/verbosity are validated; an invalid enum value is dropped rather than
/// overriding the global with garbage.
/// True when a project-supplied model spec would route the turn — and with it the
/// prompt and all file context — to a non-native third-party endpoint: its
/// `provider/` prefix resolves to a built-in preset or a user-configured provider
/// rather than the native OpenAI/Anthropic path (see [`crate::client::LlmClient::for_config`]).
/// A cloned repo's `.tomte/config.json` must not be able to do that — it would
/// silently exfiltrate prompts using the user's own `<PROVIDER>_API_KEY`. This
/// extends the [`PROJECT_PROTECTED_KEYS`] `providers` block to the `model` /
/// `fallback_models` that can reach the same endpoints. A bare id, or a native
/// `openai/` / `anthropic/` spec, is safe and allowed.
pub(super) fn project_model_redirects_offsite(cfg: &Config, spec: &str) -> bool {
    let Some((prefix, _rest)) = spec.trim().split_once('/') else {
        return false; // a bare id never routes off-site
    };
    // A native `openai/`/`anthropic/` prefix is safe; any other recognized
    // provider prefix routes off-site. `for_config` keys routing on the prefix
    // ALONE — an empty model after the slash (`openrouter/`) still reaches that
    // endpoint with the prompt attached — so an empty `rest` must NOT be treated
    // as safe.
    if crate::provider::Provider::from_name(prefix).is_some() {
        return false;
    }
    builtin_provider(prefix).is_some() || cfg.providers.contains_key(prefix)
}

pub(super) fn apply_project_overlay(cfg: &mut Config, overlay: ProjectOverlay) {
    if let Some(model) = overlay.model {
        if project_model_redirects_offsite(cfg, &model) {
            tracing::warn!(
                "ignoring project config `model` = `{model}` — it routes to a non-native provider endpoint (global-only for safety, like `providers`)"
            );
        } else {
            cfg.model = normalize_model_name(&model);
        }
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
    if let Some(auto_capture) = overlay.auto_capture {
        cfg.auto_capture = auto_capture;
    }
    if let Some(conscience) = overlay.conscience {
        if let Some(v) = normalize_conscience(&conscience) {
            cfg.conscience = v;
        }
    }
    if let Some(fallbacks) = overlay.fallback_models {
        // Failover routes the same way as `model`, so an off-site fallback is the
        // same prompt/key-leak vector — drop those, keep the safe entries.
        let filtered: Vec<String> = fallbacks
            .into_iter()
            .filter(|m| {
                let offsite = project_model_redirects_offsite(cfg, m);
                if offsite {
                    tracing::warn!(
                        "ignoring project fallback model `{m}` — it routes to a non-native provider endpoint"
                    );
                }
                !offsite
            })
            .collect();
        cfg.fallback_models = filtered;
    }
}
