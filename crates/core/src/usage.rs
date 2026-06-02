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
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
        }
        h
    }

    const NOW: i64 = 1_700_000_000;

    #[test]
    fn anthropic_api_key_counts_and_rfc3339_reset() {
        let h = headers(&[
            ("anthropic-ratelimit-tokens-limit", "100000"),
            ("anthropic-ratelimit-tokens-remaining", "25000"),
            ("anthropic-ratelimit-tokens-reset", "2023-11-14T22:13:20Z"),
        ]);
        let snap = parse_rate_limit_headers(&h, Provider::Anthropic, false, NOW).unwrap();
        let w = &snap.windows[0];
        assert_eq!(w.label, "tokens");
        assert_eq!(w.limit, Some(100000));
        assert_eq!(w.remaining, Some(25000));
        assert_eq!(w.used_percent, Some(75.0));
        // RFC3339 normalized to epoch.
        assert_eq!(w.resets_at_epoch, Some(1_700_000_000));
    }

    #[test]
    fn anthropic_oauth_unified_fraction_and_epoch() {
        let h = headers(&[
            ("anthropic-ratelimit-unified-5h-utilization", "0.25"),
            ("anthropic-ratelimit-unified-5h-reset", "1700003600"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.737"),
            ("anthropic-ratelimit-unified-7d-reset", "1700600000"),
        ]);
        let snap = parse_rate_limit_headers(&h, Provider::Anthropic, true, NOW).unwrap();
        assert_eq!(snap.windows.len(), 2);
        let five = &snap.windows[0];
        assert_eq!(five.label, "5h");
        // 0..1 fraction → 0..100 percent.
        assert_eq!(five.used_percent, Some(25.0));
        assert_eq!(five.resets_at_epoch, Some(1700003600));
        assert!((snap.windows[1].used_percent.unwrap() - 73.7).abs() < 1e-9);
    }

    #[test]
    fn openai_api_key_go_duration_and_unknown_values() {
        let h = headers(&[
            ("x-ratelimit-limit-tokens", "150000"),
            ("x-ratelimit-remaining-tokens", "149984"),
            ("x-ratelimit-reset-tokens", "6m0s"),
            // -1 limit must be treated as unknown, not a real zero quota.
            ("x-ratelimit-limit-requests", "-1"),
            ("x-ratelimit-remaining-requests", "59"),
        ]);
        let snap = parse_rate_limit_headers(&h, Provider::OpenAi, false, NOW).unwrap();
        let tokens = snap.windows.iter().find(|w| w.label == "tokens").unwrap();
        assert_eq!(tokens.limit, Some(150000));
        assert_eq!(tokens.resets_at_epoch, Some(NOW + 360));
        let requests = snap.windows.iter().find(|w| w.label == "requests").unwrap();
        assert_eq!(requests.limit, None, "-1 limit is unknown");
        assert_eq!(requests.remaining, Some(59));
        assert_eq!(
            requests.used_percent, None,
            "no percent without a real limit"
        );
    }

    #[test]
    fn codex_oauth_headers_used_percent_and_epoch() {
        let h = headers(&[
            ("x-codex-primary-used-percent", "12.5"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-primary-reset-at", "1700009000"),
            ("x-codex-secondary-used-percent", "73.0"),
            ("x-codex-secondary-window-minutes", "10080"),
            ("x-codex-secondary-reset-at", "1700600000"),
        ]);
        let snap = parse_rate_limit_headers(&h, Provider::OpenAi, true, NOW).unwrap();
        assert_eq!(snap.windows[0].label, "5h");
        assert_eq!(snap.windows[0].used_percent, Some(12.5));
        assert_eq!(snap.windows[0].resets_at_epoch, Some(1700009000));
        assert_eq!(snap.windows[1].label, "weekly");
        assert_eq!(snap.windows[1].used_percent, Some(73.0));
    }

    #[test]
    fn codex_in_body_event_parses() {
        let ev = serde_json::json!({
            "type": "codex.rate_limits",
            "plan_type": "pro",
            "rate_limits": {
                "primary": {"used_percent": 40.0, "window_minutes": 300, "reset_at": 1700009000},
                "secondary": {"used_percent": 5.0, "window_minutes": 10080, "reset_at": 1700600000}
            }
        });
        let snap = parse_codex_rate_limit_event(&ev).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("pro"));
        assert_eq!(snap.windows.len(), 2);
        assert_eq!(snap.windows[0].label, "5h");
        assert_eq!(snap.windows[0].used_percent, Some(40.0));
    }

    #[test]
    fn missing_headers_yield_none() {
        let h = headers(&[("content-type", "text/event-stream")]);
        assert!(parse_rate_limit_headers(&h, Provider::OpenAi, false, NOW).is_none());
        assert!(parse_rate_limit_headers(&h, Provider::Anthropic, true, NOW).is_none());
    }

    #[test]
    fn go_duration_parsing() {
        assert_eq!(parse_go_duration_secs("1s"), Some(1.0));
        assert_eq!(parse_go_duration_secs("6m0s"), Some(360.0));
        assert_eq!(parse_go_duration_secs("1m30s"), Some(90.0));
        assert_eq!(parse_go_duration_secs("120ms"), Some(0.12));
        assert_eq!(parse_go_duration_secs("2h"), Some(7200.0));
        assert_eq!(parse_go_duration_secs(""), None);
        assert_eq!(
            parse_go_duration_secs("30"),
            None,
            "bare number is not a duration"
        );
        assert_eq!(parse_go_duration_secs("abc"), None);
    }

    #[test]
    fn hostile_header_values_do_not_panic() {
        // Reset headers are provider-controlled; extreme values must not overflow.
        let h = headers(&[
            ("x-ratelimit-limit-tokens", "100"),
            ("x-ratelimit-remaining-tokens", "50"),
            ("x-ratelimit-reset-tokens", "9999999999999999h"),
        ]);
        let snap = parse_rate_limit_headers(&h, Provider::OpenAi, false, i64::MAX - 1).unwrap();
        // Render against a far-past now and a far-future now — both must be fine.
        let _ = snap.render(0);
        let _ = snap.render(i64::MAX);

        let extreme = QuotaSnapshot {
            provider: Some(Provider::OpenAi),
            plan: None,
            windows: vec![QuotaWindow {
                label: "5h".into(),
                used_percent: Some(10.0),
                remaining: None,
                limit: None,
                resets_at_epoch: Some(i64::MIN),
            }],
            captured_at_epoch: 0,
        };
        let _ = extreme.render(i64::MAX);
    }

    #[test]
    fn non_finite_percent_headers_are_ignored() {
        for bad in ["NaN", "nan", "inf", "-inf", "infinity"] {
            let h = headers(&[("x-codex-primary-used-percent", bad)]);
            assert_eq!(
                header_f64(&h, "x-codex-primary-used-percent"),
                None,
                "{bad} must not survive as a percent"
            );
        }
        let h = headers(&[("x-codex-primary-used-percent", "42.5")]);
        assert_eq!(header_f64(&h, "x-codex-primary-used-percent"), Some(42.5));
    }

    #[test]
    fn render_is_unit_agnostic_and_relative() {
        let snap = QuotaSnapshot {
            provider: Some(Provider::OpenAi),
            plan: Some("pro".into()),
            windows: vec![
                QuotaWindow {
                    label: "5h".into(),
                    used_percent: Some(12.5),
                    remaining: None,
                    limit: None,
                    resets_at_epoch: Some(NOW + 7200),
                },
                QuotaWindow {
                    label: "tokens".into(),
                    used_percent: Some(75.0),
                    remaining: Some(25000),
                    limit: Some(100000),
                    resets_at_epoch: Some(NOW + 360),
                },
            ],
            captured_at_epoch: NOW,
        };
        let out = snap.render(NOW);
        assert!(out.contains("Plan: pro"), "{out}");
        // Both directions are shown: consumed and remaining.
        assert!(
            out.contains("5-hour: 12.5% used (87.5% left) · resets in ~2h"),
            "{out}"
        );
        assert!(
            out.contains("Tokens: 75.0% used (25.0% left) · resets in ~6m"),
            "{out}"
        );
    }
}
