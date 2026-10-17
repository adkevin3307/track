mod ansi;
mod app;
mod reader;
mod ui;

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::panic;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use clap::CommandFactory;
use clap::Parser;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyModifiers, MouseEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    tty::IsTty,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use app::{App, InputMode};
use reader::{InputSource, ReaderEvent, spawn_reader};

/// A simple, fast terminal log viewer with horizontal & vertical scrolling
#[derive(Parser, Debug)]
#[command(name = "track", author, version, about = "Simple, lightweight log viewer.", long_about = None, help_template = "{about}\n\n\x1b[33mUSAGE:\x1b[0m\n    {usage}\n\n\x1b[33mARGUMENTS:\x1b[0m\n{positionals}\n\n\x1b[33mOPTIONS:\x1b[0m\n{options}\n", color = clap::ColorChoice::Always)]
struct Cli {
    /// File to view (reads from stdin if not provided or when piped)
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Maximum lines to retain in memory buffer
    #[arg(short = 'n', long, default_value_t = 100_000)]
    max_lines: usize,

    /// Start with line wrapping enabled
    #[arg(short = 'w', long, default_value_t = false)]
    wrap: bool,

    /// Start with line numbers displayed
    #[arg(short = 'l', long = "numbers", default_value_t = false)]
    show_numbers: bool,

    /// Follow file if reading from a file (like tail -f)
    #[arg(short = 'f', long, default_value_t = false)]
    follow: bool,
}

fn main() -> io::Result<()> {
    let args = Cli::parse();

    // Determine input source
    let (source, source_name, follow_mode) = match args.file {
        Some(path) => {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            (InputSource::FilePath(path), name, args.follow)
        }
        None => {
            if io::stdin().is_tty() {
                eprintln!("track: No input provided.");
                eprintln!();
                let mut cmd = Cli::command();
                cmd.print_help().unwrap();
                std::process::exit(0);
            }
            (InputSource::Stdin, "stdin".to_string(), true)
        }
    };

    // Setup terminal writer (use /dev/tty if stdin is piped)
    let terminal_writer: Box<dyn Write> =
        if let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") {
            Box::new(tty)
        } else if io::stdout().is_tty() {
            Box::new(io::stdout())
        } else {
            Box::new(io::stderr())
        };

    // Install panic hook to restore terminal cleanly on panic
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut tty: Box<dyn Write> = if let Ok(f) = OpenOptions::new().write(true).open("/dev/tty")
        {
            Box::new(f)
        } else {
            Box::new(io::stderr())
        };
        let _ = execute!(tty, LeaveAlternateScreen, Show);
        default_hook(info);
    }));

    // Spawn background log reader
    let (tx, rx) = mpsc::channel();
    spawn_reader(source, tx, follow_mode);

    // Wait until output is piped to track (allowing user to type sudo password in original terminal)
    let (initial_lines, is_eof) = match rx.recv() {
        Ok(ReaderEvent::Lines(batch)) => (batch, false),
        Ok(ReaderEvent::Eof) | Err(_) => (Vec::new(), true),
    };

    // If upstream exited without producing any output (e.g. sudo cancelled or failed),
    // exit cleanly without ever touching the terminal.
    if is_eof && initial_lines.is_empty() {
        return Ok(());
    }

    // Initialize terminal once data starts flowing
    enable_raw_mode()?;
    let mut writer = terminal_writer;
    execute!(
        writer,
        EnterAlternateScreen,
        Hide,
        event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(writer);
    let mut terminal = Terminal::new(backend)?;

    // Create App state
    let mut app = App::new(source_name, args.max_lines, args.wrap, args.show_numbers);
    app.add_lines(initial_lines);
    if is_eof {
        app.eof = true;
    }

    // Run the main event loop
    let result = run_app(&mut terminal, &mut app, rx);

    // Cleanup terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        Show,
        event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Box<dyn Write>>>,
    app: &mut App,
    rx: mpsc::Receiver<ReaderEvent>,
) -> io::Result<()> {
    loop {
        // Ingest any incoming log lines from the background reader
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ReaderEvent::Lines(batch) => {
                    app.add_lines(batch);
                }
                ReaderEvent::Eof => {
                    app.eof = true;
                }
            }
        }

        // Draw UI
        terminal.draw(|f| ui::render(f, app))?;

        // Poll for user keyboard/mouse events with timeout
        // Short timeout (25ms) keeps UI silky smooth and responsive (~40 FPS)
        if event::poll(Duration::from_millis(25))? {
            match event::read()? {
                Event::Key(key) => {
                    if app.show_help {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                                app.show_help = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    match app.input_mode {
                        InputMode::Normal => match (key.modifiers, key.code) {
                            (KeyModifiers::CONTROL, KeyCode::Char('c'))
                            | (_, KeyCode::Char('q')) => {
                                return Ok(());
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
                            // Help toggle
                            (_, KeyCode::Char('?')) => {
                                app.toggle_help();
                            }
                            // Search & Filter
                            (_, KeyCode::Char('/')) => {
                                app.start_search();
                            }
                            (_, KeyCode::Char('&')) => {
                                app.start_filter();
                            }
                            (_, KeyCode::Char('n')) => {
                                app.jump_to_next_match();
                            }
                            (_, KeyCode::Char('N')) => {
                                app.jump_to_prev_match();
                            }
                            (_, KeyCode::Esc) => {
                                app.clear_search_and_filter();
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
                            (_, KeyCode::Home) | (_, KeyCode::Char('0')) | (_, KeyCode::Char('^')) => {
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
                        },
                        InputMode::Search | InputMode::Filter => match (key.modifiers, key.code) {
                            (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => {
                                app.cancel_input();
                            }
                            (_, KeyCode::Enter) => {
                                app.submit_input();
                            }
                            (_, KeyCode::Backspace) => {
                                app.input_buffer.pop();
                            }
                            (_, KeyCode::Char(c)) => {
                                app.input_buffer.push(c);
                            }
                            _ => {}
                        },
                    }
                }
                Event::Mouse(mouse) => {
                    let fast = mouse.modifiers.contains(KeyModifiers::CONTROL);
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            app.scroll_up(if fast { 15 } else { 3 });
                        }
                        MouseEventKind::ScrollDown => {
                            app.scroll_down(if fast { 15 } else { 3 });
                        }
                        MouseEventKind::ScrollLeft => {
                            app.scroll_left(if fast { 20 } else { 4 });
                        }
                        MouseEventKind::ScrollRight => {
                            app.scroll_right(if fast { 20 } else { 4 });
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {
                    // Handled automatically on next draw
                }
                _ => {}
            }
        }
    }
}
