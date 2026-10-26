/// Single-line text editor used by the search / filter / goto prompts.
#[derive(Debug, Default)]
pub struct LineEditor {
    text: String,
    /// Cursor position as a byte offset into `text` (always on a char boundary).
    cursor: usize,
    /// Position while browsing history (`None` = editing a fresh entry).
    history_pos: Option<usize>,
}

impl LineEditor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_pos = None;
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.text.drain(self.cursor..next);
        }
    }

    pub fn left(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.cursor = prev;
        }
    }

    pub fn right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Ctrl+U: delete everything before the cursor.
    pub fn kill_to_start(&mut self) {
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Ctrl+W: delete the word before the cursor.
    pub fn delete_word(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end();
        let start = trimmed
            .rfind(char::is_whitespace)
            .map(|i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8))
            .unwrap_or(0);
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn history_prev(&mut self, history: &[String]) {
        if history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            Some(p) => p.saturating_sub(1),
            None => history.len() - 1,
        };
        self.history_pos = Some(pos);
        self.set_text(&history[pos]);
    }

    pub fn history_next(&mut self, history: &[String]) {
        match self.history_pos {
            Some(p) if p + 1 < history.len() => {
                self.history_pos = Some(p + 1);
                self.set_text(&history[p + 1]);
            }
            Some(_) => {
                self.history_pos = None;
                self.set_text("");
            }
            None => {}
        }
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor].char_indices().next_back().map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }
}

/// Append `entry` to `history`, moving it to the end if already present.
pub fn push_history(history: &mut Vec<String>, entry: &str) {
    if entry.is_empty() {
        return;
    }
    history.retain(|h| h != entry);
    history.push(entry.to_string());
    const MAX_HISTORY: usize = 100;
    if history.len() > MAX_HISTORY {
        history.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> LineEditor {
        let mut e = LineEditor::default();
        e.set_text(text);
        e
    }

    #[test]
    fn test_cursor_editing_with_multibyte_chars() {
        let mut e = editor("日本");
        e.left();
        e.insert('x');
        assert_eq!(e.text(), "日x本");
        e.backspace();
        e.backspace();
        assert_eq!(e.text(), "本");
        e.home();
        e.delete();
        assert_eq!(e.text(), "");
    }

    #[test]
    fn test_kill_and_delete_word() {
        let mut e = editor("foo bar baz");
        e.delete_word();
        assert_eq!(e.text(), "foo bar ");
        e.delete_word();
        assert_eq!(e.text(), "foo ");
        e.kill_to_start();
        assert_eq!(e.text(), "");
    }

    #[test]
    fn test_history_navigation() {
        let mut history = Vec::new();
        push_history(&mut history, "a");
        push_history(&mut history, "b");
        push_history(&mut history, "a");
        assert_eq!(history, vec!["b", "a"]);

        let mut e = LineEditor::default();
        e.history_prev(&history);
        assert_eq!(e.text(), "a");
        e.history_prev(&history);
        assert_eq!(e.text(), "b");
        e.history_next(&history);
        assert_eq!(e.text(), "a");
        e.history_next(&history);
        assert_eq!(e.text(), "");
    }
}
