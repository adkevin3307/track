//! Escape sequences in command output. The same file in track and monitor.
//!
//! Only colors (SGR, `ESC [ … m`) are drawn; everything else is removed before the text is
//! parsed. Left in, other sequences break that parse: an OSC string ended by ST (`ESC \`),
//! such as a hyperlink from `ls --hyperlink`, hides the rest of the line, and a character
//! set selection like `ESC ( B` (part of `tput sgr0`) shows up as `(B`.

use std::borrow::Cow;

/// `text` with only its color sequences left.
pub fn keep_colors(text: &str) -> Cow<'_, str> {
    remove(text, true)
}

/// `text` without any escape sequences: the plain text that search and filters match.
#[allow(dead_code)] // monitor searches the parsed text instead
pub fn strip(text: &str) -> Cow<'_, str> {
    remove(text, false)
}

fn remove(text: &str, keep_colors: bool) -> Cow<'_, str> {
    if !text.contains('\x1b') {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    // Escape sequences are ASCII, so these byte offsets are always char boundaries.
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            i += 1;
            continue;
        }
        let (end, color) = sequence(bytes, i);
        out.push_str(&text[copied..i]);
        if color && keep_colors {
            out.push_str(&text[i..end]);
        }
        copied = end;
        i = end;
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// The end of the escape sequence starting at `bytes[esc]` (an ESC), and whether it
/// sets colors.
fn sequence(bytes: &[u8], esc: usize) -> (usize, bool) {
    let len = bytes.len();
    let Some(&kind) = bytes.get(esc + 1) else {
        return (len, false);
    };
    match kind {
        // CSI: parameters, intermediates, then one final byte
        b'[' => {
            let mut j = esc + 2;
            while j < len && (0x30..=0x3f).contains(&bytes[j]) {
                j += 1;
            }
            while j < len && (0x20..=0x2f).contains(&bytes[j]) {
                j += 1;
            }
            match bytes.get(j) {
                Some(&b) if (0x40..=0x7e).contains(&b) => (j + 1, b == b'm'),
                _ => (j, false),
            }
        }
        // OSC, DCS, SOS, PM, APC: a string ended by BEL or ST (`ESC \`). One that is
        // never ended stops at the end of its line, so it can't swallow the rest.
        b']' | b'P' | b'X' | b'^' | b'_' => {
            let mut j = esc + 2;
            while j < len {
                match bytes[j] {
                    0x07 => return (j + 1, false),
                    0x1b if bytes.get(j + 1) == Some(&b'\\') => return (j + 2, false),
                    b'\n' => return (j, false),
                    _ => j += 1,
                }
            }
            (len, false)
        }
        // Character set selection (`ESC ( B`) and other sequences with intermediates
        0x20..=0x2f => {
            let mut j = esc + 2;
            while j < len && (0x20..=0x2f).contains(&bytes[j]) {
                j += 1;
            }
            match bytes.get(j) {
                Some(&b) if (0x30..=0x7e).contains(&b) => (j + 1, false),
                _ => (j, false),
            }
        }
        // Two-byte escapes: ESC 7, ESC 8, ESC =, ESC M, ESC c, …
        0x30..=0x7e => (esc + 2, false),
        // Not a sequence: drop the lone ESC
        _ => (esc + 1, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_colors() {
        let text = "\x1b[31mred\x1b[0m \x1b[2Kerased \x1b[1;32mgreen\x1b[m";
        assert_eq!(
            keep_colors(text),
            "\x1b[31mred\x1b[0m erased \x1b[1;32mgreen\x1b[m"
        );
        assert_eq!(strip(text), "red erased green");
        assert!(matches!(keep_colors("plain"), Cow::Borrowed("plain")));
    }

    #[test]
    fn removes_osc_ended_by_st_or_bel() {
        let link = "error: see \x1b]8;;https://x\x1b\\E0425\x1b]8;;\x1b\\ for details";
        assert_eq!(strip(link), "error: see E0425 for details");
        assert_eq!(strip("a \x1b]0;title\x07b"), "a b");
        // Never ended: only its own line is lost
        assert_eq!(strip("a\x1b]0;title\nnext"), "a\nnext");
    }

    #[test]
    fn removes_charset_selection_and_two_byte_escapes() {
        // `tput sgr0` on xterm: ESC ( B ESC [ m
        assert_eq!(
            keep_colors("ERROR\x1b(B\x1b[m disk full"),
            "ERROR\x1b[m disk full"
        );
        assert_eq!(strip("\x1b7saved\x1b8 \x1b=x"), "saved x");
        assert_eq!(strip("end\x1b"), "end");
        assert_eq!(strip("日本\x1b[1m語\x1b[0m"), "日本語");
    }
}
