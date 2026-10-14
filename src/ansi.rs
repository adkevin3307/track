use std::sync::LazyLock;
use regex::Regex;
use ansi_to_tui::IntoText;
use ratatui::text::Line;

static ANSI_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").expect("valid regex")
});

/// Strip ANSI escape codes from a string for plain-text searching/filtering.
pub fn strip_ansi(s: &str) -> String {
    ANSI_REGEX.replace_all(s, "").to_string()
}

/// Expand tab characters to 4 spaces to maintain consistent alignment.
pub fn expand_tabs(s: &str, tab_size: usize) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut result = String::with_capacity(s.len() + 16);
    let mut col = 0;
    for ch in s.chars() {
        if ch == '\t' {
            let spaces = tab_size - (col % tab_size);
            for _ in 0..spaces {
                result.push(' ');
            }
            col += spaces;
        } else {
            result.push(ch);
            col += 1;
        }
    }
    result
}

/// Parse a log line into a Ratatui `Line`, preserving ANSI colors if present.
pub fn parse_ansi_line(s: &str) -> Line<'static> {
    let clean = expand_tabs(s, 4);
    if !clean.contains('\x1b') {
        return Line::raw(clean);
    }

    match clean.into_text() {
        Ok(text) => {
            if let Some(first_line) = text.lines.into_iter().next() {
                first_line
            } else {
                Line::raw(String::new())
            }
        }
        Err(_) => Line::raw(strip_ansi(&clean)),
    }
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
}
