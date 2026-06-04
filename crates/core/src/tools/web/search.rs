//! `web_search` — keyless web search. Tries DuckDuckGo (POST HTML endpoint)
//! first and falls back to Mojeek, scraping each engine's no-login HTML so a
//! block or markup change transparently falls through to the next backend.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tools::{BuiltinTool, ToolContext};

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
        deserialize_with = "crate::tools::deserialize_optional_u64"
    )]
    max_results: Option<u64>,
    #[serde(
        default,
        alias = "allowedDomains",
        deserialize_with = "crate::tools::deserialize_optional_string_vec"
    )]
    allowed_domains: Option<Vec<String>>,
    #[serde(
        default,
        alias = "blockedDomains",
        deserialize_with = "crate::tools::deserialize_optional_string_vec"
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
        // Cap the body during streaming like web_fetch. These hosts are fixed
        // and trusted, but a misbehaving or compromised upstream (or a MITM)
        // shouldn't be able to stream an unbounded body into memory. 8 MiB is
        // far more than any real search-results page.
        const SEARCH_MAX_BYTES: usize = 8 * 1024 * 1024;
        use futures_util::StreamExt as _;
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("read search response body")?;
            let remaining = SEARCH_MAX_BYTES.saturating_sub(buf.len());
            if remaining == 0 {
                break;
            }
            let take = chunk.len().min(remaining);
            buf.extend_from_slice(&chunk[..take]);
            if take < chunk.len() {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
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
        // Only surface http(s) results: a decoded uddg target can be a
        // `file://`, `javascript:`, or `data:` URL, which must not reach the
        // model (or a later web_fetch).
        if !is_http_url(&r.url) {
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
        let a: WebSearchArgs = crate::tools::parse_args("web_search", args)?;
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
            // SSRF guard parity with web_fetch: a compromised or MITM'd search
            // backend could 302 us to an internal address (cloud metadata,
            // 127.0.0.1, RFC1918). Follow only http(s) redirects to non-blocked
            // literal IPs, capped at a few hops.
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 4 {
                    return attempt.stop();
                }
                let url = attempt.url();
                if !matches!(url.scheme(), "http" | "https") {
                    return attempt.stop();
                }
                if let Some(host) = url.host_str() {
                    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                        if super::is_blocked_ip(&ip) {
                            return attempt.stop();
                        }
                    }
                }
                attempt.follow()
            }))
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

/// Whether a result URL is a plain web URL safe to surface. Drops `file://`,
/// `javascript:`, `data:`, and other non-web schemes a decoded uddg target
/// could carry.
fn is_http_url(url: &str) -> bool {
    url::Url::parse(url)
        .map(|u| matches!(u.scheme(), "http" | "https"))
        .unwrap_or(false)
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
mod tests;
