use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

pub struct WebFetch;

#[derive(Deserialize)]
struct WebFetchArgs {
    url: String,
    #[serde(default)]
    max_bytes: Option<u64>,
}

const DEFAULT_MAX_BYTES: u64 = 1_048_576; // 1 MiB
const HARD_CAP_BYTES: u64 = 10 * 1_048_576; // 10 MiB

#[async_trait]
impl BuiltinTool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> &'static str {
        "Fetch a URL via HTTP(S) GET and return the response body as text, along with the status line and Content-Type header.\n\
\n\
Use this when you need the contents of a web page or a public API response — for example, fetching upstream documentation, a raw GitHub file, or an RFC. For HTML, the body is returned verbatim; read the meaningful parts out of it. For large responses, pass `max_bytes` to cap the size; the hard ceiling is 10 MiB.\n\
\n\
Constraints:\n\
- Only `http://` and `https://` URLs are accepted.\n\
- Hosts that resolve to loopback / private (RFC1918) / link-local / CGNAT IPs are rejected (cloud metadata endpoints like 169.254.169.254 are unreachable).\n\
- The request times out after 30 seconds.\n\
- Redirects are NOT followed automatically — a 3xx response is returned verbatim with its `Location` header so the model can decide whether to fetch the next URL.\n\
- Non-UTF8 bytes are replaced with the Unicode replacement character.\n\
\n\
Parameters:\n\
- `url`: Full URL beginning with `http://` or `https://`.\n\
- `max_bytes`: Maximum bytes to read into the response (capped at 10 MiB); pass `null` for the default of 1 MiB."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Full URL beginning with http:// or https://."},
                "max_bytes": {"type": ["integer", "null"], "description": "Cap on bytes read; null uses the default of 1 MiB."}
            },
            "required": ["url", "max_bytes"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let a: WebFetchArgs = super::parse_args("web_fetch", args)?;
        if !(a.url.starts_with("http://") || a.url.starts_with("https://")) {
            return Err(anyhow!("URL must start with http:// or https://"));
        }
        // SSRF guard: resolve the host and reject loopback / RFC1918 / link-local
        // ranges so the model can't be coaxed into hitting cloud metadata
        // (169.254.169.254) or internal admin endpoints (127.0.0.1, 10.x).
        validate_ssrf_safe(&a.url).await?;
        let cap = a.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).min(HARD_CAP_BYTES) as usize;
        let client = reqwest::Client::builder()
            .user_agent(concat!("opencli/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            // Disable automatic redirects: a redirect could send us to a
            // private address even though the initial host was public. We
            // surface 3xx + Location in the body so the model can choose.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        // Retry transient failures: connection errors, DNS hiccups, 5xx.
        // 4xx and parse failures are surfaced immediately — those aren't
        // going to succeed by trying again.
        const MAX_ATTEMPTS: u32 = 3;
        let mut attempt: u32 = 0;
        let resp = loop {
            attempt += 1;
            match client.get(&a.url).send().await {
                Ok(r) => {
                    if r.status().is_server_error() && attempt < MAX_ATTEMPTS {
                        let code = r.status().as_u16();
                        tracing::warn!(
                            url = %a.url,
                            attempt,
                            status = code,
                            "web_fetch: server error, will retry"
                        );
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    break r;
                }
                Err(e) => {
                    if attempt < MAX_ATTEMPTS && is_transient(&e) {
                        tracing::warn!(
                            url = %a.url,
                            attempt,
                            error = %e,
                            "web_fetch: transient network error, will retry"
                        );
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    return Err(anyhow::Error::from(e).context(format!("GET {}", a.url)));
                }
            }
        };
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // Surface Location for 3xx so the model can pick the next hop.
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = resp
            .bytes()
            .await
            .with_context(|| format!("read body from {}", a.url))?;
        let total = bytes.len();
        let truncated = total > cap;
        let slice = &bytes[..total.min(cap)];
        let body = String::from_utf8_lossy(slice);
        let location_line = match (&location, status.is_redirection()) {
            (Some(loc), true) => format!("Location: {loc}\n"),
            _ => String::new(),
        };
        Ok(format!(
            "HTTP {} {}\nContent-Type: {}\n{}Bytes: {}{}\n---\n{}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            content_type,
            location_line,
            total,
            if truncated { " (truncated)" } else { "" },
            body
        ))
    }
}

/// Linear-ish backoff with a small jitter floor. 200ms → 500ms → 1s.
fn backoff(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_millis(200),
        2 => Duration::from_millis(500),
        _ => Duration::from_secs(1),
    }
}

/// A network error is retryable when it's a connect/timeout/IO failure rather
/// than a definitive protocol error like an invalid URL or redirect loop.
fn is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

/// SSRF guard. Parses `url`, resolves the host, and rejects loopback,
/// private (RFC1918), link-local (169.254.0.0/16 — AWS metadata),
/// CGNAT, unspecified, and IPv6 unique-local / link-local addresses.
///
/// Note: this only validates the initial host. Automatic redirects are
/// disabled separately in the caller so a 302 cannot bypass the check.
/// We do not defend against DNS rebinding (resolving twice and getting a
/// different address); rebinding would require a determined attacker
/// controlling DNS, which is outside the local-tool threat model.
async fn validate_ssrf_safe(url_str: &str) -> Result<()> {
    let parsed = url::Url::parse(url_str).with_context(|| format!("parse URL: {url_str}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("URL has no host: {url_str}"))?;
    let port = parsed.port_or_known_default().unwrap_or(80);

    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("DNS lookup failed for {host}"))?
        .collect();

    if addrs.is_empty() {
        return Err(anyhow!("no addresses resolved for {host}"));
    }

    for sa in &addrs {
        if is_blocked_ip(&sa.ip()) {
            return Err(anyhow!(
                "blocked: {host} resolves to {} (loopback/private/link-local)",
                sa.ip()
            ));
        }
    }
    Ok(())
}

/// Returns true for any IP we should refuse to fetch from a model-issued
/// `web_fetch`. Covers v4 loopback / RFC1918 / link-local / CGNAT /
/// unspecified / broadcast / documentation, and v6 loopback /
/// unique-local (fc00::/7) / link-local (fe80::/10) / unspecified /
/// multicast / IPv4-mapped equivalents.
fn is_blocked_ip(ip: &std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let oct = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // CGNAT 100.64.0.0/10
                || (oct[0] == 100 && (oct[1] & 0xc0) == 64)
                // Benchmarking 198.18.0.0/15
                || (oct[0] == 198 && (oct[1] & 0xfe) == 18)
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return true;
            }
            let seg = v6.segments();
            // Unique-local fc00::/7
            if (seg[0] & 0xfe00) == 0xfc00 {
                return true;
            }
            // Link-local fe80::/10
            if (seg[0] & 0xffc0) == 0xfe80 {
                return true;
            }
            // IPv4-mapped: ::ffff:0:0/96 — re-check against v4 rules.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip(&IpAddr::V4(v4));
            }
            false
        }
    }
}
