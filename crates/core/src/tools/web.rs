use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

pub struct WebFetch;

#[derive(Deserialize)]
struct WebFetchArgs {
    #[serde(alias = "uri", alias = "link")]
    url: String,
    #[serde(
        default,
        alias = "maxBytes",
        deserialize_with = "super::deserialize_optional_u64"
    )]
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
Use this when you need the contents of a web page or a public API response — for example, fetching upstream documentation, a raw GitHub file, or an RFC. HTML responses are stripped to readable plain text (scripts, styles and tags removed) to keep the context lean; non-HTML (JSON, plain text, source) is returned as-is. For large responses, pass `max_bytes` to cap the size; the hard ceiling is 10 MiB.\n\
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
        let parsed = url::Url::parse(&a.url).with_context(|| format!("parse URL: {}", a.url))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(anyhow!("URL must start with http:// or https://"));
        }
        let url = parsed.to_string();
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("URL has no host: {url}"))?
            .to_string();
        // SSRF guard: resolve the host and reject loopback / RFC1918 / link-local
        // ranges so the model can't be coaxed into hitting cloud metadata
        // (169.254.169.254) or internal admin endpoints (127.0.0.1, 10.x). The
        // validated IPs are pinned into the client below so the connect-time
        // lookup can't rebind to an address we never checked.
        let pinned = validate_ssrf_safe(&url).await?;
        let cap = a.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).min(HARD_CAP_BYTES) as usize;
        let client = reqwest::Client::builder()
            .user_agent(concat!("tomte/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            // Disable automatic redirects: a redirect could send us to a
            // private address even though the initial host was public. We
            // surface 3xx + Location in the body so the model can choose.
            .redirect(reqwest::redirect::Policy::none())
            // Pin the exact IPs we validated above; reqwest would otherwise
            // resolve the host again at connect time (DNS-rebinding window).
            .resolve_to_addrs(&host, &pinned)
            .build()?;

        // Retry transient failures: connection errors, DNS hiccups, 5xx.
        // 4xx and parse failures are surfaced immediately — those aren't
        // going to succeed by trying again.
        const MAX_ATTEMPTS: u32 = 3;
        let mut attempt: u32 = 0;
        let resp = loop {
            attempt += 1;
            match client.get(&url).send().await {
                Ok(r) => {
                    if r.status().is_server_error() && attempt < MAX_ATTEMPTS {
                        let code = r.status().as_u16();
                        tracing::warn!(
                            url = %url,
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
                            url = %url,
                            attempt,
                            error = %e,
                            "web_fetch: transient network error, will retry"
                        );
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    return Err(anyhow::Error::from(e).context(format!("GET {url}")));
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
        // Enforce the byte cap *during* streaming so a fast/unbounded server
        // can't force us to buffer hundreds of MiB before we truncate.
        use futures_util::StreamExt as _;
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(cap.min(65_536));
        let mut truncated = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("read body from {url}"))?;
            let remaining = cap.saturating_sub(buf.len());
            if remaining == 0 {
                truncated = true;
                break;
            }
            let take = chunk.len().min(remaining);
            buf.extend_from_slice(&chunk[..take]);
            if take < chunk.len() {
                truncated = true;
                break;
            }
        }
        let total = buf.len();
        let body = String::from_utf8_lossy(&buf);
        // HTML pages are mostly markup, scripts and inline styles — returning
        // them verbatim buries the useful text and can dump 250k+ tokens of
        // noise into the context from a single fetch. Convert to plain text so
        // only the readable content lands in the model's context.
        let ct = content_type.to_ascii_lowercase();
        let head: String = body
            .chars()
            .take(1024)
            .collect::<String>()
            .to_ascii_lowercase();
        // When the Content-Type is missing/generic, sniff — but only treat the
        // body as HTML if it *starts* with the markup (after leading
        // whitespace/BOM). Matching "<html" anywhere in the first 1KB
        // false-positives on non-HTML that merely mentions it (a Markdown doc, a
        // source file), which html_to_text would then mangle.
        let is_html = ct.contains("html")
            || (!ct.contains("json") && !ct.contains("text/plain") && {
                // trim_start() does NOT strip a UTF-8 BOM (U+FEFF is not
                // Unicode whitespace), so strip it explicitly before sniffing.
                let h = head.trim_start_matches('\u{feff}').trim_start();
                h.starts_with("<!doctype html") || h.starts_with("<html")
            });
        let (rendered, note) = if is_html {
            (html_to_text(&body), " (HTML converted to text)")
        } else {
            (body.into_owned(), "")
        };
        let location_line = match (&location, status.is_redirection()) {
            (Some(loc), true) => format!("Location: {loc}\n"),
            _ => String::new(),
        };
        Ok(format!(
            "HTTP {} {}\nContent-Type: {}{}\n{}Bytes: {}{}\n---\n{}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            content_type,
            note,
            location_line,
            total,
            if truncated { " (truncated)" } else { "" },
            rendered
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

/// Strip an HTML document down to readable plain text: drop script/style/head
/// and comments outright, turn block-closing tags into newlines, remove the
/// remaining tags, decode the common entities, and collapse whitespace. Not a
/// full HTML→Markdown renderer — the goal is to keep the model's context lean,
/// not to preserve layout. (`regex` has no backreferences, so each noise block
/// is matched by its own alternative.)
fn html_to_text(html: &str) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static DROP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?is)<!--.*?-->|<script\b[^>]*>.*?</script>|<style\b[^>]*>.*?</style>|<head\b[^>]*>.*?</head>|<noscript\b[^>]*>.*?</noscript>|<svg\b[^>]*>.*?</svg>",
        )
        .unwrap()
    });
    // A truncated page (web_fetch caps bytes mid-document) leaves an UNCLOSED
    // <script>/<style>/etc. that DROP's close-tag alternatives never match — the
    // per-tag TAG pass would then strip only `<script>` and leak the raw JS/CSS
    // (and `<b>`-looking fragments inside it) into the output. Drop an
    // unterminated noise block and everything after it; truncated tail content
    // is garbage anyway.
    static DROP_UNCLOSED: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?is)<!--.*$|<(?:script|style|svg|noscript|head)\b[^>]*>.*$").unwrap()
    });
    static BLOCK: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)</(p|div|li|tr|h[1-6]|section|article|header|footer|blockquote|ul|ol|table)>|<br\s*/?>")
            .unwrap()
    });
    static TAG: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
    static HSPACE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t]{2,}").unwrap());
    static BLANK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{3,}").unwrap());

    let s = DROP.replace_all(html, " ");
    let s = DROP_UNCLOSED.replace_all(&s, " ");
    let s = BLOCK.replace_all(&s, "\n");
    let s = TAG.replace_all(&s, "");
    let s = decode_entities(&s);
    let s = HSPACE.replace_all(&s, " ");
    let s = BLANK.replace_all(&s, "\n\n");
    s.lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Decode the handful of HTML entities common in body text. `&amp;` is decoded
/// last so an encoded entity like `&amp;lt;` doesn't get double-decoded.
fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&amp;", "&")
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
/// Returns the validated socket addresses so the caller can PIN them into the
/// request client (`resolve_to_addrs`). Without pinning, reqwest resolves the
/// host a second time at connect and could land on an address we never checked
/// (DNS rebinding); pinning the exact validated IPs closes that window.
///
/// Note: automatic redirects are disabled separately in the caller so a 302
/// cannot bypass the check.
async fn validate_ssrf_safe(url_str: &str) -> Result<Vec<std::net::SocketAddr>> {
    use std::net::SocketAddr;
    let parsed = url::Url::parse(url_str).with_context(|| format!("parse URL: {url_str}"))?;
    let port = parsed.port_or_known_default().unwrap_or(80);

    // An IP-literal host must NOT be sent through getaddrinfo: host_str() returns
    // the bracketed form `[::1]` for a v6 literal, which lookup_host cannot parse,
    // so every v6-literal fetch used to die with a misleading "DNS lookup failed"
    // and the v6 arm of is_blocked_ip never ran on this path. Build the SocketAddr
    // straight from the literal so is_blocked_ip vets it; only real domain names
    // hit the resolver.
    let addrs: Vec<SocketAddr> = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => vec![SocketAddr::from((ip, port))],
        Some(url::Host::Ipv6(ip)) => vec![SocketAddr::from((ip, port))],
        Some(url::Host::Domain(host)) => tokio::net::lookup_host((host, port))
            .await
            .with_context(|| format!("DNS lookup failed for {host}"))?
            .collect(),
        None => return Err(anyhow!("URL has no host: {url_str}")),
    };

    if addrs.is_empty() {
        return Err(anyhow!(
            "no addresses resolved for {}",
            parsed.host_str().unwrap_or("")
        ));
    }

    for sa in &addrs {
        if is_blocked_ip(&sa.ip()) {
            return Err(anyhow!(
                "blocked: {} resolves to {} (loopback/private/link-local)",
                parsed.host_str().unwrap_or(""),
                sa.ip()
            ));
        }
    }
    Ok(addrs)
}

/// Returns true for any IP we should refuse to fetch from a model-issued
/// `web_fetch`. Covers v4 loopback / RFC1918 / link-local / CGNAT /
/// this-network (0.0.0.0/8) / unspecified / broadcast / documentation, and v6 loopback /
/// unique-local (fc00::/7) / link-local (fe80::/10) / unspecified /
/// multicast / IPv4-mapped / IPv4-compatible / NAT64-embedded equivalents.
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
                // "This host on this network" 0.0.0.0/8 (RFC 6890); subsumes the
                // lone 0.0.0.0 that is_unspecified covers — some stacks route the
                // rest of the block to the local host.
                || oct[0] == 0
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
            // Other prefixes that embed an IPv4 in the low 32 bits must re-check
            // it against the v4 rules, or a v6 literal could reach a loopback /
            // private v4 while skipping them entirely: IPv4-compatible (::/96,
            // deprecated but still routable on some stacks, e.g. ::127.0.0.1) and
            // the NAT64 well-known prefix (64:ff9b::/96, e.g. 64:ff9b::7f00:1).
            let embeds_v4 = seg[..6] == [0, 0, 0, 0, 0, 0]
                || (seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2..6] == [0, 0, 0, 0]);
            if embeds_v4 {
                let v4 = std::net::Ipv4Addr::new(
                    (seg[6] >> 8) as u8,
                    (seg[6] & 0xff) as u8,
                    (seg[7] >> 8) as u8,
                    (seg[7] & 0xff) as u8,
                );
                return is_blocked_ip(&IpAddr::V4(v4));
            }
            false
        }
    }
}

mod search;
pub use search::WebSearch;

#[cfg(test)]
mod tests;
