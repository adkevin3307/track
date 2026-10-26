# track

> A simple, blazing fast terminal log viewer with horizontal scrolling, follow mode, and pipe support.

*Developed by Antigravity (Google DeepMind).*
*Reviewed, fixed and extended by Claude Code (Anthropic).*

`track` is designed to be as simple and frictionless as `tail`, but with the ability to scroll horizontally (left/right), search, filter, and inspect long log lines without clutter or complicated configuration. It preserves standard terminal ANSI colors emitted by Docker and CLI tools.

---

## Features

- **Piped & Streaming Input**: Pipe directly from command output, or shell redirects (`<`).
- **Horizontal Scrolling**: Scroll left/right with Arrow keys or `h`/`l` to read wide log entries without line wraps truncating the view. Measured in display columns, so tabs, CJK and emoji line up.
- **Follow Mode (like `tail -F`)**: Files and pipes are followed by default (`FOLLOWING`). Scrolling up pauses (`PAUSED (+N below)`); scrolling back to the bottom or pressing `f`/`G` resumes. Truncated and rotated log files are detected and reopened, and an incomplete last line is shown as it is being written.
- **Soft Wrap Toggle**: Press `w` to toggle between horizontal scrolling and wrapped lines. The newest line always stays visible while following.
- **ANSI Color Support**: Preserves the original colors output by Docker containers and CLI applications while using the terminal's default colors for UI elements.
- **Search & Highlighting**: Press `/` to search with a regular expression (smart case: case-insensitive unless the pattern has an uppercase letter; invalid regexes are matched literally). Matches are highlighted live while typing; jump through them with `n` / `N`.
- **Stackable Filters**: Press `&` to show only matching lines, `&` again to narrow further, and `&!pattern` to hide noise. `Esc` removes the most recent one.
- **Line Numbers**: Press `#` to toggle line numbers. They stay absolute (input line numbers) even after old lines are dropped from the buffer, and stay pinned on the left when scrolling horizontally. Press `:` to jump to a line number.
- **Text Selection Friendly**: The mouse is not captured by default, so you can select and copy text. Use `-m` or press `m` to enable mouse wheel scrolling.
- **Robust Input**: Invalid UTF-8 bytes are shown as `�` instead of stopping the reader.
- **High Performance**: Background streaming reader, incremental filter/search matching, and redraws only when something changes.

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

# Read a file once, starting at the top (don't follow)
track --no-follow my-file

# Keep at most 10,000 lines in memory, wrap long lines, show line numbers
track -b 10000 -w -N my-file

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
| `End` / `$` | Jump to the end of the longest visible line |
| `Up` / `k` | Scroll up 1 line (pauses follow mode) |
| `Down` / `j` | Scroll down 1 line |
| `Ctrl+Up` / `Ctrl+Down` | Scroll up / down 5 lines |
| `PageUp` / `Ctrl+U` / `b` | Scroll up 1 page |
| `PageDown` / `Ctrl+D` / `Space` | Scroll down 1 page |
| `g` / `Ctrl+Home` | Jump to top (first line) |
| `G` / `Ctrl+End` | Jump to bottom & resume follow mode |
| `:` | Go to line number |
| `Mouse Wheel` | Scroll up / down when mouse capture is on (hold `Ctrl` to scroll faster) |

### Modes & Display
| Key | Action |
| --- | --- |
| `f` / `F` | Toggle Follow mode (`FOLLOWING` ↔ `PAUSED`) |
| `w` | Toggle soft line wrapping (`Wrap: ON` ↔ `Wrap: OFF`) |
| `#` | Toggle line numbers |
| `m` | Toggle mouse capture (wheel scrolling ↔ native text selection) |
| `Ctrl+L` | Redraw the screen |
| `?` | Show / hide Help overlay (scroll with `j` / `k`) |

### Search & Filtering
| Key | Action |
| --- | --- |
| `/` | Search (regex, smart case; highlighted live while typing) |
| `n` | Jump to next search match |
| `N` | Jump to previous search match |
| `&` | Add a filter (show only matching lines; filters stack) |
| `&` then `!pattern` | Add an exclude filter (hide matching lines) |
| `&` then `Enter` (empty) | Clear all filters |
| `Esc` | Clear the search first, then remove the most recent filter |

### Prompt Editing (in `/`, `&` and `:` prompts)
| Key | Action |
| --- | --- |
| `Left` / `Right`, `Home` / `End`, `Ctrl+A` / `Ctrl+E` | Move the cursor |
| `Ctrl+U` / `Ctrl+W` | Delete to start of line / delete previous word |
| `Up` / `Down` | Browse search / filter history |
| `Enter` / `Esc` | Submit / cancel (`Backspace` on an empty prompt also cancels) |

### General
| Key | Action |
| --- | --- |
| `q` / `Ctrl+C` | Quit `track` cleanly |

---

## Command-Line Options

```
Usage: track [OPTIONS] [FILE]

Arguments:
  [FILE]  File to view (reads from stdin if not provided)

Options:
  -b, --buffer-lines <LINES>  Maximum lines kept in memory; the oldest lines are dropped [default: 100000]
  -w, --wrap                  Start with line wrapping enabled
  -N, --line-numbers          Start with line numbers displayed
      --no-follow             Read the input once instead of following it, and start at the top
  -m, --mouse                 Capture the mouse for wheel scrolling (disables terminal text selection; toggle with `m`)
  -h, --help                  Print help
  -V, --version               Print version
```

The old spellings still work: `--max-lines`, `-l` / `--numbers`, and `-f` / `--follow` (following is now the default).

---

## Credits

Developed by **Antigravity**, an AI assistant created by Google DeepMind.

Reviewed, fixed and extended by **Claude Code**, an AI coding assistant created by Anthropic:
bug fixes (search highlight crash, invalid UTF-8 handling, absolute line numbers, wrap-mode
following, log rotation), smart-case search, stackable/exclude filters, prompt editing & history,
go-to-line, opt-in mouse capture, and on-demand redraws.

