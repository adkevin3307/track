//! Turning log lines into screen rows: match highlighting, horizontal slicing
//! and soft wrapping, all measured in terminal display columns.

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

use crate::matcher::Matcher;

fn highlight_style() -> Style {
    Style::default().bg(Color::Yellow).fg(Color::Black)
}

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

/// Highlight every match of `matcher` in `line`, preserving the existing span styles.
///
/// Matching runs on the concatenated span text, so match offsets always line up with
/// the rendered text (including ANSI-colored lines and matches that cross spans).
pub fn highlight_line(line: Line<'static>, matcher: &Matcher) -> Line<'static> {
    let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let ranges = matcher.find_ranges(&plain);
    if ranges.is_empty() {
        return line;
    }

    let mut spans = Vec::with_capacity(line.spans.len() + ranges.len() * 2);
    let mut range_idx = 0;
    let mut span_start = 0;
    for span in &line.spans {
        let text = span.content.as_ref();
        let span_end = span_start + text.len();
        let mut pos = span_start;
        while pos < span_end {
            while range_idx < ranges.len() && ranges[range_idx].end <= pos {
                range_idx += 1;
            }
            let (seg_end, highlighted) = match ranges.get(range_idx) {
                Some(r) if r.start <= pos => (r.end.min(span_end), true),
                Some(r) => (r.start.min(span_end), false),
                None => (span_end, false),
            };
            let style = if highlighted {
                span.style.patch(highlight_style())
            } else {
                span.style
            };
            spans.push(Span::styled(
                text[pos - span_start..seg_end - span_start].to_string(),
                style,
            ));
            pos = seg_end;
        }
        span_start = span_end;
    }
    Line::from(spans).style(line.style)
}

/// Columns `[skip, skip + width)` of `line`. A wide character cut by the left edge
/// is replaced by spaces; one cut by the right edge is dropped.
pub fn slice_line(line: &Line<'static>, skip: usize, width: usize) -> Line<'static> {
    let end = skip.saturating_add(width);
    let mut col = 0;
    let mut spans = Vec::new();
    for span in &line.spans {
        if col >= end {
            break;
        }
        let mut buf = String::new();
        for ch in span.content.chars() {
            if col >= end {
                break;
            }
            let w = char_width(ch);
            if col >= skip && col + w <= end {
                buf.push(ch);
            } else if col < skip && col + w > skip {
                buf.extend(std::iter::repeat_n(' ', (col + w).min(end) - skip));
            }
            col += w;
        }
        if !buf.is_empty() {
            spans.push(Span::styled(buf, span.style));
        }
    }
    Line::from(spans).style(line.style)
}

/// Break `line` into rows of at most `width` columns (character wrap, which suits
/// logs better than word wrap). Always returns at least one row.
pub fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut col = 0;
    for span in &line.spans {
        let mut buf = String::new();
        for ch in span.content.chars() {
            let w = char_width(ch);
            if col > 0 && col + w > width {
                if !buf.is_empty() {
                    row.push(Span::styled(std::mem::take(&mut buf), span.style));
                }
                rows.push(Line::from(std::mem::take(&mut row)).style(line.style));
                col = 0;
            }
            buf.push(ch);
            col += w;
        }
        if !buf.is_empty() {
            row.push(Span::styled(buf, span.style));
        }
    }
    rows.push(Line::from(row).style(line.style));
    rows
}

/// Number of rows `wrap_line(line, width)` produces, without allocating them.
pub fn wrapped_row_count(line: &Line<'static>, width: usize) -> usize {
    let width = width.max(1);
    let mut rows = 1;
    let mut col = 0;
    for ch in line.spans.iter().flat_map(|s| s.content.chars()) {
        let w = char_width(ch);
        if col > 0 && col + w > width {
            rows += 1;
            col = 0;
        }
        col += w;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::parse_ansi_line;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn highlighted(line: &Line) -> Vec<String> {
        line.spans
            .iter()
            .filter(|s| s.style.bg == Some(Color::Yellow))
            .map(|s| s.content.to_string())
            .collect()
    }

    #[test]
    fn test_highlight_does_not_panic_on_case_folding_length_changes() {
        // 'İ' lowercases to a longer byte sequence; this used to panic.
        let m = Matcher::new("x", false).unwrap();
        let line = highlight_line(Line::raw("İ日本 x"), &m);
        assert_eq!(text(&line), "İ日本 x");
        assert_eq!(highlighted(&line), vec!["x"]);
    }

    #[test]
    fn test_highlight_uses_regex() {
        let m = Matcher::new("err.*out", false).unwrap();
        let line = highlight_line(Line::raw("x err: timeout y"), &m);
        assert_eq!(highlighted(&line), vec!["err: timeout"]);
    }

    #[test]
    fn test_highlight_across_ansi_spans_keeps_colors() {
        let m = Matcher::new("or: Con", false).unwrap();
        let parsed = parse_ansi_line("\x1b[31mError:\x1b[0m Connection");
        let line = highlight_line(parsed, &m);
        assert_eq!(text(&line), "Error: Connection");
        assert_eq!(highlighted(&line), vec!["or:", " Con"]);
        // The red foreground of the first span is replaced only on the highlighted part.
        assert_eq!(line.spans[0].content, "Err");
        assert_eq!(line.spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn test_slice_line_by_display_width() {
        let line = Line::raw("ab日本cd");
        assert_eq!(text(&slice_line(&line, 0, 4)), "ab日");
        // Skipping into the middle of '日' leaves a space for its right half.
        assert_eq!(text(&slice_line(&line, 3, 4)), " 本c");
        // Cutting '本' on the right edge drops it.
        assert_eq!(text(&slice_line(&line, 0, 5)), "ab日");
        assert_eq!(text(&slice_line(&line, 100, 4)), "");
        assert_eq!(text(&slice_line(&line, usize::MAX, 4)), "");
    }

    #[test]
    fn test_wrap_line_and_row_count_agree() {
        for (s, width, expected) in [
            ("", 5, 1),
            ("abcde", 5, 1),
            ("abcdef", 5, 2),
            ("日本日本日", 4, 3),
            ("a日本", 2, 3),
        ] {
            let line = Line::raw(s);
            let rows = wrap_line(&line, width);
            assert_eq!(rows.len(), expected, "{s:?} at width {width}");
            assert_eq!(wrapped_row_count(&line, width), expected);
            assert_eq!(rows.iter().map(text).collect::<String>(), s);
        }
    }
}
