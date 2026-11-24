use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
};

use crate::app::{App, InputMode};
use crate::layout::{highlight_line, slice_line, wrap_line};
use crate::matcher::Matcher;
use crate::selection::highlight_columns;

// Fixed RGB colors (not terminal theme colors, which vary) that stay legible on
// dark backgrounds. The log text itself keeps its own ANSI colors.
const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const GREEN: Color = Color::Rgb(102, 187, 102);
const YELLOW: Color = Color::Rgb(204, 178, 85);
const MAGENTA: Color = Color::Rgb(200, 115, 200);
const RED: Color = Color::Rgb(217, 106, 106);

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

    draw_prompt(f, app);
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
            spans.push(Span::styled(
                format!(" · {below} below"),
                Style::new().fg(DIM),
            ));
        }
    }
    if app.wrap {
        spans.push(Span::styled("  wrap", Style::new().fg(DIM)));
    }

    // Like monitor's host and git-graph's commit count: the input size on the right
    let position = match app.lines.len() + app.dropped_lines {
        1 => "1 line ".to_string(),
        n => format!("{n} lines "),
    };
    let used: usize = spans.iter().map(Span::width).sum();
    let width = area.width as usize;
    if used + position.chars().count() < width {
        spans.push(Span::raw(
            " ".repeat(width - used - position.chars().count()),
        ));
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
            format!(
                "  {} of {} lines",
                app.total_display_lines(),
                app.lines.len()
            ),
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
        // Dimmed while the highlights are hidden (esc); n / N show them again
        let (query_style, count) = if app.search_hidden {
            (Style::new().fg(DIM), "hidden".to_string())
        } else {
            (Style::new().fg(YELLOW), count)
        };
        Line::from(vec![
            Span::styled(format!(" /{}", search.label()), query_style),
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
    /// Column of the whole log line that the row starts at.
    col_start: usize,
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
    app.log_area = inner;
    app.screen_rows.clear();
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
            Layout::horizontal([Constraint::Length(gutter_width), Constraint::Min(1)]).areas(inner);
        (Some((gutter, digits)), content)
    } else {
        (None, inner)
    };
    app.viewport_width = content_area.width as usize;
    app.content_area = content_area;

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
        app.search.as_ref().filter(|_| !app.search_hidden)
    };

    let rows = layout_rows(app, highlight, height);
    app.visible_start = rows.first().map_or(0, |r| r.view_index);
    app.visible_end = rows.last().map_or(0, |r| r.view_index + 1);
    app.screen_rows = rows.iter().map(|r| (r.view_index, r.col_start)).collect();
    app.resolve_drag();

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

    let content: Vec<Line> = rows
        .into_iter()
        .map(|row| {
            let selected = app
                .selection
                .zip(app.absolute_line(row.view_index))
                .and_then(|(sel, line)| sel.columns(line));
            match selected {
                Some((from, to)) => highlight_columns(
                    row.line,
                    from.saturating_sub(row.col_start),
                    to.saturating_sub(row.col_start),
                ),
                None => row.line,
            }
        })
        .collect();
    f.render_widget(Paragraph::new(content), content_area);
    let visible = app.visible_end - app.visible_start;
    draw_scroll(f, area, true, app.vertical_offset, visible, total_lines);
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
                col_start: app.horizontal_offset,
                line: slice_line(
                    &styled_line(app, i, highlight),
                    app.horizontal_offset,
                    width,
                ),
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
        let mut col_start = 0;
        for (n, line) in wrap_line(&styled_line(app, i, highlight), width)
            .into_iter()
            .enumerate()
        {
            let row_width = line.width();
            if n >= skip {
                rows.push(Row {
                    view_index: i,
                    first: n == 0,
                    col_start,
                    line,
                });
            }
            col_start += row_width;
        }
        skip = 0;
    }
    rows.truncate(height);
    rows
}

/// Footer key hints, the same in track, monitor and git-graph: movement, search,
/// the tool's own keys, then `? help` and `q quit` last. When the line is too
/// narrow, the tool's own keys are dropped from the end first, so help and quit
/// always stay visible.
fn footer_hints(keys: &[(&str, &str)], width: u16) -> Line<'static> {
    let hint_width = |(k, v): &(&str, &str)| Line::raw(*k).width() + Line::raw(*v).width() + 3;
    let mut keys = keys.to_vec();
    while keys.len() > 2 && 1 + keys.iter().map(hint_width).sum::<usize>() > width as usize {
        keys.remove(keys.len() - 3);
    }
    let mut spans = vec![Span::raw(" ")];
    for (k, v) in keys {
        spans.push(Span::styled(k.to_string(), Style::new().fg(ACCENT)));
        spans.push(Span::styled(format!(" {v}  "), Style::new().fg(DIM)));
    }
    Line::from(spans)
}

/// A scrollbar on the right border of a panel, the same in track, monitor and
/// git-graph. It takes the border's color (cyan when the panel is focused) and
/// is drawn only when the content doesn't fit.
fn draw_scroll(
    f: &mut Frame,
    panel: Rect,
    focused: bool,
    offset: usize,
    visible: usize,
    total: usize,
) {
    if total <= visible || panel.height < 3 {
        return;
    }
    let style = Style::new().fg(if focused { ACCENT } else { DIM });
    let mut state = ScrollbarState::new(total - visible)
        .position(offset)
        .viewport_content_length(visible);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(style)
            .track_style(style),
        panel.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = match &app.status_message {
        Some(msg) => Line::styled(format!(" {msg}"), Style::new().fg(YELLOW)),
        // The same words as monitor's and git-graph's footers.
        None => footer_hints(
            &[
                ("j/k", "down/up"),
                ("g/G", "top/bottom"),
                ("/", "search"),
                ("n/N", "next/prev"),
                ("&", "filter"),
                ("w", "wrap"),
                ("?", "help"),
                ("q", "quit"),
            ],
            area.width,
        ),
    };
    f.render_widget(line, area);
}

/// The search / filter / go-to prompt: a small input box near the top of the
/// screen (like snacks.nvim's input), the same in track, monitor and git-graph.
fn draw_prompt(f: &mut Frame, app: &App) {
    let (title, icon, hints): (&str, &str, &[(&str, &str)]) = match app.input_mode {
        InputMode::Search => (
            "Search",
            "\u{f002}",
            &[("enter", "search"), ("↑/↓", "history"), ("esc", "cancel")],
        ),
        InputMode::Filter => (
            "Filter",
            "\u{f0b0}",
            &[
                ("enter", "add"),
                ("!pat", "exclude"),
                ("empty", "clear all"),
                ("esc", "cancel"),
            ],
        ),
        InputMode::Goto => ("Go to line", "#", &[("enter", "jump"), ("esc", "cancel")]),
        InputMode::Normal => return,
    };
    let info = match (app.input_mode, app.preview_count) {
        (InputMode::Search, Some(0)) => Some(Span::styled("no match", Style::new().fg(RED))),
        (InputMode::Search, Some(1)) => Some(Span::styled("1 match", Style::new().fg(DIM))),
        (InputMode::Search, Some(n)) => {
            Some(Span::styled(format!("{n} matches"), Style::new().fg(DIM)))
        }
        _ => None,
    };
    draw_input(
        f,
        title,
        icon,
        app.editor.text(),
        app.editor.cursor(),
        info,
        hints,
    );
}

/// A one-line input box: rounded border, centered title, icon, block cursor,
/// optional info on the right and key hints in the bottom border.
fn draw_input(
    f: &mut Frame,
    title: &str,
    icon: &str,
    text: &str,
    cursor: usize,
    info: Option<Span<'static>>,
    hints: &[(&str, &str)],
) -> Option<Rect> {
    let full = f.area();
    let width = full.width.saturating_sub(4).min(60);
    if width < 10 || full.height < 3 {
        return None;
    }
    let area = Rect {
        x: full.x + (full.width - width) / 2,
        y: full.y + 2.min(full.height - 3),
        width,
        height: 3,
    };
    let mut hint_spans = vec![Span::raw(" ")];
    for (i, (k, v)) in hints.iter().enumerate() {
        if i > 0 {
            hint_spans.push(Span::styled(" · ", Style::new().fg(DIM)));
        }
        hint_spans.push(Span::styled(k.to_string(), Style::new().fg(ACCENT)));
        hint_spans.push(Span::styled(format!(" {v}"), Style::new().fg(DIM)));
    }
    hint_spans.push(Span::raw(" "));
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::styled(format!(" {title} "), Style::new().fg(ACCENT)).centered())
        .title_bottom(Line::from(hint_spans).centered());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let info_width = info.as_ref().map_or(0, |s| s.width() as u16 + 1);
    let [input, info_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(info_width)]).areas(inner);
    // Keep the cursor in view: drop characters from the start of long input
    let room = (input.width as usize).saturating_sub(4);
    let (mut before, after) = text.split_at(cursor);
    while Line::raw(before).width() > room {
        let mut chars = before.chars();
        chars.next();
        before = chars.as_str();
    }
    let mut after_chars = after.chars();
    let spans = vec![
        Span::styled(format!(" {icon} "), Style::new().fg(ACCENT)),
        Span::raw(before.to_string()),
        match after_chars.next() {
            Some(c) => Span::styled(c.to_string(), Style::new().reversed()),
            None => Span::styled("█", Style::new().fg(ACCENT)),
        },
        Span::raw(after_chars.as_str().to_string()),
    ];
    f.render_widget(Line::from(spans), input);
    if let Some(info) = info {
        f.render_widget(Line::from(info).right_aligned(), info_area);
    }
    Some(area)
}

// The same sections and wording as monitor's and git-graph's help.
const HELP_ROWS: &[(&str, &str)] = &[
    ("Navigation", ""),
    ("j/k ↓/↑", "down / up (one row when wrapping)"),
    ("ctrl-d/u", "half page down / up"),
    ("PgDn/PgUp, ctrl-f/b", "page down / up"),
    ("g/G", "top / bottom (G resumes following)"),
    ("h/l ←/→", "left / right 4 columns (when not wrapping)"),
    ("H/L", "left / right half a page (when not wrapping)"),
    ("home/end", "line start / end (when not wrapping)"),
    (":", "go to line number"),
    ("Search & filter", ""),
    ("/", "search (regex, smart case)"),
    ("n / N", "next / previous match"),
    ("&", "add a filter: show only matching lines"),
    ("& !pattern", "add a filter: hide matching lines"),
    ("& enter (empty)", "remove all filters"),
    ("esc", "hide search highlights, then remove the last filter"),
    ("Input box", ""),
    ("←/→ home/end ctrl-a/e", "move the cursor"),
    ("ctrl-u / ctrl-w", "delete to start / previous word"),
    ("↑/↓", "history"),
    ("enter", "submit"),
    ("esc, ctrl-c", "cancel"),
    ("Mouse", ""),
    ("wheel", "scroll (ctrl: faster)"),
    ("drag", "select text; copied on release"),
    ("shift-drag", "the terminal's own selection"),
    ("General", ""),
    ("w", "toggle wrap"),
    ("#", "toggle line numbers"),
    ("r", "refresh (redraw the screen)"),
    ("?", "help"),
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
    let max = lines
        .len()
        .saturating_sub(area.height.saturating_sub(2) as usize);
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
