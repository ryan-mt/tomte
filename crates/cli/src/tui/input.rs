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
        self.cursor = col_to_byte(&self.buffer[prev_line_start..prev_line_end], col) + prev_line_start;
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
