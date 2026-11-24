use std::collections::VecDeque;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

use crate::ansi::parse_ansi_line;
use crate::input::{LineEditor, push_history};
use crate::layout::wrapped_row_count;
use crate::matcher::Matcher;
use crate::selection::{Selection, TextPos, slice_columns};

#[derive(Debug, Clone)]
pub struct LogLine {
    pub raw: String,
    /// The text as displayed (ANSI codes removed, tabs expanded) when it differs from `raw`.
    /// Search and filters match against this, so match counts agree with the highlights.
    pub clean: Option<String>,
}

impl LogLine {
    pub fn new(raw: String) -> Self {
        let clean = if raw.contains(['\x1b', '\t']) {
            let line = parse_ansi_line(&raw);
            Some(line.spans.iter().map(|s| s.content.as_ref()).collect())
        } else {
            None
        };
        Self { raw, clean }
    }

    pub fn clean_text(&self) -> &str {
        self.clean.as_deref().unwrap_or(&self.raw)
    }

    pub fn to_ratatui_line(&self) -> Line<'static> {
        parse_ansi_line(&self.raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
    Filter,
    Goto,
}

pub struct App {
    pub lines: VecDeque<LogLine>,
    pub max_buffer_lines: usize,
    pub total_lines_received: usize,
    /// Lines dropped from the front of the buffer, so line numbers stay absolute.
    pub dropped_lines: usize,
    /// The last line is an incomplete line that will be replaced when it completes.
    partial_pending: bool,

    // Scrolling ("view" indices are positions in the filtered view, "real" ones in `lines`)
    /// Top view line, and (in wrap mode) how many of its rows are scrolled past.
    pub vertical_offset: usize,
    pub row_offset: usize,
    pub horizontal_offset: usize,
    pub following: bool,
    pub viewport_height: usize,
    pub viewport_width: usize,
    /// View range drawn in the last frame (set by the renderer).
    pub visible_start: usize,
    pub visible_end: usize,

    // Display flags
    pub wrap: bool,
    pub show_line_numbers: bool,
    pub show_help: bool,
    pub help_scroll: usize,
    pub eof: bool,
    pub input_source_name: String,
    /// One-shot message shown in the footer until the next key press.
    pub status_message: Option<String>,

    // Search and Filter
    pub input_mode: InputMode,
    pub editor: LineEditor,
    pub search_history: Vec<String>,
    pub filter_history: Vec<String>,
    pub search: Option<Matcher>,
    /// Esc hides the search highlights (like vim's :noh); n / N show them again.
    pub search_hidden: bool,
    /// Matches of the search being typed (shown in the input box).
    pub preview_count: Option<usize>,
    /// Scroll position and follow state when the search box opened, restored on cancel.
    search_origin: Option<((usize, usize), bool)>,
    pub filters: Vec<Matcher>,
    pub filtered_indices: Vec<usize>,
    pub search_match_indices: Vec<usize>,
    pub current_match_idx: Option<usize>,
    /// Scroll position right after the last jump to a match; once the view moves away,
    /// n / N search from the screen instead of from the current match.

    // Mouse (layout of the last frame, set by the renderer)
    /// Inner area of the log panel, gutter included.
    pub log_area: Rect,
    /// Where the log text is drawn (right of the gutter).
    pub content_area: Rect,
    /// For each screen row of the log: its view line and the line column it starts at.
    pub screen_rows: Vec<(usize, usize)>,
    pub selection: Option<Selection>,
}

impl App {
    pub fn new(
        input_source_name: String,
        max_buffer_lines: usize,
        wrap: bool,
        show_numbers: bool,
        following: bool,
    ) -> Self {
        Self {
            lines: VecDeque::with_capacity(1000),
            max_buffer_lines: max_buffer_lines.max(1),
            total_lines_received: 0,
            dropped_lines: 0,
            partial_pending: false,

            vertical_offset: 0,
            row_offset: 0,
            horizontal_offset: 0,
            following,
            viewport_height: 20,
            viewport_width: 80,
            visible_start: 0,
            visible_end: 0,

            wrap,
            show_line_numbers: show_numbers,
            show_help: false,
            help_scroll: 0,
            eof: false,
            input_source_name,
            status_message: None,

            input_mode: InputMode::Normal,
            editor: LineEditor::default(),
            search_history: Vec::new(),
            filter_history: Vec::new(),
            search: None,
            search_hidden: false,
            preview_count: None,
            search_origin: None,
            filters: Vec::new(),
            filtered_indices: Vec::new(),
            search_match_indices: Vec::new(),
            current_match_idx: None,

            log_area: Rect::default(),
            content_area: Rect::default(),
            screen_rows: Vec::new(),
            selection: None,
        }
    }

    // ---- Ingestion ----

    /// Append complete lines. A pending partial line is replaced by the first of them.
    pub fn add_lines(&mut self, new_lines: Vec<String>) {
        if new_lines.is_empty() {
            return;
        }
        if self.partial_pending {
            self.remove_last_line();
            self.partial_pending = false;
        }
        self.push_lines(new_lines);
    }

    /// Show (or update) the incomplete last line of a followed file.
    pub fn set_partial_line(&mut self, line: String) {
        if self.partial_pending {
            self.remove_last_line();
        }
        self.push_lines(vec![line]);
        self.partial_pending = true;
    }

    fn push_lines(&mut self, new_lines: Vec<String>) {
        let first_new_real = self.lines.len();
        let first_new_view = self.total_display_lines();
        self.total_lines_received += new_lines.len();
        self.lines.extend(new_lines.into_iter().map(LogLine::new));

        // Match only the new lines against the active filter and search
        if self.is_filtering() {
            for real in first_new_real..self.lines.len() {
                if self.passes_filters(&self.lines[real]) {
                    self.filtered_indices.push(real);
                }
            }
        }
        if let Some(search) = &self.search {
            for view in first_new_view..self.total_display_lines() {
                if self
                    .get_line(view)
                    .is_some_and(|l| search.is_match(l.clean_text()))
                {
                    self.search_match_indices.push(view);
                }
            }
        }

        // Enforce max buffer size, shifting stored indices instead of recomputing
        if self.lines.len() > self.max_buffer_lines {
            let drop_count = self.lines.len() - self.max_buffer_lines;
            self.lines.drain(..drop_count);
            self.dropped_lines += drop_count;

            let view_drop = if self.is_filtering() {
                let filtered_drop = self.filtered_indices.partition_point(|&i| i < drop_count);
                self.filtered_indices.drain(..filtered_drop);
                self.filtered_indices
                    .iter_mut()
                    .for_each(|i| *i -= drop_count);
                filtered_drop
            } else {
                drop_count
            };

            let match_drop = self
                .search_match_indices
                .partition_point(|&i| i < view_drop);
            self.search_match_indices.drain(..match_drop);
            self.search_match_indices
                .iter_mut()
                .for_each(|i| *i -= view_drop);
            self.current_match_idx = self
                .current_match_idx
                .and_then(|c| c.checked_sub(match_drop));

            if self.vertical_offset < view_drop {
                self.row_offset = 0;
            }
            self.vertical_offset = self.vertical_offset.saturating_sub(view_drop);
            // Where the open search box returns to (esc, or each new character)
            self.search_origin = self.search_origin.map(|((line, row), following)| {
                match line.checked_sub(view_drop) {
                    Some(line) => ((line, row), following),
                    None => ((0, 0), following),
                }
            });
        }

        if self.following {
            self.scroll_to_bottom();
        }
    }

    fn remove_last_line(&mut self) {
        let Some(real) = self.lines.len().checked_sub(1) else {
            return;
        };
        let view = if self.is_filtering() {
            if self.filtered_indices.last() == Some(&real) {
                self.filtered_indices.pop();
                Some(self.filtered_indices.len())
            } else {
                None
            }
        } else {
            Some(real)
        };
        if view.is_some() && self.search_match_indices.last() == view.as_ref() {
            self.search_match_indices.pop();
            if self.current_match_idx >= Some(self.search_match_indices.len()) {
                self.current_match_idx = None;
            }
        }
        self.lines.pop_back();
        self.total_lines_received -= 1;
    }

    // ---- View indexing ----

    fn is_filtering(&self) -> bool {
        !self.filters.is_empty()
    }

    fn passes_filters(&self, line: &LogLine) -> bool {
        self.filters.iter().all(|f| f.is_match(line.clean_text()))
    }

    pub fn total_display_lines(&self) -> usize {
        if self.is_filtering() {
            self.filtered_indices.len()
        } else {
            self.lines.len()
        }
    }

    fn view_to_real(&self, view_index: usize) -> Option<usize> {
        if self.is_filtering() {
            self.filtered_indices.get(view_index).copied()
        } else {
            (view_index < self.lines.len()).then_some(view_index)
        }
    }

    /// The first view index showing real line `real` or a later one.
    fn real_to_view(&self, real: usize) -> usize {
        if self.is_filtering() {
            self.filtered_indices.partition_point(|&i| i < real)
        } else {
            real
        }
    }

    pub fn get_line(&self, view_index: usize) -> Option<&LogLine> {
        self.lines.get(self.view_to_real(view_index)?)
    }

    /// 1-based line number in the whole input (stable across buffer trimming).
    pub fn get_original_line_number(&self, view_index: usize) -> usize {
        let real = self.view_to_real(view_index).unwrap_or(view_index);
        self.dropped_lines + real + 1
    }

    /// Absolute (0-based) line number of a view line, stable across buffer trimming.
    pub fn absolute_line(&self, view_index: usize) -> Option<usize> {
        Some(self.dropped_lines + self.view_to_real(view_index)?)
    }

    // ---- Vertical scrolling ----

    /// Screen rows the view line occupies (more than one only in wrap mode).
    pub fn line_rows(&self, view_index: usize) -> usize {
        match self.get_line(view_index) {
            Some(line) if self.wrap => {
                wrapped_row_count(&line.to_ratatui_line(), self.viewport_width)
            }
            _ => 1,
        }
    }

    /// Current scroll position: (top view line, rows of it scrolled past).
    pub fn position(&self) -> (usize, usize) {
        (self.vertical_offset, self.row_offset)
    }

    fn set_position(&mut self, (line, row): (usize, usize)) {
        self.vertical_offset = line;
        self.row_offset = row;
    }

    /// The last position that still fills the viewport down to the last row.
    pub fn max_position(&self) -> (usize, usize) {
        let total = self.total_display_lines();
        if !self.wrap {
            return (total.saturating_sub(self.viewport_height), 0);
        }
        let mut needed = self.viewport_height.max(1);
        for line in (0..total).rev() {
            let rows = self.line_rows(line);
            if rows >= needed {
                return (line, rows - needed);
            }
            needed -= rows;
        }
        (0, 0)
    }

    /// Bring the position back into range (after resizes, filter changes, wrap toggles).
    pub fn clamp_position(&mut self) {
        if self.wrap {
            let rows = self.line_rows(self.vertical_offset);
            self.row_offset = self.row_offset.min(rows.saturating_sub(1));
        } else {
            self.row_offset = 0;
        }
        let pos = self.position().min(self.max_position());
        self.set_position(pos);
    }

    /// `pos` moved down by `amount` screen rows, stopping at the bottom.
    fn advance(&self, (mut line, mut row): (usize, usize), mut amount: usize) -> (usize, usize) {
        let max = self.max_position();
        if !self.wrap {
            return (line.saturating_add(amount), 0).min(max);
        }
        while amount > 0 && (line, row) < max {
            let rows = self.line_rows(line);
            if row + 1 < rows {
                let step = (rows - 1 - row).min(amount);
                row += step;
                amount -= step;
            } else {
                line += 1;
                row = 0;
                amount -= 1;
            }
        }
        (line, row).min(max)
    }

    /// `pos` moved up by `amount` screen rows, stopping at the top.
    fn retreat(&self, (mut line, mut row): (usize, usize), mut amount: usize) -> (usize, usize) {
        if !self.wrap {
            return (line.saturating_sub(amount), 0);
        }
        while amount > 0 {
            if row > 0 {
                let step = row.min(amount);
                row -= step;
                amount -= step;
            } else if line > 0 {
                line -= 1;
                row = self.line_rows(line) - 1;
                amount -= 1;
            } else {
                break;
            }
        }
        (line, row)
    }

    /// One past the last view line on screen.
    fn bottom_line(&self) -> usize {
        let total = self.total_display_lines();
        if !self.wrap {
            return self
                .vertical_offset
                .saturating_add(self.viewport_height)
                .min(total);
        }
        // The top line's rows above `row_offset` are scrolled past
        let mut rows = 0;
        let mut skip = self.row_offset;
        let mut line = self.vertical_offset;
        while line < total && rows < self.viewport_height {
            rows += self.line_rows(line).saturating_sub(skip);
            skip = 0;
            line += 1;
        }
        line
    }

    pub fn is_at_bottom(&self) -> bool {
        self.position() >= self.max_position()
    }

    /// Lines after the last one drawn on screen.
    pub fn lines_below(&self) -> usize {
        self.total_display_lines().saturating_sub(self.visible_end)
    }

    pub fn scroll_to_bottom(&mut self) {
        let max = self.max_position();
        self.set_position(max);
    }

    /// Scroll up by screen rows (log lines unless wrapping).
    pub fn scroll_up(&mut self, amount: usize) {
        self.following = false;
        let pos = self.retreat(self.position(), amount);
        self.set_position(pos);
    }

    /// Scroll down by screen rows; reaching the bottom resumes following.
    pub fn scroll_down(&mut self, amount: usize) {
        let pos = self.advance(self.position(), amount);
        self.set_position(pos);
        if self.is_at_bottom() {
            self.following = true;
        }
    }

    pub fn scroll_half_page_up(&mut self) {
        self.scroll_up((self.viewport_height / 2).max(1));
    }

    pub fn scroll_half_page_down(&mut self) {
        self.scroll_down((self.viewport_height / 2).max(1));
    }

    pub fn scroll_page_up(&mut self) {
        let page = self.viewport_height.saturating_sub(2).max(1);
        self.scroll_up(page);
    }

    pub fn scroll_page_down(&mut self) {
        let page = self.viewport_height.saturating_sub(2).max(1);
        self.scroll_down(page);
    }

    pub fn scroll_to_top(&mut self) {
        self.following = false;
        self.set_position((0, 0));
    }

    pub fn scroll_to_end(&mut self) {
        self.scroll_to_bottom();
        self.following = true;
    }

    // ---- Horizontal scrolling ----

    /// Display width of the widest line currently in the viewport.
    fn max_visible_width(&self) -> usize {
        let total = self.total_display_lines();
        let start = self.vertical_offset.min(total);
        let end = start.saturating_add(self.viewport_height).min(total);
        (start..end)
            .filter_map(|i| self.get_line(i))
            .map(|l| l.to_ratatui_line().width())
            .max()
            .unwrap_or(0)
    }

    fn max_horizontal_scroll(&self) -> usize {
        if self.wrap {
            return 0;
        }
        let max_len = self.max_visible_width();
        if max_len > self.viewport_width {
            max_len - self.viewport_width + 2
        } else {
            0
        }
    }

    pub fn scroll_left(&mut self, amount: usize) {
        self.horizontal_offset = self.horizontal_offset.saturating_sub(amount);
    }

    pub fn scroll_right(&mut self, amount: usize) {
        // Never scroll past the end of the visible lines (but don't jump back either)
        let limit = self.max_horizontal_scroll().max(self.horizontal_offset);
        self.horizontal_offset = self.horizontal_offset.saturating_add(amount).min(limit);
    }

    pub fn scroll_to_col_zero(&mut self) {
        self.horizontal_offset = 0;
    }

    pub fn scroll_to_line_end(&mut self) {
        if !self.wrap {
            self.horizontal_offset = self.max_horizontal_scroll();
        }
    }

    // ---- Toggles ----

    pub fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
        self.row_offset = 0;
        if self.wrap {
            self.horizontal_offset = 0;
        }
        if self.following {
            self.scroll_to_bottom();
        }
    }

    pub fn toggle_line_numbers(&mut self) {
        self.show_line_numbers = !self.show_line_numbers;
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
        self.help_scroll = 0;
    }

    // ---- Prompts ----

    pub fn start_input(&mut self, mode: InputMode) {
        self.input_mode = mode;
        self.editor.clear();
        self.preview_count = None;
        if mode == InputMode::Search {
            self.search_origin = Some((self.position(), self.following));
        }
    }

    pub fn cancel_input(&mut self) {
        self.restore_search_origin();
        self.search_origin = None;
        self.input_mode = InputMode::Normal;
        self.editor.clear();
    }

    fn restore_search_origin(&mut self) {
        if let Some((pos, following)) = self.search_origin {
            self.set_position(pos);
            self.following = following;
        }
    }

    /// While typing a search: count its matches and jump to the first one at or
    /// after where the search started (wrapping around), like vim's incsearch.
    pub fn preview_search(&mut self) {
        self.restore_search_origin();
        let Some(matcher) = Matcher::new(self.editor.text(), false) else {
            self.preview_count = None;
            return;
        };
        let total = self.total_display_lines();
        let matches: Vec<usize> = (0..total)
            .filter(|&v| {
                self.get_line(v)
                    .is_some_and(|l| matcher.is_match(l.clean_text()))
            })
            .collect();
        self.preview_count = Some(matches.len());
        let top = self.vertical_offset;
        let target = matches.iter().find(|&&v| v >= top).or(matches.first());
        if let Some(&line) = target {
            self.reveal_line(line);
        }
    }

    pub fn history_prev(&mut self) {
        match self.input_mode {
            InputMode::Search => self.editor.history_prev(&self.search_history),
            InputMode::Filter => self.editor.history_prev(&self.filter_history),
            _ => {}
        }
    }

    pub fn history_next(&mut self) {
        match self.input_mode {
            InputMode::Search => self.editor.history_next(&self.search_history),
            InputMode::Filter => self.editor.history_next(&self.filter_history),
            _ => {}
        }
    }

    pub fn submit_input(&mut self) {
        // Spaces are significant in patterns; an all-blank pattern counts as empty
        let text = self.editor.text();
        let query = if text.trim().is_empty() {
            String::new()
        } else {
            text.to_string()
        };
        let mode = self.input_mode;
        self.input_mode = InputMode::Normal;
        self.editor.clear();

        match mode {
            InputMode::Search => {
                // Search again from where the search started
                self.restore_search_origin();
                self.search_origin = None;
                push_history(&mut self.search_history, &query);
                self.set_search(Matcher::new(&query, false));
            }
            InputMode::Filter => match Matcher::new(&query, true) {
                Some(filter) => {
                    push_history(&mut self.filter_history, &query);
                    let mut filters = self.filters.clone();
                    filters.push(filter);
                    self.set_filters(filters);
                }
                // Enter on an empty box removes all filters
                None if query.is_empty() => {
                    if self.is_filtering() {
                        self.set_filters(Vec::new());
                        self.status_message = Some("All filters cleared".to_string());
                    }
                }
                // A lone `!` has nothing to hide
                None => self.status_message = Some("Empty pattern".to_string()),
            },
            InputMode::Goto => match query.trim().parse::<usize>() {
                Ok(n) if n > 0 => self.goto_line(n),
                _ if query.is_empty() => {}
                _ => self.status_message = Some(format!("Invalid line number: {}", query.trim())),
            },
            InputMode::Normal => {}
        }
    }

    /// Esc in normal mode: hide the search highlights first (n / N show them again),
    /// then remove filters one at a time.
    pub fn clear_last_search_or_filter(&mut self) {
        if self.search.is_some() && !self.search_hidden {
            self.search_hidden = true;
        } else if let Some(removed) = self.filters.last().map(Matcher::label) {
            let mut filters = self.filters.clone();
            filters.pop();
            self.set_filters(filters);
            self.status_message = Some(format!("Filter removed: {removed}"));
        }
    }

    fn set_search(&mut self, search: Option<Matcher>) {
        self.search = search;
        self.search_hidden = false;
        self.recompute_search_matches();
        if self.search.is_some() {
            if self.search_match_indices.is_empty() {
                let label = self.search.as_ref().map(Matcher::label).unwrap_or_default();
                self.status_message = Some(format!("Pattern not found: {label}"));
            } else {
                self.jump_to_first_visible_match();
            }
        }
    }

    /// Replace the filter stack, keeping the same real line at the top of the view.
    fn set_filters(&mut self, filters: Vec<Matcher>) {
        let anchor = self.view_to_real(self.vertical_offset);
        self.filters = filters;
        self.recompute_filter();
        self.recompute_search_matches();

        if self.following {
            self.scroll_to_bottom();
        } else {
            let view = anchor.map_or(0, |real| self.real_to_view(real));
            self.set_position((view, 0).min(self.max_position()));
        }
    }

    fn recompute_filter(&mut self) {
        self.filtered_indices.clear();
        if self.is_filtering() {
            self.filtered_indices = (0..self.lines.len())
                .filter(|&real| self.passes_filters(&self.lines[real]))
                .collect();
        }
    }

    fn recompute_search_matches(&mut self) {
        self.search_match_indices.clear();
        self.current_match_idx = None;
        if let Some(search) = &self.search {
            self.search_match_indices = (0..self.total_display_lines())
                .filter(|&view| {
                    self.get_line(view)
                        .is_some_and(|l| search.is_match(l.clean_text()))
                })
                .collect();
        }
    }

    // ---- Match navigation ----

    /// Returns false (and sets a status message) when there is nothing to jump to.
    fn check_can_jump(&mut self) -> bool {
        self.search_hidden = false;
        match &self.search {
            None => {
                self.status_message = Some("No active search (press / to search)".to_string());
                false
            }
            Some(search) if self.search_match_indices.is_empty() => {
                self.status_message = Some(format!("Pattern not found: {}", search.label()));
                false
            }
            Some(_) => true,
        }
    }

    /// The current match (index into the matches), while its line is on screen: n and N
    /// continue from it. Once it is scrolled away they start from the screen instead.
    pub fn match_cursor(&self) -> Option<usize> {
        let idx = self.current_match_idx?;
        let line = *self.search_match_indices.get(idx)?;
        (line >= self.vertical_offset && line < self.bottom_line()).then_some(idx)
    }

    /// The view line of the current match, highlighted like git-graph's selected commit
    /// (not while esc has hidden the search highlights).
    pub fn current_match_line(&self) -> Option<usize> {
        if self.search_hidden {
            return None;
        }
        self.match_cursor()
            .map(|idx| self.search_match_indices[idx])
    }

    /// n: the match after the current one, or the first one from the top of the screen.
    pub fn jump_to_next_match(&mut self) {
        if !self.check_can_jump() {
            return;
        }
        let next = match self.match_cursor() {
            Some(curr) => Some(curr + 1).filter(|&i| i < self.search_match_indices.len()),
            None => {
                let top = self.vertical_offset;
                self.search_match_indices
                    .iter()
                    .position(|&line| line >= top)
            }
        };
        let idx = next.unwrap_or_else(|| {
            self.status_message = Some("Search hit BOTTOM, continuing at TOP".to_string());
            0
        });
        self.jump_to_match(idx);
    }

    /// N: the match before the current one, or the last one from the bottom of the screen.
    pub fn jump_to_prev_match(&mut self) {
        if !self.check_can_jump() {
            return;
        }
        let prev = match self.match_cursor() {
            Some(curr) => curr.checked_sub(1),
            None => {
                let bottom = self.bottom_line();
                self.search_match_indices
                    .iter()
                    .rposition(|&line| line < bottom)
            }
        };
        let idx = prev.unwrap_or_else(|| {
            self.status_message = Some("Search hit TOP, continuing at BOTTOM".to_string());
            self.search_match_indices.len() - 1
        });
        self.jump_to_match(idx);
    }

    /// Jump to the first match from the top of the screen (after a new search).
    fn jump_to_first_visible_match(&mut self) {
        let top = self.vertical_offset;
        let idx = self
            .search_match_indices
            .iter()
            .position(|&line| line >= top)
            .unwrap_or(0);
        self.jump_to_match(idx);
    }

    fn jump_to_match(&mut self, idx: usize) {
        self.current_match_idx = Some(idx);
        self.reveal_line(self.search_match_indices[idx]);
    }

    /// Jump to an absolute (1-based) input line number.
    pub fn goto_line(&mut self, line_number: usize) {
        if line_number <= self.dropped_lines {
            self.status_message = Some(format!(
                "Line {line_number} is no longer in the buffer (first is {})",
                self.dropped_lines + 1
            ));
            return;
        }
        let real = line_number - 1 - self.dropped_lines;
        if real >= self.lines.len() {
            self.status_message = Some(format!(
                "Line {line_number} is past the end (last is {})",
                self.dropped_lines + self.lines.len()
            ));
            return;
        }
        let view = self.real_to_view(real);
        if self.view_to_real(view) != Some(real) {
            self.status_message = Some(format!("Line {line_number} is hidden by the filter"));
        }
        self.jump_to_line(view.min(self.total_display_lines().saturating_sub(1)));
    }

    /// Scrolls only as far as needed to show view line `target` with a few lines of
    /// context, the way git-graph keeps its selected commit in view (search jumps).
    fn reveal_line(&mut self, target: usize) {
        self.following = false;
        let margin = 3.min(self.viewport_height / 4);
        let (top, top_row) = self.position();
        let above = target < top || (target == top && top_row > 0);
        // Screen row of the target's first row, when it starts on screen
        let first_row = (!above && target < self.bottom_line())
            .then(|| (top..target).map(|l| self.line_rows(l)).sum::<usize>() - top_row);
        let rows = self.line_rows(target);
        let pos = match first_row {
            Some(r) if r >= margin && r + rows + margin <= self.viewport_height => return,
            // Close to (or above) the top, or too tall to fit: context above it
            Some(r) if r < margin => self.retreat((target, 0), margin),
            None if above => self.retreat((target, 0), margin),
            _ if rows + 2 * margin > self.viewport_height => self.retreat((target, 0), margin),
            // Close to (or below) the bottom: context below it
            _ => self.retreat(
                (target, rows - 1),
                self.viewport_height.saturating_sub(margin + 1),
            ),
        };
        self.set_position(pos.min(self.max_position()));
    }

    fn jump_to_line(&mut self, line_idx: usize) {
        self.following = false;
        // Center the target in the viewport if possible
        let half = self.viewport_height / 2;
        let pos = self.retreat((line_idx, 0), half).min(self.max_position());
        self.set_position(pos);
    }

    // ---- Mouse ----

    /// Handles a mouse event. Returns the text to copy when a selection is finished.
    pub fn on_mouse(&mut self, mouse: MouseEvent) -> Option<String> {
        let fast = mouse.modifiers.contains(KeyModifiers::CONTROL);
        if self.show_help {
            match mouse.kind {
                MouseEventKind::ScrollUp => self.help_scroll = self.help_scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => self.help_scroll += 3,
                _ => {}
            }
            return None;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_up(if fast { 15 } else { 3 }),
            MouseEventKind::ScrollDown => self.scroll_down(if fast { 15 } else { 3 }),
            MouseEventKind::ScrollLeft => self.scroll_left(if fast { 20 } else { 4 }),
            MouseEventKind::ScrollRight => self.scroll_right(if fast { 20 } else { 4 }),
            MouseEventKind::Down(MouseButton::Left) => {
                self.status_message = None;
                self.cancel_selection();
                if self
                    .log_area
                    .contains(Position::new(mouse.column, mouse.row))
                    && let Some(at) = self.text_pos_at(mouse.column, mouse.row)
                {
                    self.selection = Some(Selection::new(at, self.following));
                    self.following = false;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selection.is_some() => {
                // Dragging past the top or bottom edge scrolls (without resuming following)
                if mouse.row < self.content_area.y {
                    self.scroll_up(1);
                } else if mouse.row >= self.content_area.bottom() {
                    self.scroll_down(1);
                    self.following = false;
                }
                if let Some(sel) = self.selection.as_mut() {
                    sel.dragged = true;
                    // Resolved after the next layout, which reflects the scrolling above
                    sel.pending = Some((mouse.column, mouse.row));
                }
            }
            MouseEventKind::Up(MouseButton::Left) if self.selection.is_some() => {
                self.resolve_drag();
                let sel = self.selection.take()?;
                self.following = sel.was_following;
                let text = self.selected_text(&sel);
                if text.is_empty() {
                    return None;
                }
                // main copies it and shows how that went
                return Some(text);
            }
            _ => {}
        }
        None
    }

    /// Drops an unfinished selection (e.g. on a key press) and resumes following if it was on.
    pub fn cancel_selection(&mut self) {
        if let Some(sel) = self.selection.take() {
            self.following |= sel.was_following;
        }
    }

    /// Moves the selection cursor to the latest drag position, using the last layout.
    pub fn resolve_drag(&mut self) {
        let Some((x, y)) = self.selection.as_mut().and_then(|s| s.pending.take()) else {
            return;
        };
        if let Some(at) = self.text_pos_at(x, y)
            && let Some(sel) = self.selection.as_mut()
        {
            sel.cursor = at;
        }
    }

    /// The log position under terminal cell (x, y), clamped to the log text.
    fn text_pos_at(&self, x: u16, y: u16) -> Option<TextPos> {
        let area = self.content_area;
        let &(last_view, _) = self.screen_rows.last()?;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let row = (y.clamp(area.y, area.bottom() - 1) - area.y) as usize;
        let (view, col) = match self.screen_rows.get(row) {
            Some(&(view, col_start)) => (
                view,
                col_start + (x.clamp(area.x, area.right() - 1) - area.x) as usize,
            ),
            // Below the last line: its end
            None => (last_view, usize::MAX / 2),
        };
        Some(TextPos {
            line: self.absolute_line(view)?,
            col,
        })
    }

    /// The selected text of the lines still shown, one log line per text line.
    pub fn selected_text(&self, sel: &Selection) -> String {
        let (start, end) = sel.range();
        let first_real = start.line.saturating_sub(self.dropped_lines);
        let mut lines = Vec::new();
        for view in self.real_to_view(first_real)..self.total_display_lines() {
            let Some(line) = self.absolute_line(view) else {
                break;
            };
            if line > end.line {
                break;
            }
            let Some((from, to)) = sel.columns(line) else {
                continue;
            };
            if let Some(log_line) = self.get_line(view) {
                lines.push(slice_columns(log_line.clean_text(), from, to));
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(lines: &[&str]) -> App {
        let mut app = App::new("test".to_string(), 100, false, false, true);
        app.viewport_height = 10;
        app.add_lines(lines.iter().map(|s| s.to_string()).collect());
        app
    }

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) -> Option<String> {
        app.on_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn test_drag_selects_and_copies_text() {
        let mut app = app_with(&["alpha one", "beta two", "gamma three", "delta four"]);
        // Log text drawn at (1, 1), one row per line, scrolled 2 columns sideways
        app.log_area = Rect::new(1, 1, 20, 4);
        app.content_area = app.log_area;
        app.screen_rows = (0..4).map(|v| (v, 2)).collect();

        let left = MouseButton::Left;
        assert_eq!(mouse(&mut app, MouseEventKind::Down(left), 2, 1), None);
        assert!(!app.following, "following pauses while selecting");
        mouse(&mut app, MouseEventKind::Drag(left), 3, 3);
        let copied = mouse(&mut app, MouseEventKind::Up(left), 3, 3);
        // From column 3 of line 1 to column 4 (inclusive) of line 3
        assert_eq!(copied.as_deref(), Some("ha one\nbeta two\ngamma"));
        assert!(app.following, "following resumes on release");
        assert!(app.selection.is_none());

        // A plain click copies nothing
        mouse(&mut app, MouseEventKind::Down(left), 2, 1);
        assert_eq!(mouse(&mut app, MouseEventKind::Up(left), 2, 1), None);
    }

    #[test]
    fn test_selected_text_skips_filtered_lines() {
        let mut app = app_with(&["a 1", "b 2", "a 3", "b 4"]);
        submit(&mut app, InputMode::Filter, "a");
        let mut sel = Selection::new(TextPos { line: 0, col: 0 }, false);
        sel.cursor = TextPos { line: 3, col: 99 };
        sel.dragged = true;
        assert_eq!(app.selected_text(&sel), "a 1\na 3");
    }

    fn submit(app: &mut App, mode: InputMode, text: &str) {
        app.start_input(mode);
        app.editor.set_text(text);
        app.submit_input();
    }

    fn view_texts(app: &App) -> Vec<String> {
        (0..app.total_display_lines())
            .map(|i| app.get_line(i).unwrap().raw.clone())
            .collect()
    }

    #[test]
    fn test_horizontal_scrolling() {
        let mut app = App::new("test".to_string(), 100, false, false, true);
        app.viewport_width = 10;
        app.add_lines(vec!["x".repeat(100)]);
        assert_eq!(app.horizontal_offset, 0);

        app.scroll_right(4);
        assert_eq!(app.horizontal_offset, 4);

        app.scroll_right(20);
        assert_eq!(app.horizontal_offset, 24);

        app.scroll_left(4);
        assert_eq!(app.horizontal_offset, 20);

        app.scroll_left(50); // should not underflow
        assert_eq!(app.horizontal_offset, 0);

        app.scroll_right(15);
        app.scroll_to_col_zero();
        assert_eq!(app.horizontal_offset, 0);

        // Scrolling right stops at the end of the longest visible line
        app.scroll_right(1000);
        assert_eq!(app.horizontal_offset, 100 - 10 + 2);
    }

    #[test]
    fn test_scroll_to_line_end() {
        let mut app = App::new("test".to_string(), 100, false, false, true);
        app.viewport_width = 20;
        app.viewport_height = 5;

        // No lines
        app.scroll_to_line_end();
        assert_eq!(app.horizontal_offset, 0);

        // Lines shorter than viewport width
        app.add_lines(vec!["short line".to_string()]);
        app.scroll_to_line_end();
        assert_eq!(app.horizontal_offset, 0);

        // Long line exceeding viewport width
        let long_line = "a".repeat(50);
        app.add_lines(vec![long_line]);
        app.scroll_to_line_end();
        assert_eq!(app.horizontal_offset, 50 - 20 + 2); // 32

        // Measured in display columns: tabs expand and wide chars count twice
        app.add_lines(vec!["日".repeat(30), "\t".repeat(15)]);
        app.scroll_to_line_end();
        assert_eq!(app.horizontal_offset, 60 - 20 + 2);

        // Back to col zero
        app.scroll_to_col_zero();
        assert_eq!(app.horizontal_offset, 0);

        // In wrap mode, horizontal offset stays 0
        app.toggle_wrap();
        app.scroll_to_line_end();
        assert_eq!(app.horizontal_offset, 0);
    }

    #[test]
    fn test_follow_mode_and_vertical_scrolling() {
        let mut app = App::new("test".to_string(), 100, false, false, true);
        app.viewport_height = 10;

        let lines: Vec<String> = (0..50).map(|i| format!("Line {}", i)).collect();
        app.add_lines(lines);

        // Since following is true, vertical_offset should track bottom (50 - 10 = 40)
        assert!(app.following);
        assert_eq!(app.vertical_offset, 40);

        // Scrolling up pauses follow
        app.scroll_up(5);
        assert!(!app.following);
        assert_eq!(app.vertical_offset, 35);

        // Scrolling to top
        app.scroll_to_top();
        assert!(!app.following);
        assert_eq!(app.vertical_offset, 0);

        // G (scroll to end) jumps to the bottom and resumes follow
        app.scroll_to_end();
        assert!(app.following);
        assert_eq!(app.vertical_offset, 40);

        // Scrolling down to bottom resumes follow
        app.scroll_up(2);
        assert!(!app.following);
        app.scroll_down(5); // scrolls past max_scroll
        assert!(app.following);
        assert_eq!(app.vertical_offset, 40);
    }

    #[test]
    fn test_start_without_follow() {
        let mut app = App::new("test".to_string(), 100, false, false, false);
        app.viewport_height = 10;
        app.add_lines((0..50).map(|i| i.to_string()).collect());
        assert!(!app.following);
        assert_eq!(app.vertical_offset, 0);
    }

    #[test]
    fn test_buffer_trimming() {
        let mut app = App::new("test".to_string(), 30, false, false, true);
        app.viewport_height = 10;

        let lines: Vec<String> = (0..50).map(|i| format!("Line {}", i)).collect();
        app.add_lines(lines);

        // Max lines is 30, so buffer size should be 30
        assert_eq!(app.lines.len(), 30);
        assert_eq!(app.total_lines_received, 50);
        // The first remaining line should be "Line 20"
        assert_eq!(app.lines[0].clean_text(), "Line 20");
    }

    #[test]
    fn test_line_numbers_stay_absolute_after_trimming() {
        let mut app = App::new("test".to_string(), 3, false, false, true);
        app.add_lines((1..=5).map(|i| format!("L{i}")).collect());
        assert_eq!(app.get_line(0).unwrap().raw, "L3");
        assert_eq!(app.get_original_line_number(0), 3);

        // Also through the filter
        submit(&mut app, InputMode::Filter, "L5");
        assert_eq!(app.get_original_line_number(0), 5);
    }

    #[test]
    fn test_trimming_shifts_filter_and_search_indices() {
        let mut app = App::new("test".to_string(), 4, false, false, true);
        app.add_lines(vec!["a1".into(), "b1".into(), "a2".into(), "b2".into()]);
        submit(&mut app, InputMode::Filter, "a");
        submit(&mut app, InputMode::Search, "2");
        assert_eq!(app.search_match_indices, vec![1]);
        assert_eq!(app.current_match_idx, Some(0));

        app.add_lines(vec!["a3".into(), "c".into()]); // drops "a1", "b1"
        assert_eq!(view_texts(&app), vec!["a2", "a3"]);
        assert_eq!(app.filtered_indices, vec![0, 2]);
        assert_eq!(app.search_match_indices, vec![0]);
        assert_eq!(app.current_match_idx, Some(0));
        assert_eq!(app.get_line(app.search_match_indices[0]).unwrap().raw, "a2");
    }

    #[test]
    fn test_search_and_jump() {
        let mut app = app_with(&[
            "2026-10-15 [INFO] Server started",
            "2026-10-15 [WARN] Memory high",
            "2026-10-15 [ERROR] Disk failure",
            "2026-10-15 [INFO] Request completed",
            "2026-10-15 [ERROR] Network timeout",
        ]);

        submit(&mut app, InputMode::Search, "error");

        assert_eq!(
            app.search.as_ref().map(|s| s.pattern.as_str()),
            Some("error")
        );
        assert_eq!(app.search_match_indices, vec![2, 4]);

        // Jumps through matches
        assert_eq!(app.current_match_idx, Some(0)); // line 2
        app.jump_to_next_match();
        assert_eq!(app.current_match_idx, Some(1)); // line 4
        app.jump_to_next_match();
        assert_eq!(app.current_match_idx, Some(0)); // wraps to line 2
        assert!(app.status_message.as_deref().unwrap().contains("BOTTOM"));

        app.jump_to_prev_match();
        assert_eq!(app.current_match_idx, Some(1)); // wraps back to line 4
    }

    #[test]
    fn test_n_searches_from_view_after_scrolling() {
        let lines: Vec<String> = (0..100)
            .map(|i| {
                if i % 10 == 0 {
                    format!("hit {i}")
                } else {
                    i.to_string()
                }
            })
            .collect();
        let mut app = App::new("test".to_string(), 1000, false, false, false);
        app.viewport_height = 10;
        app.add_lines(lines);

        submit(&mut app, InputMode::Search, "hit");
        assert_eq!(app.current_match_idx, Some(0));
        app.jump_to_next_match();
        assert_eq!(app.current_match_idx, Some(1)); // line 10, view top 5

        // Scrolled away: n continues from the top of the screen, not from line 10
        app.scroll_down(60); // top 65
        app.jump_to_next_match();
        assert_eq!(app.search_match_indices[app.current_match_idx.unwrap()], 70);
        assert!(app.status_message.is_none());

        // Scrolled away: N continues from the bottom of the screen
        app.scroll_to_top();
        app.scroll_down(42); // lines 42..52 on screen
        app.jump_to_prev_match();
        assert_eq!(app.search_match_indices[app.current_match_idx.unwrap()], 50);

        // Without scrolling, n / N step from the current match
        app.jump_to_prev_match();
        assert_eq!(app.search_match_indices[app.current_match_idx.unwrap()], 40);
    }

    #[test]
    fn test_jump_without_search_sets_message() {
        let mut app = app_with(&["a", "b"]);
        app.jump_to_next_match();
        assert!(
            app.status_message
                .as_deref()
                .unwrap()
                .contains("No active search")
        );

        submit(&mut app, InputMode::Search, "zzz");
        assert!(app.status_message.as_deref().unwrap().contains("not found"));
    }

    #[test]
    fn test_search_is_recomputed_after_filter_change() {
        let mut app = app_with(&["x", "x", "err a", "y", "err b"]);
        submit(&mut app, InputMode::Search, "err");
        assert_eq!(app.search_match_indices, vec![2, 4]);

        submit(&mut app, InputMode::Filter, "err|y");
        assert_eq!(view_texts(&app), vec!["err a", "y", "err b"]);
        assert_eq!(app.search_match_indices, vec![0, 2]);
        assert_eq!(app.current_match_idx, None);
    }

    #[test]
    fn test_matching_uses_displayed_text() {
        // Tabs are expanded and ANSI codes removed, as on screen
        let mut app = app_with(&["a\tb", "\x1b[31mred\x1b[0m alert"]);
        submit(&mut app, InputMode::Search, "a   b");
        assert_eq!(app.search_match_indices, vec![0]);
        submit(&mut app, InputMode::Search, "red alert");
        assert_eq!(app.search_match_indices, vec![1]);
    }

    #[test]
    fn test_spaces_in_patterns_are_kept() {
        let mut app = app_with(&["code 500 err", "size 1500", "x"]);
        submit(&mut app, InputMode::Search, " 500 ");
        assert_eq!(app.search_match_indices, vec![0]);

        submit(&mut app, InputMode::Filter, "   ");
        assert!(app.filters.is_empty());
        submit(&mut app, InputMode::Goto, " 2 ");
        assert!(app.status_message.is_none());
    }

    #[test]
    fn test_filter_mode() {
        let mut app = app_with(&["apple", "banana", "apricot", "cherry"]);

        submit(&mut app, InputMode::Filter, "ap");

        assert_eq!(app.total_display_lines(), 2);
        assert_eq!(app.get_line(0).unwrap().clean_text(), "apple");
        assert_eq!(app.get_line(1).unwrap().clean_text(), "apricot");
        // Original line numbers: apple was line 1, apricot was line 3
        assert_eq!(app.get_original_line_number(0), 1);
        assert_eq!(app.get_original_line_number(1), 3);

        app.clear_last_search_or_filter();
        assert_eq!(app.total_display_lines(), 4);
    }

    #[test]
    fn test_stacked_and_inverted_filters() {
        let mut app = app_with(&["GET /a 200", "GET /b 500", "POST /c 200", "GET /health 200"]);
        submit(&mut app, InputMode::Filter, "GET");
        submit(&mut app, InputMode::Filter, "!health");
        assert_eq!(view_texts(&app), vec!["GET /a 200", "GET /b 500"]);

        // New lines are matched against the whole stack
        app.add_lines(vec!["GET /health 500".into(), "GET /d 200".into()]);
        assert_eq!(
            view_texts(&app),
            vec!["GET /a 200", "GET /b 500", "GET /d 200"]
        );

        // Esc removes the most recent filter first
        app.clear_last_search_or_filter();
        assert_eq!(app.total_display_lines(), 5);

        // An empty filter prompt clears all filters
        submit(&mut app, InputMode::Filter, "200");
        submit(&mut app, InputMode::Filter, "");
        assert_eq!(app.total_display_lines(), 6);
    }

    #[test]
    fn test_esc_hides_search_before_removing_filters() {
        let mut app = app_with(&["a1", "b1", "a2"]);
        submit(&mut app, InputMode::Filter, "a");
        submit(&mut app, InputMode::Search, "2");

        // The first esc hides the highlights but keeps the search
        app.clear_last_search_or_filter();
        assert!(app.search.is_some() && app.search_hidden);
        assert_eq!(app.filters.len(), 1);

        // n shows them again
        app.jump_to_next_match();
        assert!(!app.search_hidden);

        app.clear_last_search_or_filter();
        app.clear_last_search_or_filter();
        assert!(app.filters.is_empty());

        // An empty search clears it
        submit(&mut app, InputMode::Search, "");
        assert!(app.search.is_none());
    }

    #[test]
    fn test_search_preview_jumps_and_cancel_restores() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let mut app = App::new("test".to_string(), 1000, false, false, false);
        app.viewport_height = 10;
        app.add_lines(lines);

        app.start_input(InputMode::Search);
        app.editor.set_text("line 77");
        app.preview_search();
        assert_eq!(app.preview_count, Some(1));
        assert!(app.vertical_offset <= 77 && 77 < app.vertical_offset + 10);

        app.cancel_input();
        assert_eq!(app.position(), (0, 0));
    }

    #[test]
    fn test_filter_changes_keep_position_when_paused() {
        let lines: Vec<String> = (0..100)
            .map(|i| format!("{} {}", if i % 2 == 0 { "even" } else { "odd" }, i))
            .collect();
        let mut app = App::new("test".to_string(), 1000, false, false, true);
        app.viewport_height = 10;
        app.add_lines(lines);

        app.scroll_to_top();
        app.scroll_down(41); // top line: "odd 41"
        submit(&mut app, InputMode::Filter, "even");
        assert!(!app.following);
        assert_eq!(app.get_line(app.vertical_offset).unwrap().raw, "even 42");

        // Clearing the filter returns to the same real line, not filtered index 21
        app.clear_last_search_or_filter();
        assert_eq!(app.get_line(app.vertical_offset).unwrap().raw, "even 42");
    }

    #[test]
    fn test_filter_keeps_following() {
        let mut app = app_with(&[]);
        app.add_lines((0..50).map(|i| format!("line {i}")).collect());
        submit(&mut app, InputMode::Filter, "line [0-9]$");
        assert!(app.following);
        app.add_lines(vec!["line 7".into()]);
        assert!(app.is_at_bottom());
    }

    #[test]
    fn test_partial_lines_are_replaced() {
        let mut app = app_with(&["a"]);
        submit(&mut app, InputMode::Filter, "!skip");
        submit(&mut app, InputMode::Search, "done");
        app.set_partial_line("work".into());
        app.set_partial_line("work in progress".into());
        assert_eq!(view_texts(&app), vec!["a", "work in progress"]);

        app.add_lines(vec!["work in progress done".into(), "next".into()]);
        assert_eq!(view_texts(&app), vec!["a", "work in progress done", "next"]);
        assert_eq!(app.total_lines_received, 3);
        assert_eq!(app.search_match_indices, vec![1]);
    }

    #[test]
    fn test_goto_line() {
        let mut app = App::new("test".to_string(), 50, false, false, true);
        app.viewport_height = 10;
        app.add_lines((1..=100).map(|i| format!("L{i}")).collect()); // keeps 51..=100

        submit(&mut app, InputMode::Goto, "70");
        assert!(!app.following);
        assert_eq!(app.vertical_offset, 70 - 51 - 5);

        submit(&mut app, InputMode::Goto, "10");
        assert!(
            app.status_message
                .as_deref()
                .unwrap()
                .contains("no longer in the buffer")
        );
        submit(&mut app, InputMode::Goto, "abc");
        assert!(app.status_message.as_deref().unwrap().contains("Invalid"));
    }

    #[test]
    fn test_wrap_mode_bottom_shows_last_lines() {
        let mut app = App::new("test".to_string(), 100, true, false, true);
        app.viewport_height = 4;
        app.viewport_width = 10;
        // Last two lines take 2 rows each
        app.add_lines(vec!["a".into(), "b".into(), "c".repeat(15), "d".repeat(15)]);
        assert_eq!(app.position(), (2, 0));

        // A line taller than the viewport shows its last rows
        app.add_lines(vec!["e".repeat(100)]);
        assert_eq!(app.position(), (4, 6));
    }

    #[test]
    fn test_wrap_mode_scrolls_by_rows_through_tall_lines() {
        let mut app = App::new("test".to_string(), 100, true, false, true);
        app.viewport_height = 4;
        app.viewport_width = 10;
        app.add_lines(vec!["a".into(), "x".repeat(100), "b".into()]); // rows: 1, 10, 1
        assert_eq!(app.position(), (1, 7));

        // Every row of the tall line is reachable
        app.scroll_up(1);
        assert!(!app.following);
        assert_eq!(app.position(), (1, 6));
        app.scroll_up(6);
        assert_eq!(app.position(), (1, 0));
        app.scroll_up(1);
        assert_eq!(app.position(), (0, 0));
        app.scroll_up(5);
        assert_eq!(app.position(), (0, 0));

        app.scroll_down(3);
        assert_eq!(app.position(), (1, 2));
        app.scroll_down(100);
        assert_eq!(app.position(), (1, 7));
        assert!(app.following);

        // Leaving wrap mode resets the row offset
        app.toggle_wrap();
        assert_eq!(app.row_offset, 0);
    }

    #[test]
    fn test_wrap_toggle() {
        let mut app = App::new("test".to_string(), 100, false, false, true);
        app.viewport_width = 10;
        app.add_lines(vec!["x".repeat(50)]);
        app.scroll_right(20);
        assert_eq!(app.horizontal_offset, 20);

        app.toggle_wrap();
        assert!(app.wrap);
        // Toggling wrap should reset horizontal scroll
        assert_eq!(app.horizontal_offset, 0);
    }
}
