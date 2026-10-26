use std::sync::LazyLock;
use regex::Regex;
use ansi_to_tui::IntoText;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

const TAB_SIZE: usize = 4;

static ANSI_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").expect("valid regex")
});

/// Strip ANSI escape codes from a string for plain-text searching/filtering.
pub fn strip_ansi(s: &str) -> String {
    ANSI_REGEX.replace_all(s, "").to_string()
}

/// Expand tab characters to the next multiple of `tab_size` display columns.
#[cfg(test)]
pub fn expand_tabs(s: &str, tab_size: usize) -> String {
    let mut col = 0;
    expand_tabs_from(s, tab_size, &mut col)
}

/// Like `expand_tabs`, but starting at display column `*col` and advancing it.
fn expand_tabs_from(s: &str, tab_size: usize, col: &mut usize) -> String {
    let mut result = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        if ch == '\t' {
            let spaces = tab_size - (*col % tab_size);
            result.extend(std::iter::repeat_n(' ', spaces));
            *col += spaces;
        } else {
            result.push(ch);
            *col += ch.width().unwrap_or(0);
        }
    }
    result
}

/// Parse a log line into a Ratatui `Line`, preserving ANSI colors if present.
/// Tabs are expanded after parsing so escape sequences don't affect tab stops.
pub fn parse_ansi_line(s: &str) -> Line<'static> {
    let line = if !s.contains('\x1b') {
        Line::raw(s.to_string())
    } else {
        match s.into_text() {
            Ok(text) => text.lines.into_iter().next().unwrap_or_default(),
            Err(_) => Line::raw(strip_ansi(s)),
        }
    };

    if !line.spans.iter().any(|span| span.content.contains('\t')) {
        return line;
    }
    let mut col = 0;
    let spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| Span::styled(expand_tabs_from(&span.content, TAB_SIZE, &mut col), span.style))
        .collect();
    Line::from(spans).style(line.style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi() {
        let input = "\x1b[31mError:\x1b[0m Connection \x1b[1;32mfailed\x1b[0m";
        assert_eq!(strip_ansi(input), "Error: Connection failed");
    }

    #[test]
    fn test_expand_tabs() {
        let input = "a\tb";
        assert_eq!(expand_tabs(input, 4), "a   b");
    }

    #[test]
    fn test_parse_ansi_line() {
        let line = parse_ansi_line("\x1b[32mOK\x1b[0m");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "OK");
    }

    #[test]
    fn test_tabs_expand_by_display_column_across_ansi_spans() {
        // The escape codes must not count toward the tab stop.
        let line = parse_ansi_line("\x1b[31mab\x1b[0m\tc");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "ab  c");
        // Wide characters take two columns.
        assert_eq!(expand_tabs("日\tx", 4), "日  x");
    }
}
