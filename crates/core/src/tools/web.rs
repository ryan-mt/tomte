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
- The request times out after 30 seconds.\n\
- Redirects are followed automatically.\n\
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
        let a: WebFetchArgs = serde_json::from_value(args)?;
        if !(a.url.starts_with("http://") || a.url.starts_with("https://")) {
            return Err(anyhow!("URL must start with http:// or https://"));
        }
        let cap = a
            .max_bytes
            .unwrap_or(DEFAULT_MAX_BYTES)
            .min(HARD_CAP_BYTES) as usize;
        let client = reqwest::Client::builder()
            .user_agent(concat!("opencli/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        let resp = client
            .get(&a.url)
            .send()
            .await
            .with_context(|| format!("GET {}", a.url))?;
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp.bytes().await?;
        let total = bytes.len();
        let truncated = total > cap;
        let slice = &bytes[..total.min(cap)];
        let body = String::from_utf8_lossy(slice);
        Ok(format!(
            "HTTP {} {}\nContent-Type: {}\nBytes: {}{}\n---\n{}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            content_type,
            total,
            if truncated { " (truncated)" } else { "" },
            body
        ))
    }
}
