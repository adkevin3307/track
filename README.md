# track

A simple, fast terminal log viewer: as frictionless as `tail -f`, but you can
scroll sideways through long lines, search, filter and jump around without
any configuration.

> **About this README and this code.** The first version of `track` was
> written by Antigravity, an AI assistant made by Google DeepMind. It was then
> reviewed, fixed and extended in Claude Code by Claude, an AI model made by
> Anthropic, working from the repository owner's requests and feedback. The
> owner decided what to build and how it should look and behave; the AI
> assistants implemented it, checked it, and committed each change.

```
 track  app.log  following                                                                 10 lines
╭ Log  filter: !health  8 of 10 lines ──────────────────────────────────────────────── /error  1/2 ╮
│  1 │ 2026-11-24 10:00:01 INFO  api     server listening on 0.0.0.0:8080                          │
│  2 │ 2026-11-24 10:00:02 INFO  db      connected to postgres://db:5432/app (pool=16)             │
│  4 │ 2026-11-24 10:00:07 WARN  worker  job 4812 retrying (attempt 2/5): upstream timeout after 30│
│  5 │ 2026-11-24 10:00:09 INFO  api     POST /orders 201 18ms user=1042 items=3 total=59.90       │
│  6 │ 2026-11-24 10:00:11 ERROR worker  job 4812 failed: connection reset by peer (upstream=paymen│
│  7 │ 2026-11-24 10:00:12 INFO  api     GET /orders/88 200 4ms                                    │
│  8 │ 2026-11-24 10:00:15 WARN  db      slow query 1.2s: SELECT * FROM orders WHERE status = 'pend│
│ 10 │ 2026-11-24 10:00:18 ERROR api     POST /checkout 502 30012ms upstream=payments:443          │
│                                                                                                  │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
 j/k scroll  / search  n/N next/prev  & filter  : line  f follow  w wrap  # numbers  ? help  q quit
```

*(`track -N app.log` with the filter `&!health` and the search `/error`.)*

## Features

- **Pipes, files and redirects.** `cmd | track`, `cmd 2>&1 | track`,
  `track file` or `track < file`. Lines appear as soon as they are written;
  an unfinished line (a prompt, or a progress bar redrawn with `\r`) is shown
  live and updated in place. See [Piping](#piping).
- **Follow mode, like `tail -F`.** Files and pipes are followed by default.
  Scrolling up pauses (`paused · N below`); scrolling back to the bottom or
  pressing `f`/`G` resumes. Truncated and rotated log files are detected and
  reopened.
- **Horizontal scrolling** through wide lines, measured in display columns
  so tabs, CJK and emoji line up.
- **Soft wrap** with `w`. Scrolling then moves by screen rows, so even a line
  taller than the screen can be read in full.
- **ANSI colors preserved** from Docker and other CLI tools; the UI itself
  uses the terminal's default colors and background.
- **Regex search** with smart case (case-insensitive unless the pattern has an
  uppercase letter); invalid regexes are matched literally. `/` opens a small
  input box; matches are highlighted and jumped to while typing, `n`/`N` step
  through them (continuing from the screen after you scroll away), and `esc`
  hides the highlights. See [Shared search](#shared-search).
- **Stackable filters.** `&` shows only matching lines, `&` again narrows
  further, and `&!pattern` hides noise. `esc` removes the most recent one
  (after hiding the search highlights).
  Search and filters match the text as displayed (colors stripped, tabs
  expanded).
- **Line numbers** with `#`. They stay absolute (input line numbers) even
  after old lines are dropped from the buffer, and stay pinned on the left
  when scrolling sideways. `:` jumps to a line number.
- **Mouse support.** The wheel scrolls (in tmux too), and dragging selects
  text and copies it to the clipboard when you release (see
  [Copying text](#copying-text)). Shift + drag still gives the terminal's own
  selection.
- **Robust and light.** Invalid UTF-8 is shown as `�` instead of stopping the
  reader; input is read on a background thread, filters and searches are
  matched incrementally, and the screen is redrawn only when something
  changes. At most 100,000 lines are kept in memory by default.

## Install

Requires Rust 1.88 or newer.

```sh
cargo install --path .
```

This installs a `track` binary into `~/.cargo/bin`.

### Build an executable only

```sh
cargo build --release
```

The binary is written to `target/release/track` (about 2.5 MB). Run it
directly, or copy it anywhere on your `PATH`:

```sh
./target/release/track /var/log/syslog
cp target/release/track ~/.local/bin/
```

## Usage

```
track [OPTIONS] [FILE]
```

`FILE` is the file to view; without it, `track` reads from stdin.

| Option | Description |
|---|---|
| `-b, --buffer-lines <LINES>` | Maximum lines kept in memory; the oldest lines are dropped (default: 100000) |
| `-w, --wrap` | Start with line wrapping enabled |
| `-N, --line-numbers` | Start with line numbers displayed |
| `--no-follow` | Read the input once instead of following it, and start at the top |
| `--no-mouse` | Don't capture the mouse: plain drag uses the terminal's own selection, and the wheel scrolls only where the terminal supports it |
| `-h, --help` | Print help |
| `-V, --version` | Print version |

The old spellings still work: `--max-lines`, `-l` / `--numbers`, and
`-f` / `--follow` (following is now the default). `-m` / `--mouse` is accepted
and ignored, since the mouse is captured by default.

```sh
track app.log                    # view and follow a file
track --no-follow app.log        # read it once, starting at the top
track -b 10000 -w -N app.log     # keep 10,000 lines, wrap, show line numbers
track < app.log                  # shell redirection
my-command | track               # stdout only
my-command 2>&1 | track          # stdout and stderr
```

### Piping

- **Use `2>&1` to see stderr.** Without it, the command's stderr bypasses the
  pipe and is written straight onto the terminal, drawing over `track`'s
  screen (`ctrl-l` redraws).
- **`track` starts at the first complete line**, so the command can still ask
  for a password or other input in the terminal before that.
- **After quitting**, the shell returns once the piped command exits or writes
  again, as with `less`; press `ctrl-c` to stop a command that is waiting
  quietly.
- **Late or out-of-order stdout** is usually the command buffering its output
  because it is writing to a pipe. `stdbuf -oL my-command 2>&1 | track`, or
  `PYTHONUNBUFFERED=1` for Python, makes it line-buffered.

## Layout

```
 track  source  following  wrap                                          812 lines
╭ Log  filter: … & …  N of M lines  ← col C ──────────────────── /search  1/12 ╮
│  1 │ log lines, scrolled sideways or wrapped                                  █
│  2 │ (line numbers in the gutter with #)                                      ║
╰─────────────────────────────────────────────────────────────────── 1-40/812 ╯
 key hints or a status message (the / & : prompts open in a box at the top)
```

The header shows the input, the follow state (`following`, `paused · N below`
or `end of input`), whether wrapping is on, and how many lines were read. The
log panel's title shows the active filters and how many lines they keep, the
column when scrolled sideways, and, on the right, the search and the current
match. When the lines don't fit, a scrollbar runs down the right border and
the visible range is on the bottom border, as in monitor and git-graph.

## Keys

Press `?` in the app for this list (`j`/`k` scroll it if it doesn't fit; any
other key closes it).

**Navigation**

| Key | Action |
|---|---|
| `j`/`k`, `↓`/`↑` | Scroll one line (one screen row in wrap mode); scrolling up pauses follow |
| `ctrl-↓`/`ctrl-↑` | Scroll five lines |
| `ctrl-d`/`ctrl-u` | Half page |
| `PgDn`/`PgUp`, `ctrl-f`/`ctrl-b`, `space`/`b` | Full page |
| `g` / `G`, `ctrl-home` / `ctrl-end` | Top / bottom (bottom resumes follow) |
| `:` | Go to line number |
| `h`/`l`, `←`/`→` | Scroll sideways 4 columns |
| `H`/`L` | Scroll sideways half a page |
| `home` / `end` | Line start / end of the longest visible line |
| mouse wheel | Scroll (hold `ctrl` to scroll faster) |
| mouse drag | Select text; it's copied to the clipboard when you release |
| shift + drag | Your terminal's own text selection |

**Search & filter**

| Key | Action |
|---|---|
| `/` | Search (regex, smart case; highlights and jumps while typing) |
| `n` / `N` | Next / previous match (from the screen after scrolling away); shows hidden highlights |
| `&` | Add a filter: show only matching lines (filters stack) |
| `&` then `!pattern` | Add an exclude filter: hide matching lines |
| `&` then `enter` (empty) | Clear all filters |
| `esc` | Hide the search highlights first, then remove the most recent filter |

**Prompt editing** (in the `/`, `&` and `:` input boxes)

| Key | Action |
|---|---|
| `←`/`→`, `home`/`end`, `ctrl-a`/`ctrl-e` | Move the cursor |
| `ctrl-u` / `ctrl-w` | Delete to start of line / previous word |
| `↑`/`↓` | Browse search / filter history |
| `enter` / `esc` | Submit / cancel (`backspace` on an empty prompt also cancels) |

**Display & general**

| Key | Action |
|---|---|
| `f` / `F` | Toggle follow mode (`following` ↔ `paused`) |
| `w` | Toggle soft wrap |
| `#` | Toggle line numbers |
| `ctrl-l` | Redraw the screen |
| `?` | Help |
| `q`, `ctrl-c` | Quit |

### Shared key guideline

`track`, `monitor` and `git-graph` follow one key guideline, so the same
keys do the same thing in all three:

| Key | Meaning |
|---|---|
| `j`/`k`, `↓`/`↑` | One line down / up |
| `ctrl-d`/`ctrl-u` | Half a page down / up |
| `PgDn`/`PgUp`, `ctrl-f`/`ctrl-b` | A full page down / up |
| `g` / `G` | Top / bottom |
| `h`/`l` | Sideways 4 columns |
| `H`/`L` | Sideways half a page |
| `home` / `end` | Line start / end (not top / bottom) |
| `/`, `n` / `N` | Search, next / previous match |
| `esc` | Hide the search highlights, then go back |
| `?` | Help, in the same window everywhere (`j`/`k` or the wheel scroll it; any other key closes it) |
| `q`, `ctrl-c` | Quit |
| mouse wheel | Scroll (`ctrl` + wheel: faster) |
| mouse drag | Select text and copy it with OSC 52 on release |
| shift + drag | The terminal's own selection |

Other keys are specific to one tool. Where a view doesn't scroll sideways,
`h`/`l`/`H`/`L` may do something else there (in `git-graph`'s commit list
they move between panes and jump to HEAD).

### Shared search

`/` opens the same small input box near the top of the screen in all three
(in the style of snacks.nvim's input):

```
          ╭───────────────────── Search ─────────────────────╮
          │  error█                               12 matches │
          ╰──── enter search · ↑/↓ history · esc cancel ─────╯
```

- Patterns are regular expressions with smart case (case-insensitive unless
  the pattern has an uppercase letter); an invalid regex is matched literally.
- While you type, matches are highlighted, the view jumps to the first match
  from where you started, and the box shows how many there are.
- `enter` keeps the search; `esc` cancels it and goes back to where you were;
  an empty search clears it.
- Editing: `←`/`→`, `home`/`end`, `ctrl-a`/`ctrl-e` move the cursor,
  `ctrl-u`/`ctrl-w` delete to the start / the previous word, `↑`/`↓` browse
  earlier searches, and `backspace` on an empty box closes it.
- The main panel's title shows the search and the current match, e.g.
  `/error  3/12`.
- Matches are highlighted in black on yellow. `esc` hides the highlights but
  keeps the search (the title then says `hidden`), like vim's `:noh`; `n` / `N`
  show them again and jump to the next / previous match.
### Shared screen layout

All three draw the same frame:

```
 tool  what it shows  state · flags that are on              tool-specific info
╭ Panel  details ─────────────────────────────────────── /search  current/total ╮
│ content                                                                       █
│                                                                               ║
╰──────────────────────────────────────────────────────────────────── 1-24/812 ╯
 j/k move  / search  n/N next/prev  tool keys…  esc back  ? help  q quit
```

- The header starts with the tool's name on a cyan badge; flags appear only
  while they are on (e.g. `wrap`, `diff · bell · wrap`).
- Panels have rounded borders; the focused one is cyan.
- A panel whose content doesn't fit has a scrollbar on its right border (cyan
  thumb on a gray track) and the visible range on its bottom border.
- The footer lists keys in the same order everywhere: movement, search, the
  tool's own keys, `esc` back where it applies, then `? help` and `q quit`.
  Keys are cyan and their descriptions gray; on a narrow terminal the tool's
  own keys are dropped first, so help and quit always stay visible. A status
  message (yellow) replaces the hints until the next key.

## Copying text

Drag over the log to select text. Following pauses while you drag, so new
lines can't move the text under the mouse, and dragging past the top or
bottom edge scrolls. When you release the button, the text is copied to the
system clipboard with OSC 52, one log line per line (wrapped lines are joined
back together, and lines hidden by a filter are left out). Windows Terminal,
iTerm2, kitty, WezTerm, Alacritty and foot support OSC 52. Inside tmux, run
`tmux set -s set-clipboard on`.

Shift + drag (Option + drag in iTerm2) still gives your terminal's own
selection, or start `track --no-mouse` to leave the mouse to the terminal.

## Development

```sh
cargo fmt
cargo clippy --all-targets
cargo test     # reader, scrolling, search/filter, layout, selection and editor tests
```

| File | Contents |
|---|---|
| `src/reader.rs` | Background reader: stdin and files, partial lines, follow with truncation and rotation |
| `src/app.rs` | Application state: line buffer, scrolling, filters, search and match navigation |
| `src/matcher.rs` | Search / filter patterns: smart-case regex, literal fallback, `!` exclusion (shared) |
| `src/layout.rs` | Highlighting, horizontal slicing and wrapping by display width |
| `src/ansi.rs` | ANSI color parsing and tab expansion |
| `src/input.rs` | Prompt line editor and history (shared) |
| `src/selection.rs` | Mouse text selection, highlighting and OSC 52 copying |
| `src/ui.rs` | Rendering with ratatui |
| `src/main.rs` | Command-line options, terminal setup and the event loop |

### Code style

track, monitor and git-graph share one code style, so a change can be
carried between them:

- `rustfmt.toml` is the same in all three; run `cargo fmt` before committing.
- `cargo clippy --all-targets` must report nothing, and `cargo test` must pass.
- `src/input.rs` (line editor and history) and `src/matcher.rs` (smart-case
  regex search) are identical copies; change them in all three together.
- The same `[profile.release]` (LTO, one codegen unit, stripped) and
  `rust-version = "1.88"`; dependencies are listed alphabetically, with the
  same versions where they overlap.
