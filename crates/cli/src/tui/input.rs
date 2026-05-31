use unicode_width::UnicodeWidthChar;

/// Multi-line text input with a single cursor.
/// Supports basic editing primitives suitable for a CLI composer.
#[derive(Debug, Default)]
pub struct TextInput {
    pub buffer: String,
    pub cursor: usize, // byte offset in `buffer`
}

impl TextInput {
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    pub fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let before = &self.buffer[..self.cursor];
        let prev = before.chars().next_back();
        if let Some(c) = prev {
            let new_cursor = self.cursor - c.len_utf8();
            self.buffer.replace_range(new_cursor..self.cursor, "");
            self.cursor = new_cursor;
        }
    }

    pub fn delete_word_left(&mut self) {
        let before = &self.buffer[..self.cursor];
        let mut new_cursor = self.cursor;
        let mut in_ws = true;
        for (i, c) in before.char_indices().rev() {
            if in_ws && c.is_whitespace() {
                new_cursor = i;
                continue;
            }
            if c.is_whitespace() {
                break;
            }
            in_ws = false;
            new_cursor = i;
        }
        if new_cursor != self.cursor {
            self.buffer.replace_range(new_cursor..self.cursor, "");
            self.cursor = new_cursor;
        }
    }

    /// Jump the cursor to the very start of the message (Ctrl+A). Distinct from
    /// `move_home`, which stops at the start of the current line.
    pub fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    /// Delete from the cursor to the end of the current line (Ctrl+K). When the
    /// cursor already sits at the line end, consume the trailing newline instead
    /// so repeated presses remove the message one line at a time rather than
    /// clearing everything at once.
    pub fn kill_to_line_end(&mut self) {
        let rest = &self.buffer[self.cursor..];
        let end = match rest.find('\n') {
            Some(0) => 1,       // at the line end: drop the newline
            Some(i) => i,       // up to (not including) the newline
            None => rest.len(), // last line: to the end of the buffer
        };
        if end > 0 {
            self.buffer
                .replace_range(self.cursor..self.cursor + end, "");
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let before = &self.buffer[..self.cursor];
        if let Some(c) = before.chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        let after = &self.buffer[self.cursor..];
        if let Some(c) = after.chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn move_home(&mut self) {
        let before = &self.buffer[..self.cursor];
        if let Some(idx) = before.rfind('\n') {
            self.cursor = idx + 1;
        } else {
            self.cursor = 0;
        }
    }

    pub fn move_end(&mut self) {
        let after = &self.buffer[self.cursor..];
        if let Some(idx) = after.find('\n') {
            self.cursor += idx;
        } else {
            self.cursor = self.buffer.len();
        }
    }

    pub fn move_up(&mut self) {
        let before = &self.buffer[..self.cursor];
        let Some(prev_nl) = before.rfind('\n') else {
            self.cursor = 0;
            return;
        };
        let col = display_width(&before[prev_nl + 1..]);
        let prev_line_end = prev_nl;
        let prev_line_start = before[..prev_nl].rfind('\n').map(|i| i + 1).unwrap_or(0);
        self.cursor =
            col_to_byte(&self.buffer[prev_line_start..prev_line_end], col) + prev_line_start;
    }

    pub fn move_down(&mut self) {
        let after = &self.buffer[self.cursor..];
        let Some(nl) = after.find('\n') else {
            self.cursor = self.buffer.len();
            return;
        };
        let line_start_byte = self.cursor + nl + 1;
        let before = &self.buffer[..self.cursor];
        let cur_line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = display_width(&self.buffer[cur_line_start..self.cursor]);
        let next_line_end = self.buffer[line_start_byte..]
            .find('\n')
            .map(|i| line_start_byte + i)
            .unwrap_or(self.buffer.len());
        self.cursor =
            line_start_byte + col_to_byte(&self.buffer[line_start_byte..next_line_end], col);
    }

    /// Returns (current_line_index, current_col_display_width)
    pub fn cursor_pos(&self) -> (usize, usize) {
        let before = &self.buffer[..self.cursor];
        let line_idx = before.matches('\n').count();
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = display_width(&before[line_start..]);
        (line_idx, col)
    }

    pub fn lines(&self) -> Vec<&str> {
        if self.buffer.is_empty() {
            vec![""]
        } else {
            self.buffer.split('\n').collect()
        }
    }

    pub fn take(&mut self) -> String {
        let out = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        out
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Replace the buffer with `s` and place the cursor at the end. Used by the
    /// input-history recall (Up/Down through previously submitted prompts).
    pub fn set_text(&mut self, s: String) {
        self.buffer = s;
        self.cursor = self.buffer.len();
    }

    /// Number of newline-separated lines in the buffer (always >= 1).
    pub fn line_count(&self) -> usize {
        self.buffer.matches('\n').count() + 1
    }
}

fn display_width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

fn col_to_byte(line: &str, target_col: usize) -> usize {
    let mut col = 0usize;
    for (i, c) in line.char_indices() {
        let w = c.width().unwrap_or(0);
        if col + w > target_col {
            return i;
        }
        col += w;
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::TextInput;

    fn at(buffer: &str, cursor: usize) -> TextInput {
        TextInput {
            buffer: buffer.to_string(),
            cursor,
        }
    }

    #[test]
    fn move_to_start_jumps_past_lines() {
        let mut i = at("ab\ncd", 5);
        i.move_to_start();
        assert_eq!(i.cursor, 0);
    }

    #[test]
    fn kill_to_line_end_clears_only_the_line_then_the_newline() {
        // First press kills the line content but keeps the newline…
        let mut i = at("hello\nworld", 0);
        i.kill_to_line_end();
        assert_eq!(i.buffer, "\nworld");
        assert_eq!(i.cursor, 0);
        // …a second press removes the now-empty line — line by line, not all.
        i.kill_to_line_end();
        assert_eq!(i.buffer, "world");
    }

    #[test]
    fn kill_to_line_end_from_mid_line() {
        let mut i = at("abc", 1);
        i.kill_to_line_end();
        assert_eq!(i.buffer, "a");
        assert_eq!(i.cursor, 1);
    }

    #[test]
    fn kill_to_line_end_at_buffer_end_is_noop() {
        let mut i = at("abc", 3);
        i.kill_to_line_end();
        assert_eq!(i.buffer, "abc");
    }
}
