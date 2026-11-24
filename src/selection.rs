//! Mouse text selection: positions, highlighting and copying to the clipboard.

use ratatui::{
    style::Color,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

const SELECTION_BG: Color = Color::Rgb(70, 90, 130);

/// A position in the log: absolute line (0-based, stable across buffer trimming)
/// and display column within the whole line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: TextPos,
    pub cursor: TextPos,
    /// The mouse moved after the button went down (a plain click selects nothing).
    pub dragged: bool,
    /// Following is paused while selecting, so new lines can't move the text.
    pub was_following: bool,
    /// Latest drag position (terminal cell), resolved against the next layout.
    pub pending: Option<(u16, u16)>,
}

impl Selection {
    pub fn new(at: TextPos, was_following: bool) -> Self {
        Self { anchor: at, cursor: at, dragged: false, was_following, pending: None }
    }

    /// Start and end in reading order; both ends are inclusive.
    pub fn range(&self) -> (TextPos, TextPos) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// Columns `[from, to)` selected on absolute line `line`, if any.
    pub fn columns(&self, line: usize) -> Option<(usize, usize)> {
        if !self.dragged {
            return None;
        }
        let (start, end) = self.range();
        if line < start.line || line > end.line {
            return None;
        }
        let from = if line == start.line { start.col } else { 0 };
        let to = if line == end.line { end.col.saturating_add(1) } else { usize::MAX };
        Some((from, to))
    }
}

/// Whether a character at `col` of width `w` overlaps columns `[from, to)`.
fn overlaps(col: usize, w: usize, from: usize, to: usize) -> bool {
    col < to && col + w.max(1) > from
}

/// The characters of `text` within display columns `[from, to)`.
pub fn slice_columns(text: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    for c in text.chars() {
        if col >= to {
            break;
        }
        let w = c.width().unwrap_or(0);
        if overlaps(col, w, from, to) {
            out.push(c);
        }
        col += w;
    }
    out
}

/// Gives columns `[from, to)` of a screen row a selection background.
pub fn highlight_columns(line: Line<'static>, from: usize, to: usize) -> Line<'static> {
    if from >= to {
        return line;
    }
    let mut spans = Vec::with_capacity(line.spans.len() + 2);
    let mut col = 0;
    for span in &line.spans {
        let mut buf = String::new();
        let mut buf_selected = false;
        for c in span.content.chars() {
            let w = c.width().unwrap_or(0);
            let inside = overlaps(col, w, from, to);
            if inside != buf_selected && !buf.is_empty() {
                spans.push(styled(std::mem::take(&mut buf), span, buf_selected));
            }
            buf_selected = inside;
            buf.push(c);
            col += w;
        }
        if !buf.is_empty() {
            spans.push(styled(buf, span, buf_selected));
        }
    }
    Line::from(spans).style(line.style)
}

fn styled(text: String, span: &Span<'static>, selected: bool) -> Span<'static> {
    let style = if selected { span.style.bg(SELECTION_BG) } else { span.style };
    Span::styled(text, style)
}

/// The OSC 52 sequence that puts `text` on the system clipboard (supported by most
/// modern terminals, and by tmux with `set-clipboard on`).
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: usize, col: usize) -> TextPos {
        TextPos { line, col }
    }

    #[test]
    fn test_columns_in_reading_order() {
        let mut sel = Selection::new(pos(5, 3), false);
        sel.cursor = pos(3, 2);
        sel.dragged = true;
        assert_eq!(sel.columns(2), None);
        assert_eq!(sel.columns(3), Some((2, usize::MAX)));
        assert_eq!(sel.columns(4), Some((0, usize::MAX)));
        assert_eq!(sel.columns(5), Some((0, 4)));
        assert_eq!(sel.columns(6), None);
        // A plain click selects nothing.
        assert_eq!(Selection::new(pos(1, 1), false).columns(1), None);
    }

    #[test]
    fn test_slice_columns_by_display_width() {
        assert_eq!(slice_columns("ab日本cd", 0, 3), "ab日");
        assert_eq!(slice_columns("ab日本cd", 3, 5), "日本");
        assert_eq!(slice_columns("ab日本cd", 6, usize::MAX), "cd");
        assert_eq!(slice_columns("abc", 5, 9), "");
    }

    #[test]
    fn test_highlight_columns_keeps_text() {
        let line = highlight_columns(Line::raw("hello world"), 2, 7);
        let text: Vec<&str> = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, vec!["he", "llo w", "orld"]);
        assert_eq!(line.spans[1].style.bg, Some(SELECTION_BG));
        assert_eq!(line.spans[0].style.bg, None);
    }

    #[test]
    fn test_base64_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("héllo\n".as_bytes()), "aMOpbGxvCg==");
    }
}
