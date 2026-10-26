use std::collections::VecDeque;
use ratatui::text::Line;

use crate::ansi::{parse_ansi_line, strip_ansi};
use crate::input::{LineEditor, push_history};
use crate::layout::wrapped_row_count;
use crate::matcher::Matcher;

#[derive(Debug, Clone)]
pub struct LogLine {
    pub raw: String,
    pub clean: Option<String>,
}

impl LogLine {
    pub fn new(raw: String) -> Self {
        let clean = if raw.contains('\x1b') {
            Some(strip_ansi(&raw))
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
    pub vertical_offset: usize,
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
    pub mouse_capture: bool,
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
    pub filters: Vec<Matcher>,
    pub filtered_indices: Vec<usize>,
    pub search_match_indices: Vec<usize>,
    pub current_match_idx: Option<usize>,
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
            mouse_capture: false,
            eof: false,
            input_source_name,
            status_message: None,

            input_mode: InputMode::Normal,
            editor: LineEditor::default(),
            search_history: Vec::new(),
            filter_history: Vec::new(),
            search: None,
            filters: Vec::new(),
            filtered_indices: Vec::new(),
            search_match_indices: Vec::new(),
            current_match_idx: None,
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
                if self.get_line(view).is_some_and(|l| search.is_match(l.clean_text())) {
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
                self.filtered_indices.iter_mut().for_each(|i| *i -= drop_count);
                filtered_drop
            } else {
                drop_count
            };

            let match_drop = self.search_match_indices.partition_point(|&i| i < view_drop);
            self.search_match_indices.drain(..match_drop);
            self.search_match_indices.iter_mut().for_each(|i| *i -= view_drop);
            self.current_match_idx = self
                .current_match_idx
                .and_then(|c| c.checked_sub(match_drop));

            self.vertical_offset = self.vertical_offset.saturating_sub(view_drop);
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

    /// The largest top line that still fills the viewport down to the last line.
    pub fn max_vertical_scroll(&self) -> usize {
        let total = self.total_display_lines();
        if !self.wrap {
            return total.saturating_sub(self.viewport_height);
        }
        let mut rows = 0;
        let mut top = total;
        while top > 0 {
            rows += self.line_rows(top - 1);
            if rows > self.viewport_height {
                break;
            }
            top -= 1;
        }
        top.min(total.saturating_sub(1))
    }

    pub fn is_at_bottom(&self) -> bool {
        self.vertical_offset >= self.max_vertical_scroll()
    }

    /// Lines after the last one drawn on screen.
    pub fn lines_below(&self) -> usize {
        self.total_display_lines().saturating_sub(self.visible_end)
    }

    pub fn scroll_to_bottom(&mut self) {
        self.vertical_offset = self.max_vertical_scroll();
    }

    pub fn scroll_up(&mut self, amount: usize) {
        self.following = false;
        self.vertical_offset = self.vertical_offset.saturating_sub(amount);
    }

    pub fn scroll_down(&mut self, amount: usize) {
        let max_scroll = self.max_vertical_scroll();
        let new_offset = self.vertical_offset.saturating_add(amount);
        if new_offset >= max_scroll {
            self.vertical_offset = max_scroll;
            self.following = true;
        } else {
            self.vertical_offset = new_offset;
        }
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
        self.vertical_offset = 0;
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

    pub fn toggle_follow(&mut self) {
        self.following = !self.following;
        if self.following {
            self.scroll_to_bottom();
        }
    }

    pub fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
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
    }

    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.editor.clear();
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
        let query = self.editor.text().trim().to_string();
        let mode = self.input_mode;
        self.input_mode = InputMode::Normal;
        self.editor.clear();

        match mode {
            InputMode::Search => {
                push_history(&mut self.search_history, &query);
                self.set_search(Matcher::new(&query, false));
            }
            InputMode::Filter => {
                push_history(&mut self.filter_history, &query);
                match Matcher::new(&query, true) {
                    Some(filter) => {
                        let mut filters = self.filters.clone();
                        filters.push(filter);
                        self.set_filters(filters);
                    }
                    None if self.is_filtering() => {
                        self.set_filters(Vec::new());
                        self.status_message = Some("All filters cleared".to_string());
                    }
                    None => {}
                }
            }
            InputMode::Goto => match query.parse::<usize>() {
                Ok(n) if n > 0 => self.goto_line(n),
                _ if query.is_empty() => {}
                _ => self.status_message = Some(format!("Invalid line number: {query}")),
            },
            InputMode::Normal => {}
        }
    }

    /// Esc in normal mode: clear the search first, then remove filters one at a time.
    pub fn clear_last_search_or_filter(&mut self) {
        if self.search.is_some() {
            self.set_search(None);
            self.status_message = Some("Search cleared".to_string());
        } else if let Some(removed) = self.filters.last().map(Matcher::label) {
            let mut filters = self.filters.clone();
            filters.pop();
            self.set_filters(filters);
            self.status_message = Some(format!("Filter removed: {removed}"));
        }
    }

    fn set_search(&mut self, search: Option<Matcher>) {
        self.search = search;
        self.recompute_search_matches();
        if self.search.is_some() {
            if self.search_match_indices.is_empty() {
                let label = self.search.as_ref().map(Matcher::label).unwrap_or_default();
                self.status_message = Some(format!("Pattern not found: {label}"));
            } else {
                self.jump_to_next_match();
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
            self.vertical_offset = view.min(self.max_vertical_scroll());
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
                .filter(|&view| self.get_line(view).is_some_and(|l| search.is_match(l.clean_text())))
                .collect();
        }
    }

    // ---- Match navigation ----

    /// Returns false (and sets a status message) when there is nothing to jump to.
    fn check_can_jump(&mut self) -> bool {
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

    pub fn jump_to_next_match(&mut self) {
        if !self.check_can_jump() {
            return;
        }
        let count = self.search_match_indices.len();
        let next_idx = match self.current_match_idx {
            Some(curr) if curr + 1 < count => curr + 1,
            Some(_) => {
                self.status_message = Some("Search hit BOTTOM, continuing at TOP".to_string());
                0
            }
            // Find first match at or after the current position
            None => self
                .search_match_indices
                .iter()
                .position(|&line_idx| line_idx >= self.vertical_offset)
                .unwrap_or(0),
        };
        self.current_match_idx = Some(next_idx);
        self.jump_to_line(self.search_match_indices[next_idx]);
    }

    pub fn jump_to_prev_match(&mut self) {
        if !self.check_can_jump() {
            return;
        }
        let count = self.search_match_indices.len();
        let prev_idx = match self.current_match_idx {
            Some(curr) if curr > 0 => (curr - 1).min(count - 1),
            Some(_) => {
                self.status_message = Some("Search hit TOP, continuing at BOTTOM".to_string());
                count - 1
            }
            None => count - 1,
        };
        self.current_match_idx = Some(prev_idx);
        self.jump_to_line(self.search_match_indices[prev_idx]);
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

    fn jump_to_line(&mut self, line_idx: usize) {
        self.following = false;
        // Center the target in the viewport if possible
        let half = self.viewport_height / 2;
        self.vertical_offset = line_idx.saturating_sub(half).min(self.max_vertical_scroll());
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

        // Toggling follow jumps to bottom
        app.toggle_follow();
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

        assert_eq!(app.search.as_ref().map(|s| s.pattern.as_str()), Some("error"));
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
    fn test_jump_without_search_sets_message() {
        let mut app = app_with(&["a", "b"]);
        app.jump_to_next_match();
        assert!(app.status_message.as_deref().unwrap().contains("No active search"));

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
        assert_eq!(view_texts(&app), vec!["GET /a 200", "GET /b 500", "GET /d 200"]);

        // Esc removes the most recent filter first
        app.clear_last_search_or_filter();
        assert_eq!(app.total_display_lines(), 5);

        // An empty filter prompt clears all filters
        submit(&mut app, InputMode::Filter, "200");
        submit(&mut app, InputMode::Filter, "");
        assert_eq!(app.total_display_lines(), 6);
    }

    #[test]
    fn test_esc_clears_search_before_filters() {
        let mut app = app_with(&["a1", "b1", "a2"]);
        submit(&mut app, InputMode::Filter, "a");
        submit(&mut app, InputMode::Search, "2");

        app.clear_last_search_or_filter();
        assert!(app.search.is_none());
        assert_eq!(app.filters.len(), 1);

        app.clear_last_search_or_filter();
        assert!(app.filters.is_empty());
    }

    #[test]
    fn test_filter_changes_keep_position_when_paused() {
        let lines: Vec<String> = (0..100).map(|i| format!("{} {}", if i % 2 == 0 { "even" } else { "odd" }, i)).collect();
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
        assert!(app.status_message.as_deref().unwrap().contains("no longer in the buffer"));
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
        assert_eq!(app.vertical_offset, 2);

        // A single line taller than the viewport is the top line
        app.add_lines(vec!["e".repeat(100)]);
        assert_eq!(app.vertical_offset, 4);
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
