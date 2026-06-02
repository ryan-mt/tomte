//! Shared HTTP retry for LLM provider requests.
//!
//! Wrapping `.send()` here means every provider — and any future one added
//! behind the `ProviderClient` trait — gets the same resilience to transient
//! network failures and server overload. A flaky connection no longer fails a
//! whole turn (or silently kills a sub-agent) on the first hiccup.

use std::time::Duration;

/// Maximum send attempts (1 initial try + retries).
const MAX_ATTEMPTS: u32 = 4;

/// Send `builder`, retrying transient transport failures and overload statuses
/// (429 / retryable 5xx) with backoff. Permanent errors (other 4xx, malformed
/// requests) and any success status are returned immediately so the caller can
/// surface them. A `Retry-After` header is honored (capped) when present.
///
/// Only the request *initiation* is retried: once a success status is returned
/// the response (and its stream body) is handed back untouched, so a partially
/// consumed stream is never re-sent.
pub(crate) async fn send_with_retry(
    builder: reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        // `try_clone` returns None only for non-rewindable streaming request
        // bodies; ours are in-memory JSON, so this is `Some` in practice. If it
        // ever isn't, fall back to a single un-retried send rather than panic.
        let Some(this) = builder.try_clone() else {
            return builder.send().await;
        };
        match this.send().await {
            Ok(resp) => {
                let status = resp.status();
                if attempt < MAX_ATTEMPTS && is_retryable_status(status) {
                    let wait = retry_after(&resp).unwrap_or_else(|| backoff(attempt));
                    tracing::warn!(attempt, %status, "llm request overloaded; retrying");
                    tokio::time::sleep(wait).await;
                    continue;
                }
                return Ok(resp);
            }
            Err(e) => {
                if attempt < MAX_ATTEMPTS && is_transient(&e) {
                    tracing::warn!(attempt, error = %e, "llm request transient failure; retrying");
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Connection / timeout / request-build IO failures are worth retrying; a bad
/// URL or redirect loop is not.
fn is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

/// Overload / transient HTTP statuses: 429 (rate limit) and the retryable 5xx
/// family. 501 (not implemented) is excluded — retrying it won't help.
fn is_retryable_status(s: reqwest::StatusCode) -> bool {
    matches!(s.as_u16(), 429 | 500 | 502 | 503 | 504)
}

/// Cap on a honored `Retry-After` so a hostile or huge value can't stall a turn.
const RETRY_AFTER_CAP_SECS: u64 = 30;

/// Read and parse a `Retry-After` header from the response (capped).
fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let raw = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    parse_retry_after(raw, chrono::Utc::now())
}

/// Parse a `Retry-After` value, supporting BOTH forms allowed by RFC 7231:
/// delta-seconds (`"120"`) and an HTTP-date (`"Wed, 21 Oct 2026 07:28:00 GMT"`).
/// The previous version only handled delta-seconds, silently ignoring a valid
/// HTTP-date and falling back to plain backoff. Capped at `RETRY_AFTER_CAP_SECS`.
fn parse_retry_after(raw: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(RETRY_AFTER_CAP_SECS)));
    }
    // HTTP-date (IMF-fixdate). Treat the parsed naive time as UTC ("GMT").
    let when = chrono::NaiveDateTime::parse_from_str(raw, "%a, %d %b %Y %H:%M:%S GMT")
        .ok()?
        .and_utc();
    let secs = (when - now)
        .num_seconds()
        .clamp(0, RETRY_AFTER_CAP_SECS as i64) as u64;
    Some(Duration::from_secs(secs))
}

/// Exponential-ish backoff between attempts.
fn backoff(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_millis(300),
        2 => Duration::from_millis(800),
        3 => Duration::from_millis(1800),
        _ => Duration::from_secs(3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_overload_statuses_retry() {
        for c in [429u16, 500, 502, 503, 504] {
            assert!(is_retryable_status(
                reqwest::StatusCode::from_u16(c).unwrap()
            ));
        }
        for c in [200u16, 400, 401, 403, 404, 422, 501] {
            assert!(!is_retryable_status(
                reqwest::StatusCode::from_u16(c).unwrap()
            ));
        }
    }

    #[test]
    fn backoff_is_monotonic() {
        assert!(backoff(1) < backoff(2));
        assert!(backoff(2) < backoff(3));
        assert!(backoff(3) <= backoff(4));
    }

    #[test]
    fn retry_after_parses_both_seconds_and_http_date() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-21T07:28:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // delta-seconds, capped.
        assert_eq!(parse_retry_after("5", now), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_retry_after("9999", now),
            Some(Duration::from_secs(RETRY_AFTER_CAP_SECS))
        );
        // HTTP-date 10s in the future.
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2026 07:28:10 GMT", now),
            Some(Duration::from_secs(10))
        );
        // HTTP-date in the past clamps to zero (don't wait).
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2026 07:27:00 GMT", now),
            Some(Duration::from_secs(0))
        );
        // Garbage → None (caller falls back to backoff).
        assert_eq!(parse_retry_after("soon", now), None);
    }
}
