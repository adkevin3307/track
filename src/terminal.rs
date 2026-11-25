//! Entering and leaving the full-screen UI, and copying through the terminal. The same
//! file in track, monitor and git-graph.
//!
//! [`enter`] switches to raw mode on the alternate screen, hides the cursor and captures
//! the mouse; with `--no-mouse` it asks for alternate scroll mode instead. [`restore`]
//! undoes exactly what was turned on, once, however the program ends: a normal exit (the
//! [`Guard`] is dropped), a termination signal, or a panic on any thread ([`on_panic`]).
//! [`tui`] creates the ratatui terminal, drawing through [`WideSafe`].

use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{Command, execute};
use ratatui::layout::{Position, Size};
use unicode_width::UnicodeWidthStr;

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

/// The ratatui terminal the tools draw on.
pub type Tui<W> = Terminal<WideSafe<CrosstermBackend<W>>>;

pub fn tui<W: Write>(out: W) -> io::Result<Tui<W>> {
    Terminal::new(WideSafe::new(CrosstermBackend::new(out)))
}

/// A backend that keeps double-width characters (CJK) whole on terminals that don't.
///
/// ratatui rewrites only the cells that changed, and when one half of a double-width
/// character is overwritten it counts on the terminal to blank the other half, as xterm and
/// tmux do. Neovim's terminal (toggleterm) doesn't: the leftover half stays, and the rest of
/// the row is drawn one column off. This remembers what each cell shows and writes a space
/// over such a leftover half itself.
pub struct WideSafe<B> {
    inner: B,
    /// What the screen shows: the width of the character starting at each cell (0 for the
    /// right half of a double-width character), row after row.
    shown: Vec<u8>,
    size: Size,
}

impl<B: Backend> WideSafe<B> {
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            shown: Vec::new(),
            size: Size::default(),
        }
    }
}

/// Cell writes to send so that no half of a double-width character is left behind, given
/// what each cell shows (`shown`, updated) on a screen `width` cells wide.
fn whole_wide_chars<'a>(
    shown: &mut [u8],
    width: u16,
    updates: impl Iterator<Item = (u16, u16, &'a Cell)>,
) -> Vec<(u16, u16, &'a Cell)> {
    static BLANK: Cell = Cell::EMPTY;
    let mut out = Vec::new();
    for (x, y, cell) in updates {
        if x >= width {
            continue;
        }
        let row = y as usize * width as usize;
        let w = cell.symbol().width().clamp(1, 2) as u16;
        let end = (x + w).min(width);
        let Some(cells) = shown.get_mut(row..row + width as usize) else {
            out.push((x, y, cell));
            continue;
        };
        // The right half of a double-width character starts this write: blank its left half
        if x > 0 && cells[x as usize] == 0 {
            out.push((x - 1, y, &BLANK));
            cells[x as usize - 1] = 1;
        }
        // The left half of one ends it: its right half would be left behind
        let orphan = end < width && cells[end as usize - 1] == 2;
        out.push((x, y, cell));
        cells[x as usize] = w as u8;
        for c in x + 1..end {
            cells[c as usize] = 0;
        }
        if orphan {
            out.push((end, y, &BLANK));
            cells[end as usize] = 1;
        }
    }
    out
}

impl<B: Backend> Backend for WideSafe<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let size = self.inner.size()?;
        if size != self.size {
            self.size = size;
            self.shown = vec![1; size.width as usize * size.height as usize];
        }
        let updates = whole_wide_chars(&mut self.shown, size.width, content);
        self.inner.draw(updates.into_iter())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.shown.fill(1);
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.shown.fill(1);
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

/// Escape sequences go straight to the terminal (OSC 52 copies, clearing the screen).
impl<B: Write> Write for WideSafe<B> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
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
    fn leftover_halves_of_wide_chars_are_blanked() {
        let (a, wide) = (Cell::from('a'), Cell::from('中'));
        let plan = |shown: &mut Vec<u8>, writes: &[(u16, &Cell)]| -> Vec<(u16, String)> {
            whole_wide_chars(shown, 6, writes.iter().map(|&(x, c)| (x, 0, c)))
                .into_iter()
                .map(|(x, _, c)| (x, c.symbol().to_string()))
                .collect()
        };
        let s = |x: u16, sym: &str| (x, sym.to_string());
        // 中 at 0-1 and 2-3; `a` over the left half of the second leaves its right half
        let mut shown = vec![2, 0, 2, 0, 1, 1];
        assert_eq!(plan(&mut shown, &[(2, &a)]), vec![s(2, "a"), s(3, " ")]);
        assert_eq!(shown, vec![2, 0, 1, 1, 1, 1]);
        // `a` over the right half of the first: blank its left half first
        assert_eq!(plan(&mut shown, &[(1, &a)]), vec![s(0, " "), s(1, "a")]);
        // 中 one column off an old one (at 3-4 over 2-3 … 4-5)
        let mut shown = vec![1, 1, 2, 0, 2, 0];
        assert_eq!(
            plan(&mut shown, &[(3, &wide)]),
            vec![s(2, " "), s(3, "中"), s(5, " ")]
        );
        assert_eq!(shown, vec![1, 1, 1, 2, 0, 1]);
        // Aligned writes need nothing extra
        let mut shown = vec![2, 0, 1, 1, 1, 1];
        assert_eq!(plan(&mut shown, &[(0, &wide)]), vec![s(0, "中")]);
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
