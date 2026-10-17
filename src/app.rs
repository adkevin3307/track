use std::collections::VecDeque;
use regex::RegexBuilder;
use ratatui::text::Line;

use crate::ansi::{parse_ansi_line, strip_ansi};

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

#[derive(Debug, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
    Filter,
}

pub struct App {
    pub lines: VecDeque<LogLine>,
    pub max_buffer_lines: usize,
    pub total_lines_received: usize,

    // Scrolling
    pub vertical_offset: usize,
    pub horizontal_offset: usize,
    pub following: bool,
    pub viewport_height: usize,
    pub viewport_width: usize,

    // Display flags
    pub wrap: bool,
    pub show_line_numbers: bool,
    pub show_help: bool,
    pub eof: bool,
    pub input_source_name: String,

    // Search and Filter
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub search_query: Option<String>,
    pub filter_query: Option<String>,
    pub filtered_indices: Vec<usize>,
    pub search_match_indices: Vec<usize>,
    pub current_match_idx: Option<usize>,
}

impl App {
    pub fn new(input_source_name: String, max_buffer_lines: usize, wrap: bool, show_numbers: bool) -> Self {
        Self {
            lines: VecDeque::with_capacity(1000),
            max_buffer_lines,
            total_lines_received: 0,

            vertical_offset: 0,
            horizontal_offset: 0,
            following: true,
            viewport_height: 20,
            viewport_width: 80,

            wrap,
            show_line_numbers: show_numbers,
            show_help: false,
            eof: false,
            input_source_name,

            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            search_query: None,
            filter_query: None,
            filtered_indices: Vec::new(),
            search_match_indices: Vec::new(),
            current_match_idx: None,
        }
    }

    pub fn add_lines(&mut self, new_lines: Vec<String>) {
        if new_lines.is_empty() {
            return;
        }

        let added_count = new_lines.len();
        self.total_lines_received += added_count;

        for line in new_lines {
            let log_line = LogLine::new(line);
            self.lines.push_back(log_line);
        }

        // Enforce max buffer size
        if self.lines.len() > self.max_buffer_lines {
            let drop_count = self.lines.len() - self.max_buffer_lines;
            self.lines.drain(0..drop_count);
            self.vertical_offset = self.vertical_offset.saturating_sub(drop_count);
        }

        // Update filter if active
        if self.filter_query.is_some() {
            self.recompute_filter();
        }

        // Update search matches if active
        if self.search_query.is_some() {
            self.recompute_search_matches();
        }

        if self.following {
            self.scroll_to_bottom();
        }
    }

    pub fn total_display_lines(&self) -> usize {
        if self.filter_query.is_some() {
            self.filtered_indices.len()
        } else {
            self.lines.len()
        }
    }

    pub fn get_line(&self, view_index: usize) -> Option<&LogLine> {
        if self.filter_query.is_some() {
            let real_idx = *self.filtered_indices.get(view_index)?;
            self.lines.get(real_idx)
        } else {
            self.lines.get(view_index)
        }
    }

    pub fn get_original_line_number(&self, view_index: usize) -> usize {
        if self.filter_query.is_some() {
            if let Some(&real_idx) = self.filtered_indices.get(view_index) {
                real_idx + 1
            } else {
                view_index + 1
            }
        } else {
            view_index + 1
        }
    }

    pub fn max_vertical_scroll(&self) -> usize {
        self.total_display_lines().saturating_sub(self.viewport_height)
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

    pub fn scroll_left(&mut self, amount: usize) {
        self.horizontal_offset = self.horizontal_offset.saturating_sub(amount);
    }

    pub fn scroll_right(&mut self, amount: usize) {
        self.horizontal_offset = self.horizontal_offset.saturating_add(amount);
    }

    pub fn scroll_to_col_zero(&mut self) {
        self.horizontal_offset = 0;
    }

    pub fn scroll_to_line_end(&mut self) {
        if self.wrap {
            return;
        }
        let total = self.total_display_lines();
        let height = self.viewport_height;
        let start_idx = self.vertical_offset.min(total);
        let end_idx = (start_idx + height).min(total);
        let max_len = (start_idx..end_idx)
            .filter_map(|i| self.get_line(i))
            .map(|l| l.clean_text().chars().count())
            .max()
            .unwrap_or(0);

        if max_len > self.viewport_width {
            self.horizontal_offset = max_len.saturating_sub(self.viewport_width) + 2;
        } else {
            self.horizontal_offset = 0;
        }
    }

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
    }

    pub fn toggle_line_numbers(&mut self) {
        self.show_line_numbers = !self.show_line_numbers;
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    // Search and Filter handling
    pub fn start_search(&mut self) {
        self.input_mode = InputMode::Search;
        self.input_buffer.clear();
    }

    pub fn start_filter(&mut self) {
        self.input_mode = InputMode::Filter;
        self.input_buffer.clear();
    }

    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    pub fn submit_input(&mut self) {
        let query = self.input_buffer.trim().to_string();
        match self.input_mode {
            InputMode::Search => {
                if query.is_empty() {
                    self.search_query = None;
                    self.search_match_indices.clear();
                    self.current_match_idx = None;
                } else {
                    self.search_query = Some(query);
                    self.recompute_search_matches();
                    self.jump_to_next_match();
                }
            }
            InputMode::Filter => {
                if query.is_empty() {
                    self.filter_query = None;
                    self.filtered_indices.clear();
                } else {
                    self.filter_query = Some(query);
                    self.recompute_filter();
                    self.scroll_to_top();
                }
            }
            InputMode::Normal => {}
        }
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    pub fn clear_search_and_filter(&mut self) {
        self.search_query = None;
        self.search_match_indices.clear();
        self.current_match_idx = None;
        self.filter_query = None;
        self.filtered_indices.clear();
    }

    pub fn recompute_filter(&mut self) {
        self.filtered_indices.clear();
        if let Some(ref q) = self.filter_query {
            let re = RegexBuilder::new(q).case_insensitive(true).build();
            for (idx, line) in self.lines.iter().enumerate() {
                let matches = match &re {
                    Ok(r) => r.is_match(line.clean_text()),
                    Err(_) => line.clean_text().to_lowercase().contains(&q.to_lowercase()),
                };
                if matches {
                    self.filtered_indices.push(idx);
                }
            }
        }
    }

    pub fn recompute_search_matches(&mut self) {
        self.search_match_indices.clear();
        if let Some(ref q) = self.search_query {
            let re = RegexBuilder::new(q).case_insensitive(true).build();
            let total = self.total_display_lines();
            for view_idx in 0..total {
                if let Some(line) = self.get_line(view_idx) {
                    let matches = match &re {
                        Ok(r) => r.is_match(line.clean_text()),
                        Err(_) => line.clean_text().to_lowercase().contains(&q.to_lowercase()),
                    };
                    if matches {
                        self.search_match_indices.push(view_idx);
                    }
                }
            }
        }
    }

    pub fn jump_to_next_match(&mut self) {
        if self.search_match_indices.is_empty() {
            return;
        }

        let next_idx = match self.current_match_idx {
            Some(curr) => (curr + 1) % self.search_match_indices.len(),
            None => {
                // Find first match at or after current vertical_offset
                self.search_match_indices
                    .iter()
                    .position(|&line_idx| line_idx >= self.vertical_offset)
                    .unwrap_or(0)
            }
        };

        self.current_match_idx = Some(next_idx);
        let target_line = self.search_match_indices[next_idx];
        self.jump_to_line(target_line);
    }

    pub fn jump_to_prev_match(&mut self) {
        if self.search_match_indices.is_empty() {
            return;
        }

        let prev_idx = match self.current_match_idx {
            Some(curr) => {
                if curr == 0 {
                    self.search_match_indices.len() - 1
                } else {
                    curr - 1
                }
            }
            None => self.search_match_indices.len() - 1,
        };

        self.current_match_idx = Some(prev_idx);
        let target_line = self.search_match_indices[prev_idx];
        self.jump_to_line(target_line);
    }

    fn jump_to_line(&mut self, line_idx: usize) {
        self.following = false;
        // Center the match in the viewport if possible
        let half = self.viewport_height / 2;
        self.vertical_offset = line_idx.saturating_sub(half).min(self.max_vertical_scroll());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_horizontal_scrolling() {
        let mut app = App::new("test".to_string(), 100, false, false);
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
    }

    #[test]
    fn test_scroll_to_line_end() {
        let mut app = App::new("test".to_string(), 100, false, false);
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
        let mut app = App::new("test".to_string(), 100, false, false);
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
    fn test_buffer_trimming() {
        let mut app = App::new("test".to_string(), 30, false, false);
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
    fn test_search_and_jump() {
        let mut app = App::new("test".to_string(), 100, false, false);
        app.viewport_height = 10;

        app.add_lines(vec![
            "2026-10-15 [INFO] Server started".to_string(),
            "2026-10-15 [WARN] Memory high".to_string(),
            "2026-10-15 [ERROR] Disk failure".to_string(),
            "2026-10-15 [INFO] Request completed".to_string(),
            "2026-10-15 [ERROR] Network timeout".to_string(),
        ]);

        app.input_buffer = "ERROR".to_string();
        app.input_mode = InputMode::Search;
        app.submit_input();

        assert_eq!(app.search_query.as_deref(), Some("ERROR"));
        assert_eq!(app.search_match_indices, vec![2, 4]);

        // Jumps through matches
        assert_eq!(app.current_match_idx, Some(0)); // line 2
        app.jump_to_next_match();
        assert_eq!(app.current_match_idx, Some(1)); // line 4
        app.jump_to_next_match();
        assert_eq!(app.current_match_idx, Some(0)); // wraps to line 2
    }

    #[test]
    fn test_filter_mode() {
        let mut app = App::new("test".to_string(), 100, false, false);
        app.viewport_height = 10;

        app.add_lines(vec![
            "apple".to_string(),
            "banana".to_string(),
            "apricot".to_string(),
            "cherry".to_string(),
        ]);

        app.input_buffer = "ap".to_string();
        app.input_mode = InputMode::Filter;
        app.submit_input();

        assert_eq!(app.total_display_lines(), 2);
        assert_eq!(app.get_line(0).unwrap().clean_text(), "apple");
        assert_eq!(app.get_line(1).unwrap().clean_text(), "apricot");
        // Original line numbers: apple was line 1, apricot was line 3
        assert_eq!(app.get_original_line_number(0), 1);
        assert_eq!(app.get_original_line_number(1), 3);

        app.clear_search_and_filter();
        assert_eq!(app.total_display_lines(), 4);
    }

    #[test]
    fn test_wrap_toggle() {
        let mut app = App::new("test".to_string(), 100, false, false);
        app.scroll_right(20);
        assert_eq!(app.horizontal_offset, 20);

        app.toggle_wrap();
        assert!(app.wrap);
        // Toggling wrap should reset horizontal scroll
        assert_eq!(app.horizontal_offset, 0);
    }
}

