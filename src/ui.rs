use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{App, InputMode};
use crate::layout::{highlight_line, slice_line, wrap_line};
use crate::matcher::Matcher;

pub fn render(f: &mut Frame, app: &mut App) {
    let size = f.area();
    if size.width == 0 || size.height == 0 {
        return;
    }

    // Top status (1 row), Main viewport (min 1), Bottom bar (1 row)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(size);

    // The viewport goes first: it records which lines are visible for the header
    render_main_viewport(f, app, chunks[1]);
    render_header(f, app, chunks[0]);
    render_footer(f, app, chunks[2]);

    if app.show_help {
        render_help_modal(f, app, size);
    }
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let total_lines = app.total_display_lines();
    let current_top = if total_lines == 0 { 0 } else { app.visible_start + 1 };
    let current_bottom = app.visible_end;
    let pct = (current_bottom * 100).checked_div(total_lines).unwrap_or(100);

    let follow_span = if app.eof && app.is_at_bottom() {
        Span::styled(" [EOF] ", Style::default().fg(Color::Cyan).dim())
    } else if app.following {
        Span::styled(" FOLLOWING ", Style::default().fg(Color::Green))
    } else {
        let below = app.lines_below();
        let text = if below > 0 {
            format!(" PAUSED (+{below} below) ")
        } else {
            " PAUSED ".to_string()
        };
        Span::styled(text, Style::default().fg(Color::Yellow))
    };

    let flag_color = |on: bool| Style::default().fg(if on { Color::Cyan } else { Color::DarkGray });
    let flag_spans = vec![
        Span::styled(
            if app.wrap { "[Wrap:ON] " } else { "[Wrap:OFF] " },
            flag_color(app.wrap),
        ),
        Span::styled("[#] ", flag_color(app.show_line_numbers)),
    ];

    let mut query_spans = Vec::new();
    if !app.filters.is_empty() {
        let labels: Vec<String> = app.filters.iter().map(Matcher::label).collect();
        query_spans.push(Span::styled(
            format!("[Filter: {} ({} lines)] ", labels.join(" & "), total_lines),
            Style::default().fg(Color::Magenta),
        ));
    }
    if let Some(search) = &app.search {
        let match_info = if app.search_match_indices.is_empty() {
            "0 matches".to_string()
        } else {
            let curr = app.current_match_idx.map(|i| i + 1).unwrap_or(0);
            format!("{}/{} matches", curr, app.search_match_indices.len())
        };
        query_spans.push(Span::styled(
            format!("[Search: {} ({})] ", search.label(), match_info),
            Style::default().fg(Color::Yellow),
        ));
    }

    let col_info = if app.wrap {
        "Col: - (wrap)".to_string()
    } else {
        format!("Col: {}", app.horizontal_offset)
    };
    let line_info = if total_lines == 0 {
        "0 lines".to_string()
    } else {
        format!("{}-{} of {} ({}%)", current_top, current_bottom, total_lines, pct)
    };
    let right_text = format!("{} │ {} ", col_info, line_info);

    let mut left_spans = vec![
        Span::styled(" TRACK ", Style::default().fg(Color::Black).bg(Color::White)),
        Span::raw(" "),
        Span::styled(format!("[{}]", app.input_source_name), Style::default().fg(Color::White)),
        follow_span,
    ];

    // On narrow terminals drop the least important parts first: flags, then position
    let width_of = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    let total_width = area.width as usize;
    let essential = width_of(&left_spans) + width_of(&query_spans);
    let flags_fit = essential + width_of(&flag_spans) + right_text.chars().count() <= total_width;
    if flags_fit {
        left_spans.extend(flag_spans);
    }
    left_spans.extend(query_spans);

    let left_len = width_of(&left_spans);
    let right_len = right_text.chars().count();
    if left_len + right_len <= total_width {
        left_spans.push(Span::raw(" ".repeat(total_width - left_len - right_len)));
        left_spans.push(Span::styled(right_text, Style::default().fg(Color::DarkGray)));
    }

    f.render_widget(Paragraph::new(Line::from(left_spans)), area);
}

/// One screen row of the main viewport.
struct Row {
    view_index: usize,
    /// First row of its log line (continuation rows of wrapped lines get no number).
    first: bool,
    line: Line<'static>,
}

fn render_main_viewport(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let total_lines = app.total_display_lines();
    let height = area.height as usize;
    app.viewport_height = height;

    let (gutter_area, content_area) = if app.show_line_numbers {
        let max_num = (app.dropped_lines + app.lines.len()).max(1);
        let digits = max_num.to_string().len().max(3);
        let gutter_width = (digits + 3) as u16; // e.g. " 1234 │ "
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(gutter_width), Constraint::Min(1)])
            .split(area);
        (Some((chunks[0], digits)), chunks[1])
    } else {
        (None, area)
    };
    app.viewport_width = content_area.width as usize;

    if total_lines == 0 {
        app.visible_start = 0;
        app.visible_end = 0;
        let empty_msg = if app.eof {
            "--- End of input (No lines) ---"
        } else if !app.filters.is_empty() {
            "No lines match the filter (Esc removes it)"
        } else {
            "Waiting for log input... (press ? for help, q to quit)"
        };
        let p = Paragraph::new(Line::from(vec![Span::styled(
            empty_msg,
            Style::default().fg(Color::DarkGray).italic(),
        )]))
        .alignment(Alignment::Center);
        f.render_widget(p, area);
        return;
    }

    // Keep the offset valid after resizes, filter changes and wrap toggles
    if app.following {
        app.scroll_to_bottom();
    } else {
        app.vertical_offset = app.vertical_offset.min(app.max_vertical_scroll());
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
                let style = if row.first && current_match_line == Some(row.view_index) {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(Span::styled(format!("{:>width$} │", num, width = digits), style))
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

    // At the bottom in wrap mode, align the last line with the bottom edge (showing the
    // tail of the line above if it doesn't fully fit) so the newest output is always visible.
    let bottom_aligned = app.is_at_bottom();
    let start = if bottom_aligned {
        app.vertical_offset.saturating_sub(1)
    } else {
        app.vertical_offset
    };

    let mut rows = Vec::new();
    for i in start..total {
        if !bottom_aligned && rows.len() >= height {
            break;
        }
        let wrapped = wrap_line(&styled_line(app, i, highlight), width);
        rows.extend(wrapped.into_iter().enumerate().map(|(n, line)| Row {
            view_index: i,
            first: n == 0,
            line,
        }));
    }
    if bottom_aligned {
        rows.drain(..rows.len().saturating_sub(height));
    } else {
        rows.truncate(height);
    }
    rows
}

fn key_hint(key: &'static str, desc: &'static str) -> [Span<'static>; 2] {
    [
        Span::styled(key, Style::default().fg(Color::White).bold()),
        Span::styled(desc, Style::default().fg(Color::DarkGray)),
    ]
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let (label, color, hint) = match app.input_mode {
        InputMode::Normal => {
            let line = if let Some(msg) = &app.status_message {
                Line::from(Span::styled(format!(" {msg}"), Style::default().fg(Color::Yellow)))
            } else {
                // Ordered by importance so clipping on narrow terminals drops the least useful
                let hints = [
                    key_hint(" q", ":quit "),
                    key_hint("?", ":help "),
                    key_hint("/", ":search "),
                    key_hint("&", ":filter "),
                    key_hint("f", ":follow "),
                    key_hint("w", ":wrap "),
                    key_hint("#", ":numbers "),
                    key_hint(":", ":goto "),
                    key_hint("m", ":mouse "),
                    key_hint("h/j/k/l", ":move"),
                ];
                Line::from(hints.into_iter().flatten().collect::<Vec<_>>())
            };
            f.render_widget(Paragraph::new(line), area);
            return;
        }
        InputMode::Search => (" Search: /", Color::Yellow, "  [Enter: search, Esc: cancel, ↑↓: history]"),
        InputMode::Filter => (" Filter: &", Color::Magenta, "  [Enter: add filter, !pat: exclude, empty: clear all]"),
        InputMode::Goto => (" Go to line: ", Color::Cyan, "  [Enter: jump, Esc: cancel]"),
    };

    // Prompt with a block cursor at the editing position
    let text = app.editor.text();
    let (before, after) = text.split_at(app.editor.cursor());
    let mut after_chars = after.chars();
    let cursor_char = after_chars.next();
    let input_style = Style::default().fg(Color::White).bold();
    let content = vec![
        Span::styled(label, Style::default().fg(color).bold()),
        Span::styled(before.to_string(), input_style),
        match cursor_char {
            Some(c) => Span::styled(c.to_string(), input_style.reversed()),
            None => Span::styled("█", Style::default().fg(color)),
        },
        Span::styled(after_chars.as_str().to_string(), input_style),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ];
    f.render_widget(Paragraph::new(Line::from(content)), area);
}

const HELP_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    ("NAVIGATION", &[
        ("Left / h", "Scroll left (4 columns)"),
        ("Right / l", "Scroll right (4 columns)"),
        ("Ctrl+Left / H / [", "Fast scroll left (20 columns)"),
        ("Ctrl+Right / L / ]", "Fast scroll right (20 columns)"),
        ("Home / ^", "Scroll to start of line (column 0)"),
        ("End / $", "Scroll to end of the longest visible line"),
        ("Up / k", "Scroll up 1 line (pauses follow mode)"),
        ("Down / j", "Scroll down 1 line"),
        ("Ctrl+Up / Ctrl+Down", "Fast scroll up / down (5 lines)"),
        ("PageUp / Ctrl+U / b", "Scroll up 1 page"),
        ("PageDn / Ctrl+D / Space", "Scroll down 1 page"),
        ("g / Ctrl+Home", "Jump to top of log (first line)"),
        ("G / Ctrl+End", "Jump to bottom & resume live follow"),
        (":", "Go to line number"),
    ]),
    ("MODES & DISPLAY", &[
        ("f / F", "Toggle follow mode (FOLLOWING / PAUSED)"),
        ("w", "Toggle soft line wrap (wrap vs horizontal scroll)"),
        ("#", "Toggle line numbers gutter (pinned left)"),
        ("m", "Toggle mouse capture (wheel scroll vs text selection)"),
        ("Ctrl+L", "Redraw the screen"),
    ]),
    ("SEARCH & FILTER", &[
        ("/", "Search (regex; case-insensitive unless it has uppercase)"),
        ("n / N", "Jump to next / previous search match"),
        ("&", "Add a filter: show only matching lines (filters stack)"),
        ("& !pattern", "Add an exclude filter: hide matching lines"),
        ("& (empty)", "Clear all filters"),
        ("Esc", "Clear the search, then remove the last filter"),
    ]),
    ("PROMPT EDITING", &[
        ("Left / Right", "Move the cursor (Home / End, Ctrl+A / Ctrl+E)"),
        ("Ctrl+U / Ctrl+W", "Delete to start of line / delete previous word"),
        ("Up / Down", "Browse search / filter history"),
        ("Enter / Esc", "Submit / cancel"),
    ]),
    ("GENERAL", &[
        ("q / Ctrl+C", "Quit track"),
        ("?", "Show / hide this help"),
    ]),
];

fn help_lines() -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, (title, keys)) in HELP_SECTIONS.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!(" {title}"),
            Style::default().fg(Color::Cyan).bold(),
        )));
        for (key, desc) in keys.iter() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<24}"), Style::default().fg(Color::White).bold()),
                Span::styled(*desc, Style::default().fg(Color::Gray)),
            ]));
        }
    }
    lines
}

fn render_help_modal(f: &mut Frame, app: &mut App, area: Rect) {
    let lines = help_lines();
    let modal_width = 84.min(area.width.saturating_sub(4));
    let modal_height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));

    let x = (area.width.saturating_sub(modal_width)) / 2;
    let y = (area.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    // Scrollable when the terminal is too short to show everything
    let inner_height = modal_height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height);
    app.help_scroll = app.help_scroll.min(max_scroll);
    let footer = if max_scroll > 0 {
        " j/k to scroll · Esc, q or ? to close "
    } else {
        " Esc, q or ? to close "
    };

    f.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Track - Keyboard Shortcuts ")
        .title_alignment(Alignment::Center)
        .title_style(Style::default().fg(Color::Cyan).bold())
        .title_bottom(Line::from(Span::styled(footer, Style::default().fg(Color::Yellow))).centered());

    let p = Paragraph::new(lines)
        .block(block)
        .scroll((app.help_scroll.min(u16::MAX as usize) as u16, 0));
    f.render_widget(p, modal_area);
}
