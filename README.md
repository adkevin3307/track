# track

> A simple, blazing fast terminal log viewer with horizontal scrolling, follow mode, and pipe support.

*Developed by Antigravity (Google DeepMind).*

`track` is designed to be as simple and frictionless as `tail`, but with the ability to scroll horizontally (left/right), search, filter, and inspect long log lines without clutter or complicated configuration. It preserves standard terminal ANSI colors emitted by Docker and CLI tools.

---

## Features

- **Piped & Streaming Input**: Pipe directly from command output, or shell redirects (`<`).
- **Horizontal Scrolling**: Scroll left/right with Arrow keys or `h`/`l` to read wide log entries without line wraps truncating the view.
- **Follow Mode (like `tail -f`)**: Follows streaming logs in real time (`● FOLLOWING`). Automatically pauses when you scroll up (`⏸ PAUSED`), and resumes when you scroll back to the bottom or press `f`/`G`.
- **Soft Wrap Toggle**: Press `w` to toggle between horizontal scrolling and wrapped lines.
- **ANSI Color Support**: Preserves the original colors output by Docker containers and CLI applications while using the terminal's default colors for UI elements.
- **Search & Highlighting**: Press `/` to search with regular expressions or plain text; jump through matches with `n` / `N`.
- **Filter Mode**: Press `&` to view only log lines that match a specific pattern.
- **Line Numbers**: Press `#` to toggle line numbers on/off. When scrolling horizontally, line numbers remain pinned on the left.
- **High Performance**: Background streaming reader with non-blocking ingestion and low memory overhead.

---

## Installation & Building

### Quick Build Script
You can use the provided build script to compile and optionally install `track` to `~/.local/bin`:

```bash
./build.sh
```

To automatically copy without prompting:
```bash
./build.sh -y
```

### Manual Cargo Build
To compile an optimized, standalone executable:

```bash
cargo build --release
```

The compiled binary will be located at:
```bash
./target/release/track
```

### Install Globally
To install `track` into your Cargo bin path (`~/.cargo/bin/track`):

```bash
cargo install --path .
```

---

## Usage Examples

### Piped Commands
```bash
# Pipe any command output (stdout) into track
my-command | track

# Pipe both stdout and stderr into track
my-command 2>&1 | track
```

### Direct File or Shell Redirection
```bash
# View a log file directly
track my-file

# View and follow a growing file (like tail -f)
track -f my-file

# Shell input redirection
track < my-file
```

---

## Keybindings

### Navigation
| Key | Action |
| --- | --- |
| `Left` / `h` | Scroll left 4 columns |
| `Right` / `l` | Scroll right 4 columns |
| `Ctrl+Left` / `H` / `[` | Scroll left 20 columns |
| `Ctrl+Right` / `L` / `]` | Scroll right 20 columns |
| `Home` / `^` | Jump to column 0 (beginning of line) |
| `End` / `$` | Jump to end of line |
| `Up` / `k` | Scroll up 1 line (pauses follow mode) |
| `Down` / `j` | Scroll down 1 line |
| `Ctrl+Up` / `Ctrl+Down` | Scroll up / down 5 lines |
| `PageUp` / `Ctrl+U` / `b` | Scroll up 1 page |
| `PageDown` / `Ctrl+D` / `Space` | Scroll down 1 page |
| `g` / `Ctrl+Home` | Jump to top (first line) |
| `G` / `Ctrl+End` | Jump to bottom & resume follow mode |
| `Mouse Wheel` | Scroll up / down (hold `Ctrl` to scroll faster) |

### Modes & Display
| Key | Action |
| --- | --- |
| `f` / `F` | Toggle Follow mode (`FOLLOWING` ↔ `PAUSED`) |
| `w` | Toggle soft line wrapping (`Wrap: ON` ↔ `Wrap: OFF`) |
| `#` | Toggle line numbers |
| `?` | Show / hide Help overlay |

### Search & Filtering
| Key | Action |
| --- | --- |
| `/` | Start search prompt (case-insensitive substring or regex) |
| `n` | Jump to next search match |
| `N` | Jump to previous search match |
| `&` | Filter mode (show only matching lines) |
| `Esc` | Clear search / filter or cancel current prompt |

### General
| Key | Action |
| --- | --- |
| `q` / `Ctrl+C` | Quit `track` cleanly |

---

## Command-Line Options

```
Usage: track [OPTIONS] [FILE]

Arguments:
  [FILE]  File to view (reads from stdin if not provided or when piped)

Options:
  -n, --max-lines <MAX_LINES>  Maximum lines to retain in memory buffer [default: 100000]
  -w, --wrap                   Start with line wrapping enabled
  -l, --numbers                Start with line numbers displayed
  -f, --follow                 Follow file if reading from a file (like tail -f)
  -h, --help                   Print help
  -V, --version                Print version
```

---

## Credits

Developed by **Antigravity**, an AI assistant created by Google DeepMind.

