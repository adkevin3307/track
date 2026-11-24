use std::ops::Range;

use regex::{Regex, RegexBuilder};

/// A compiled search or filter pattern.
///
/// - Smart case: case-insensitive unless the pattern contains an uppercase letter.
/// - Patterns that are not valid regular expressions are matched literally.
/// - Filters may start with `!` to exclude matching lines (use `\!` to match a literal `!`).
#[derive(Debug, Clone)]
pub struct Matcher {
    pub pattern: String,
    pub literal: bool,
    pub invert: bool,
    regex: Regex,
}

impl Matcher {
    /// Returns `None` when the input is empty.
    pub fn new(input: &str, allow_invert: bool) -> Option<Self> {
        let (invert, pattern) = match input.strip_prefix('!') {
            Some(rest) if allow_invert => (true, rest),
            _ => (false, input),
        };
        if pattern.is_empty() {
            return None;
        }

        let case_insensitive = !has_uppercase(pattern);
        let build = |p: &str| {
            RegexBuilder::new(p)
                .case_insensitive(case_insensitive)
                .build()
        };
        let (regex, literal) = match build(pattern) {
            Ok(re) => (re, false),
            Err(_) => (build(&regex::escape(pattern)).ok()?, true),
        };

        Some(Self {
            pattern: pattern.to_string(),
            literal,
            invert,
            regex,
        })
    }

    #[allow(dead_code)] // monitor only highlights matches; the others filter with it
    pub fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text) != self.invert
    }

    /// Byte ranges of non-empty matches in `text` (empty for inverted matchers).
    pub fn find_ranges(&self, text: &str) -> Vec<Range<usize>> {
        if self.invert {
            return Vec::new();
        }
        self.regex
            .find_iter(text)
            .filter(|m| !m.is_empty())
            .map(|m| m.range())
            .collect()
    }

    /// Human-readable label for the status bar, e.g. `!debug` or `a(b (literal)`.
    pub fn label(&self) -> String {
        let mut label = String::new();
        if self.invert {
            label.push('!');
        }
        label.push_str(&self.pattern);
        if self.literal {
            label.push_str(" (literal)");
        }
        label
    }
}

/// Smart case: does the pattern contain an uppercase letter to match? Regex syntax
/// doesn't count: the letter of an escape such as `\W`, `\S`, `\D` or `\B`, and the
/// class name in `\p{Lu}` or `\PL`.
fn has_uppercase(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            if c.is_uppercase() {
                return true;
            }
            continue;
        }
        if let Some('p' | 'P') = chars.next() {
            if chars.clone().next() == Some('{') {
                chars.by_ref().find(|&c| c == '}');
            } else {
                chars.next();
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smart_case() {
        let lower = Matcher::new("error", false).unwrap();
        assert!(lower.is_match("ERROR: disk"));
        let upper = Matcher::new("Error", false).unwrap();
        assert!(!upper.is_match("ERROR: disk"));
        assert!(upper.is_match("Error: disk"));
        // Escapes and class names are syntax, not uppercase letters to match.
        for (pattern, text) in [
            (r"\Werror", " ERROR"),
            (r"\Serror", "xERROR"),
            (r"\d+ err\B", "1 ERROR"),
            (r"\p{Lu}rror", "ERROR"),
            (r"\PLrror", "1RROR"),
        ] {
            assert!(
                Matcher::new(pattern, false).unwrap().is_match(text),
                "{pattern}"
            );
        }
        assert!(!Matcher::new(r"\WError", false).unwrap().is_match(" ERROR"));
    }

    #[test]
    fn test_invalid_regex_falls_back_to_literal() {
        let m = Matcher::new("a(b", false).unwrap();
        assert!(m.literal);
        assert!(m.is_match("xa(by"));
        assert_eq!(m.label(), "a(b (literal)");
    }

    #[test]
    fn test_invert_only_when_allowed() {
        let filter = Matcher::new("!debug", true).unwrap();
        assert!(filter.invert);
        assert!(!filter.is_match("debug line"));
        assert!(filter.is_match("info line"));

        let search = Matcher::new("!debug", false).unwrap();
        assert!(!search.invert);
        assert!(search.is_match("a !debug b"));

        assert!(Matcher::new("", true).is_none());
        assert!(Matcher::new("!", true).is_none());
    }

    #[test]
    fn test_find_ranges_skips_empty_matches() {
        let m = Matcher::new("x*", false).unwrap();
        assert_eq!(m.find_ranges("axxb"), vec![1..3]);
    }
}
