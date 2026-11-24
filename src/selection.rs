//! Mouse text selection: positions, highlighting and copying to the clipboard.

use ratatui::{
    style::{Color, Modifier},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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
        Self {
            anchor: at,
            cursor: at,
            dragged: false,
            was_following,
            pending: None,
        }
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
        let to = if line == end.line {
            end.col.saturating_add(1)
        } else {
            usize::MAX
        };
        Some((from, to))
    }
}

/// Whether a character at `col` of width `w` overlaps columns `[from, to)`.
fn overlaps(col: usize, w: usize, from: usize, to: usize) -> bool {
    col < to && col + w.max(1) > from
}

/// The characters of `text` within display columns `[from, to)`, measured per grapheme
/// cluster the way ratatui draws them.
pub fn slice_columns(text: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    for g in text.graphemes(true) {
        if col >= to {
            break;
        }
        let w = g.width();
        if overlaps(col, w, from, to) {
            out.push_str(g);
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
        for g in span.content.graphemes(true) {
            let w = g.width();
            let inside = overlaps(col, w, from, to);
            if inside != buf_selected && !buf.is_empty() {
                spans.push(styled(std::mem::take(&mut buf), span, buf_selected));
            }
            buf_selected = inside;
            buf.push_str(g);
            col += w;
        }
        if !buf.is_empty() {
            spans.push(styled(buf, span, buf_selected));
        }
    }
    Line::from(spans).style(line.style)
}

fn styled(text: String, span: &Span<'static>, selected: bool) -> Span<'static> {
    // Reverse video is dropped, or it would swap the selection background into the
    // text color (log lines can use it, e.g. `less`-style highlights)
    let style = if selected {
        span.style
            .remove_modifier(Modifier::REVERSED)
            .bg(SELECTION_BG)
    } else {
        span.style
    };
    Span::styled(text, style)
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
    fn test_highlight_columns_drops_reverse_video() {
        let reversed = Line::from(Span::styled("abc", ratatui::style::Style::new().reversed()));
        let line = highlight_columns(reversed, 0, 1);
        assert!(
            line.spans[0]
                .style
                .sub_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            line.spans[1]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }
}
