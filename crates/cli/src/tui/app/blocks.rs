//! Assistant/tool block manipulation helpers. Split out of `app`; logic unchanged.

use super::*;

pub fn collapse_reasoning_into_thought(app: &mut App) {
    if let Some(Block::Assistant {
        reasoning,
        reasoning_started_at,
        thought_for_secs,
        ..
    }) = last_assistant_mut_open(&mut app.blocks)
    {
        if thought_for_secs.is_none() && !reasoning.is_empty() {
            let secs = reasoning_started_at
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            *thought_for_secs = Some(secs);
            reasoning.clear();
        }
    }
}

pub fn last_assistant_mut_open(blocks: &mut [Block]) -> Option<&mut Block> {
    blocks
        .iter_mut()
        .rev()
        .find(|b| matches!(b, Block::Assistant { done: false, .. }))
}

pub fn find_tool_mut<'a>(blocks: &'a mut [Block], call_id: &str) -> Option<&'a mut Block> {
    blocks
        .iter_mut()
        .rev()
        .find(|b| matches!(b, Block::Tool { call_id: c, .. } if c == call_id))
}

pub fn finish_open_assistant_block(blocks: &mut Vec<Block>) {
    let Some(i) = blocks
        .iter()
        .rposition(|b| matches!(b, Block::Assistant { done: false, .. }))
    else {
        return;
    };
    let should_remove = matches!(
        &blocks[i],
        Block::Assistant {
            text,
            reasoning,
            thought_for_secs,
            ..
        } if text.is_empty() && reasoning.is_empty() && thought_for_secs.is_none()
    );
    if should_remove {
        blocks.remove(i);
    } else if let Block::Assistant { done, .. } = &mut blocks[i] {
        *done = true;
    }
}

/// Maintain the invariant "at most one open assistant block" by closing any
/// still-open blocks (and dropping empty ones), then pushing a fresh open
/// block. Used after a tool result so subsequent reasoning/text appears below
/// the tool, without leaving stale empties behind.
pub fn rotate_assistant_block(blocks: &mut Vec<Block>) {
    let mut i = 0;
    while i < blocks.len() {
        if let Block::Assistant {
            done,
            text,
            reasoning,
            thought_for_secs,
            ..
        } = &mut blocks[i]
        {
            if !*done && text.is_empty() && reasoning.is_empty() && thought_for_secs.is_none() {
                blocks.remove(i);
                continue;
            }
            *done = true;
        }
        i += 1;
    }
    blocks.push(Block::Assistant {
        text: String::new(),
        reasoning: String::new(),
        done: false,
        thought_for_secs: None,
        reasoning_started_at: None,
    });
}
