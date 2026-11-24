use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::app::{App, InputMode};
use crate::layout::{highlight_line, slice_line, wrap_line};
use crate::matcher::Matcher;

// Fixed RGB colors (not terminal theme colors, which vary) that stay legible on
// dark backgrounds. The log text itself keeps its own ANSI colors.
const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const GREEN: Color = Color::Rgb(102, 187, 102);
const YELLOW: Color = Color::Rgb(204, 178, 85);
const MAGENTA: Color = Color::Rgb(200, 115, 200);

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width == 0 || area.height == 0 {
        return;
    }
    let [header, main, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);

    // The log goes first: it records which lines are visible for the header
    draw_log(f, app, main);
    draw_header(f, app, header);
    draw_footer(f, app, footer);

    if app.show_help {
        draw_help(f, app);
    }
}

fn panel<'a>(title: impl Into<Line<'a>>, focused: bool) -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { ACCENT } else { DIM }))
        .title(title)
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(" track ", Style::new().bg(ACCENT).fg(Color::Black)),
        Span::raw(" "),
        Span::raw(app.input_source_name.clone()),
    ];
    if app.eof && app.is_at_bottom() {
        spans.push(Span::styled("  end of input", Style::new().fg(DIM)));
    } else if app.following {
        spans.push(Span::styled("  following", Style::new().fg(GREEN)));
    } else {
        spans.push(Span::styled("  paused", Style::new().fg(YELLOW)));
        let below = app.lines_below();
        if below > 0 {
            spans.push(Span::styled(format!(" · {below} below"), Style::new().fg(DIM)));
        }
    }
    let flags: Vec<&str> = [(app.wrap, "wrap"), (app.mouse_capture, "mouse")]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
    if !flags.is_empty() {
        spans.push(Span::styled(format!("  {}", flags.join(" · ")), Style::new().fg(DIM)));
    }

    let total = app.total_display_lines();
    let position = if total == 0 {
        "0 lines ".to_string()
    } else {
        let pct = (app.visible_end * 100).checked_div(total).unwrap_or(100);
        format!("{}-{}/{} · {pct}% ", app.visible_start + 1, app.visible_end, total)
    };
    let used: usize = spans.iter().map(Span::width).sum();
    let width = area.width as usize;
    if used + position.chars().count() < width {
        spans.push(Span::raw(" ".repeat(width - used - position.chars().count())));
        spans.push(Span::styled(position, Style::new().fg(DIM)));
    }
    f.render_widget(Line::from(spans), area);
}

/// Panel titles: filters and horizontal position on the left, the search on the right.
fn log_titles(app: &App) -> (Line<'static>, Option<Line<'static>>) {
    let mut left = vec![Span::styled(" Log", Style::new().fg(ACCENT))];
    if !app.filters.is_empty() {
        let labels: Vec<String> = app.filters.iter().map(Matcher::label).collect();
        left.push(Span::styled(
            format!("  filter: {}", labels.join(" & ")),
            Style::new().fg(MAGENTA),
        ));
        left.push(Span::styled(
            format!("  {} of {} lines", app.total_display_lines(), app.lines.len()),
            Style::new().fg(DIM),
        ));
    }
    if !app.wrap && app.horizontal_offset > 0 {
        left.push(Span::styled(
            format!("  ← col {}", app.horizontal_offset + 1),
            Style::new().fg(DIM),
        ));
    }
    left.push(Span::raw(" "));

    let right = app.search.as_ref().map(|search| {
        let count = match (app.search_match_indices.len(), app.current_match_idx) {
            (0, _) => "no matches".to_string(),
            (n, Some(i)) => format!("{}/{n}", i + 1),
            (1, None) => "1 match".to_string(),
            (n, None) => format!("{n} matches"),
        };
        Line::from(vec![
            Span::styled(format!(" /{}", search.label()), Style::new().fg(YELLOW)),
            Span::styled(format!("  {count} "), Style::new().fg(DIM)),
        ])
        .right_aligned()
    });
    (Line::from(left), right)
}

/// One screen row of the log.
struct Row {
    view_index: usize,
    /// First row of its log line (continuation rows of wrapped lines get no number).
    first: bool,
    line: Line<'static>,
}

fn draw_log(f: &mut Frame, app: &mut App, area: Rect) {
    let (title, search_title) = log_titles(app);
    let mut block = panel(title, true);
    if let Some(search_title) = search_title {
        block = block.title(search_title);
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let total_lines = app.total_display_lines();
    let height = inner.height as usize;
    app.viewport_height = height;

    let (gutter_area, content_area) = if app.show_line_numbers {
        let max_num = (app.dropped_lines + app.lines.len()).max(1);
        let digits = max_num.to_string().len().max(3);
        let gutter_width = (digits + 3) as u16; // e.g. "1234 │ "
        let [gutter, content] =
            Layout::horizontal([Constraint::Length(gutter_width), Constraint::Min(1)])
                .areas(inner);
        (Some((gutter, digits)), content)
    } else {
        (None, inner)
    };
    app.viewport_width = content_area.width as usize;

    if total_lines == 0 {
        app.visible_start = 0;
        app.visible_end = 0;
        let empty_msg = if app.eof {
            "end of input (no lines)"
        } else if !app.filters.is_empty() {
            "no lines match the filter (esc removes it)"
        } else {
            "waiting for input…  (? help, q quit)"
        };
        let p = Paragraph::new(Line::styled(empty_msg, Style::new().fg(DIM)))
            .alignment(Alignment::Center);
        f.render_widget(p, inner);
        return;
    }

    // Keep the offset valid after resizes, filter changes and wrap toggles
    if app.following {
        app.scroll_to_bottom();
    } else {
        app.clamp_position();
    }

    // While typing a search, preview its matches
    let preview;
    let highlight = if app.input_mode == InputMode::Search {
        preview = Matcher::new(app.editor.text(), false);
        preview.as_ref()
    } else {
        app.search.as_ref()
    };

    let rows = layout_rows(app, highlight, height);
    app.visible_start = rows.first().map_or(0, |r| r.view_index);
    app.visible_end = rows.last().map_or(0, |r| r.view_index + 1);

    if let Some((g_area, digits)) = gutter_area {
        let current_match_line = app
            .current_match_idx
            .and_then(|idx| app.search_match_indices.get(idx))
            .copied();
        let gutter_lines: Vec<Line> = rows
            .iter()
            .map(|row| {
                let num = if row.first {
                    app.get_original_line_number(row.view_index).to_string()
                } else {
                    String::new()
                };
                let num_style = if row.first && current_match_line == Some(row.view_index) {
                    Style::new().fg(YELLOW)
                } else {
                    Style::new().fg(DIM)
                };
                Line::from(vec![
                    Span::styled(format!("{num:>digits$}"), num_style),
                    Span::styled(" │", Style::new().fg(DIM)),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(gutter_lines), g_area);
    }

    let content: Vec<Line> = rows.into_iter().map(|row| row.line).collect();
    f.render_widget(Paragraph::new(content), content_area);
}

fn styled_line(app: &App, view_index: usize, highlight: Option<&Matcher>) -> Line<'static> {
    let line = app
        .get_line(view_index)
        .map(|l| l.to_ratatui_line())
        .unwrap_or_default();
    match highlight {
        Some(m) => highlight_line(line, m),
        None => line,
    }
}

/// Lay out the screen rows, slicing (horizontal scroll) or wrapping each log line.
fn layout_rows(app: &App, highlight: Option<&Matcher>, height: usize) -> Vec<Row> {
    let total = app.total_display_lines();
    let width = app.viewport_width;

    if !app.wrap {
        return (app.vertical_offset..total.min(app.vertical_offset + height))
            .map(|i| Row {
                view_index: i,
                first: true,
                line: slice_line(&styled_line(app, i, highlight), app.horizontal_offset, width),
            })
            .collect();
    }

    // The top line may be partly scrolled past (`row_offset` rows)
    let mut rows = Vec::new();
    let mut skip = app.row_offset;
    for i in app.vertical_offset..total {
        if rows.len() >= height {
            break;
        }
        let wrapped = wrap_line(&styled_line(app, i, highlight), width);
        rows.extend(wrapped.into_iter().enumerate().skip(skip).map(|(n, line)| Row {
            view_index: i,
            first: n == 0,
            line,
        }));
        skip = 0;
    }
    rows.truncate(height);
    rows
}

/// `key desc  key desc` hints: keys in the accent color, descriptions dimmed.
fn key_hints(keys: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (k, v) in keys {
        spans.push(Span::styled(k.to_string(), Style::new().fg(ACCENT)));
        spans.push(Span::styled(format!(" {v}  "), Style::new().fg(DIM)));
    }
    spans
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let (prompt, hints): (&str, &[(&str, &str)]) = match app.input_mode {
        InputMode::Normal => {
            let line = match &app.status_message {
                Some(msg) => Line::styled(format!(" {msg}"), Style::new().fg(YELLOW)),
                None => {
                    // Ordered by importance so narrow terminals clip the least useful
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(key_hints(&[
                        ("q", "quit"),
                        ("?", "help"),
                        ("/", "search"),
                        ("&", "filter"),
                        ("n/N", "next/prev"),
                        ("f", "follow"),
                        ("w", "wrap"),
                        ("#", "numbers"),
                        (":", "line"),
                        ("m", "mouse"),
                    ]));
                    Line::from(spans)
                }
            };
            f.render_widget(line, area);
            return;
        }
        InputMode::Search => (
            " / ",
            &[("enter", "search"), ("↑/↓", "history"), ("esc", "cancel")],
        ),
        InputMode::Filter => (
            " & ",
            &[
                ("enter", "add filter"),
                ("!pat", "exclude"),
                ("empty", "clear all"),
                ("esc", "cancel"),
            ],
        ),
        InputMode::Goto => (" line: ", &[("enter", "jump"), ("esc", "cancel")]),
    };

    // Input with a block cursor at the editing position
    let text = app.editor.text();
    let (before, after) = text.split_at(app.editor.cursor());
    let mut after_chars = after.chars();
    let cursor_char = after_chars.next();
    let mut spans = vec![
        Span::styled(prompt, Style::new().fg(ACCENT)),
        Span::raw(before.to_string()),
        match cursor_char {
            Some(c) => Span::styled(c.to_string(), Style::new().reversed()),
            None => Span::styled("█", Style::new().fg(ACCENT)),
        },
        Span::raw(after_chars.as_str().to_string()),
        Span::raw("   "),
    ];
    spans.extend(key_hints(hints));
    f.render_widget(Line::from(spans), area);
}

const HELP_ROWS: &[(&str, &str)] = &[
    ("Navigation", ""),
    ("j/k ↑/↓", "scroll one line (one row when wrapping)"),
    ("ctrl-↑/↓", "scroll five lines"),
    ("ctrl-d/u", "half page"),
    ("PgDn/PgUp, ctrl-f/b", "full page (also space / b)"),
    ("g/G", "top / bottom (bottom resumes following)"),
    (":", "go to line number"),
    ("h/l ←/→", "scroll sideways 4 columns"),
    ("H/L [/] ctrl-←/→", "scroll sideways 20 columns"),
    ("^/$ home/end", "line start / end of the longest visible line"),
    ("Search & filter", ""),
    ("/", "search: regex, smart case, highlighted while typing"),
    ("n / N", "next / previous match (from the screen after scrolling)"),
    ("&", "add a filter: show only matching lines (filters stack)"),
    ("& !pattern", "add an exclude filter: hide matching lines"),
    ("& enter (empty)", "clear all filters"),
    ("esc", "clear the search, then remove the last filter"),
    ("Prompts", ""),
    ("←/→ home/end ctrl-a/e", "move the cursor"),
    ("ctrl-u / ctrl-w", "delete to start / previous word"),
    ("↑/↓", "search / filter history"),
    ("enter / esc", "submit / cancel"),
    ("Display", ""),
    ("f", "toggle following"),
    ("w", "toggle soft wrap"),
    ("#", "toggle line numbers"),
    ("m", "toggle mouse capture (wheel scrolling vs text selection)"),
    ("ctrl-l", "redraw the screen"),
    ("General", ""),
    ("q, ctrl-c", "quit"),
];

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_help(f: &mut Frame, app: &mut App) {
    let lines: Vec<Line> = HELP_ROWS
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                Line::styled(format!(" {k}"), Style::new().fg(ACCENT))
            } else {
                Line::from(vec![
                    Span::styled(format!("   {k:<24}"), Style::new().fg(YELLOW)),
                    Span::raw(*v),
                ])
            }
        })
        .collect();
    let area = centered(f.area(), 88, lines.len() as u16 + 2);
    let max = lines.len().saturating_sub(area.height.saturating_sub(2) as usize);
    app.help_scroll = app.help_scroll.min(max);
    let title = if max > 0 {
        " Help — j/k scroll, any other key closes "
    } else {
        " Help — any key to close "
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .scroll((app.help_scroll.min(u16::MAX as usize) as u16, 0))
            .block(panel(Span::styled(title, Style::new().fg(ACCENT)), true)),
        area,
    );
}
