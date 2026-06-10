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
            // Keep the reasoning text (don't clear it): the collapsed "Thought
            // for Xs" line is click-to-expand, and re-showing the thought needs
            // the original text. The render suppresses it while collapsed unless
            // `thinking_expanded` is set, so retaining it costs no extra rows.
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
        thinking_expanded: false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(text: &str) -> Block {
        Block::Assistant {
            text: text.to_string(),
            reasoning: String::new(),
            done: false,
            thought_for_secs: None,
            reasoning_started_at: None,
            thinking_expanded: false,
        }
    }

    fn is_done(b: &Block) -> bool {
        matches!(b, Block::Assistant { done: true, .. })
    }

    #[test]
    fn last_assistant_mut_open_finds_only_an_open_block() {
        let mut blocks = vec![open("done"), open("live")];
        if let Some(Block::Assistant { done, .. }) = blocks.first_mut() {
            *done = true; // close the first
        }
        match last_assistant_mut_open(&mut blocks) {
            Some(Block::Assistant { text, .. }) => assert_eq!(text, "live"),
            _ => panic!("expected the open block"),
        }
    }

    #[test]
    fn finish_open_drops_an_empty_block_but_closes_a_nonempty_one() {
        // A non-empty open block is closed in place.
        let mut blocks = vec![open("hi")];
        finish_open_assistant_block(&mut blocks);
        assert_eq!(blocks.len(), 1);
        assert!(is_done(&blocks[0]));
        // An empty open block is removed (no stale empty stanza left behind).
        let mut empty = vec![open("")];
        finish_open_assistant_block(&mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn rotate_closes_all_open_drops_empties_and_pushes_one_fresh() {
        let mut blocks = vec![open("a"), open("")];
        rotate_assistant_block(&mut blocks);
        // "a" closed, the empty dropped, exactly one fresh open block appended.
        assert_eq!(blocks.len(), 2);
        assert!(is_done(&blocks[0]));
        assert!(matches!(&blocks[0], Block::Assistant { text, .. } if text == "a"));
        assert!(matches!(blocks[1], Block::Assistant { done: false, .. }));
    }
}
