use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, InputMode, LogLine};

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

    render_header(f, app, chunks[0]);
    render_main_viewport(f, app, chunks[1]);
    render_footer(f, app, chunks[2]);

    if app.show_help {
        render_help_modal(f, size);
    }
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let total_lines = app.total_display_lines();
    let current_top = if total_lines == 0 {
        0
    } else {
        (app.vertical_offset + 1).min(total_lines)
    };
    let current_bottom = (app.vertical_offset + app.viewport_height).min(total_lines);

    let pct = (current_bottom * 100).checked_div(total_lines).unwrap_or(100);

    let follow_span = if app.eof {
        Span::styled(" [EOF] ", Style::default().fg(Color::Cyan).dim())
    } else if app.following {
        Span::styled(
            " FOLLOWING ",
            Style::default().fg(Color::Green),
        )
    } else {
        Span::styled(
            " PAUSED ",
            Style::default().fg(Color::Yellow),
        )
    };

    let wrap_span = if app.wrap {
        Span::styled("[Wrap:ON] ", Style::default().fg(Color::Cyan))
    } else {
        Span::styled("[Wrap:OFF] ", Style::default().fg(Color::DarkGray))
    };

    let num_span = if app.show_line_numbers {
        Span::styled("[#] ", Style::default().fg(Color::Cyan))
    } else {
        Span::styled("[#] ", Style::default().fg(Color::DarkGray))
    };

    let filter_span = if let Some(ref q) = app.filter_query {
        Span::styled(
            format!("[Filter: \"{}\" ({} lines)] ", q, total_lines),
            Style::default().fg(Color::Magenta),
        )
    } else {
        Span::raw("")
    };

    let search_span = if let Some(ref q) = app.search_query {
        let match_info = if app.search_match_indices.is_empty() {
            "0 matches".to_string()
        } else {
            let curr = app.current_match_idx.map(|i| i + 1).unwrap_or(0);
            format!("{}/{} matches", curr, app.search_match_indices.len())
        };
        Span::styled(
            format!("[Search: \"{}\" ({})] ", q, match_info),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::raw("")
    };

    let mut left_spans = vec![
        Span::styled(
            " TRACK ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::White),
        ),
        Span::raw(" "),
        Span::styled(
            format!("[{}]", app.input_source_name),
            Style::default().fg(Color::White),
        ),
        follow_span,
        wrap_span,
        num_span,
        filter_span,
        search_span,
    ];

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
    let right_len = right_text.chars().count();

    // Calculate available space for left items
    let left_len: usize = left_spans.iter().map(|s| s.content.chars().count()).sum();
    let total_width = area.width as usize;

    if left_len + right_len < total_width {
        let padding = total_width - left_len - right_len;
        left_spans.push(Span::raw(" ".repeat(padding)));
    }
    left_spans.push(Span::styled(
        right_text,
        Style::default().fg(Color::DarkGray),
    ));

    let header_line = Line::from(left_spans);
    let header_p = Paragraph::new(header_line).style(Style::default().bg(Color::Reset));
    f.render_widget(header_p, area);
}

fn render_main_viewport(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    app.viewport_height = area.height as usize;
    app.viewport_width = area.width as usize;

    let total_lines = app.total_display_lines();

    if total_lines == 0 {
        let empty_msg = if app.eof {
            "--- End of input (No lines) ---"
        } else {
            "Waiting for log input... (press ? for help, q to quit)"
        };
        let p = Paragraph::new(Line::from(vec![
            Span::styled(empty_msg, Style::default().fg(Color::DarkGray).italic())
        ]))
        .alignment(Alignment::Center);
        f.render_widget(p, area);
        return;
    }

    let height = area.height as usize;
    let start_idx = app.vertical_offset;
    let end_idx = (start_idx + height).min(total_lines);

    let (gutter_area, content_area) = if app.show_line_numbers {
        let max_num = if app.lines.is_empty() { 1 } else { app.lines.len() };
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

    // Render line number gutter
    if let Some((g_area, digits)) = gutter_area {
        let mut gutter_lines = Vec::with_capacity(height);
        for i in start_idx..end_idx {
            let orig_line_num = app.get_original_line_number(i);
            let is_current_match = app.current_match_idx
                .and_then(|idx| app.search_match_indices.get(idx))
                .map(|&match_line| match_line == i)
                .unwrap_or(false);

            let num_str = format!("{:>width$} │", orig_line_num, width = digits);
            let style = if is_current_match {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            gutter_lines.push(Line::from(Span::styled(num_str, style)));
        }
        let gutter_p = Paragraph::new(gutter_lines);
        f.render_widget(gutter_p, g_area);
    }

    // Render log lines
    let mut display_lines = Vec::with_capacity(height);
    for i in start_idx..end_idx {
        if let Some(log_line) = app.get_line(i) {
            let rendered_line = render_log_line(log_line, app.search_query.as_deref());
            display_lines.push(rendered_line);
        }
    }

    let mut content_p = Paragraph::new(display_lines);
    if app.wrap {
        content_p = content_p.wrap(Wrap { trim: false });
    } else {
        content_p = content_p.scroll((0, app.horizontal_offset as u16));
    }
    f.render_widget(content_p, content_area);
}

fn render_log_line(line: &LogLine, search_query: Option<&str>) -> Line<'static> {
    // If there is an active search query and this line matches
    if let Some(query) = search_query.filter(|q| !q.is_empty()) {
        let clean = line.clean_text();
        let lower_clean = clean.to_lowercase();
        let lower_query = query.to_lowercase();

            if lower_clean.contains(&lower_query) {
                // If the line was plain text, do exact span highlighting
                if line.clean.is_none() {
                    let mut spans = Vec::new();
                    let mut last_idx = 0;
                    for (start, _) in lower_clean.match_indices(&lower_query) {
                        let end = start + query.len();
                        if start > last_idx {
                            spans.push(Span::raw(clean[last_idx..start].to_string()));
                        }
                        spans.push(Span::styled(
                            clean[start..end].to_string(),
                            Style::default()
                                .bg(Color::Yellow)
                                .fg(Color::Black),
                        ));
                        last_idx = end;
                    }
                    if last_idx < clean.len() {
                        spans.push(Span::raw(clean[last_idx..].to_string()));
                    }
                    return Line::from(spans);
                } else {
                    // For ANSI-parsed line: parse ANSI spans
                    let parsed = line.to_ratatui_line();
                    // If matched, highlight matching spans or append indicator
                    let mut new_spans = Vec::new();
                    for span in parsed.spans {
                        let span_lower = span.content.to_lowercase();
                        if span_lower.contains(&lower_query) {
                            new_spans.push(Span::styled(
                                span.content.to_string(),
                                span.style.bg(Color::Yellow).fg(Color::Black).bold(),
                            ));
                        } else {
                            new_spans.push(span);
                        }
                    }
                    return Line::from(new_spans);
                }
            }
        }

    line.to_ratatui_line()
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    match app.input_mode {
        InputMode::Normal => {
            let keys = vec![
                Span::styled(" q", Style::default().fg(Color::White).bold()),
                Span::styled(":quit ", Style::default().fg(Color::DarkGray)),
                Span::styled("f", Style::default().fg(Color::White).bold()),
                Span::styled(":follow ", Style::default().fg(Color::DarkGray)),
                Span::styled("h/l", Style::default().fg(Color::White).bold()),
                Span::styled(":left/right ", Style::default().fg(Color::DarkGray)),
                Span::styled("j/k", Style::default().fg(Color::White).bold()),
                Span::styled(":up/down ", Style::default().fg(Color::DarkGray)),
                Span::styled("w", Style::default().fg(Color::White).bold()),
                Span::styled(":wrap ", Style::default().fg(Color::DarkGray)),
                Span::styled("#", Style::default().fg(Color::White).bold()),
                Span::styled(":numbers ", Style::default().fg(Color::DarkGray)),
                Span::styled("/", Style::default().fg(Color::White).bold()),
                Span::styled(":search ", Style::default().fg(Color::DarkGray)),
                Span::styled("&", Style::default().fg(Color::White).bold()),
                Span::styled(":filter ", Style::default().fg(Color::DarkGray)),
                Span::styled("?", Style::default().fg(Color::White).bold()),
                Span::styled(":help", Style::default().fg(Color::DarkGray)),
            ];
            let p = Paragraph::new(Line::from(keys));
            f.render_widget(p, area);
        }
        InputMode::Search => {
            let content = vec![
                Span::styled(" Search: /", Style::default().fg(Color::Yellow).bold()),
                Span::styled(&app.input_buffer, Style::default().fg(Color::White).bold()),
                Span::styled("█", Style::default().fg(Color::Yellow)),
                Span::styled("  [Enter: Search, Esc: Cancel]", Style::default().fg(Color::DarkGray)),
            ];
            let p = Paragraph::new(Line::from(content)).style(Style::default().bg(Color::Reset));
            f.render_widget(p, area);
        }
        InputMode::Filter => {
            let content = vec![
                Span::styled(" Filter: &", Style::default().fg(Color::Magenta).bold()),
                Span::styled(&app.input_buffer, Style::default().fg(Color::White).bold()),
                Span::styled("█", Style::default().fg(Color::Magenta)),
                Span::styled("  [Enter: Filter, Esc: Cancel]", Style::default().fg(Color::DarkGray)),
            ];
            let p = Paragraph::new(Line::from(content)).style(Style::default().bg(Color::Reset));
            f.render_widget(p, area);
        }
    }
}

fn render_help_modal(f: &mut Frame, area: Rect) {
    let modal_width = 72.min(area.width.saturating_sub(4));
    let modal_height = 27.min(area.height.saturating_sub(2));

    let x = (area.width.saturating_sub(modal_width)) / 2;
    let y = (area.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    f.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Track - Simple Log Viewer Help ")
        .title_alignment(Alignment::Center)
        .style(Style::default().fg(Color::White).bg(Color::Black));

    let help_text = vec![
        Line::from(Span::styled(" NAVIGATION", Style::default().fg(Color::Cyan).bold())),
        Line::from(vec![
            Span::styled("  Left / h           ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll left 4 columns", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  Right / l          ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll right 4 columns", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl+Left / H / [  ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll left 20 columns", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl+Right / L / ] ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll right 20 columns", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  Home / 0 / ^       ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll to column 0 (line start)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  End / $            ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll to end of line", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  Up / k             ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll up 1 line (pauses follow)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  Down / j           ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll down 1 line", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl+Up / Ctrl+Down", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll up / down 5 lines", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  PageUp / Ctrl+U    ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll up 1 page", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  PageDn / Ctrl+D    ", Style::default().fg(Color::White).bold()),
            Span::styled("Scroll down 1 page (Space also works)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  g / Ctrl+Home      ", Style::default().fg(Color::White).bold()),
            Span::styled("Jump to top (first line)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  G / Ctrl+End       ", Style::default().fg(Color::White).bold()),
            Span::styled("Jump to bottom & resume follow mode", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" MODES & DISPLAY", Style::default().fg(Color::Cyan).bold())),
        Line::from(vec![
            Span::styled("  f / F              ", Style::default().fg(Color::White).bold()),
            Span::styled("Toggle follow mode (FOLLOWING / PAUSED)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  w                  ", Style::default().fg(Color::White).bold()),
            Span::styled("Toggle soft line wrapping", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  #                  ", Style::default().fg(Color::White).bold()),
            Span::styled("Toggle line numbers", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" SEARCH & FILTER", Style::default().fg(Color::Cyan).bold())),
        Line::from(vec![
            Span::styled("  /                  ", Style::default().fg(Color::White).bold()),
            Span::styled("Search text / regex", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  n / N              ", Style::default().fg(Color::White).bold()),
            Span::styled("Jump to Next / Previous search match", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  &                  ", Style::default().fg(Color::White).bold()),
            Span::styled("Filter mode (show only matching lines)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  Esc                ", Style::default().fg(Color::White).bold()),
            Span::styled("Clear search/filter or close prompt", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  q / Ctrl+C         ", Style::default().fg(Color::White).bold()),
            Span::styled("Quit track", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" Press <Esc>, <q>, or <?> to close this help ", Style::default().fg(Color::Yellow))),
    ];

    let p = Paragraph::new(help_text).block(block);
    f.render_widget(p, modal_area);
}
