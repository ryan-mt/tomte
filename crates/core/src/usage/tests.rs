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
