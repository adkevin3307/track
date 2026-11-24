//! Entering and leaving the full-screen UI, and copying through the terminal. The same
//! file in track, monitor and git-graph.
//!
//! [`enter`] switches to raw mode on the alternate screen, hides the cursor and captures
//! the mouse; with `--no-mouse` it asks for alternate scroll mode instead. [`restore`]
//! undoes exactly what was turned on, once, however the program ends: a normal exit (the
//! [`Guard`] is dropped), a termination signal, or a panic on any thread ([`on_panic`]).

use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{Command, execute};

/// Opens the stream the UI is drawn on.
pub type Open = fn() -> Box<dyn Write>;

/// How the UI was entered: the stream, and whether the mouse is captured.
static ENTERED: OnceLock<(Open, bool)> = OnceLock::new();
/// The terminal is in UI mode and not yet restored.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Mouse capture: button presses, drags (1002, not every motion) and the wheel, in SGR
/// encoding (1006). The tools select and copy text themselves while it is on.
struct MouseCapture(bool);

impl Command for MouseCapture {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str(if self.0 {
            "\x1b[?1000h\x1b[?1002h\x1b[?1006h"
        } else {
            "\x1b[?1006l\x1b[?1002l\x1b[?1000l"
        })
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Alternate scroll mode (1007), used with `--no-mouse`: on the alternate screen the
/// terminal turns the wheel into up / down keys and keeps its own text selection. Not every
/// terminal (or tmux) supports it, and some have it on by default, so it is only turned off
/// again if it was turned on here.
struct AlternateScroll(bool);

impl Command for AlternateScroll {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str(if self.0 { "\x1b[?1007h" } else { "\x1b[?1007l" })
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Restores the terminal when dropped.
#[must_use]
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

/// Switches the terminal (written through `open`) to the full-screen UI.
pub fn enter(open: Open, mouse: bool) -> io::Result<Guard> {
    let _ = ENTERED.set((open, mouse));
    enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    // From here on, an error still restores the terminal.
    let guard = Guard;
    let mut out = open();
    execute!(out, EnterAlternateScreen, Hide)?;
    if mouse {
        execute!(out, MouseCapture(true))?;
    } else {
        execute!(out, AlternateScroll(true))?;
    }
    Ok(guard)
}

/// Undoes [`enter`]. Only the first call does anything, and errors are ignored: this runs
/// on the way out, perhaps after the terminal is gone.
pub fn restore() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let Some(&(open, mouse)) = ENTERED.get() else {
        return;
    };
    let _ = disable_raw_mode();
    let mut out = open();
    let _ = if mouse {
        execute!(out, MouseCapture(false), LeaveAlternateScreen, Show)
    } else {
        execute!(out, AlternateScroll(false), LeaveAlternateScreen, Show)
    };
}

/// A panic on any thread then restores the terminal, runs `cleanup` (stopping child
/// processes, say), prints the panic as usual and ends the program with status 101, rather
/// than leaving the UI running on a restored terminal.
pub fn on_panic(cleanup: impl Fn() + Send + Sync + 'static) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        cleanup();
        default(info);
        std::process::exit(101);
    }));
}

/// The largest text copied to the clipboard. tmux drops a copy sequence longer than its
/// 1 MiB input buffer, and base64 makes the text a third bigger, so a larger copy would
/// silently do nothing.
pub const COPY_LIMIT: usize = 512 * 1024;

/// Puts `text` on the clipboard through the terminal (OSC 52; tmux needs
/// `set-clipboard on`) and returns the status message to show.
pub fn copy(out: &mut impl Write, text: &str) -> String {
    let n = text.chars().count();
    if text.len() > COPY_LIMIT {
        return format!(
            "Selection too large to copy ({n} characters; the limit is {} KB)",
            COPY_LIMIT / 1024
        );
    }
    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.write_all(seq.as_bytes()).and_then(|()| out.flush());
    format!(
        "Copied {n} character{} to the clipboard",
        if n == 1 { "" } else { "s" }
    )
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
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

    #[test]
    fn base64_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("日本".as_bytes()), "5pel5pys");
    }

    #[test]
    fn copy_writes_osc52_up_to_the_limit() {
        let mut out = Vec::new();
        assert_eq!(
            copy(&mut out, "foo"),
            "Copied 3 characters to the clipboard"
        );
        assert_eq!(out, b"\x1b]52;c;Zm9v\x07");

        let mut out = Vec::new();
        let big = "x".repeat(COPY_LIMIT + 1);
        assert!(copy(&mut out, &big).starts_with("Selection too large to copy"));
        assert!(out.is_empty());
    }
}
