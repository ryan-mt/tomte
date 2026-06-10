use super::*;

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
    /// True input context window (in tokens) of this endpoint's model. tomte
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

/// Env var that overrides the resolved context-window size (tokens). Lets a user
/// pin the window for a gateway/proxy whose real limit tomte can't infer, or
/// shrink it to force earlier compaction. Accepts a bare integer or a `k`/`m`
/// suffix (`200000`, `200k`, `1m`). Mirrors Claude Code's
/// `CLAUDE_CODE_MAX_CONTEXT_TOKENS`.
pub const CONTEXT_OVERRIDE_ENV: &str = "TOMTE_MAX_CONTEXT_TOKENS";

/// Floor for a [`CONTEXT_OVERRIDE_ENV`] value. Below this a typo (`2000`) would
/// make every turn read as ≥100% full and thrash the compaction path, so a
/// too-small override is clamped up rather than honored.
pub const MIN_CONTEXT_OVERRIDE: u64 = 8_000;

/// Ceiling for a [`CONTEXT_OVERRIDE_ENV`] value, so an absurd number can't skew
/// the gauge math or starve the warn/compact thresholds.
pub const MAX_CONTEXT_OVERRIDE: u64 = 20_000_000;

/// Parse a human token-count override: a bare integer, or a `k`/`m` suffix
/// (`"200k"` → 200_000, `"1m"` → 1_000_000; case-insensitive, underscores and
/// surrounding whitespace tolerated). Returns `None` for anything unparseable or
/// non-positive so the caller falls back to the catalog/provider value. A valid
/// value is clamped to `[MIN_CONTEXT_OVERRIDE, MAX_CONTEXT_OVERRIDE]`.
pub fn parse_context_override(raw: &str) -> Option<u64> {
    let s = raw.trim().replace('_', "").to_ascii_lowercase();
    let (num, mult) = if let Some(n) = s.strip_suffix('m') {
        (n, 1_000_000f64)
    } else if let Some(n) = s.strip_suffix('k') {
        (n, 1_000f64)
    } else {
        (s.as_str(), 1f64)
    };
    let value: f64 = num.trim().parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let tokens = (value * mult) as u64;
    Some(tokens.clamp(MIN_CONTEXT_OVERRIDE, MAX_CONTEXT_OVERRIDE))
}

/// Apply a context-window override on top of `base`. Pure (`raw` is the literal
/// env value) so the precedence is unit-testable without mutating process env.
/// An unset or unparseable override leaves `base` untouched.
pub(crate) fn apply_context_override(base: u64, raw: Option<&str>) -> u64 {
    raw.and_then(parse_context_override).unwrap_or(base)
}

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

/// Built-in base URLs + key-env conventions for well-known OpenAI-compatible
/// providers, so a `<id>/<model>` spec works out of the box (e.g.
/// `groq/llama-3.3-70b`) without hand-writing a `providers` entry. Each tuple is
/// `(id, base_url, api_key_env, forward_reasoning_effort)`; local servers
/// (Ollama / LM Studio) take no key.
pub(super) const BUILTIN_PROVIDERS: &[(&str, &str, Option<&str>, bool)] = &[
    (
        "groq",
        "https://api.groq.com/openai/v1",
        Some("GROQ_API_KEY"),
        true,
    ),
    (
        "openrouter",
        "https://openrouter.ai/api/v1",
        Some("OPENROUTER_API_KEY"),
        false,
    ),
    (
        "deepseek",
        "https://api.deepseek.com/v1",
        Some("DEEPSEEK_API_KEY"),
        true,
    ),
    ("xai", "https://api.x.ai/v1", Some("XAI_API_KEY"), true),
    (
        "together",
        "https://api.together.xyz/v1",
        Some("TOGETHER_API_KEY"),
        true,
    ),
    (
        "fireworks",
        "https://api.fireworks.ai/inference/v1",
        Some("FIREWORKS_API_KEY"),
        false,
    ),
    (
        "cerebras",
        "https://api.cerebras.ai/v1",
        Some("CEREBRAS_API_KEY"),
        false,
    ),
    (
        "mistral",
        "https://api.mistral.ai/v1",
        Some("MISTRAL_API_KEY"),
        false,
    ),
    ("ollama", "http://localhost:11434/v1", None, false),
    ("lmstudio", "http://localhost:1234/v1", None, false),
];

/// Synthesize a [`ProviderConfig`] for a well-known provider id (see
/// [`BUILTIN_PROVIDERS`]), or `None` if the id isn't recognized. The API key is
/// resolved later from the conventional `<ID>_API_KEY` env var. This is only a
/// fallback: a user's own `config.providers["<id>"]` always takes precedence.
pub fn builtin_provider(id: &str) -> Option<ProviderConfig> {
    let id = id.trim();
    BUILTIN_PROVIDERS
        .iter()
        .find(|entry| entry.0.eq_ignore_ascii_case(id))
        .map(|entry| ProviderConfig {
            base_url: entry.1.to_string(),
            api_key: None,
            api_key_env: entry.2.map(str::to_string),
            context_limit: None,
            forward_reasoning_effort: entry.3,
        })
}
