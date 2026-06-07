//! Agent tests (compaction_tests), split out of `agent`.

use super::{
    classify_input_tokens, clear_stale_tool_outputs, compacted_history, emit_usage,
    is_context_overflow_message, should_compact, CLEARED_TOOL_OUTPUT_MARKER, COMPACT_MIN_ITEMS,
};
use crate::openai::{InputItem, MessageContent};
use serde_json::json;
use tokio::sync::mpsc;

fn tool_output(call_id: &str, output: &str) -> InputItem {
    InputItem::FunctionCallOutput {
        call_id: call_id.into(),
        output: output.into(),
        error: false,
        media: Vec::new(),
    }
}

#[test]
fn microcompact_clears_old_large_outputs_keeps_recent_and_structure() {
    let big = "x".repeat(2048);
    let mut history = vec![
        InputItem::Message {
            role: "user".into(),
            content: vec![MessageContent::text("go")],
        },
        tool_output("c1", &big),   // old + large → cleared
        tool_output("c2", "tiny"), // old but small → kept
        tool_output("c3", &big),   // within keep_recent → kept
        tool_output("c4", &big),   // within keep_recent → kept
    ];

    let cleared = clear_stale_tool_outputs(&mut history, 2, 1024);

    assert_eq!(cleared, 1, "only the old large output is cleared");
    // Message untouched; item count unchanged (structure preserved).
    assert_eq!(history.len(), 5);
    assert!(matches!(history[0], InputItem::Message { .. }));
    match &history[1] {
        InputItem::FunctionCallOutput {
            output, call_id, ..
        } => {
            assert_eq!(output, CLEARED_TOOL_OUTPUT_MARKER);
            assert_eq!(call_id, "c1", "call_id (tool_use pairing) is preserved");
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
    match &history[2] {
        InputItem::FunctionCallOutput { output, .. } => assert_eq!(output, "tiny"),
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
    // The two most recent large outputs are kept verbatim.
    match &history[4] {
        InputItem::FunctionCallOutput { output, .. } => assert_eq!(output.len(), 2048),
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn emergency_shed_clears_small_old_outputs_keeps_two_recent() {
    // Emergency config: keep_recent=2, min_bytes=0 → even small old outputs go.
    let mut history = vec![
        tool_output("c1", "small-but-old"),
        tool_output("c2", "also-old"),
        tool_output("c3", "recent-1"),
        tool_output("c4", "recent-2"),
    ];
    let cleared = clear_stale_tool_outputs(&mut history, 2, 0);
    assert_eq!(cleared, 2, "both old outputs cleared regardless of size");
    assert_eq!(
        match &history[3] {
            InputItem::FunctionCallOutput { output, .. } => output.as_str(),
            _ => "",
        },
        "recent-2",
        "the two most recent outputs survive"
    );
}

#[test]
fn context_overflow_message_matches_provider_phrasings() {
    for m in [
        "OpenAI 400: This model's maximum context length is 128000 tokens",
        "prompt is too long: 250000 tokens > 200000",
        "input length and max_tokens exceed the context window",
        "context_length_exceeded",
    ] {
        assert!(is_context_overflow_message(m), "should match: {m}");
    }
    // Unrelated errors must NOT trigger auto-recovery.
    assert!(!is_context_overflow_message(
        "401 Unauthorized: invalid api key"
    ));
    assert!(!is_context_overflow_message("connection reset by peer"));
}

#[test]
fn microcompact_is_idempotent() {
    let big = "y".repeat(2048);
    let mut history = vec![tool_output("c1", &big), tool_output("c2", &big)];
    assert_eq!(clear_stale_tool_outputs(&mut history, 1, 1024), 1);
    // Second pass finds nothing new to clear (already a marker).
    assert_eq!(clear_stale_tool_outputs(&mut history, 1, 1024), 0);
}

#[test]
fn microcompact_seen_prefix_spares_unsent_batch() {
    // Regression: microcompaction passes only the already-seen prefix
    // (`history[..history_seen_len]`) to clear_stale_tool_outputs, so a large
    // just-produced tool batch beyond that prefix is never cleared before the
    // model has been shown those results.
    let big = "x".repeat(2048);
    let mut history = [
        tool_output("seen1", &big), // seen + old + large → cleared
        tool_output("seen2", &big), // seen, within keep_recent → kept
        // ---- unsent tail (the fresh batch) begins here ----
        tool_output("fresh1", &big),
        tool_output("fresh2", &big),
        tool_output("fresh3", &big),
    ];
    let seen = 2; // only the first two were sent to the model
    let cleared = clear_stale_tool_outputs(&mut history[..seen], 1, 1024);
    assert_eq!(cleared, 1, "only old SEEN outputs are cleared");
    // The fresh, never-sent batch is fully intact.
    for (i, id) in [(2, "fresh1"), (3, "fresh2"), (4, "fresh3")] {
        match &history[i] {
            InputItem::FunctionCallOutput {
                output, call_id, ..
            } => {
                assert_eq!(output.len(), 2048, "{id} must survive verbatim");
                assert_eq!(call_id, id);
            }
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn emit_usage_returns_none_without_usable_usage() {
    let (tx, _rx) = mpsc::channel(16);
    // No usage block (e.g. a Failed event) → None, so the caller leaves
    // last_input_tokens untouched instead of clobbering it with 0.
    assert!(emit_usage(&json!({}), &tx, 100_000).await.is_none());
    // Present but null (a Chat Completions stream with no usage chunk) → None.
    assert!(emit_usage(&json!({ "usage": null }), &tx, 100_000)
        .await
        .is_none());
    // A real usage block → occupancy = the cache-folded input total.
    let u = emit_usage(&json!({ "usage": { "input_tokens": 1234 } }), &tx, 100_000)
        .await
        .expect("usage present");
    assert_eq!(u.occupancy, 1234);
    assert_eq!(u.uncached_input, 1234);
    // Cache fields fold into occupancy but stay split for accurate costing.
    let u = emit_usage(
            &json!({ "usage": { "input_tokens": 100, "cache_read_input_tokens": 20, "cache_creation_input_tokens": 5, "output_tokens": 7 } }),
            &tx,
            100_000,
        )
        .await
        .expect("usage present");
    assert_eq!(u.occupancy, 125);
    assert_eq!(u.uncached_input, 100);
    assert_eq!(u.cache_read, 20);
    assert_eq!(u.cache_write, 5);
    assert_eq!(u.output, 7);
}

#[test]
fn record_cost_accumulates_per_model() {
    use super::{Agent, TurnUsage};
    use crate::auth::Credential;
    use crate::client::LlmClient;
    use crate::config::Config;
    use crate::provider::Provider;

    let client = LlmClient::new(Credential::ApiKey {
        provider: Provider::OpenAi,
        key: "sk-dummy".into(),
    })
    .unwrap();
    let mut agent = Agent::new(client, Config::default());

    let turn = TurnUsage {
        occupancy: 120,
        uncached_input: 100,
        cache_read: 15,
        cache_write: 5,
        output: 40,
    };
    // Two responses on the same model fold into one entry.
    agent.record_cost("claude-opus-4-8", &turn);
    agent.record_cost("claude-opus-4-8", &turn);
    // A different model opens a second entry, billed independently — this is
    // what makes a mid-session /model switch cost correctly.
    agent.record_cost("gpt-5", &turn);

    assert_eq!(agent.cost_usage.len(), 2);
    let opus = agent
        .cost_usage
        .iter()
        .find(|e| e.model == "claude-opus-4-8")
        .expect("opus entry");
    assert_eq!(opus.input_tokens, 200);
    assert_eq!(opus.output_tokens, 80);
    assert_eq!(opus.cache_read_tokens, 30);
    assert_eq!(opus.cache_write_tokens, 10);
    let gpt = agent
        .cost_usage
        .iter()
        .find(|e| e.model == "gpt-5")
        .expect("gpt entry");
    assert_eq!(gpt.input_tokens, 100);
}

#[test]
fn classify_input_tokens_handles_both_provider_shapes() {
    // OpenAI Responses: `input_tokens` is the TOTAL, cache hit nested. The
    // cached portion must split out so it bills at the cache-read rate.
    let openai = json!({
        "input_tokens": 1000,
        "output_tokens": 50,
        "input_tokens_details": { "cached_tokens": 800 }
    });
    assert_eq!(classify_input_tokens(&openai), (200, 800, 0));
    // Occupancy (sum) must still equal the total input.
    let (u, r, w) = classify_input_tokens(&openai);
    assert_eq!(u + r + w, 1000);

    // Anthropic: `input_tokens` EXCLUDES cache; classes are siblings.
    let anthropic = json!({
        "input_tokens": 200,
        "cache_read_input_tokens": 800,
        "cache_creation_input_tokens": 10
    });
    assert_eq!(classify_input_tokens(&anthropic), (200, 800, 10));

    // No cache reported anywhere.
    assert_eq!(
        classify_input_tokens(&json!({ "input_tokens": 200 })),
        (200, 0, 0)
    );
}

#[test]
fn compacted_history_is_single_orphan_free_user_message() {
    let h = compacted_history("a summary of the work");
    assert_eq!(h.len(), 1, "compaction must collapse to exactly one item");
    match &h[0] {
        InputItem::Message { role, content } => {
            assert_eq!(role, "user");
            assert_eq!(content.len(), 1);
            match &content[0] {
                MessageContent::InputText { text } => {
                    assert!(text.contains("a summary of the work"));
                }
                other => panic!("expected input_text, got {other:?}"),
            }
        }
        other => panic!("expected a Message, got {other:?}"),
    }
    // The point of full replacement: no tool-call pairing survives, so the
    // compacted history can never present an orphaned call/output (which
    // both Anthropic and OpenAI reject with a 4xx).
    assert!(!h.iter().any(|i| matches!(
        i,
        InputItem::FunctionCall { .. } | InputItem::FunctionCallOutput { .. }
    )));
}

#[test]
fn should_compact_respects_min_items() {
    assert!(!should_compact(0));
    assert!(!should_compact(COMPACT_MIN_ITEMS));
    assert!(should_compact(COMPACT_MIN_ITEMS + 1));
}
