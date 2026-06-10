use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

mod sandbox;
pub use sandbox::SandboxConfig;

mod overlay;
mod providers;
mod store;

pub use overlay::*;
pub use providers::*;
pub use store::*;

// `ultracode` is a top-tier effort: it pairs `xhigh` thinking with standing
// permission to launch multi-agent workflows. It is not a distinct API effort
// level — tomte accepts it as a selectable effort and maps it onto `xhigh` on
// the wire (see translate.rs / openai client).
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
    /// After a turn that changed files, ask the model (provider-agnostically)
    /// whether it made a non-obvious decision worth keeping, and if so append it
    /// to the decision trail — so the *why* is preserved without the model having
    /// to remember to call `record_decision`. On by default; the capture
    /// self-check is skipped on turns with no edits and in unattended headless
    /// runs. Pillar 2 — auto-capture.
    #[serde(default = "default_auto_capture")]
    pub auto_capture: bool,
    /// Show the model's live reasoning/thinking text in the TUI while it thinks,
    /// then collapse it to a compact "Thought for Xs" line once the answer
    /// starts. On by default; `/thinking off` (or `show_thinking: false`) hides
    /// the text and keeps only the spinner's "thinking" cue. Provider-agnostic —
    /// it renders whatever reasoning the active model streams (Anthropic
    /// thinking, OpenAI reasoning), so it carries across a model switch.
    #[serde(default = "default_show_thinking")]
    pub show_thinking: bool,
    /// Pillar 5 — the conscience around an edit to a file that has recorded
    /// decisions. `"off"` disables it; `"surface"` (default) shows the file's
    /// decisions as "house rules" in the pre-flight card (Tier 1, no model cost);
    /// `"check"` additionally asks the editing model whether the edit contradicts
    /// a decision (Tier 2) and, on a conflict, escalates to an
    /// abort/supersede/edit-anyway card.
    #[serde(default = "default_conscience")]
    pub conscience: String,
    #[serde(default)]
    pub auto_approve_read: bool,
    #[serde(default)]
    pub auto_approve_write: bool,
    /// Permission mode the TUI starts in, persisted across launches. One of
    /// `default`, `acceptEdits`, `plan`, `bypassPermissions`. Shift+Tab in the
    /// TUI cycles the mode and writes the new value here, so relaunching keeps
    /// the last-chosen mode.
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
    /// `<provider-id>/<model>` for a configured endpoint). The list is
    /// provider-agnostic: a fallback may target a different provider or a local
    /// endpoint. Wired into the turn loop via [`crate::fallback`] and
    /// `agent::turn`'s `try_fail_over` — reactive failover that only fires before
    /// any answer text has streamed. Empty by default; with
    /// [`auto_fallback`](Config::auto_fallback) on, an empty list uses the
    /// built-in same-provider ladder instead.
    #[serde(default)]
    pub fallback_models: Vec<String>,
    /// Whether an empty [`fallback_models`](Config::fallback_models) falls back
    /// to the built-in same-provider ladder (see
    /// [`crate::fallback::default_fallbacks`]) when the active model is
    /// overloaded — same-or-lower capability tier only, so failover never
    /// silently moves a session onto a more expensive model. `false` restores
    /// the old behavior: no configured fallbacks → the overload error surfaces.
    #[serde(default = "default_auto_fallback")]
    pub auto_fallback: bool,
    /// OS-level sandbox for `run_shell` child processes — see [`SandboxConfig`].
    #[serde(default)]
    pub sandbox: SandboxConfig,
    /// Customize the spinner companion words (the gerunds shown while a turn
    /// runs). Optional; unset uses tomte's built-in pool. See [`SpinnerVerbs`].
    #[serde(default)]
    pub spinner_verbs: Option<SpinnerVerbs>,
    /// How the TUI occupies the terminal: `"inline"` (default — SOUL Pillar 4,
    /// finished turns flow into the terminal's native scrollback, no mouse
    /// capture so selection/copy stay native) or `"alt"` (the full-screen
    /// alternate-buffer renderer with in-app scroll + drag-selection). The
    /// `TOMTE_INLINE` env var overrides this both ways (`1` forces inline,
    /// `0` forces alt-screen); an unrecognized value falls back to inline.
    #[serde(default = "default_render_mode")]
    pub render_mode: String,
}

/// User overrides for the spinner companion words: a `verbs` list that either
/// *appends* to the built-in pool (default) or *replaces* it when
/// `exclude_default` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpinnerVerbs {
    /// Extra (or replacement) words to show, e.g. `["Hacking", "Vibing"]`.
    #[serde(default)]
    pub verbs: Vec<String>,
    /// When true, show ONLY `verbs` (ignore the built-in pool). When false (the
    /// default), `verbs` are added to the built-in pool. An empty `verbs` with
    /// `exclude_default` set is ignored — the built-in pool is kept rather than
    /// leaving nothing to show.
    #[serde(default)]
    pub exclude_default: bool,
}

impl Config {
    /// Effective input context window (tokens) for the active model — the value
    /// the warn/auto-compact thresholds and the status bar must use. A model
    /// routed through a configured provider (`<id>/<model>` whose `<id>` is in
    /// [`providers`](Config::providers)) takes that provider's declared
    /// `context_limit`, or [`DEFAULT_PROVIDER_CONTEXT_LIMIT`] when unset, because
    /// tomte can't infer a custom endpoint's real window from the model name.
    /// Built-in OpenAI/Anthropic models use the catalog value. The
    /// [`CONTEXT_OVERRIDE_ENV`] env var wins over both when set to a valid value.
    pub fn effective_context_limit(&self) -> u64 {
        let base = self.resolved_context_limit();
        apply_context_override(base, std::env::var(CONTEXT_OVERRIDE_ENV).ok().as_deref())
    }

    /// The context window from config/catalog alone, before the env override.
    fn resolved_context_limit(&self) -> u64 {
        if let Some((prefix, _)) = self.model.split_once('/') {
            if let Some(pc) = self
                .providers
                .get(prefix)
                .cloned()
                .or_else(|| builtin_provider(prefix))
            {
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

fn default_auto_fallback() -> bool {
    true
}

fn default_auto_capture() -> bool {
    true
}

fn default_show_thinking() -> bool {
    true
}

fn default_permission_mode() -> String {
    "default".to_string()
}

fn default_render_mode() -> String {
    "inline".to_string()
}

/// Map model ids that tomte once surfaced but that don't resolve at the
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

pub const VALID_CONSCIENCE: &[&str] = &["off", "surface", "check"];

fn default_conscience() -> String {
    "surface".to_string()
}

/// Normalize a conscience mode, returning `None` for an unrecognized value so a
/// bad project-config string falls back to the existing setting instead of
/// silently disabling the conscience.
pub fn normalize_conscience(value: &str) -> Option<String> {
    let v = value.trim().to_ascii_lowercase();
    VALID_CONSCIENCE.contains(&v.as_str()).then_some(v)
}

impl Config {
    /// Tier 1 (house rules in the pre-flight) is on unless the conscience is off.
    pub fn conscience_surfaces(&self) -> bool {
        self.conscience != "off"
    }
    /// Tier 2 (the editing-model self-check) fires only in `"check"` mode.
    pub fn conscience_checks(&self) -> bool {
        self.conscience == "check"
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            reasoning_effort: default_reasoning_effort(),
            verbosity: default_verbosity(),
            auto_compact: true,
            auto_capture: true,
            show_thinking: true,
            conscience: default_conscience(),
            auto_approve_read: true,
            auto_approve_write: false,
            default_permission_mode: default_permission_mode(),
            providers: HashMap::new(),
            fallback_models: Vec::new(),
            auto_fallback: true,
            sandbox: SandboxConfig::default(),
            spinner_verbs: None,
            render_mode: default_render_mode(),
        }
    }
}

/// Validate a sandbox mode supplied on the CLI (`--sandbox`). Returns the
/// canonical lowercase form, or `None` for an unrecognized value so the caller
/// can surface a hard error (env/file paths fall back instead).
pub fn normalize_sandbox_mode(value: &str) -> Option<String> {
    sandbox::parse_mode_override(Some(value))
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

pub(crate) fn unique_tmp_path(path: &Path) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SAVE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("tmp.{}.{}.{}", std::process::id(), now, seq))
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests;
