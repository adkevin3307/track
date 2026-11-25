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

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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
/// The current search match's line, like git-graph's selected commit.
const SELECTED_BG: Color = Color::Rgb(45, 55, 75);

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
    let mut status = Vec::new();
    if app.eof && app.is_at_bottom() {
        status.push(Span::styled("  end of input", Style::new().fg(DIM)));
    } else if app.following {
        status.push(Span::styled("  following", Style::new().fg(GREEN)));
    } else {
        status.push(Span::styled("  paused", Style::new().fg(YELLOW)));
        let below = app.lines_below();
        if below > 0 {
            status.push(Span::styled(
                format!(" · {below} below"),
                Style::new().fg(DIM),
            ));
        }
    }
    if app.wrap {
        status.push(Span::styled("  wrap", Style::new().fg(DIM)));
    }
    // Like monitor's host and clock and git-graph's commit position: the input size
    let right = match app.lines.len() + app.dropped_lines {
        1 => "1 line ".to_string(),
        n => format!("{n} lines "),
    };
    let line = header("track", &app.input_source_name, status, right, area.width);
    f.render_widget(line, area);
}

/// The header line, the same in track, monitor and git-graph: the tool's name, then
/// `source` (cut short with "…" when space runs out), then `status`, and `right` at the
/// right edge when everything fits.
fn header(
    name: &str,
    source: &str,
    status: Vec<Span<'static>>,
    right: String,
    width: u16,
) -> Line<'static> {
    let width = width as usize;
    let badge = format!(" {name} ");
    let fixed = badge.width() + 1 + status.iter().map(Span::width).sum::<usize>();
    let mut spans = vec![
        Span::styled(badge, Style::new().bg(ACCENT).fg(Color::Black)),
        Span::raw(" "),
        Span::raw(fit(source, width.saturating_sub(fixed))),
    ];
    spans.extend(status);
    let used: usize = spans.iter().map(Span::width).sum();
    let right_width = right.width();
    if used + 1 + right_width <= width {
        spans.push(Span::raw(" ".repeat(width - used - right_width)));
        spans.push(Span::styled(right, Style::new().fg(DIM)));
    }
    Line::from(spans)
}

/// `text` cut to at most `width` display columns, ending in "…" when it was cut. Widths
/// are measured per grapheme cluster, the way ratatui draws them. The same in track,
/// monitor and git-graph.
fn fit(text: &str, width: usize) -> String {
    let total: usize = text.graphemes(true).map(UnicodeWidthStr::width).sum();
    if total <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in text.graphemes(true) {
        let w = g.width();
        if used + w + 1 > width {
            break;
        }
        out.push_str(g);
        used += w;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// Clears `area` for a popup (search box, help, picker). A double-width character (CJK)
/// half under the popup's left edge is replaced by a space: a terminal can't show half
/// of it, and ratatui would print it whole, over the popup's left border. The same in
/// track, monitor and git-graph.
fn clear_popup(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    let buf = f.buffer_mut();
    if area.x > buf.area.x {
        for y in area.top()..area.bottom() {
            let cell = &mut buf[(area.x - 1, y)];
            if cell.symbol().width() > 1 {
                cell.set_symbol(" ");
            }
        }
    }
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
        let count = match (app.search_match_indices.len(), app.match_cursor()) {
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
        let current_match_line = app.current_match_line();
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

    // The current search match's rows, across the whole width like git-graph's selected
    // commit; painted first, so the text (and match highlights) go on top
    let current_match = app.current_match_line();
    for (i, row) in rows.iter().enumerate() {
        if Some(row.view_index) == current_match {
            let y = content_area.y + i as u16;
            f.buffer_mut().set_style(
                Rect::new(content_area.x, y, content_area.width, 1),
                Style::new().bg(SELECTED_BG),
            );
        }
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
/// narrow, the tool's own keys are dropped from the end first, so help and quit stay
/// visible as long as they fit.
fn footer_hints(keys: &[(&str, &str)], width: u16) -> Line<'static> {
    let hint_width = |(k, v): &(&str, &str)| Line::raw(*k).width() + Line::raw(*v).width() + 3;
    // A leading space and two between hints: no trailing spaces needed
    let needed =
        |keys: &[(&str, &str)]| keys.iter().map(hint_width).sum::<usize>().saturating_sub(1);
    let mut keys = keys.to_vec();
    while keys.len() > 2 && needed(&keys) > width as usize {
        keys.remove(keys.len() - 3);
    }
    // Too narrow even for help and quit: quit alone, then nothing (never half a hint)
    while !keys.is_empty() && needed(&keys) > width as usize {
        keys.remove(0);
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
    clear_popup(f, area);
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
    ("n/N", "next / previous match"),
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
    clear_popup(f, area);
    f.render_widget(
        Paragraph::new(lines)
            .scroll((app.help_scroll.min(u16::MAX as usize) as u16, 0))
            .block(panel(Span::styled(title, Style::new().fg(ACCENT)), true)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn popup_over_cjk_keeps_its_left_border() {
        let mut term = Terminal::new(TestBackend::new(12, 3)).unwrap();
        term.draw(|f| {
            // Double-width characters at columns 0, 2, 4, …
            f.render_widget(Line::raw("中文中文中文"), f.area());
            // The popup's left edge (column 3) covers the right half of 文 (2–3)
            let area = Rect::new(3, 0, 5, 3);
            clear_popup(f, area);
            f.render_widget(Block::bordered(), area);
        })
        .unwrap();
        // What the terminal shows: the half-covered character is gone, the border is drawn
        let shown = term.backend().buffer();
        assert_eq!(shown[(0, 0)].symbol(), "中");
        assert_eq!(shown[(2, 0)].symbol(), " ");
        assert_eq!(shown[(3, 0)].symbol(), "┌");
        assert_eq!(shown[(3, 1)].symbol(), "│");
    }
}
