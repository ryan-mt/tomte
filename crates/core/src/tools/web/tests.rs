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
fn ssrf_blocks_v6_embedded_internal_ipv4() {
    use std::net::IpAddr;
    let blocked = |s: &str| is_blocked_ip(&s.parse::<IpAddr>().unwrap());
    // IPv4-mapped (already handled) — still blocked.
    assert!(blocked("::ffff:127.0.0.1"));
    assert!(blocked("::ffff:169.254.169.254"));
    // IPv4-compatible (::/96, deprecated) embedding loopback / private / CGNAT.
    assert!(blocked("::127.0.0.1"));
    assert!(blocked("::10.0.0.1"));
    // NAT64 well-known prefix (64:ff9b::/96) embedding loopback / link-local.
    assert!(blocked("64:ff9b::7f00:1")); // 127.0.0.1
    assert!(blocked("64:ff9b::a9fe:a9fe")); // 169.254.169.254 (cloud metadata)
                                            // A public IPv4 under the same prefixes is not blocked (same as fetching
                                            // that public v4 directly) — the re-check must not over-block.
    assert!(!blocked("64:ff9b::5db8:d822")); // 93.184.216.34 (example.com)
                                             // A real, unrelated public v6 host is not blocked.
    assert!(!blocked("2606:2800:220:1:248:1893:25c8:1946"));
}

#[test]
fn ssrf_blocks_this_network_0_0_0_0_8() {
    use std::net::IpAddr;
    let blocked = |s: &str| is_blocked_ip(&s.parse::<IpAddr>().unwrap());
    // 0.0.0.0/8 ("this host on this network") is reserved; some stacks route it
    // to the local host. The whole block is refused, not just 0.0.0.0 itself.
    assert!(blocked("0.0.0.0"));
    assert!(blocked("0.0.0.1"));
    assert!(blocked("0.1.2.3"));
    // A normal public address stays allowed.
    assert!(!blocked("93.184.216.34"));
}

#[tokio::test]
async fn ssrf_vets_ip_literals_without_dns() {
    // v6 literals used to fail with a misleading DNS error because host_str()
    // returns the bracketed form `[::1]`, which getaddrinfo can't parse — so the
    // v6 arm of is_blocked_ip never ran on this path. They must now be parsed
    // directly and rejected/allowed by is_blocked_ip, with no network lookup.
    assert!(validate_ssrf_safe("http://[::1]/").await.is_err()); // loopback
    assert!(validate_ssrf_safe("http://127.0.0.1/").await.is_err()); // loopback v4
    assert!(validate_ssrf_safe("http://[fd00::1]/").await.is_err()); // unique-local
    assert!(validate_ssrf_safe("http://[fe80::1]/").await.is_err()); // link-local
    assert!(validate_ssrf_safe("http://[64:ff9b::a9fe:a9fe]/")
        .await
        .is_err()); // NAT64 metadata
                    // A public v6 literal is allowed — no DNS, no spurious "DNS lookup failed".
    let ok = validate_ssrf_safe("http://[2606:2800:220:1:248:1893:25c8:1946]/")
        .await
        .expect("public v6 literal must be allowed, not rejected by a DNS error");
    assert!(ok.iter().all(|sa| !is_blocked_ip(&sa.ip())));
}

#[test]
fn web_fetch_args_accept_camel_case_aliases() {
    let fetch: WebFetchArgs = serde_json::from_value(json!({
        "uri": "https://example.com",
        "maxBytes": "4096"
    }))
    .unwrap();
    assert_eq!(fetch.url, "https://example.com");
    assert_eq!(fetch.max_bytes, Some(4096));
}
