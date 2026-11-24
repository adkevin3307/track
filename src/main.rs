mod ansi;
mod app;
mod input;
mod layout;
mod matcher;
mod reader;
mod selection;
mod ui;

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

use clap::CommandFactory;
use clap::Parser;
use clap::builder::styling::{AnsiColor, Styles};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    tty::IsTty,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use app::{App, InputMode};
use reader::{InputSource, ReaderEvent, spawn_reader};

type Tui = Terminal<CrosstermBackend<Box<dyn Write>>>;

/// How long one frame may spend ingesting input before redrawing.
const INGEST_BUDGET: Duration = Duration::from_millis(15);
/// Event poll timeout when idle (~40 FPS worst-case latency for new input).
const IDLE_POLL: Duration = Duration::from_millis(25);

/// A simple, fast terminal log viewer with horizontal & vertical scrolling
#[derive(Parser, Debug)]
#[command(
    name = "track",
    version,
    about = "Simple, lightweight log viewer.",
    long_about = None,
    styles = Styles::styled()
        .header(AnsiColor::Yellow.on_default())
        .usage(AnsiColor::Yellow.on_default())
        .literal(AnsiColor::Green.on_default()),
)]
struct Cli {
    /// File to view (reads from stdin if not provided)
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Maximum lines kept in memory; the oldest lines are dropped
    #[arg(
        short = 'b',
        long = "buffer-lines",
        alias = "max-lines",
        value_name = "LINES",
        default_value_t = 100_000
    )]
    buffer_lines: usize,

    /// Start with line wrapping enabled
    #[arg(short = 'w', long, default_value_t = false)]
    wrap: bool,

    /// Start with line numbers displayed
    #[arg(
        short = 'N',
        long = "line-numbers",
        short_alias = 'l',
        alias = "numbers",
        default_value_t = false
    )]
    show_numbers: bool,

    /// Read the input once instead of following it, and start at the top
    #[arg(long, default_value_t = false)]
    no_follow: bool,

    /// Follow the input for new lines (the default; kept for compatibility)
    #[arg(short = 'f', long, hide = true, conflicts_with = "no_follow")]
    follow: bool,

    /// Don't capture the mouse: plain drag uses the terminal's own selection, and the
    /// wheel scrolls only where the terminal supports alternate scroll mode
    #[arg(long)]
    no_mouse: bool,

    /// Ignored: the mouse is captured by default (kept for compatibility)
    #[arg(short = 'm', long, hide = true, conflicts_with = "no_mouse")]
    mouse: bool,
}

fn open_input_file(path: &Path) -> io::Result<File> {
    let file = File::open(path)?;
    if file.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "Is a directory",
        ));
    }
    Ok(file)
}

/// The terminal to draw on: /dev/tty, so drawing still works when stdin is piped.
fn open_terminal_writer() -> Box<dyn Write> {
    if let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") {
        Box::new(tty)
    } else if io::stdout().is_tty() {
        Box::new(io::stdout())
    } else {
        Box::new(io::stderr())
    }
}

/// Mouse capture: button presses, drags (1002, not every motion) and the wheel, in
/// SGR encoding (1006). track selects and copies text itself while it is on.
struct MouseCapture(bool);

impl crossterm::Command for MouseCapture {
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

/// Alternate scroll mode (DECSET 1007), used with `--no-mouse`: in the alternate
/// screen the terminal turns mouse wheel scrolling into Up / Down keys, leaving its
/// own text selection working. Not every terminal (or tmux) supports it.
struct AlternateScroll(bool);

impl crossterm::Command for AlternateScroll {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str(if self.0 { "\x1b[?1007h" } else { "\x1b[?1007l" })
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut writer = open_terminal_writer();
    let _ = execute!(
        writer,
        MouseCapture(false),
        AlternateScroll(false),
        LeaveAlternateScreen,
        Show
    );
}

/// Puts the terminal into TUI mode and restores it when dropped, on every exit path.
struct TerminalGuard;

impl TerminalGuard {
    fn enter(writer: &mut impl Write, mouse: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        let guard = TerminalGuard;
        execute!(writer, EnterAlternateScreen, Hide)?;
        if mouse {
            execute!(writer, MouseCapture(true))?;
        } else {
            execute!(writer, AlternateScroll(true))?;
        }
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn main() -> io::Result<()> {
    let args = Cli::parse();
    let follow = !args.no_follow;

    // Determine input source; file errors are reported before touching the terminal
    let (source, source_name) = match args.file {
        Some(path) => {
            let file = match open_input_file(&path) {
                Ok(file) => file,
                Err(e) => {
                    eprintln!("track: {}: {}", path.display(), e);
                    std::process::exit(1);
                }
            };
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            (InputSource::File { file, path, follow }, name)
        }
        None => {
            if io::stdin().is_tty() {
                eprintln!("track: No input provided.");
                eprintln!();
                let mut cmd = Cli::command();
                cmd.print_help()?;
                std::process::exit(2);
            }
            (InputSource::Stdin, "stdin".to_string())
        }
    };
    let is_stdin = matches!(source, InputSource::Stdin);

    // Install panic hook to restore terminal cleanly on panic
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    // Spawn background log reader
    let (tx, rx) = mpsc::channel();
    spawn_reader(source, tx);

    // For stdin, wait until a complete line arrives before taking over the terminal
    // (this lets the user answer e.g. a sudo password or other prompt in the terminal)
    let mut first_events = Vec::new();
    if is_stdin {
        loop {
            match rx.recv() {
                Ok(ReaderEvent::Eof) | Err(_) => return Ok(()),
                // Keep only the latest incomplete line
                Ok(event @ ReaderEvent::Partial(_)) => first_events = vec![event],
                Ok(event) => {
                    first_events.push(event);
                    break;
                }
            }
        }
    }

    let mut app = App::new(
        source_name,
        args.buffer_lines,
        args.wrap,
        args.show_numbers,
        follow,
    );
    for event in first_events {
        apply_reader_event(&mut app, event);
    }

    // Initialize terminal; the guard restores it however we exit (declared before
    // the terminal so it is dropped after it)
    let mut writer = open_terminal_writer();
    let _guard = TerminalGuard::enter(&mut writer, !args.no_mouse)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(writer))?;

    run_app(&mut terminal, &mut app, rx)
}

fn apply_reader_event(app: &mut App, event: ReaderEvent) {
    match event {
        ReaderEvent::Lines(batch) => app.add_lines(batch),
        ReaderEvent::Partial(line) => app.set_partial_line(line),
        ReaderEvent::Notice(msg) => app.status_message = Some(msg),
        ReaderEvent::Eof => app.eof = true,
    }
}

/// Ingest reader events for at most `INGEST_BUDGET`.
/// Returns (anything changed, more input may be pending right now).
fn ingest(app: &mut App, rx: &mpsc::Receiver<ReaderEvent>) -> (bool, bool) {
    let deadline = Instant::now() + INGEST_BUDGET;
    let mut changed = false;
    loop {
        match rx.try_recv() {
            Ok(event) => {
                apply_reader_event(app, event);
                changed = true;
                if Instant::now() >= deadline {
                    return (true, true);
                }
            }
            Err(TryRecvError::Empty) => return (changed, false),
            Err(TryRecvError::Disconnected) => {
                // Reader ended without an Eof (e.g. it panicked)
                if !app.eof {
                    app.eof = true;
                    changed = true;
                }
                return (changed, false);
            }
        }
    }
}

fn run_app(terminal: &mut Tui, app: &mut App, rx: mpsc::Receiver<ReaderEvent>) -> io::Result<()> {
    let mut dirty = true;
    loop {
        let (changed, backlog) = ingest(app, &rx);
        dirty |= changed;

        // Only redraw when something changed
        if dirty {
            terminal.draw(|f| ui::render(f, app))?;
            dirty = false;
        }

        let timeout = if backlog { Duration::ZERO } else { IDLE_POLL };
        if event::poll(timeout)? {
            // Handle every queued event before the next draw
            loop {
                if handle_event(terminal, app, event::read()?)? {
                    return Ok(());
                }
                dirty = true;
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
}

/// Returns true when the app should quit.
fn handle_event(terminal: &mut Tui, app: &mut App, event: Event) -> io::Result<bool> {
    match event {
        // Ignore key releases (reported on Windows and with keyboard enhancements)
        Event::Key(key) if key.kind != KeyEventKind::Release => handle_key(terminal, app, key),
        Event::Mouse(mouse) => {
            if let Some(text) = app.on_mouse(mouse) {
                let writer = terminal.backend_mut();
                writer.write_all(selection::osc52(&text).as_bytes())?;
                writer.flush()?;
            }
            Ok(false)
        }
        // Resize is handled on the next draw
        _ => Ok(false),
    }
}

fn handle_key(terminal: &mut Tui, app: &mut App, key: KeyEvent) -> io::Result<bool> {
    app.status_message = None;
    app.cancel_selection();

    if app.show_help {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(true);
        }
        // Scroll when the help doesn't fit; any other key closes it
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => app.help_scroll = app.help_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => app.help_scroll += 1,
            KeyCode::PageUp => app.help_scroll = app.help_scroll.saturating_sub(10),
            KeyCode::PageDown => app.help_scroll += 10,
            KeyCode::Char('g') => app.help_scroll = 0,
            KeyCode::Char('G') => app.help_scroll = usize::MAX,
            _ => app.show_help = false,
        }
        return Ok(false);
    }

    match app.input_mode {
        InputMode::Normal => return handle_normal_key(terminal, app, key),
        InputMode::Search | InputMode::Filter | InputMode::Goto => handle_prompt_key(app, key),
    }
    Ok(false)
}

fn handle_normal_key(terminal: &mut Tui, app: &mut App, key: KeyEvent) -> io::Result<bool> {
    // Ctrl or Alt + character: only the explicit bindings, never the plain key's action.
    // (Ctrl+Alt together is AltGr on some platforms, which types ordinary characters.)
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if let KeyCode::Char(c) = key.code
        && ctrl != alt
    {
        if ctrl {
            match c {
                'c' => return Ok(true),
                'l' => terminal.clear()?,
                'u' => app.scroll_half_page_up(),
                'd' => app.scroll_half_page_down(),
                'b' => app.scroll_page_up(),
                'f' => app.scroll_page_down(),
                _ => {}
            }
        }
        return Ok(false);
    }

    match (key.modifiers, key.code) {
        (_, KeyCode::Char('q')) => {
            return Ok(true);
        }
        // Wrap toggle
        (_, KeyCode::Char('w')) => {
            app.toggle_wrap();
        }
        // Line numbers toggle
        (_, KeyCode::Char('#')) => {
            app.toggle_line_numbers();
        }
        // Help toggle
        (_, KeyCode::Char('?')) => {
            app.toggle_help();
        }
        // Search, Filter & Goto
        (_, KeyCode::Char('/')) => {
            app.start_input(InputMode::Search);
        }
        (_, KeyCode::Char('&')) => {
            app.start_input(InputMode::Filter);
        }
        (_, KeyCode::Char(':')) => {
            app.start_input(InputMode::Goto);
        }
        (_, KeyCode::Char('n')) => {
            app.jump_to_next_match();
        }
        (_, KeyCode::Char('N')) => {
            app.jump_to_prev_match();
        }
        (_, KeyCode::Esc) => {
            app.clear_last_search_or_filter();
        }

        // Document navigation (Top / Bottom)
        (_, KeyCode::Char('g')) => {
            app.scroll_to_top();
        }
        (_, KeyCode::Char('G')) => {
            app.scroll_to_end();
        }

        // Horizontal scrolling (Left / Right / Home / End)
        (_, KeyCode::Char('H')) => {
            app.scroll_left((app.viewport_width / 2).max(1));
        }
        (_, KeyCode::Char('L')) => {
            app.scroll_right((app.viewport_width / 2).max(1));
        }
        (_, KeyCode::Left) | (_, KeyCode::Char('h')) => {
            app.scroll_left(4);
        }
        (_, KeyCode::Right) | (_, KeyCode::Char('l')) => {
            app.scroll_right(4);
        }
        (_, KeyCode::Home) => {
            app.scroll_to_col_zero();
        }
        (_, KeyCode::End) => {
            app.scroll_to_line_end();
        }

        // Vertical scrolling (Up / Down)
        (_, KeyCode::Up) | (_, KeyCode::Char('k')) => {
            app.scroll_up(1);
        }
        (_, KeyCode::Down) | (_, KeyCode::Char('j')) => {
            app.scroll_down(1);
        }
        (_, KeyCode::PageUp) => {
            app.scroll_page_up();
        }
        (_, KeyCode::PageDown) => {
            app.scroll_page_down();
        }
        _ => {}
    }
    Ok(false)
}

fn handle_prompt_key(app: &mut App, key: KeyEvent) {
    let before = app.editor.text().to_string();
    edit_prompt(app, key);
    // Searching while typing: highlight and jump to the first match
    if app.input_mode == InputMode::Search && app.editor.text() != before {
        app.preview_search();
    }
}

fn edit_prompt(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.cancel_input(),
        KeyCode::Char('c') if ctrl => app.cancel_input(),
        KeyCode::Enter => app.submit_input(),
        // Backspace on an empty prompt cancels it (like vim / less)
        KeyCode::Backspace if app.editor.text().is_empty() => app.cancel_input(),
        KeyCode::Backspace => app.editor.backspace(),
        KeyCode::Delete => app.editor.delete(),
        KeyCode::Left => app.editor.left(),
        KeyCode::Right => app.editor.right(),
        KeyCode::Home => app.editor.home(),
        KeyCode::End => app.editor.end(),
        KeyCode::Char('a') if ctrl => app.editor.home(),
        KeyCode::Char('e') if ctrl => app.editor.end(),
        KeyCode::Char('u') if ctrl => app.editor.kill_to_start(),
        KeyCode::Char('w') if ctrl => app.editor.delete_word(),
        KeyCode::Up => app.history_prev(),
        KeyCode::Down => app.history_next(),
        KeyCode::Char(_) if ctrl => {}
        KeyCode::Char(c) => app.editor.insert(c),
        _ => {}
    }
}
