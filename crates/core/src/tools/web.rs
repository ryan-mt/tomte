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
        // SSRF guard: resolve the host and reject loopback / RFC1918 / link-local
        // ranges so the model can't be coaxed into hitting cloud metadata
        // (169.254.169.254) or internal admin endpoints (127.0.0.1, 10.x).
        validate_ssrf_safe(&url).await?;
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
        // noise into the context from a single fetch. Convert to plain text
        // (like Claude Code's web fetch) so only the readable content lands in
        // the model's context.
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

pub struct WebSearch;

#[derive(Deserialize)]
struct WebSearchArgs {
    #[serde(alias = "q", alias = "search_query", alias = "searchQuery")]
    query: String,
    #[serde(
        default,
        alias = "maxResults",
        alias = "num_results",
        alias = "numResults",
        alias = "limit",
        deserialize_with = "super::deserialize_optional_u64"
    )]
    max_results: Option<u64>,
    #[serde(
        default,
        alias = "allowedDomains",
        deserialize_with = "super::deserialize_optional_string_vec"
    )]
    allowed_domains: Option<Vec<String>>,
    #[serde(
        default,
        alias = "blockedDomains",
        deserialize_with = "super::deserialize_optional_string_vec"
    )]
    blocked_domains: Option<Vec<String>>,
}

const DEFAULT_MAX_RESULTS: usize = 10;
const HARD_MAX_RESULTS: usize = 25;

/// Keyless search backends, tried in order; the first to return any results
/// wins. Each scrapes a no-login HTML endpoint, so a block or markup change on
/// one engine transparently falls through to the next.
#[derive(Clone, Copy)]
enum Backend {
    DuckDuckGo,
    Mojeek,
}

impl Backend {
    const ALL: [Backend; 2] = [Backend::DuckDuckGo, Backend::Mojeek];

    fn label(self) -> &'static str {
        match self {
            Backend::DuckDuckGo => "duckduckgo",
            Backend::Mojeek => "mojeek",
        }
    }

    async fn fetch(self, client: &reqwest::Client, query: &str) -> Result<String> {
        let enc = urlencoding::encode(query);
        let resp = match self {
            // DuckDuckGo's HTML endpoint expects a POST form; a GET is often
            // answered with a 202 bot-challenge page that carries no results.
            Backend::DuckDuckGo => {
                client
                    .post("https://html.duckduckgo.com/html/")
                    .form(&[("q", query)])
                    .send()
                    .await?
            }
            Backend::Mojeek => {
                client
                    .get(format!("https://www.mojeek.com/search?q={enc}"))
                    .send()
                    .await?
            }
        };
        if !resp.status().is_success() {
            return Err(anyhow!(
                "{} returned HTTP {}",
                self.label(),
                resp.status().as_u16()
            ));
        }
        resp.text().await.context("read search response body")
    }

    fn parse(self, html: &str) -> Vec<SearchResult> {
        match self {
            Backend::DuckDuckGo => parse_ddg(html),
            Backend::Mojeek => parse_mojeek(html),
        }
    }
}

/// Drop empty/duplicate results, apply domain allow/block lists, then cap to
/// `max`. Shared by every backend so filtering behaves identically.
fn apply_filters(
    raw: Vec<SearchResult>,
    max: usize,
    allowed: Option<&[String]>,
    blocked: Option<&[String]>,
) -> Vec<SearchResult> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for r in raw {
        if r.title.is_empty() || r.url.is_empty() {
            continue;
        }
        if is_search_ad_or_tracking_url(&r.url) {
            continue;
        }
        if !seen.insert(r.url.clone()) {
            continue;
        }
        let host = host_of(&r.url);
        if let Some(allow) = allowed {
            if !allow.iter().any(|d| host_matches(&host, d)) {
                continue;
            }
        }
        if let Some(block) = blocked {
            if block.iter().any(|d| host_matches(&host, d)) {
                continue;
            }
        }
        out.push(r);
        if out.len() >= max {
            break;
        }
    }
    out
}

fn format_results(query: &str, results: &[SearchResult]) -> String {
    let mut out = format!("Search results for: {query}\n\n");
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, r.title, r.url));
        if !r.snippet.is_empty() {
            out.push_str(&format!("   {}\n", r.snippet));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[async_trait]
impl BuiltinTool for WebSearch {
    fn name(&self) -> &'static str {
        "web_search"
    }
    fn description(&self) -> &'static str {
        "Search the web and return ranked results (title, URL, snippet). Keyless — tries DuckDuckGo first and falls back to Mojeek, so a block or outage on one engine transparently uses another.\n\
\n\
When to use:\n\
- You need current information beyond your training cutoff (releases, news, recent docs).\n\
- You don't know the exact URL — search first, then `web_fetch` the most promising result to read it in full.\n\
- Verifying a fact, finding the canonical source, discovering library/API documentation.\n\
\n\
When NOT to use:\n\
- You already know the URL — call `web_fetch` directly.\n\
- The answer is in the codebase — use `grep` / `read_file`.\n\
\n\
Workflow: `web_search` to find candidates → pick the best URL → `web_fetch` to read its full contents.\n\
\n\
Parameters:\n\
- `query`: The search query.\n\
- `max_results`: Cap on results returned (default 10, max 25); pass `null` for the default.\n\
- `allowed_domains`: If set, only return results whose host matches one of these domains; `null` for no allow-list.\n\
- `blocked_domains`: If set, drop results whose host matches one of these domains; `null` for no block-list."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "The search query."},
                "max_results": {"type": ["integer", "null"], "description": "Cap on results (default 10, max 25); null for the default."},
                "allowed_domains": {"type": ["array", "null"], "items": {"type": "string"}, "description": "Only include results from these domains; null for no allow-list."},
                "blocked_domains": {"type": ["array", "null"], "items": {"type": "string"}, "description": "Exclude results from these domains; null for no block-list."}
            },
            "required": ["query", "max_results", "allowed_domains", "blocked_domains"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let a: WebSearchArgs = super::parse_args("web_search", args)?;
        let query = a.query.trim();
        if query.is_empty() {
            return Err(anyhow!("query must not be empty"));
        }
        let max = (a.max_results.unwrap_or(DEFAULT_MAX_RESULTS as u64) as usize)
            .clamp(1, HARD_MAX_RESULTS);
        // A plain Chrome UA: search HTML endpoints answer obvious bots (or
        // exotic UA strings) with empty challenge pages.
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .timeout(Duration::from_secs(20))
            .build()?;
        let mut last_err: Option<String> = None;
        for backend in Backend::ALL {
            match backend.fetch(&client, query).await {
                Ok(html) => {
                    let results = apply_filters(
                        backend.parse(&html),
                        max,
                        a.allowed_domains.as_deref(),
                        a.blocked_domains.as_deref(),
                    );
                    if !results.is_empty() {
                        return Ok(format_results(query, &results));
                    }
                    tracing::info!(
                        backend = backend.label(),
                        "web_search: backend returned no results, trying next"
                    );
                }
                Err(e) => {
                    tracing::warn!(backend = backend.label(), error = %e, "web_search backend failed");
                    last_err = Some(e.to_string());
                }
            }
        }
        match last_err {
            // At least one backend errored and none produced results: an
            // infrastructure failure, not a genuine empty result set. Surface it
            // as a tool error so the model can tell "the web has nothing" apart
            // from "search was blocked/offline" and retry, instead of concluding
            // the fact doesn't exist.
            Some(e) => Err(anyhow!(
                "web_search failed: every backend errored or returned nothing (last error: {e})"
            )),
            // Every backend responded but matched nothing — a real empty result.
            None => Ok(format!("No results found for: {query}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Attach each snippet to the result that most immediately precedes it in the
/// document. Results and snippets are captured by separate regex passes; pairing
/// them by list index silently mis-aligns every later snippet as soon as one
/// result lacks a snippet (ad/markup-variant rows). Positional assignment — each
/// snippet belongs to the last result whose tag starts before it — is robust to
/// such gaps. `results`/`snippets` carry each match's byte offset.
fn attach_snippets(
    mut results: Vec<(usize, SearchResult)>,
    snippets: Vec<(usize, String)>,
) -> Vec<SearchResult> {
    for (spos, text) in snippets {
        // Attach to the NEAREST result whose tag starts before this snippet,
        // filling only if it's still empty. Surplus snippets (deep-link/sub-row
        // blocks a result emits after its main one) are dropped rather than
        // back-filled onto an EARLIER empty result — the old `&& is_empty()` in
        // the `find` predicate walked past the filled nearest result and
        // mis-assigned the surplus to a preceding row.
        if let Some((_, r)) = results.iter_mut().rev().find(|(rpos, _)| *rpos < spos) {
            if r.snippet.is_empty() {
                r.snippet = text;
            }
        }
    }
    results.into_iter().map(|(_, r)| r).collect()
}

/// Parse DuckDuckGo's HTML results into raw results (no filtering). Pure and
/// network-free so it can be unit-tested against a fixture.
fn parse_ddg(html: &str) -> Vec<SearchResult> {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static RESULT_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#).unwrap()
    });
    static SNIPPET_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"(?s)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#).unwrap());

    let results: Vec<(usize, SearchResult)> = RESULT_RE
        .captures_iter(html)
        .map(|cap| {
            (
                cap.get(0).map(|m| m.start()).unwrap_or(0),
                SearchResult {
                    title: clean_html(&cap[2]),
                    url: extract_real_url(&cap[1]),
                    snippet: String::new(),
                },
            )
        })
        .collect();
    let snippets: Vec<(usize, String)> = SNIPPET_RE
        .captures_iter(html)
        .map(|c| (c.get(0).map(|m| m.start()).unwrap_or(0), clean_html(&c[1])))
        .collect();
    attach_snippets(results, snippets)
}

/// Parse Mojeek's HTML results. Mojeek hrefs are already absolute, and each
/// result is `<a class="title" … href="URL">Title</a>` followed by a
/// `<p class="s">snippet</p>`.
fn parse_mojeek(html: &str) -> Vec<SearchResult> {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static RESULT_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"(?s)<a class="title"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#).unwrap()
    });
    static SNIPPET_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"(?s)<p class="s">(.*?)</p>"#).unwrap());

    let results: Vec<(usize, SearchResult)> = RESULT_RE
        .captures_iter(html)
        .map(|cap| {
            (
                cap.get(0).map(|m| m.start()).unwrap_or(0),
                SearchResult {
                    title: clean_html(&cap[2]),
                    url: cap[1].trim().to_string(),
                    snippet: String::new(),
                },
            )
        })
        .collect();
    let snippets: Vec<(usize, String)> = SNIPPET_RE
        .captures_iter(html)
        .map(|c| (c.get(0).map(|m| m.start()).unwrap_or(0), clean_html(&c[1])))
        .collect();
    attach_snippets(results, snippets)
}

/// DuckDuckGo wraps each target in `//duckduckgo.com/l/?uddg=<encoded>&rut=…`.
/// Pull the `uddg` param out and URL-decode it; fall back to the raw href
/// (adding a scheme for protocol-relative links) when there's no wrapper.
fn extract_real_url(href: &str) -> String {
    let href = href.trim().replace("&amp;", "&");
    let normalized = if let Some(stripped) = href.strip_prefix("//") {
        format!("https://{stripped}")
    } else {
        href
    };
    if let Ok(url) = url::Url::parse(&normalized) {
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        if host_matches(&host, "duckduckgo.com") && url.path().starts_with("/l/") {
            if let Some((_, target)) = url.query_pairs().find(|(k, _)| k == "uddg") {
                return target.into_owned();
            }
        }
    }
    normalized
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_default()
}

fn is_search_ad_or_tracking_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let path = url.path().to_ascii_lowercase();
    let query = url.query().unwrap_or("").to_ascii_lowercase();
    (host_matches(&host, "duckduckgo.com") && path.ends_with("/y.js"))
        || (host_matches(&host, "bing.com") && path.contains("/aclick"))
        || query.contains("ad_provider=")
        || query.contains("ad_domain=")
}

/// Domain match: exact host or any subdomain, ignoring a leading `www.`.
fn host_matches(host: &str, domain: &str) -> bool {
    let domain = domain.trim().to_ascii_lowercase();
    let d = domain.strip_prefix("www.").unwrap_or(&domain);
    let h = host.strip_prefix("www.").unwrap_or(host);
    h == d || h.ends_with(&format!(".{d}"))
}

/// Strip HTML tags and decode the handful of entities DuckDuckGo emits, then
/// collapse whitespace. Good enough for titles and snippets.
fn clean_html(s: &str) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]+>").unwrap());
    TAG_RE
        .replace_all(s, "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&#x2F;", "/")
        // Non-breaking space → ordinary space (collapsed by split_whitespace
        // below), matching web_fetch's decode_entities so snippets don't keep a
        // literal `&nbsp;`. Before `&amp;` so a double-encoded `&amp;nbsp;`
        // stays literal rather than collapsing to a space.
        .replace("&nbsp;", " ")
        // `&amp;` LAST so an encoded entity like `&amp;lt;` decodes to the literal
        // `&lt;`, not double-decoded into `<`.
        .replace("&amp;", "&")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod web_search_tests {
    use super::*;

    #[test]
    fn html_to_text_strips_markup_scripts_and_entities() {
        let html = r#"<!doctype html><html><head><title>T</title><style>.a{color:red}</style></head>
<body><script>var a = 1 < 2;</script><h1>Hello</h1><p>World &amp; <b>friends</b></p><!-- hidden --></body></html>"#;
        let out = html_to_text(html);
        assert!(out.contains("Hello"), "kept heading text: {out:?}");
        assert!(
            out.contains("World & friends"),
            "decoded entity + inline tag: {out:?}"
        );
        assert!(!out.contains("color:red"), "dropped <style>: {out:?}");
        assert!(!out.contains("var a"), "dropped <script>: {out:?}");
        assert!(!out.contains("hidden"), "dropped comment: {out:?}");
        assert!(!out.contains('<'), "all tags stripped: {out:?}");
    }

    const SAMPLE: &str = r#"<a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ftokio.rs%2F&amp;rut=abc">Tokio <b>runtime</b></a><a class="result__snippet" href="//x">An async <b>runtime</b> for Rust.</a><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fgithub.com%2Ftokio-rs%2Ftokio&amp;rut=def">GitHub tokio-rs</a><a class="result__snippet" href="//y">The source repository.</a>"#;

    const MOJEEK_SAMPLE: &str = r#"<a class="title" title="https://tokio.rs/" href="https://tokio.rs/">Tokio - async <b>runtime</b></a></h2><p class="s"><strong>Tokio</strong> is an async runtime.</p><a class="title" title="https://github.com/tokio-rs/tokio" href="https://github.com/tokio-rs/tokio">GitHub tokio-rs</a></h2><p class="s">The source repo.</p>"#;

    #[test]
    fn ddg_parses_title_url_and_snippet() {
        let r = parse_ddg(SAMPLE);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Tokio runtime");
        assert_eq!(r[0].url, "https://tokio.rs/");
        assert_eq!(r[0].snippet, "An async runtime for Rust.");
        assert_eq!(r[1].url, "https://github.com/tokio-rs/tokio");
    }

    #[test]
    fn ddg_extracts_uddg_only_from_duckduckgo_wrappers() {
        assert_eq!(
            extract_real_url(
                "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdoc%3Fx%3D1&amp;rut=abc"
            ),
            "https://example.com/doc?x=1"
        );
        assert_eq!(
            extract_real_url("https://example.com/doc?next=uddg%3Dhttps%253A%252F%252Fevil.test"),
            "https://example.com/doc?next=uddg%3Dhttps%253A%252F%252Fevil.test"
        );
        assert_eq!(
            extract_real_url("https://evilduckduckgo.com/l/?uddg=https%3A%2F%2Fevil.test"),
            "https://evilduckduckgo.com/l/?uddg=https%3A%2F%2Fevil.test"
        );
    }

    #[test]
    fn ddg_extracts_uddg_after_html_escaped_param_separator() {
        assert_eq!(
            extract_real_url(
                "//duckduckgo.com/l/?rut=abc&amp;uddg=https%3A%2F%2Fexample.com%2Fdoc"
            ),
            "https://example.com/doc"
        );
    }

    #[test]
    fn mojeek_parses_title_url_and_snippet() {
        let r = parse_mojeek(MOJEEK_SAMPLE);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Tokio - async runtime");
        assert_eq!(r[0].url, "https://tokio.rs/");
        assert_eq!(r[0].snippet, "Tokio is an async runtime.");
        assert_eq!(r[1].url, "https://github.com/tokio-rs/tokio");
    }

    #[test]
    fn respects_max_results() {
        assert_eq!(apply_filters(parse_ddg(SAMPLE), 1, None, None).len(), 1);
    }

    #[test]
    fn allowed_domains_keeps_only_matches() {
        let r = apply_filters(
            parse_ddg(SAMPLE),
            10,
            Some(&["github.com".to_string()]),
            None,
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].url, "https://github.com/tokio-rs/tokio");
    }

    #[test]
    fn blocked_domains_drops_matches() {
        let r = apply_filters(parse_ddg(SAMPLE), 10, None, Some(&["tokio.rs".to_string()]));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].url, "https://github.com/tokio-rs/tokio");
    }

    #[test]
    fn web_search_args_accept_string_domain_lists() {
        let args: WebSearchArgs = serde_json::from_value(json!({
            "query": "tokio",
            "max_results": "5",
            "allowed_domains": "tokio.rs, github.com",
            "blocked_domains": "ads.example bad.example"
        }))
        .unwrap();

        assert_eq!(args.max_results, Some(5));
        assert_eq!(
            args.allowed_domains,
            Some(vec!["tokio.rs".to_string(), "github.com".to_string()])
        );
        assert_eq!(
            args.blocked_domains,
            Some(vec!["ads.example".to_string(), "bad.example".to_string()])
        );
    }

    #[test]
    fn web_args_accept_camel_case_aliases() {
        let fetch: WebFetchArgs = serde_json::from_value(json!({
            "uri": "https://example.com",
            "maxBytes": "4096"
        }))
        .unwrap();
        assert_eq!(fetch.url, "https://example.com");
        assert_eq!(fetch.max_bytes, Some(4096));

        let search: WebSearchArgs = serde_json::from_value(json!({
            "searchQuery": "tokio",
            "numResults": "5",
            "allowedDomains": "tokio.rs github.com",
            "blockedDomains": ["ads.example"]
        }))
        .unwrap();

        assert_eq!(search.query, "tokio");
        assert_eq!(search.max_results, Some(5));
        assert_eq!(
            search.allowed_domains,
            Some(vec!["tokio.rs".to_string(), "github.com".to_string()])
        );
        assert_eq!(
            search.blocked_domains,
            Some(vec!["ads.example".to_string()])
        );
    }

    #[test]
    fn dedups_repeated_urls() {
        let mut raw = parse_ddg(SAMPLE);
        raw.push(raw[0].clone());
        assert_eq!(apply_filters(raw, 10, None, None).len(), 2);
    }

    #[test]
    fn drops_duckduckgo_ad_tracking_results() {
        let raw = vec![
            SearchResult {
                title: "Official".into(),
                url: "https://openai.com/".into(),
                snippet: String::new(),
            },
            SearchResult {
                title: "Sponsored".into(),
                url: "https://duckduckgo.com/y.js?ad_domain=example.com&ad_provider=bingv7aa"
                    .into(),
                snippet: String::new(),
            },
        ];
        let r = apply_filters(raw, 10, None, None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].url, "https://openai.com/");
    }

    #[test]
    fn host_matches_handles_subdomains_and_www() {
        assert!(host_matches("docs.rs", "docs.rs"));
        assert!(host_matches("www.github.com", "github.com"));
        assert!(host_matches("api.github.com", "github.com"));
        assert!(!host_matches("notgithub.com", "github.com"));
    }

    #[test]
    fn html_to_text_drops_unclosed_script() {
        // A truncated page (byte cap mid-document) leaves an unclosed <script>;
        // it must not leak the raw JS into the output.
        let html = "<body>visible text<script>var x = 1; if (a < b) { leak() }";
        let out = html_to_text(html);
        assert!(out.contains("visible text"), "kept body text: {out:?}");
        assert!(
            !out.contains("leak"),
            "dropped unclosed script body: {out:?}"
        );
        assert!(
            !out.contains("var x"),
            "dropped unclosed script body: {out:?}"
        );
    }

    #[test]
    fn ddg_pairs_snippets_by_position_when_one_missing() {
        // Result A has NO snippet; result B does. Index pairing would mislabel A
        // with B's snippet — positional pairing keeps A empty and B correct.
        let html = r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com%2F&amp;rut=x">Result A</a><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fb.com%2F&amp;rut=y">Result B</a><a class="result__snippet" href="//y">Snippet for B.</a>"#;
        let r = parse_ddg(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].url, "https://a.com/");
        assert_eq!(r[0].snippet, "", "A must not steal B's snippet: {r:?}");
        assert_eq!(r[1].url, "https://b.com/");
        assert_eq!(r[1].snippet, "Snippet for B.");
    }

    #[test]
    fn clean_html_does_not_double_decode_amp() {
        // `&amp;lt;` is an encoded `&lt;`; it must decode to the literal `&lt;`,
        // not be double-decoded into `<`.
        assert_eq!(clean_html("&amp;lt;script&amp;gt;"), "&lt;script&gt;");
        // A bare `&amp;` still decodes normally.
        assert_eq!(clean_html("A &amp; B"), "A & B");
    }
}
