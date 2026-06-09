//! Local, provider-agnostic cost estimation for `/cost`.
//!
//! Rates are hard-coded per model family (USD per million tokens) and split by
//! billing class — fresh input, output, cache read, cache write — because
//! prompt caching makes those rates differ by 10x or more. An estimate at
//! published API rates beats none; update the tables when official pricing
//! changes.
//!
//! Note: subscription auth (Claude Pro/Max, ChatGPT) is billed as a flat plan,
//! so the USD figure is "what these tokens would cost at API rates", not a bill.

use crate::provider::Provider;
use crate::session::ModelUsage;

/// Per-model rates in USD per million tokens, by billing class.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Pricing {
    const fn new(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    /// USD cost of one model's accumulated usage at these rates.
    pub fn cost_of(&self, u: &ModelUsage) -> f64 {
        (u.input_tokens as f64 * self.input
            + u.output_tokens as f64 * self.output
            + u.cache_read_tokens as f64 * self.cache_read
            + u.cache_write_tokens as f64 * self.cache_write)
            / 1_000_000.0
    }
}

/// Best-effort published rates for a model id. Anthropic families match on the
/// tier substring (ids look like `claude-opus-4-8`, possibly date-suffixed);
/// OpenAI ids match exactly. An unknown id falls back to a mid GPT-5 rate.
pub fn pricing_for(model: &str) -> Pricing {
    // Anthropic: cache read = 0.1x input, cache write (5m TTL) = 1.25x input.
    // Rates per the published model docs (June 2026).
    if model.contains("fable") || model.contains("mythos") {
        return Pricing::new(10.0, 50.0, 1.0, 12.5);
    }
    if model.contains("opus") {
        // Opus 4.5 and later published $5/$25; Opus 4.1 and older (or an id
        // whose version can't be parsed, like dated `claude-opus-4-20250514`)
        // keep the original $15/$75.
        return match crate::catalog::claude_version(&model.to_ascii_lowercase()) {
            Some((major, minor)) if major > 4 || (major == 4 && minor >= 5) => {
                Pricing::new(5.0, 25.0, 0.5, 6.25)
            }
            _ => Pricing::new(15.0, 75.0, 1.5, 18.75),
        };
    }
    if model.contains("sonnet") {
        return Pricing::new(3.0, 15.0, 0.3, 3.75);
    }
    if model.contains("haiku") {
        return Pricing::new(1.0, 5.0, 0.1, 1.25);
    }
    // OpenAI Responses families. Cache read is a 10x discount on fresh input;
    // OpenAI does not surcharge cache creation, so cache write = input rate.
    let (input, output) = match model {
        "gpt-5.5" | "gpt-5.5-chat-latest" | "gpt-5.5-codex" => (5.00, 30.0),
        "gpt-5.4" | "gpt-5.4-chat-latest" | "gpt-5.4-codex" => (2.50, 15.0),
        "gpt-5.3" | "gpt-5.3-chat-latest" | "gpt-5.3-codex" => (1.75, 14.0),
        "gpt-5" => (1.25, 10.0),
        "gpt-5.5-pro" | "gpt-5.4-pro" => (30.00, 180.0),
        "gpt-5-pro" => (15.00, 120.0),
        "gpt-5.4-mini" => (0.75, 4.50),
        "gpt-5-mini" => (0.25, 2.00),
        "gpt-5.4-nano" => (0.20, 1.25),
        "gpt-5-nano" => (0.05, 0.40),
        _ => (1.25, 10.0),
    };
    Pricing::new(input, output, input * 0.1, input)
}

/// Render the `/cost` report: a per-model breakdown plus a session total.
/// `current_model` is the active model; `turns` is the user-visible turn count.
pub fn render_cost_report(usage: &[ModelUsage], current_model: &str, turns: u64) -> String {
    if usage.is_empty() {
        return format!(
            "Session usage — model: {current_model}\n  Turns: {turns}\n  \
             No billable tokens yet — send a message, then run /cost."
        );
    }
    let mut out = format!("Session usage — active model: {current_model}\n  Turns: {turns}\n");
    let mut total_tokens: u64 = 0;
    let mut total_cost = 0.0;
    // Per-provider subtotals — the cross-provider receipt. Only surfaced when the
    // session spanned more than one provider (otherwise it just repeats the total).
    let mut openai_cost = 0.0;
    let mut anthropic_cost = 0.0;
    let mut openai_seen = false;
    let mut anthropic_seen = false;
    for u in usage {
        let p = pricing_for(&u.model);
        let cost = p.cost_of(u);
        total_cost += cost;
        match Provider::from_model(&u.model) {
            Provider::OpenAi => {
                openai_cost += cost;
                openai_seen = true;
            }
            Provider::Anthropic => {
                anthropic_cost += cost;
                anthropic_seen = true;
            }
        }
        total_tokens = total_tokens
            .saturating_add(u.input_tokens)
            .saturating_add(u.output_tokens)
            .saturating_add(u.cache_read_tokens)
            .saturating_add(u.cache_write_tokens);
        out.push_str(&format!("\n  {} — ${:.4}\n", u.model, cost));
        out.push_str(&format!(
            "    input (fresh): {:>12}  ·  ${:.4}\n",
            u.input_tokens,
            u.input_tokens as f64 * p.input / 1_000_000.0
        ));
        if u.cache_read_tokens > 0 || u.cache_write_tokens > 0 {
            out.push_str(&format!(
                "    cache read:    {:>12}  ·  ${:.4}\n",
                u.cache_read_tokens,
                u.cache_read_tokens as f64 * p.cache_read / 1_000_000.0
            ));
            out.push_str(&format!(
                "    cache write:   {:>12}  ·  ${:.4}\n",
                u.cache_write_tokens,
                u.cache_write_tokens as f64 * p.cache_write / 1_000_000.0
            ));
        }
        out.push_str(&format!(
            "    output:        {:>12}  ·  ${:.4}\n",
            u.output_tokens,
            u.output_tokens as f64 * p.output / 1_000_000.0
        ));
    }
    if openai_seen && anthropic_seen {
        out.push_str(&format!(
            "\n  By provider:\n    OpenAI:    ${openai_cost:.4}\n    Anthropic: ${anthropic_cost:.4}\n"
        ));
    }
    out.push_str(&format!(
        "\n  Total tokens: {total_tokens}\n  Estimated cost: ${total_cost:.4}  (API-rate estimate)"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_families_priced_by_tier_and_date_suffix() {
        // Fable (and Mythos, same published rate) is the $10/$50 top tier.
        assert_eq!(
            pricing_for("claude-fable-5"),
            Pricing::new(10.0, 50.0, 1.0, 12.5)
        );
        assert_eq!(
            pricing_for("claude-mythos-5"),
            Pricing::new(10.0, 50.0, 1.0, 12.5)
        );
        // Opus 4.5+ published $5/$25.
        assert_eq!(
            pricing_for("claude-opus-4-8"),
            Pricing::new(5.0, 25.0, 0.5, 6.25)
        );
        // A dated snapshot id still resolves to the tier's rates.
        assert_eq!(
            pricing_for("claude-opus-4-8-20260101"),
            Pricing::new(5.0, 25.0, 0.5, 6.25)
        );
        assert_eq!(
            pricing_for("claude-opus-4-5-20251101"),
            Pricing::new(5.0, 25.0, 0.5, 6.25)
        );
        // Opus 4.1 and older keep the original $15/$75 — including the dated
        // bare-major Opus 4.0 id, whose version can't be parsed.
        assert_eq!(
            pricing_for("claude-opus-4-1-20250805"),
            Pricing::new(15.0, 75.0, 1.5, 18.75)
        );
        assert_eq!(
            pricing_for("claude-opus-4-20250514"),
            Pricing::new(15.0, 75.0, 1.5, 18.75)
        );
        assert_eq!(
            pricing_for("claude-sonnet-4-6"),
            Pricing::new(3.0, 15.0, 0.3, 3.75)
        );
        assert_eq!(
            pricing_for("claude-haiku-4-5"),
            Pricing::new(1.0, 5.0, 0.1, 1.25)
        );
    }

    #[test]
    fn openai_models_match_api_docs() {
        assert_eq!(pricing_for("gpt-5.5").input, 5.00);
        assert_eq!(pricing_for("gpt-5.5").output, 30.0);
        assert_eq!(pricing_for("gpt-5.4").input, 2.50);
        assert_eq!(pricing_for("gpt-5-pro").input, 15.00);
        assert_eq!(pricing_for("gpt-5.5-pro").output, 180.0);
        assert_eq!(pricing_for("gpt-5-nano").output, 0.40);
        // Codex / chat-latest variants are priced at their base family's rate
        // (the catalog recognizes e.g. `gpt-5.5-codex` as a real id), not the
        // unknown-model fallback — mirroring the existing gpt-5.3 entry.
        assert_eq!(pricing_for("gpt-5.5-codex").input, 5.00);
        assert_eq!(pricing_for("gpt-5.5-codex").output, 30.0);
        assert_eq!(pricing_for("gpt-5.4-codex").input, 2.50);
        // Cache read is a 10x discount on fresh input for OpenAI too.
        assert!((pricing_for("gpt-5").cache_read - 0.125).abs() < 1e-9);
    }

    #[test]
    fn cost_splits_by_billing_class() {
        // On Sonnet: 1M fresh input @ $3 + 1M output @ $15 + 1M cache-read @ $0.30.
        let u = ModelUsage {
            model: "claude-sonnet-4-6".into(),
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 0,
        };
        let p = pricing_for(&u.model);
        assert!((p.cost_of(&u) - (3.0 + 15.0 + 0.30)).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_uses_fallback_not_panic() {
        assert_eq!(pricing_for("some-future-model").input, 1.25);
    }

    #[test]
    fn empty_usage_renders_hint() {
        let report = render_cost_report(&[], "claude-opus-4-8", 0);
        assert!(report.contains("No billable tokens yet"));
    }

    #[test]
    fn report_breaks_down_per_model() {
        let usage = vec![
            ModelUsage {
                model: "claude-opus-4-8".into(),
                input_tokens: 1000,
                output_tokens: 500,
                cache_read_tokens: 2000,
                cache_write_tokens: 0,
            },
            ModelUsage {
                model: "gpt-5".into(),
                input_tokens: 1000,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
        ];
        let report = render_cost_report(&usage, "claude-opus-4-8", 3);
        assert!(report.contains("claude-opus-4-8"));
        assert!(report.contains("gpt-5"));
        assert!(report.contains("cache read"));
        assert!(report.contains("Estimated cost"));
    }

    #[test]
    fn report_adds_provider_subtotals_only_when_cross_provider() {
        let one = |model: &str| ModelUsage {
            model: model.into(),
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        // Cross-provider session → the normalized OpenAI/Anthropic receipt shows.
        let cross = vec![one("claude-opus-4-8"), one("gpt-5")];
        let report = render_cost_report(&cross, "gpt-5", 1);
        assert!(report.contains("By provider:"));
        assert!(report.contains("OpenAI:"));
        assert!(report.contains("Anthropic:"));
        // Single-provider session → no provider block (it would just repeat the total).
        let report = render_cost_report(&[one("gpt-5")], "gpt-5", 1);
        assert!(!report.contains("By provider:"));
    }
}
