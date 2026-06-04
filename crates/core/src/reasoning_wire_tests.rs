//! Cross-provider contract: a reasoning/thinking item that is *foreign* to the
//! target provider must never reach that provider's wire body. Otherwise a
//! `/model` switch (or a resumed cross-provider session) makes a provider
//! receive a reasoning id/format it didn't issue and rejects the turn with a
//! `400` (e.g. OpenAI's `Invalid 'input[N].id'`).
//!
//! Each native provider drops foreign reasoning at its own IR→wire boundary;
//! there is no single enforcement point in the `ProviderClient` trait, so this
//! is enforced by discipline. This module pins that discipline in ONE place:
//! when you add a provider, add its boundary here so the contract is checked
//! for it too.

use crate::anthropic::translate::to_messages_request;
use crate::openai::chat::translate_chat_request;
use crate::openai::client::strip_unsendable_reasoning;
use crate::openai::models::{InputItem, MessageContent, ResponsesRequest};

/// Thinking text carried only by the Anthropic-style (signed) reasoning item —
/// foreign to OpenAI.
const ANTHROPIC_THOUGHT: &str = "ANTHROPIC_ONLY_THOUGHT";
/// Reasoning id carried only by the OpenAI-style (id, no signature) item —
/// foreign to Anthropic.
const OPENAI_THOUGHT_ID: &str = "rs_openai_only";

/// A request whose history carries reasoning from *both* providers, as happens
/// after a mid-session `/model` switch or a resumed cross-provider session.
fn request_with_both_reasoning_styles() -> ResponsesRequest {
    ResponsesRequest::new(
        "test-model",
        vec![
            InputItem::Message {
                role: "user".into(),
                content: vec![MessageContent::text("hello")],
            },
            // Anthropic-origin: signed thinking, foreign to the OpenAI wire.
            InputItem::Reasoning {
                id: String::new(),
                summary: Vec::new(),
                thinking: Some(ANTHROPIC_THOUGHT.into()),
                signature: Some("sig".into()),
                redacted_thinking: None,
            },
            // OpenAI-origin: real reasoning id, no signature; foreign to Anthropic.
            InputItem::Reasoning {
                id: OPENAI_THOUGHT_ID.into(),
                summary: Vec::new(),
                thinking: None,
                signature: None,
                redacted_thinking: None,
            },
        ],
    )
}

#[test]
fn openai_responses_wire_drops_foreign_reasoning_keeps_its_own() {
    let mut input = request_with_both_reasoning_styles().input;
    strip_unsendable_reasoning(&mut input);
    let body = serde_json::to_string(&input).unwrap();
    assert!(
        !body.contains(ANTHROPIC_THOUGHT),
        "OpenAI Responses must drop Anthropic signed thinking: {body}"
    );
    assert!(
        body.contains(OPENAI_THOUGHT_ID),
        "OpenAI Responses must keep its own reasoning id: {body}"
    );
}

#[test]
fn anthropic_wire_drops_foreign_reasoning() {
    let req = request_with_both_reasoning_styles();
    let body = serde_json::to_string(&to_messages_request(&req)).unwrap();
    assert!(
        !body.contains(OPENAI_THOUGHT_ID),
        "Anthropic must not forward an OpenAI reasoning id: {body}"
    );
}

#[test]
fn chat_completions_wire_drops_all_reasoning() {
    // Chat Completions has no input representation for reasoning, so it must drop
    // every reasoning item regardless of origin.
    let req = request_with_both_reasoning_styles();
    let body = serde_json::to_string(&translate_chat_request(&req)).unwrap();
    assert!(
        !body.contains(ANTHROPIC_THOUGHT) && !body.contains(OPENAI_THOUGHT_ID),
        "Chat Completions must drop all reasoning items: {body}"
    );
}
