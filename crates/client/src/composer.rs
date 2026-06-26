//! The client-side composer line buffer: a single-line editor with history.
//! Pure logic — no I/O. Cursor positions are char indices (Unicode-correct).

#[derive(Default)]
pub struct ComposerBuffer {
    chars: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Index into history while browsing (None = editing the live line).
    history_pos: Option<usize>,
}

impl ComposerBuffer {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Cursor position as a char index in `[0, len]`.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Take the current line: if non-empty, push it to history, clear the
    /// buffer, and return the line. Empty lines return None.
    pub fn take_line(&mut self) -> Option<String> {
        if self.chars.is_empty() {
            return None;
        }
        let line: String = self.chars.iter().collect();
        self.history.push(line.clone());
        self.chars.clear();
        self.cursor = 0;
        self.history_pos = None;
        Some(line)
    }

    /// Step back to an older history entry, replacing the current line.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_pos {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_pos = Some(next);
        self.set_from_history();
    }

    /// Step forward toward the live (empty) line.
    pub fn history_next(&mut self) {
        match self.history_pos {
            Some(p) if p + 1 < self.history.len() => {
                self.history_pos = Some(p + 1);
                self.set_from_history();
            }
            Some(_) => {
                self.history_pos = None;
                self.chars.clear();
                self.cursor = 0;
            }
            None => {}
        }
    }

    fn set_from_history(&mut self) {
        if let Some(p) = self.history_pos {
            self.chars = self.history[p].chars().collect();
            self.cursor = self.chars.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_edit() {
        let mut b = ComposerBuffer::default();
        for c in "helo".chars() { b.insert(c); }
        assert_eq!(b.text(), "helo");
        assert_eq!(b.cursor(), 4);
        b.left();
        b.insert('l');
        assert_eq!(b.text(), "hello");
        b.home();
        b.delete();
        assert_eq!(b.text(), "ello");
        b.end();
        b.backspace();
        assert_eq!(b.text(), "ell");
    }

    #[test]
    fn cursor_bounds() {
        let mut b = ComposerBuffer::default();
        b.left();
        b.backspace();
        assert_eq!(b.text(), "");
        b.insert('a');
        b.right();
        assert_eq!(b.cursor(), 1);
    }

    #[test]
    fn take_line_clears_and_records_history() {
        let mut b = ComposerBuffer::default();
        assert_eq!(b.take_line(), None);
        for c in "ls -l".chars() { b.insert(c); }
        assert_eq!(b.take_line(), Some("ls -l".to_string()));
        assert_eq!(b.text(), "");
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn history_navigation() {
        let mut b = ComposerBuffer::default();
        for c in "one".chars() { b.insert(c); }
        b.take_line();
        for c in "two".chars() { b.insert(c); }
        b.take_line();

        b.history_prev();
        assert_eq!(b.text(), "two");
        b.history_prev();
        assert_eq!(b.text(), "one");
        b.history_prev();
        assert_eq!(b.text(), "one");
        b.history_next();
        assert_eq!(b.text(), "two");
        b.history_next();
        assert_eq!(b.text(), "");
    }
}
