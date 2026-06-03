//! Streaming tool-call accumulation, argument capping, and usage
//! normalization for the Chat Completions bridge. Split out of `chat`.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::openai::stream::ResponseStreamEvent;
use crate::tool_args::accumulate_argument_fragment;

/// Normalize Chat Completions usage (`prompt_tokens`/`completion_tokens`) into
/// the `input_tokens`/`output_tokens` shape the agent's accounting expects.
pub(super) fn normalize_usage(usage: &Value) -> Value {
    let get = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    json!({
        "input_tokens": get("prompt_tokens"),
        "output_tokens": get("completion_tokens"),
        "total_tokens": get("total_tokens"),
    })
}

pub(super) struct ToolAcc {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) args: String,
    pub(super) args_truncated: bool,
    pub(super) added: bool,
    pub(super) emitted_args_len: usize,
}

const CHAT_TOOL_ARGUMENT_MAX_BYTES: usize = 2 * 1024 * 1024;
const CHAT_TOOL_ARGUMENT_BUFFER_BYTES: usize = CHAT_TOOL_ARGUMENT_MAX_BYTES + 1;

/// Cap on distinct tool calls accumulated during one response. A real response
/// has a handful; the cap stops a malformed stream of unique `index` values
/// from growing the accumulator map without bound.
const MAX_PENDING_TOOL_CALLS: usize = 512;

pub(super) async fn apply_tool_delta(
    tools: &mut BTreeMap<u32, ToolAcc>,
    index: u32,
    id: Option<String>,
    name: Option<String>,
    args_fragment: Option<String>,
    tx: &mpsc::Sender<anyhow::Result<ResponseStreamEvent>>,
) {
    // A real response has a handful of tool calls; refuse to accumulate
    // unboundedly many distinct indices from a malformed stream.
    if !tools.contains_key(&index) && tools.len() >= MAX_PENDING_TOOL_CALLS {
        return;
    }
    let acc = tools.entry(index).or_insert_with(|| ToolAcc {
        id: id.clone().unwrap_or_else(|| format!("call_{index}")),
        name: None,
        args: String::new(),
        args_truncated: false,
        added: false,
        emitted_args_len: 0,
    });
    if !acc.added {
        if let Some(id) = id {
            acc.id = id;
        }
    }
    if acc.name.is_none() {
        acc.name = name;
    }
    if let Some(frag) = args_fragment {
        append_tool_args_capped(acc, &frag);
    }
    if !acc.added {
        if let Some(name) = acc.name.as_deref() {
            let item = json!({
                "type": "function_call",
                "id": acc.id.clone(),
                "call_id": acc.id.clone(),
                "name": name,
                "arguments": "",
            });
            let _ = tx
                .send(Ok(ResponseStreamEvent::OutputItemAdded {
                    item,
                    output_index: index,
                }))
                .await;
            acc.added = true;
        }
    }
    if acc.added && acc.args.len() > acc.emitted_args_len {
        let delta = acc.args[acc.emitted_args_len..].to_string();
        acc.emitted_args_len = acc.args.len();
        let _ = tx
            .send(Ok(ResponseStreamEvent::FunctionCallArgsDelta {
                item_id: acc.id.clone(),
                delta,
            }))
            .await;
    }
}

fn append_tool_args_capped(acc: &mut ToolAcc, fragment: &str) {
    // Drop a leading empty-args placeholder, keep mid-object fragments verbatim
    // (so a bare `"limit": null` survives) — shared rule, see
    // `accumulate_argument_fragment`.
    let Some(fragment) = accumulate_argument_fragment(acc.args.is_empty(), fragment) else {
        return;
    };
    if acc.args_truncated {
        return;
    }
    let remaining = CHAT_TOOL_ARGUMENT_BUFFER_BYTES.saturating_sub(acc.args.len());
    if remaining == 0 {
        acc.args_truncated = true;
        return;
    }
    if fragment.len() <= remaining {
        acc.args.push_str(fragment);
        return;
    }
    let mut cut = remaining;
    while cut > 0 && !fragment.is_char_boundary(cut) {
        cut -= 1;
    }
    if cut == 0 {
        acc.args.push_str(&" ".repeat(remaining));
    } else {
        acc.args.push_str(&fragment[..cut]);
    }
    acc.args_truncated = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn apply_tool_delta_bounds_distinct_indices() {
        let (tx, _rx) = mpsc::channel::<anyhow::Result<ResponseStreamEvent>>(1 << 16);
        let mut tools: BTreeMap<u32, ToolAcc> = BTreeMap::new();
        // Feed far more distinct tool-call indices than the cap allows.
        for i in 0..(MAX_PENDING_TOOL_CALLS as u32 + 100) {
            apply_tool_delta(
                &mut tools,
                i,
                Some(format!("call_{i}")),
                Some("read_file".into()),
                Some("{}".into()),
                &tx,
            )
            .await;
        }
        assert_eq!(tools.len(), MAX_PENDING_TOOL_CALLS);
    }

    #[tokio::test]
    async fn chat_tool_argument_buffer_is_capped_before_agent() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut tools = BTreeMap::new();

        apply_tool_delta(
            &mut tools,
            0,
            Some("call_x".to_string()),
            Some("read_file".to_string()),
            Some("x".repeat(CHAT_TOOL_ARGUMENT_BUFFER_BYTES + 8192)),
            &tx,
        )
        .await;

        let acc = tools.get(&0).unwrap();
        assert_eq!(acc.args.len(), CHAT_TOOL_ARGUMENT_BUFFER_BYTES);
        assert!(acc.args_truncated);

        let _started = rx.recv().await.unwrap().unwrap();
        match rx.recv().await.unwrap().unwrap() {
            ResponseStreamEvent::FunctionCallArgsDelta { delta, .. } => {
                assert_eq!(delta.len(), CHAT_TOOL_ARGUMENT_BUFFER_BYTES);
            }
            other => panic!("expected FunctionCallArgsDelta, got {other:?}"),
        }
    }

    #[test]
    fn chat_tool_argument_cap_preserves_utf8_boundary() {
        let mut acc = ToolAcc {
            id: "call_x".into(),
            name: Some("read_file".into()),
            args: "x".repeat(CHAT_TOOL_ARGUMENT_BUFFER_BYTES - 1),
            args_truncated: false,
            added: true,
            emitted_args_len: 0,
        };

        append_tool_args_capped(&mut acc, "é");

        assert_eq!(acc.args.len(), CHAT_TOOL_ARGUMENT_BUFFER_BYTES);
        assert!(acc.args.is_char_boundary(acc.args.len()));
        assert!(acc.args_truncated);
    }

    fn empty_acc() -> ToolAcc {
        ToolAcc {
            id: "call_x".into(),
            name: Some("read_file".into()),
            args: String::new(),
            args_truncated: false,
            added: true,
            emitted_args_len: 0,
        }
    }

    #[test]
    fn append_tool_args_keeps_bare_null_value_mid_stream() {
        // Regression: a streamed `"limit": null` whose `null` arrives as its own
        // delta chunk must be kept, not dropped as an "empty placeholder", or the
        // accumulated Chat Completions args become invalid JSON.
        let mut acc = empty_acc();
        for frag in [r#"{"path":"a.py","limit":"#, "null", r#","offset":null}"#] {
            append_tool_args_capped(&mut acc, frag);
        }
        assert_eq!(acc.args, r#"{"path":"a.py","limit":null,"offset":null}"#);
        let v: serde_json::Value = serde_json::from_str(&acc.args).unwrap();
        assert!(v["limit"].is_null());
        assert_eq!(v["path"], "a.py");
    }

    #[test]
    fn append_tool_args_drops_only_leading_placeholder() {
        // A leading `{}` placeholder is dropped; the real object that follows is
        // kept verbatim (a provider that prefixes args with an empty object).
        let mut acc = empty_acc();
        append_tool_args_capped(&mut acc, "{}");
        append_tool_args_capped(&mut acc, r#"{"path":"a.py"}"#);
        assert_eq!(acc.args, r#"{"path":"a.py"}"#);
    }
}
