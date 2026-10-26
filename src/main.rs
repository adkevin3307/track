mod ansi;
mod app;
mod input;
mod layout;
mod matcher;
mod reader;
mod ui;

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

use clap::builder::styling::{AnsiColor, Styles};
use clap::CommandFactory;
use clap::Parser;
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
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
    #[arg(short = 'b', long = "buffer-lines", alias = "max-lines", value_name = "LINES", default_value_t = 100_000)]
    buffer_lines: usize,

    /// Start with line wrapping enabled
    #[arg(short = 'w', long, default_value_t = false)]
    wrap: bool,

    /// Start with line numbers displayed
    #[arg(short = 'N', long = "line-numbers", short_alias = 'l', alias = "numbers", default_value_t = false)]
    show_numbers: bool,

    /// Read the input once instead of following it, and start at the top
    #[arg(long, default_value_t = false)]
    no_follow: bool,

    /// Follow the input for new lines (the default; kept for compatibility)
    #[arg(short = 'f', long, hide = true, conflicts_with = "no_follow")]
    follow: bool,

    /// Capture the mouse for wheel scrolling (disables terminal text selection; toggle with `m`)
    #[arg(short = 'm', long, default_value_t = false)]
    mouse: bool,
}

fn open_input_file(path: &Path) -> io::Result<File> {
    let file = File::open(path)?;
    if file.metadata()?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::IsADirectory, "Is a directory"));
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

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut writer = open_terminal_writer();
    let _ = execute!(writer, DisableMouseCapture, LeaveAlternateScreen, Show);
}

/// Puts the terminal into TUI mode and restores it when dropped, on every exit path.
struct TerminalGuard;

impl TerminalGuard {
    fn enter(writer: &mut impl Write, mouse: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        let guard = TerminalGuard;
        execute!(writer, EnterAlternateScreen, Hide)?;
        if mouse {
            execute!(writer, EnableMouseCapture)?;
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

    // For stdin, wait until output arrives before taking over the terminal
    // (this lets the user type e.g. a sudo password in the original terminal)
    let mut first_event = None;
    if is_stdin {
        match rx.recv() {
            Ok(ReaderEvent::Eof) | Err(_) => return Ok(()),
            Ok(event) => first_event = Some(event),
        }
    }

    let mut app = App::new(source_name, args.buffer_lines, args.wrap, args.show_numbers, follow);
    app.mouse_capture = args.mouse;
    if let Some(event) = first_event {
        apply_reader_event(&mut app, event);
    }

    // Initialize terminal; the guard restores it however we exit (declared before
    // the terminal so it is dropped after it)
    let mut writer = open_terminal_writer();
    let _guard = TerminalGuard::enter(&mut writer, args.mouse)?;
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
            let fast = mouse.modifiers.contains(KeyModifiers::CONTROL);
            match mouse.kind {
                MouseEventKind::ScrollUp => app.scroll_up(if fast { 15 } else { 3 }),
                MouseEventKind::ScrollDown => app.scroll_down(if fast { 15 } else { 3 }),
                MouseEventKind::ScrollLeft => app.scroll_left(if fast { 20 } else { 4 }),
                MouseEventKind::ScrollRight => app.scroll_right(if fast { 20 } else { 4 }),
                _ => {}
            }
            Ok(false)
        }
        // Resize is handled on the next draw
        _ => Ok(false),
    }
}

fn handle_key(terminal: &mut Tui, app: &mut App, key: KeyEvent) -> io::Result<bool> {
    app.status_message = None;

    if app.show_help {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => app.show_help = false,
            KeyCode::Up | KeyCode::Char('k') => app.help_scroll = app.help_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => app.help_scroll += 1,
            KeyCode::PageUp => app.help_scroll = app.help_scroll.saturating_sub(10),
            KeyCode::PageDown | KeyCode::Char(' ') => app.help_scroll += 10,
            KeyCode::Char('g') | KeyCode::Home => app.help_scroll = 0,
            KeyCode::Char('G') | KeyCode::End => app.help_scroll = usize::MAX,
            _ => {}
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
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Char('q')) => {
            return Ok(true);
        }
        (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
            terminal.clear()?;
        }
        // Follow mode toggle
        (_, KeyCode::Char('f')) | (_, KeyCode::Char('F')) => {
            app.toggle_follow();
        }
        // Wrap toggle
        (_, KeyCode::Char('w')) => {
            app.toggle_wrap();
        }
        // Line numbers toggle
        (_, KeyCode::Char('#')) => {
            app.toggle_line_numbers();
        }
        // Mouse capture toggle
        (_, KeyCode::Char('m')) => {
            app.mouse_capture = !app.mouse_capture;
            if app.mouse_capture {
                execute!(terminal.backend_mut(), EnableMouseCapture)?;
                app.status_message = Some("Mouse capture ON: wheel scrolls (text selection disabled)".to_string());
            } else {
                execute!(terminal.backend_mut(), DisableMouseCapture)?;
                app.status_message = Some("Mouse capture OFF: text selection enabled".to_string());
            }
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
        (m, KeyCode::Home) if m.contains(KeyModifiers::CONTROL) => {
            app.scroll_to_top();
        }
        (m, KeyCode::End) if m.contains(KeyModifiers::CONTROL) => {
            app.scroll_to_end();
        }
        (_, KeyCode::Char('g')) => {
            app.scroll_to_top();
        }
        (_, KeyCode::Char('G')) => {
            app.scroll_to_end();
        }

        // Horizontal scrolling (Left / Right / Home / End)
        (m, KeyCode::Left) if m.contains(KeyModifiers::CONTROL) => {
            app.scroll_left(20);
        }
        (_, KeyCode::Char('H')) | (_, KeyCode::Char('[')) => {
            app.scroll_left(20);
        }
        (m, KeyCode::Right) if m.contains(KeyModifiers::CONTROL) => {
            app.scroll_right(20);
        }
        (_, KeyCode::Char('L')) | (_, KeyCode::Char(']')) => {
            app.scroll_right(20);
        }
        (_, KeyCode::Left) | (_, KeyCode::Char('h')) => {
            app.scroll_left(4);
        }
        (_, KeyCode::Right) | (_, KeyCode::Char('l')) => {
            app.scroll_right(4);
        }
        (_, KeyCode::Home) | (_, KeyCode::Char('^')) => {
            app.scroll_to_col_zero();
        }
        (_, KeyCode::End) | (_, KeyCode::Char('$')) => {
            app.scroll_to_line_end();
        }

        // Vertical scrolling (Up / Down)
        (m, KeyCode::Up) if m.contains(KeyModifiers::CONTROL) => {
            app.scroll_up(5);
        }
        (m, KeyCode::Down) if m.contains(KeyModifiers::CONTROL) => {
            app.scroll_down(5);
        }
        (_, KeyCode::Up) | (_, KeyCode::Char('k')) => {
            app.scroll_up(1);
        }
        (_, KeyCode::Down) | (_, KeyCode::Char('j')) => {
            app.scroll_down(1);
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u'))
        | (_, KeyCode::PageUp)
        | (_, KeyCode::Char('b')) => {
            app.scroll_page_up();
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d'))
        | (_, KeyCode::PageDown)
        | (_, KeyCode::Char(' ')) => {
            app.scroll_page_down();
        }
        _ => {}
    }
    Ok(false)
}

fn handle_prompt_key(app: &mut App, key: KeyEvent) {
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
