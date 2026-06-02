use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::Engine;
use rand::RngCore;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;

use crate::auth::Credential;

use super::models::{InputItem, ResponsesRequest};
use super::stream::StreamHandle;

/// Cap connect time so a black-holed DNS or unreachable host fails fast
/// rather than hanging the agent turn. Streaming responses can legitimately
/// be long-lived, so we deliberately leave the per-request `.timeout()`
/// unset and rely on `STREAM_IDLE_TIMEOUT` in the agent layer to catch
/// silent stream stalls.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Total timeout for a NON-streaming `create()` (e.g. the compaction summary).
/// Unlike a stream, a one-shot response has no idle-watchdog, so without this a
/// server that connects then never responds would hang the turn forever. Each
/// retry attempt is bounded by it.
const CREATE_TIMEOUT: Duration = Duration::from_secs(300);

fn redact_auth_in(body: &str) -> String {
    crate::sensitive::redact_auth_in(body)
}

fn parse_json_response(text: &str) -> Result<serde_json::Value> {
    serde_json::from_str(text).map_err(|e| {
        anyhow!(
            "parse response: {e}; body: {}",
            crate::sensitive::error_excerpt(text)
        )
    })
}

const API_BASE: &str = "https://api.openai.com/v1";
const CHATGPT_BACKEND_BASE: &str = "https://chatgpt.com/backend-api/codex";

/// Drop reasoning items the Responses API can't replay before they hit the wire.
///
/// History is a shared, provider-agnostic IR: a single conversation can cross
/// providers (a `/model` switch, or resuming a session that ran on another
/// provider). Reasoning/thinking items, however, are provider-specific and
/// opaque — only the provider that produced one can replay it. Sending a foreign
/// reasoning item to the Responses API fails with `400 Invalid 'input[N].id'`
/// (Anthropic thinking blocks carry no OpenAI reasoning id, so they are stored
/// with an empty `id`).
///
/// The rule is therefore provider-general, not a patch for one other backend:
/// keep a reasoning item only if it looks like a genuine OpenAI Responses
/// reasoning item — a non-empty reasoning id and no Anthropic thinking
/// `signature`. Anything else originated elsewhere and is dropped. This mirrors
/// the Anthropic translator, which drops reasoning lacking a signature at its
/// own IR→wire boundary; every native provider must sanitize foreign reasoning
/// the same way at its send boundary. Dropping is lossless here: the OpenAI path
/// never stores its own reasoning ids today, so nothing legitimate is lost.
fn strip_unsendable_reasoning(input: &mut Vec<InputItem>) {
    input.retain(|item| match item {
        InputItem::Reasoning { id, signature, .. } => !id.trim().is_empty() && signature.is_none(),
        _ => true,
    });
}

fn normalize_openai_reasoning_effort(
    model: &str,
    effort: &str,
    is_chatgpt_subscription: bool,
) -> String {
    // Any `-pro` variant deliberates at the top tier. Match the `-pro` family
    // rather than one exact id, so a current `gpt-5.5-pro` and any future pro
    // model are pinned to `high` instead of silently dropping to the generic
    // effort mapping (the old `== "gpt-5-pro"` check missed `gpt-5.5-pro`).
    if model.contains("-pro") {
        return "high".to_string();
    }

    match effort {
        // The ChatGPT/Codex OAuth backend for current GPT-5.4/5.5 models rejects
        // `minimal` and accepts `none`. Public API GPT-5-era models historically
        // used `minimal`, so keep the old `none -> minimal` shim only there.
        "none" if !is_chatgpt_subscription => "minimal".to_string(),
        "minimal" if is_chatgpt_subscription => "none".to_string(),
        // `max` and Claude Code's `ultracode` aren't OpenAI effort levels; both
        // clamp to the top OpenAI tier, `xhigh`.
        "max" | "ultracode" => "xhigh".to_string(),
        other => other.to_string(),
    }
}

pub struct OpenAiClient {
    http: reqwest::Client,
    credential: Credential,
    session_id: String,
}

fn random_id() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

impl OpenAiClient {
    pub fn new(credential: Credential) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("opencli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            credential,
            session_id: random_id(),
        })
    }

    fn responses_endpoint(&self) -> String {
        if self.credential.is_chatgpt_subscription() {
            format!("{CHATGPT_BACKEND_BASE}/responses")
        } else {
            format!("{API_BASE}/responses")
        }
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&self.credential.auth_header_value())?,
        );
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert("Accept", HeaderValue::from_static("text/event-stream"));
        if let Credential::OAuth {
            account_id: Some(id),
            ..
        } = &self.credential
        {
            if !id.is_empty() {
                h.insert("ChatGPT-Account-ID", HeaderValue::from_str(id)?);
            }
        }
        if self.credential.is_chatgpt_subscription() {
            h.insert("OpenAI-Beta", HeaderValue::from_static("responses=v1"));
            h.insert("OAI-Product-Sku", HeaderValue::from_static("codex"));
            h.insert("originator", HeaderValue::from_static("opencli"));
            h.insert("session_id", HeaderValue::from_str(&self.session_id)?);
        }
        Ok(h)
    }

    /// Stream a Responses API request, returning a handle producing SSE events.
    pub async fn stream(&self, mut req: ResponsesRequest) -> Result<StreamHandle> {
        req.stream = true;
        strip_unsendable_reasoning(&mut req.input);
        self.apply_credential_defaults(&mut req);
        self.send_internal(req).await
    }

    /// Non-streaming variant.
    pub async fn create(&self, mut req: ResponsesRequest) -> Result<serde_json::Value> {
        req.stream = false;
        strip_unsendable_reasoning(&mut req.input);
        self.apply_credential_defaults(&mut req);
        let builder = self
            .http
            .post(self.responses_endpoint())
            .headers(self.headers()?)
            .timeout(CREATE_TIMEOUT)
            .json(&req);
        let resp = crate::retry::send_with_retry(builder).await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!("OpenAI {} {}", status, redact_auth_in(&text)));
        }
        parse_json_response(&text)
    }

    fn apply_credential_defaults(&self, req: &mut ResponsesRequest) {
        // ChatGPT backend rejects requests unless `store: false` is explicitly set.
        // For the public API (`api.openai.com`), the default behavior (server-side
        // storage) is fine; we leave `store` unset.
        if self.credential.is_chatgpt_subscription() && req.store.is_none() {
            req.store = Some(false);
        }
        if let Some(reasoning) = req.reasoning.as_mut() {
            if let Some(effort) = reasoning.effort.as_deref() {
                reasoning.effort = Some(normalize_openai_reasoning_effort(
                    &req.model,
                    effort,
                    self.credential.is_chatgpt_subscription(),
                ));
            }
        }
    }

    async fn send_internal(&self, req: ResponsesRequest) -> Result<StreamHandle> {
        let builder = self
            .http
            .post(self.responses_endpoint())
            .headers(self.headers()?)
            .json(&req);
        let resp = crate::retry::send_with_retry(builder).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => format!("(failed to read error body: {e})"),
            };
            return Err(anyhow!("OpenAI {} {}", status, redact_auth_in(&text)));
        }
        // Capture rate-limit/quota headers before the body stream is consumed.
        // ChatGPT/Codex subscription creds expose `x-codex-*`; api.openai.com
        // keys expose `x-ratelimit-*` — `parse_rate_limit_headers` branches on
        // `is_chatgpt_subscription`.
        let quota = crate::usage::parse_rate_limit_headers(
            resp.headers(),
            crate::provider::Provider::OpenAi,
            self.credential.is_chatgpt_subscription(),
            chrono::Utc::now().timestamp(),
        );
        Ok(StreamHandle::from_response(resp).with_quota(quota))
    }
}

/// Helper: convenient way to call a non-streaming endpoint with arbitrary body.
pub async fn raw_post<B: Serialize>(
    credential: &Credential,
    path: &str,
    body: &B,
) -> Result<serde_json::Value> {
    let http = reqwest::Client::new();
    let url = if credential.is_chatgpt_subscription() {
        format!("{CHATGPT_BACKEND_BASE}/{}", path.trim_start_matches('/'))
    } else {
        format!("{API_BASE}/{}", path.trim_start_matches('/'))
    };
    let mut req = http
        .post(&url)
        .header(AUTHORIZATION, credential.auth_header_value())
        .header(CONTENT_TYPE, "application/json")
        .json(body);
    if let Credential::OAuth {
        account_id: Some(id),
        ..
    } = credential
    {
        if !id.is_empty() {
            req = req.header("ChatGPT-Account-ID", id.clone());
        }
    }
    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("OpenAI {} {}", status, redact_auth_in(&text)));
    }
    parse_json_response(&text)
}

#[async_trait::async_trait]
impl crate::client::ProviderClient for OpenAiClient {
    fn provider(&self) -> crate::provider::Provider {
        crate::provider::Provider::OpenAi
    }
    // Inherent methods take priority over trait methods of the same name, so
    // these delegate to `OpenAiClient::{stream,create}` without recursing.
    async fn stream(&self, req: ResponsesRequest) -> Result<StreamHandle> {
        self.stream(req).await
    }
    async fn create(&self, req: ResponsesRequest) -> Result<serde_json::Value> {
        self.create(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_openai_reasoning_effort, parse_json_response, strip_unsendable_reasoning,
    };
    use crate::openai::models::{InputItem, MessageContent};

    #[test]
    fn strips_empty_id_reasoning_but_keeps_real_ids_and_other_items() {
        let mut input = vec![
            InputItem::Message {
                role: "user".into(),
                content: vec![MessageContent::text("hi")],
            },
            // Anthropic-origin reasoning carried across a /model switch.
            InputItem::Reasoning {
                id: String::new(),
                summary: Vec::new(),
                thinking: Some("internal".into()),
                signature: Some("sig".into()),
                redacted_thinking: None,
            },
            // A genuine OpenAI reasoning item must survive.
            InputItem::Reasoning {
                id: "rs_123".into(),
                summary: Vec::new(),
                thinking: None,
                signature: None,
                redacted_thinking: None,
            },
            InputItem::FunctionCallOutput {
                call_id: "call_1".into(),
                output: "ok".into(),
                error: false,
            },
        ];

        strip_unsendable_reasoning(&mut input);

        assert_eq!(input.len(), 3);
        assert!(matches!(input[0], InputItem::Message { .. }));
        assert!(
            matches!(&input[1], InputItem::Reasoning { id, .. } if id == "rs_123"),
            "real-id reasoning must be preserved"
        );
        assert!(matches!(input[2], InputItem::FunctionCallOutput { .. }));
    }

    #[test]
    fn strips_signed_reasoning_even_with_a_nonempty_id() {
        // A reasoning item that still carries an Anthropic thinking signature is
        // foreign to the Responses API regardless of its id.
        let mut input = vec![InputItem::Reasoning {
            id: "rs_looks_real".into(),
            summary: Vec::new(),
            thinking: Some("internal".into()),
            signature: Some("sig".into()),
            redacted_thinking: None,
        }];
        strip_unsendable_reasoning(&mut input);
        assert!(input.is_empty());
    }

    #[test]
    fn strips_whitespace_only_id_reasoning() {
        let mut input = vec![InputItem::Reasoning {
            id: "   ".into(),
            summary: Vec::new(),
            thinking: None,
            signature: None,
            redacted_thinking: None,
        }];
        strip_unsendable_reasoning(&mut input);
        assert!(input.is_empty());
    }

    #[test]
    fn chatgpt_oauth_keeps_none_and_maps_minimal_to_none() {
        assert_eq!(
            normalize_openai_reasoning_effort("gpt-5.5", "none", true),
            "none"
        );
        assert_eq!(
            normalize_openai_reasoning_effort("gpt-5.5", "minimal", true),
            "none"
        );
    }

    #[test]
    fn public_api_preserves_legacy_none_to_minimal_shim() {
        assert_eq!(
            normalize_openai_reasoning_effort("gpt-5", "none", false),
            "minimal"
        );
        assert_eq!(
            normalize_openai_reasoning_effort("gpt-5", "minimal", false),
            "minimal"
        );
    }

    #[test]
    fn openai_pro_clamps_every_effort_to_high() {
        // Matched by the `-pro` family, so both the catalogued `gpt-5-pro` and a
        // real current `gpt-5.5-pro` (and any future pro tier) pin to `high`.
        for model in ["gpt-5-pro", "gpt-5.5-pro"] {
            for effort in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
                assert_eq!(
                    normalize_openai_reasoning_effort(model, effort, false),
                    "high"
                );
            }
        }
    }

    #[test]
    fn openai_non_pro_keeps_normal_effort_mapping() {
        // The `-pro` guard must not catch a non-pro model.
        assert_eq!(
            normalize_openai_reasoning_effort("gpt-5.5", "low", true),
            "low"
        );
        assert_eq!(
            normalize_openai_reasoning_effort("gpt-5.5", "medium", false),
            "medium"
        );
    }

    #[test]
    fn parse_json_response_redacts_and_caps_error_body() {
        let body = format!(
            "{{\"error\":\"bad key sk-proj-secret and Bearer oauth-secret\",\"padding\":\"{}\"",
            "x".repeat(512)
        );

        let err = parse_json_response(&body).expect_err("malformed JSON must fail");
        let message = err.to_string();

        assert!(!message.contains("sk-proj-secret"), "{message}");
        assert!(!message.contains("oauth-secret"), "{message}");
        assert!(!message.contains(&"x".repeat(256)), "{message}");
        assert!(message.contains("<redacted>"), "{message}");
        assert!(message.contains("truncated"), "{message}");
        assert!(message.len() < 320, "{message}");
    }
}
