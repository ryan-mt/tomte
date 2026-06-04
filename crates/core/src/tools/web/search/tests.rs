use super::*;

const SAMPLE: &str = r#"<a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ftokio.rs%2F&amp;rut=abc">Tokio <b>runtime</b></a><a class="result__snippet" href="//x">An async <b>runtime</b> for Rust.</a><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fgithub.com%2Ftokio-rs%2Ftokio&amp;rut=def">GitHub tokio-rs</a><a class="result__snippet" href="//y">The source repository.</a>"#;

const MOJEEK_SAMPLE: &str = r#"<a class="title" title="https://tokio.rs/" href="https://tokio.rs/">Tokio - async <b>runtime</b></a></h2><p class="s"><strong>Tokio</strong> is an async runtime.</p><a class="title" title="https://github.com/tokio-rs/tokio" href="https://github.com/tokio-rs/tokio">GitHub tokio-rs</a></h2><p class="s">The source repo.</p>"#;

#[test]
fn apply_filters_drops_non_http_schemes() {
    let mk = |url: &str| SearchResult {
        title: "t".into(),
        url: url.into(),
        snippet: String::new(),
    };
    let raw = vec![
        mk("https://tokio.rs/"),
        mk("javascript:alert(1)"),
        mk("file:///etc/passwd"),
        mk("data:text/html,x"),
    ];
    let out = apply_filters(raw, 10, None, None);
    assert_eq!(out.len(), 1, "only the http(s) result should survive");
    assert_eq!(out[0].url, "https://tokio.rs/");
}

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
        extract_real_url("//duckduckgo.com/l/?rut=abc&amp;uddg=https%3A%2F%2Fexample.com%2Fdoc"),
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
fn web_search_args_accept_camel_case_aliases() {
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
            url: "https://duckduckgo.com/y.js?ad_domain=example.com&ad_provider=bingv7aa".into(),
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
