//! OpenAI-compatible Chat Completions adapter. Lets tomte talk to any
//! provider that implements `/v1/chat/completions` (Groq, OpenRouter, DeepSeek,
//! Together, local Ollama / LM Studio, …) by translating the shared
//! [`ResponsesRequest`] IR into a Chat Completions request and bridging its SSE
//! stream back onto the shared [`ResponseStreamEvent`] shape — the same IR the
//! OpenAI Responses and Anthropic paths use, so the agent loop is unchanged.
//!
//! This is the wire layer only (translation + streaming). Client construction,
//! config, auth, and routing live in the provider/config plumbing.

mod accumulator;
mod client;
mod extract;
mod request;
mod stream;

pub use client::ChatCompletionsClient;
pub use request::translate_chat_request;
pub use stream::handle_chat_response;
