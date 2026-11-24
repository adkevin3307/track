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
 track  app.log  following                                                             1-8/8 · 100%
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
 q quit  ? help  / search  & filter  n/N next/prev  f follow  w wrap  # numbers  : line  m mouse
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
  uppercase letter); invalid regexes are matched literally. Matches are
  highlighted live while typing; `n`/`N` step through them, continuing from
  the screen after you scroll away.
- **Stackable filters.** `&` shows only matching lines, `&` again narrows
  further, and `&!pattern` hides noise. `esc` removes the most recent one.
  Search and filters match the text as displayed (colors stripped, tabs
  expanded).
- **Line numbers** with `#`. They stay absolute (input line numbers) even
  after old lines are dropped from the buffer, and stay pinned on the left
  when scrolling sideways. `:` jumps to a line number.
- **Text selection friendly.** The mouse is not captured by default, so you
  can select and copy text; `-m` or `m` enables wheel scrolling.
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
| `-m, --mouse` | Capture the mouse for wheel scrolling (disables terminal text selection; toggle with `m`) |
| `-h, --help` | Print help |
| `-V, --version` | Print version |

The old spellings still work: `--max-lines`, `-l` / `--numbers`, and
`-f` / `--follow` (following is now the default).

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
 track  source  following · wrap · mouse                              1-40/812 · 5%
╭ Log  filter: … & …  N of M lines  ← col C ──────────────────── /search  1/12 ╮
│  1 │ log lines, scrolled sideways or wrapped                                  │
│  2 │ (line numbers in the gutter with #)                                      │
╰──────────────────────────────────────────────────────────────────────────────╯
 key hints, the / & : prompts, or a status message
```

The header shows the input, the follow state (`following`, `paused · N below`
or `end of input`), the wrap and mouse flags, and the visible range. The log
panel's title shows the active filters and how many lines they keep, the
column when scrolled sideways, and, on the right, the search and the current
match.

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
| `H`/`L`, `[`/`]`, `ctrl-←`/`ctrl-→` | Scroll sideways 20 columns |
| `^` / `$`, `home` / `end` | Line start / end of the longest visible line |
| mouse wheel | Scroll when mouse capture is on (hold `ctrl` to scroll faster) |

**Search & filter**

| Key | Action |
|---|---|
| `/` | Search (regex, smart case; highlighted live while typing) |
| `n` / `N` | Next / previous match (from the screen after scrolling away) |
| `&` | Add a filter: show only matching lines (filters stack) |
| `&` then `!pattern` | Add an exclude filter: hide matching lines |
| `&` then `enter` (empty) | Clear all filters |
| `esc` | Clear the search first, then remove the most recent filter |

**Prompt editing** (in the `/`, `&` and `:` prompts)

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
| `m` | Toggle mouse capture (wheel scrolling ↔ terminal text selection) |
| `ctrl-l` | Redraw the screen |
| `?` | Help |
| `q`, `ctrl-c` | Quit |

## Development

```sh
cargo test     # reader, scrolling, search/filter, layout and editor tests
cargo clippy
```

| File | Contents |
|---|---|
| `src/reader.rs` | Background reader: stdin and files, partial lines, follow with truncation and rotation |
| `src/app.rs` | Application state: line buffer, scrolling, filters, search and match navigation |
| `src/matcher.rs` | Search / filter patterns: smart-case regex, literal fallback, `!` exclusion |
| `src/layout.rs` | Highlighting, horizontal slicing and wrapping by display width |
| `src/ansi.rs` | ANSI color parsing and tab expansion |
| `src/input.rs` | Prompt line editor and history |
| `src/ui.rs` | Rendering with ratatui |
| `src/main.rs` | Command-line options, terminal setup and the event loop |
