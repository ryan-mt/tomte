//! `tomte models` / `/models` — the model lineup, from real state.
//!
//! One card answers the questions otherwise scattered across the catalog, the
//! auth store, and config.json: which models tomte can drive, each one's
//! context window and thinking capabilities, which credentials are actually
//! present (presence/source only — never token contents), which model is
//! active, and the failover chain an overload would walk (configured list,
//! built-in ladder, or none). Collection is split from rendering and the
//! credential matrix is injected, so every rule is unit-testable without an
//! auth store; `--json` emits the same data machine-readably.

use serde::Serialize;

use crate::auth::{CredentialCoverage, CredentialPresence};
use crate::catalog;
use crate::config::Config;
use crate::provider::Provider;

/// The full lineup card.
#[derive(Debug, Clone, Serialize)]
pub struct ModelsReport {
    /// The configured model spec, as written in config.json.
    pub active_model: String,
    pub active_provider: String,
    pub reasoning_effort: String,
    pub providers: Vec<ProviderModels>,
    /// The chain an overload would walk, in order (empty when there is none).
    pub failover: Vec<String>,
    /// Where the chain comes from: `configured` (fallback_models),
    /// `auto` (the built-in ladder), `off` (auto_fallback: false), or
    /// `none` (auto is on but the active model has no ladder).
    pub failover_source: &'static str,
}

/// One provider's catalogue plus its credential presence.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderModels {
    pub provider: String,
    pub display_name: String,
    pub oauth: &'static str,
    pub api_key: &'static str,
    /// Any credential present (OAuth or key) — whether a model here is usable.
    pub signed_in: bool,
    pub models: Vec<ModelRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRow {
    pub id: String,
    pub context_limit: u64,
    /// `adaptive thinking` / `extended thinking` (Anthropic),
    /// `reasoning effort` (OpenAI).
    pub thinking: &'static str,
    pub xhigh: bool,
    pub active: bool,
    /// True for OpenAI ids the ChatGPT-subscription OAuth backend rejects —
    /// they need an API key.
    pub api_key_only: bool,
}

/// Collect the report from the live credential matrix.
pub fn collect_current(cfg: &Config) -> ModelsReport {
    collect(cfg, &crate::auth::credential_coverage())
}

/// Pure collection: config + an injected credential matrix.
pub fn collect(cfg: &Config, cov: &CredentialCoverage) -> ModelsReport {
    let (active_provider, active_id) = Provider::parse_model(&cfg.model);
    let active_lc = active_id.to_ascii_lowercase();

    let providers = [Provider::OpenAi, Provider::Anthropic]
        .into_iter()
        .map(|p| {
            let (oauth, api_key) = match p {
                Provider::OpenAi => (cov.openai_oauth, cov.openai_api_key),
                Provider::Anthropic => (cov.anthropic_oauth, cov.anthropic_api_key),
            };
            let models = catalog::available_models(p)
                .iter()
                .map(|id| ModelRow {
                    id: (*id).to_string(),
                    context_limit: catalog::context_limit(id),
                    thinking: thinking_label(p, id),
                    xhigh: catalog::supports_xhigh(id),
                    active: p == active_provider && id.eq_ignore_ascii_case(&active_lc),
                    api_key_only: p == Provider::OpenAi
                        && !catalog::openai_chatgpt_oauth_models().contains(id),
                })
                .collect();
            ProviderModels {
                provider: p.as_str().to_string(),
                display_name: p.display_name().to_string(),
                oauth: oauth.label(),
                api_key: api_key.label(),
                signed_in: oauth != CredentialPresence::Missing
                    || api_key != CredentialPresence::Missing,
                models,
            }
        })
        .collect();

    let (failover, failover_source) = if !cfg.fallback_models.is_empty() {
        (cfg.fallback_models.clone(), "configured")
    } else if !cfg.auto_fallback {
        (Vec::new(), "off")
    } else {
        let chain = crate::fallback::default_fallbacks(&cfg.model);
        let source = if chain.is_empty() { "none" } else { "auto" };
        (chain, source)
    };

    ModelsReport {
        active_model: cfg.model.clone(),
        active_provider: active_provider.as_str().to_string(),
        reasoning_effort: cfg.reasoning_effort.clone(),
        providers,
        failover,
        failover_source,
    }
}

fn thinking_label(provider: Provider, id: &str) -> &'static str {
    match provider {
        Provider::OpenAi => "reasoning effort",
        Provider::Anthropic => {
            if catalog::supports_adaptive_thinking(id) {
                "adaptive thinking"
            } else if catalog::supports_extended_thinking(id) {
                "extended thinking"
            } else {
                "—"
            }
        }
    }
}

/// `1_000_000` → `1M`, `1_050_000` → `1.05M`, `400_000` → `400K`.
fn fmt_ctx(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        if tokens.is_multiple_of(1_000_000) {
            format!("{}M", tokens / 1_000_000)
        } else {
            let m = format!("{:.2}", tokens as f64 / 1_000_000.0);
            format!("{}M", m.trim_end_matches('0').trim_end_matches('.'))
        }
    } else {
        format!("{}K", tokens / 1_000)
    }
}

/// Render the card. The active row is marked `▸`; a provider with no
/// credential at all points at `tomte login` instead of pretending its models
/// are one keypress away.
pub fn render(r: &ModelsReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "models — active: {} ({}) · reasoning effort: {}\n",
        r.active_model, r.active_provider, r.reasoning_effort
    ));

    for p in &r.providers {
        let cred = if p.signed_in {
            format!("OAuth: {} · API key: {}", p.oauth, p.api_key)
        } else {
            "not signed in — `tomte login`".to_string()
        };
        out.push_str(&format!("\n{} — {}\n", p.display_name, cred));
        let id_width = p.models.iter().map(|m| m.id.len()).max().unwrap_or(0);
        for m in &p.models {
            let mark = if m.active { "▸" } else { " " };
            let mut caps = format!("{:>5} ctx · {}", fmt_ctx(m.context_limit), m.thinking);
            if m.xhigh {
                caps.push_str(" · xhigh");
            }
            if m.api_key_only {
                caps.push_str(" · API key only");
            }
            out.push_str(&format!("  {mark} {:<id_width$}  {caps}\n", m.id));
        }
    }

    out.push('\n');
    match r.failover_source {
        "configured" => out.push_str(&format!(
            "failover: configured — {}\n",
            r.failover.join(" → ")
        )),
        "auto" => out.push_str(&format!(
            "failover: built-in ladder — {}  (set `fallback_models` in config.json to customize; `auto_fallback: false` disables)\n",
            r.failover.join(" → ")
        )),
        "off" => out.push_str("failover: off (`auto_fallback: false`)\n"),
        _ => out.push_str(
            "failover: none — the active model has no built-in ladder; set `fallback_models` in config.json\n",
        ),
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests;
