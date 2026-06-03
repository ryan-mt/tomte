//! Provider-agnostic quota / rate-limit snapshot, captured from the real
//! response of the request the agent already makes — no extra API calls.
//!
//! Each provider/auth combination exposes its remaining quota differently, so
//! the wire details are normalized at the parser boundary into one neutral
//! [`QuotaSnapshot`] (always epoch-seconds resets, always a 0..100 percent):
//!
//! | provider / auth            | source                                   | units on the wire                     |
//! |----------------------------|------------------------------------------|---------------------------------------|
//! | Anthropic API key          | `anthropic-ratelimit-{tokens,requests}-*`| RFC3339 reset, integer counts         |
//! | Anthropic OAuth (Pro/Max)  | `anthropic-ratelimit-unified-{5h,7d}-*`  | epoch reset, 0..1 utilization frac    |
//! | OpenAI API key             | `x-ratelimit-{limit,remaining,reset}-*`  | Go-duration reset, integer counts     |
//! | ChatGPT/Codex OAuth        | `x-codex-{primary,secondary}-*` + SSE    | epoch reset, 0..100 used-percent      |
//!
//! The documented families (Anthropic API key, OpenAI API key) are stable; the
//! `unified`/`x-codex` families are reverse-engineered and may change, so every
//! field is parsed independently and a parse failure is non-fatal (the field is
//! simply absent). When nothing parses, capture returns `None` and `/usage`
//! falls back to "no live quota yet".

use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::provider::Provider;

/// One rate-limit window of a provider's quota, normalized.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct QuotaWindow {
    /// Short window name: `5h` / `weekly` / `tokens` / `requests`.
    pub label: String,
    /// Percentage of the window consumed, 0..100. Present when the provider
    /// reports a utilization, or derivable from `remaining`/`limit`.
    pub used_percent: Option<f64>,
    /// Count-based remaining (Anthropic / OpenAI API-key token & request budgets).
    pub remaining: Option<i64>,
    /// Count-based ceiling for the window.
    pub limit: Option<i64>,
    /// Unix epoch seconds when the window resets.
    pub resets_at_epoch: Option<i64>,
}

/// A point-in-time view of the active provider's quota.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct QuotaSnapshot {
    pub provider: Option<Provider>,
    /// Subscription plan, when the provider reports one (Codex `plan_type`).
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    /// Unix epoch seconds when this snapshot was captured (for an "as of" age).
    pub captured_at_epoch: i64,
}

/// Parse a quota snapshot from a streaming response's HTTP headers. Branches on
/// the provider/auth combination; returns `None` when no window is parseable.
/// `now_epoch` anchors OpenAI's relative (Go-duration) resets and the capture
/// time.
pub fn parse_rate_limit_headers(
    headers: &HeaderMap,
    provider: Provider,
    is_oauth: bool,
    now_epoch: i64,
) -> Option<QuotaSnapshot> {
    let windows = match (provider, is_oauth) {
        (Provider::Anthropic, true) => anthropic_unified_windows(headers),
        (Provider::Anthropic, false) => anthropic_api_key_windows(headers),
        (Provider::OpenAi, true) => codex_header_windows(headers),
        (Provider::OpenAi, false) => openai_api_key_windows(headers, now_epoch),
    };
    if windows.is_empty() {
        return None;
    }
    Some(QuotaSnapshot {
        provider: Some(provider),
        plan: None,
        windows,
        captured_at_epoch: now_epoch,
    })
}

/// Parse the in-stream `codex.rate_limits` SSE event some Codex routes emit
/// instead of (or in addition to) the `x-codex-*` headers. `value` is the full
/// event JSON. `captured_at_epoch` is left 0 for the caller to stamp.
pub fn parse_codex_rate_limit_event(value: &Value) -> Option<QuotaSnapshot> {
    let rate_limits = value.get("rate_limits")?;
    let mut windows = Vec::new();
    if let Some(w) = codex_event_window(rate_limits.get("primary"), "primary") {
        windows.push(w);
    }
    if let Some(w) = codex_event_window(rate_limits.get("secondary"), "secondary") {
        windows.push(w);
    }
    if windows.is_empty() {
        return None;
    }
    Some(QuotaSnapshot {
        provider: Some(Provider::OpenAi),
        plan: value
            .get("plan_type")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        windows,
        captured_at_epoch: 0,
    })
}

impl QuotaSnapshot {
    /// Render a human-readable, multi-line summary. `now_epoch` is used for
    /// relative reset times and the "as of" age.
    pub fn render(&self, now_epoch: i64) -> String {
        let mut out = String::new();
        if let Some(plan) = &self.plan {
            out.push_str(&format!("  Plan: {plan}\n"));
        }
        for w in &self.windows {
            out.push_str(&format!("  {}\n", w.render(now_epoch)));
        }
        let age = now_epoch.saturating_sub(self.captured_at_epoch);
        if self.captured_at_epoch > 0 && age >= 60 {
            out.push_str(&format!("  (as of {} ago)", format_duration(age)));
        }
        out.trim_end().to_string()
    }
}

impl QuotaWindow {
    fn render(&self, now_epoch: i64) -> String {
        let label = match self.label.as_str() {
            "5h" => "5-hour".to_string(),
            "weekly" => "Weekly".to_string(),
            other => {
                let mut c = other.chars();
                match c.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                    None => other.to_string(),
                }
            }
        };
        let body = if let Some(pct) = self.used_percent {
            // Show both directions so the reading is unambiguous regardless of
            // whether the provider's native UI frames quota as consumed
            // (Claude: counts up) or remaining (ChatGPT/Codex: counts down).
            let left = (100.0 - pct).clamp(0.0, 100.0);
            format!("{label}: {pct:.1}% used ({left:.1}% left)")
        } else if let (Some(rem), Some(lim)) = (self.remaining, self.limit) {
            format!("{label}: {rem}/{lim} remaining")
        } else if let Some(rem) = self.remaining {
            format!("{label}: {rem} remaining")
        } else {
            format!("{label}: (no count reported)")
        };
        match self.resets_at_epoch {
            Some(reset) => {
                let delta = reset.saturating_sub(now_epoch);
                if delta > 0 {
                    format!("{body} · resets in {}", format_duration(delta))
                } else {
                    format!("{body} · resets imminently")
                }
            }
            None => body,
        }
    }
}

// ---- per-family parsers ----------------------------------------------------

fn anthropic_unified_windows(h: &HeaderMap) -> Vec<QuotaWindow> {
    let mut v = Vec::new();
    if let Some(w) = anthropic_unified_window(h, "5h", "5h") {
        v.push(w);
    }
    if let Some(w) = anthropic_unified_window(h, "7d", "weekly") {
        v.push(w);
    }
    v
}

fn anthropic_unified_window(h: &HeaderMap, key: &str, label: &str) -> Option<QuotaWindow> {
    // utilization is a 0..1 fraction string; reset is epoch seconds string.
    let util = header_f64(h, &format!("anthropic-ratelimit-unified-{key}-utilization"));
    let reset = header_i64(h, &format!("anthropic-ratelimit-unified-{key}-reset"));
    if util.is_none() && reset.is_none() {
        return None;
    }
    Some(QuotaWindow {
        label: label.to_string(),
        used_percent: util.map(|f| (f * 100.0).clamp(0.0, 100.0)),
        remaining: None,
        limit: None,
        resets_at_epoch: reset,
    })
}

fn anthropic_api_key_windows(h: &HeaderMap) -> Vec<QuotaWindow> {
    let mut v = Vec::new();
    if let Some(w) = anthropic_count_window(h, "tokens", "tokens") {
        v.push(w);
    }
    if let Some(w) = anthropic_count_window(h, "requests", "requests") {
        v.push(w);
    }
    v
}

fn anthropic_count_window(h: &HeaderMap, key: &str, label: &str) -> Option<QuotaWindow> {
    let limit = header_i64(h, &format!("anthropic-ratelimit-{key}-limit"));
    let remaining = header_i64(h, &format!("anthropic-ratelimit-{key}-remaining"));
    // reset is RFC3339 here, unlike the unified/codex epoch-seconds families.
    let reset =
        header_str(h, &format!("anthropic-ratelimit-{key}-reset")).and_then(rfc3339_to_epoch);
    if limit.is_none() && remaining.is_none() && reset.is_none() {
        return None;
    }
    Some(QuotaWindow {
        label: label.to_string(),
        used_percent: percent_used(remaining, limit),
        remaining,
        limit,
        resets_at_epoch: reset,
    })
}

fn codex_header_windows(h: &HeaderMap) -> Vec<QuotaWindow> {
    let mut v = Vec::new();
    if let Some(w) = codex_header_window(h, "primary") {
        v.push(w);
    }
    if let Some(w) = codex_header_window(h, "secondary") {
        v.push(w);
    }
    v
}

fn codex_header_window(h: &HeaderMap, key: &str) -> Option<QuotaWindow> {
    let used = header_f64(h, &format!("x-codex-{key}-used-percent"));
    let window_minutes = header_i64(h, &format!("x-codex-{key}-window-minutes"));
    let reset = header_i64(h, &format!("x-codex-{key}-reset-at"));
    if used.is_none() && reset.is_none() {
        return None;
    }
    Some(QuotaWindow {
        label: codex_window_label(window_minutes, key),
        used_percent: used.map(|p| p.clamp(0.0, 100.0)),
        remaining: None,
        limit: None,
        resets_at_epoch: reset,
    })
}

fn codex_event_window(window: Option<&Value>, fallback_key: &str) -> Option<QuotaWindow> {
    let window = window?;
    let used = window.get("used_percent").and_then(Value::as_f64);
    let window_minutes = window.get("window_minutes").and_then(Value::as_i64);
    let reset = window.get("reset_at").and_then(Value::as_i64);
    if used.is_none() && reset.is_none() {
        return None;
    }
    Some(QuotaWindow {
        label: codex_window_label(window_minutes, fallback_key),
        used_percent: used.map(|p| p.clamp(0.0, 100.0)),
        remaining: None,
        limit: None,
        resets_at_epoch: reset,
    })
}

/// Codex primary window ≈ 5h, secondary ≈ weekly. Prefer the reported window
/// length; fall back to position when it's absent.
fn codex_window_label(window_minutes: Option<i64>, fallback_key: &str) -> String {
    match window_minutes {
        Some(m) if m <= 360 => "5h".to_string(),
        Some(_) => "weekly".to_string(),
        None if fallback_key == "primary" => "5h".to_string(),
        None => "weekly".to_string(),
    }
}

fn openai_api_key_windows(h: &HeaderMap, now_epoch: i64) -> Vec<QuotaWindow> {
    let mut v = Vec::new();
    if let Some(w) = openai_count_window(h, "tokens", "tokens", now_epoch) {
        v.push(w);
    }
    if let Some(w) = openai_count_window(h, "requests", "requests", now_epoch) {
        v.push(w);
    }
    v
}

fn openai_count_window(
    h: &HeaderMap,
    key: &str,
    label: &str,
    now_epoch: i64,
) -> Option<QuotaWindow> {
    // Some endpoints/models return -1, 0, or omit these — treat non-positive
    // limits and negative remaining as unknown, not as a real zero quota.
    let limit = header_i64(h, &format!("x-ratelimit-limit-{key}")).filter(|&n| n > 0);
    let remaining = header_i64(h, &format!("x-ratelimit-remaining-{key}")).filter(|&n| n >= 0);
    // reset is a Go-duration string ("6m0s") → absolute epoch.
    let reset = header_str(h, &format!("x-ratelimit-reset-{key}"))
        .and_then(parse_go_duration_secs)
        .map(|secs| now_epoch.saturating_add(secs.round() as i64));
    if limit.is_none() && remaining.is_none() && reset.is_none() {
        return None;
    }
    Some(QuotaWindow {
        label: label.to_string(),
        used_percent: percent_used(remaining, limit),
        remaining,
        limit,
        resets_at_epoch: reset,
    })
}

// ---- small helpers ---------------------------------------------------------

fn percent_used(remaining: Option<i64>, limit: Option<i64>) -> Option<f64> {
    match (remaining, limit) {
        (Some(r), Some(l)) if l > 0 => {
            Some((100.0 * (1.0 - r as f64 / l as f64)).clamp(0.0, 100.0))
        }
        _ => None,
    }
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn header_i64(h: &HeaderMap, name: &str) -> Option<i64> {
    header_str(h, name)?.trim().parse().ok()
}

fn header_f64(h: &HeaderMap, name: &str) -> Option<f64> {
    // Reject non-finite values: `"NaN"`/`"inf"` parse fine as f64 but survive a
    // later `.clamp(0.0, 100.0)` (NaN comparisons are false), rendering garbage
    // like `NaN% used`. A hostile/MITM provider could send exactly that.
    header_str(h, name)?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

fn rfc3339_to_epoch(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|dt| dt.timestamp())
}

/// Parse a Go-style duration string (`"1s"`, `"6m0s"`, `"120ms"`, `"1m30s"`)
/// into seconds. Accepts `ms`/`s`/`m`/`h` segments, possibly combined and
/// fractional. Returns `None` on an empty, unitless, or unrecognized string.
fn parse_go_duration_secs(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0f64;
    let mut num = String::new();
    let mut saw_segment = false;
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            chars.next();
            continue;
        }
        let mut unit = String::new();
        while let Some(&u) = chars.peek() {
            if u.is_ascii_alphabetic() {
                unit.push(u);
                chars.next();
            } else {
                break;
            }
        }
        let value: f64 = num.parse().ok()?;
        num.clear();
        let secs = match unit.as_str() {
            "ms" => value / 1000.0,
            "s" => value,
            "m" => value * 60.0,
            "h" => value * 3600.0,
            _ => return None,
        };
        total += secs;
        saw_segment = true;
    }
    // A trailing number with no unit (e.g. a bare "30") is not a Go duration.
    if !num.is_empty() {
        return None;
    }
    saw_segment.then_some(total)
}

fn format_duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m > 0 {
            format!("~{h}h {m}m")
        } else {
            format!("~{h}h")
        }
    } else if secs >= 60 {
        format!("~{}m", secs / 60)
    } else {
        "<1m".to_string()
    }
}

#[cfg(test)]
mod tests;
