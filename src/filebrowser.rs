//! A minimal file browser. Used two ways:
//! * inline as the always-visible song panel (`render_inline`), where picking a
//!   `.mid` plays it and marks it with a dot;
//! * as a modal popup for adding a soundfont to the retained list (`render`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

/// How many files a `/` search scans below the current directory before giving up.
const MAX_SEARCH_CANDIDATES: usize = 4000;
/// How many subdirectory levels a `/` search descends into.
const MAX_SEARCH_DEPTH: u32 = 12;
/// How many ranked matches are kept (and shown) for a query.
const MAX_SEARCH_RESULTS: usize = 200;

pub enum Outcome {
    /// Still browsing.
    Pending,
    /// User pressed Esc (only meaningful for the modal use).
    Cancelled,
    /// User chose a file.
    Picked(PathBuf),
}

struct Entry {
    label: String,
    path: PathBuf,
    is_dir: bool,
}

/// Live fuzzy-search state: the typed query plus every matching file found
/// under the directory the search started in (gathered once, re-scored on
/// every keystroke rather than re-walking the disk).
struct SearchState {
    query: String,
    /// (path relative to the search root, displayed with '/' separators; path)
    candidates: Vec<(String, PathBuf)>,
}

pub struct FileBrowser {
    title: String,
    /// Allowed lowercase extensions (without the dot). Empty = accept any file.
    extensions: Vec<String>,
    cwd: PathBuf,
    entries: Vec<Entry>,
    state: ListState,
    show_hidden: bool,
    error: Option<String>,
    /// Path currently marked with a dot (the playing file), if any.
    active: Option<PathBuf>,
    /// `Some` while a `/` fuzzy search is in progress; `entries` then holds
    /// ranked search results instead of the current directory's listing.
    search: Option<SearchState>,
}

impl FileBrowser {
    pub fn new(title: impl Into<String>, start_dir: PathBuf, extensions: &[&str]) -> Self {
        let cwd = start_dir
            .canonicalize()
            .unwrap_or(start_dir)
            .to_path_buf();
        let mut browser = Self {
            title: title.into(),
            extensions: extensions.iter().map(|e| e.to_lowercase()).collect(),
            cwd,
            entries: Vec::new(),
            state: ListState::default(),
            show_hidden: false,
            error: None,
            active: None,
            search: None,
        };
        browser.refresh();
        browser
    }

    pub fn set_active(&mut self, path: Option<PathBuf>) {
        self.active = path;
    }

    pub fn is_searching(&self) -> bool {
        self.search.is_some()
    }

    fn accepts(&self, path: &Path) -> bool {
        if self.extensions.is_empty() {
            return true;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => self.extensions.contains(&ext.to_lowercase()),
            None => false,
        }
    }

    fn refresh(&mut self) {
        self.entries.clear();
        self.error = None;

        if self.cwd.parent().is_some() {
            self.entries.push(Entry {
                label: "..".to_string(),
                path: self.cwd.parent().unwrap().to_path_buf(),
                is_dir: true,
            });
        }

        match std::fs::read_dir(&self.cwd) {
            Ok(read_dir) => {
                let mut dirs: Vec<Entry> = Vec::new();
                let mut files: Vec<Entry> = Vec::new();
                for entry in read_dir.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }
                    let is_dir = path.is_dir();
                    if is_dir {
                        dirs.push(Entry {
                            label: format!("{name}/"),
                            path,
                            is_dir: true,
                        });
                    } else if self.accepts(&path) {
                        files.push(Entry {
                            label: name,
                            path,
                            is_dir: false,
                        });
                    }
                }
                dirs.sort_by_key(|e| e.label.to_lowercase());
                files.sort_by_key(|e| e.label.to_lowercase());
                self.entries.extend(dirs);
                self.entries.extend(files);
            }
            Err(e) => {
                self.error = Some(format!("Cannot read {}: {e}", self.cwd.display()));
            }
        }

        let selection = if self.entries.is_empty() { None } else { Some(0) };
        self.state.select(selection);
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len() as isize;
        let current = self.state.selected().unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(len);
        self.state.select(Some(next as usize));
    }

    fn go_parent(&mut self) {
        if let Some(parent) = self.cwd.parent() {
            self.cwd = parent.to_path_buf();
            self.refresh();
        }
    }

    fn start_search(&mut self) {
        let candidates = self.collect_candidates();
        self.search = Some(SearchState {
            query: String::new(),
            candidates,
        });
        self.apply_search();
    }

    fn cancel_search(&mut self) {
        self.search = None;
        self.refresh();
    }

    /// Breadth-first walk from `self.cwd`, collecting every accepted file
    /// (up to `MAX_SEARCH_CANDIDATES`, `MAX_SEARCH_DEPTH` levels deep) as a
    /// `(display path relative to cwd, absolute path)` pair.
    fn collect_candidates(&self) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
        queue.push_back((self.cwd.clone(), 0));

        while let Some((dir, depth)) = queue.pop_front() {
            if out.len() >= MAX_SEARCH_CANDIDATES {
                break;
            }
            let Ok(read_dir) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in read_dir.flatten() {
                if out.len() >= MAX_SEARCH_CANDIDATES {
                    break;
                }
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if !self.show_hidden && name.starts_with('.') {
                    continue;
                }
                if path.is_dir() {
                    if depth < MAX_SEARCH_DEPTH {
                        queue.push_back((path, depth + 1));
                    }
                } else if self.accepts(&path) {
                    let label = path
                        .strip_prefix(&self.cwd)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((label, path));
                }
            }
        }

        out
    }

    /// Re-rank `search`'s candidates against the current query and refresh
    /// `entries`/`state` from the top matches.
    fn apply_search(&mut self) {
        let Some(search) = &self.search else {
            return;
        };

        let mut scored: Vec<(i32, usize)> = search
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(i, (label, _))| fuzzy_score(label, &search.query).map(|score| (score, i)))
            .collect();
        // Stable sort: ties keep the walk order, which groups by folder.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(MAX_SEARCH_RESULTS);

        let entries: Vec<Entry> = scored
            .into_iter()
            .map(|(_, i)| {
                let (label, path) = &search.candidates[i];
                Entry {
                    label: label.clone(),
                    path: path.clone(),
                    is_dir: false,
                }
            })
            .collect();

        self.entries = entries;
        self.state
            .select(if self.entries.is_empty() { None } else { Some(0) });
    }

    fn activate(&mut self) -> Outcome {
        let Some(index) = self.state.selected() else {
            return Outcome::Pending;
        };
        let Some(entry) = self.entries.get(index) else {
            return Outcome::Pending;
        };
        if entry.is_dir {
            self.cwd = entry.path.clone();
            self.refresh();
            Outcome::Pending
        } else {
            Outcome::Picked(entry.path.clone())
        }
    }

    /// Handle a key. While a `/` search is active, this owns the keyboard
    /// almost entirely (letters are query text) — callers should route every
    /// key here rather than intercepting shortcuts themselves; check
    /// [`is_searching`](Self::is_searching) if a hard override (e.g. Ctrl+C)
    /// is needed.
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        if self.search.is_some() {
            return self.on_search_key(key);
        }
        match key.code {
            KeyCode::Esc => Outcome::Cancelled,
            KeyCode::Up => {
                self.move_selection(-1);
                Outcome::Pending
            }
            KeyCode::Down => {
                self.move_selection(1);
                Outcome::Pending
            }
            KeyCode::PageUp => {
                self.move_selection(-10);
                Outcome::Pending
            }
            KeyCode::PageDown => {
                self.move_selection(10);
                Outcome::Pending
            }
            KeyCode::Enter | KeyCode::Right => self.activate(),
            KeyCode::Left | KeyCode::Backspace => {
                self.go_parent();
                Outcome::Pending
            }
            KeyCode::Char('h') => {
                self.show_hidden = !self.show_hidden;
                self.refresh();
                Outcome::Pending
            }
            KeyCode::Char('/') => {
                self.start_search();
                Outcome::Pending
            }
            _ => Outcome::Pending,
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => {
                self.cancel_search();
                Outcome::Pending
            }
            KeyCode::Enter => self.activate(),
            KeyCode::Up => {
                self.move_selection(-1);
                Outcome::Pending
            }
            KeyCode::Down => {
                self.move_selection(1);
                Outcome::Pending
            }
            KeyCode::PageUp => {
                self.move_selection(-10);
                Outcome::Pending
            }
            KeyCode::PageDown => {
                self.move_selection(10);
                Outcome::Pending
            }
            KeyCode::Backspace => {
                if let Some(search) = &mut self.search {
                    search.query.pop();
                }
                self.apply_search();
                Outcome::Pending
            }
            KeyCode::Char(c) => {
                if let Some(search) = &mut self.search {
                    search.query.push(c);
                }
                self.apply_search();
                Outcome::Pending
            }
            _ => Outcome::Pending,
        }
    }

    fn entry_items(&self) -> Vec<ListItem<'static>> {
        self.entries
            .iter()
            .map(|e| {
                let dot = if self.active.as_deref() == Some(e.path.as_path()) {
                    "● "
                } else {
                    "  "
                };
                let prefix = if e.is_dir { "▸ " } else { "  " };
                ListItem::new(format!("{dot}{prefix}{}", e.label))
            })
            .collect()
    }

    fn head_line(&self) -> Line<'static> {
        if let Some(search) = &self.search {
            return Line::from(format!("/{}│", search.query))
                .style(Style::default().add_modifier(Modifier::BOLD));
        }
        match &self.error {
            Some(err) => Line::from(err.clone()).style(Style::default().add_modifier(Modifier::BOLD)),
            None => Line::from(display_dir(&self.cwd))
                .style(Style::default().add_modifier(Modifier::DIM)),
        }
    }

    fn panel_title(&self) -> String {
        match &self.search {
            Some(_) => {
                let n = self.entries.len();
                format!("{} — {n} match{}", self.title, if n == 1 { "" } else { "es" })
            }
            None => self.title.clone(),
        }
    }

    /// Draw the browser directly into `area` as the always-visible song panel.
    pub fn render_inline(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let subtitle = self.panel_title();
        let (border_style, title, highlight_style, symbol) = if focused {
            (
                Style::default(),
                format!(" ▶ {subtitle} "),
                Style::default().add_modifier(Modifier::REVERSED),
                "» ",
            )
        } else {
            (
                Style::default().add_modifier(Modifier::DIM),
                format!("   {subtitle} "),
                Style::default().add_modifier(Modifier::DIM | Modifier::BOLD),
                "  ",
            )
        };

        let block = Block::bordered().border_style(border_style).title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
        frame.render_widget(Paragraph::new(self.head_line()), rows[0]);

        let list = List::new(self.entry_items())
            .highlight_style(highlight_style)
            .highlight_symbol(symbol);
        frame.render_stateful_widget(list, rows[1], &mut self.state);
    }

    /// Draw the browser as a centred modal popup (used for adding a soundfont).
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let popup = centered_rect(80, 80, area);
        frame.render_widget(Clear, popup);

        let block = Block::bordered().title(format!(" {} ", self.panel_title()));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let rows = Layout::vertical([
            Constraint::Length(1), // cwd
            Constraint::Min(1),    // list
            Constraint::Length(1), // error / count
            Constraint::Length(1), // keys
        ])
        .split(inner);

        frame.render_widget(Paragraph::new(self.head_line()), rows[0]);

        let list = List::new(self.entry_items())
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("");
        frame.render_stateful_widget(list, rows[1], &mut self.state);

        frame.render_widget(
            Paragraph::new(Line::from(format!("{} item(s)", self.entries.len())))
                .style(Style::default().add_modifier(Modifier::DIM)),
            rows[2],
        );

        let keys = if self.search.is_some() {
            "↑↓ move   ⏎ open   ⌫ delete   type to search   Esc cancel search"
        } else {
            "↑↓ move   ⏎ open   ⌫ up a dir   h toggle hidden   / search   Esc cancel"
        };
        frame.render_widget(
            Paragraph::new(Line::from(keys)).style(Style::default().add_modifier(Modifier::DIM)),
            rows[3],
        );
    }
}

/// Case-insensitive fuzzy match. A query with spaces is split into terms
/// (e.g. "chrono corridor") and every term must independently subsequence-match
/// `haystack` — this catches concatenated names like "ChronoTrigger/Corridor..."
/// that contain no literal space. Returns `None` if any term fails to match,
/// otherwise a score where higher is better.
fn fuzzy_score(haystack: &str, needle: &str) -> Option<i32> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Some(0);
    }
    needle
        .split_whitespace()
        .map(|term| fuzzy_score_term(haystack, term))
        .sum()
}

/// Single-term subsequence match: every character of `needle` must appear in
/// `haystack` in order (not necessarily contiguous). Consecutive runs and
/// matches at word boundaries score higher, so "zocv" ranks "overworld.mid"
/// above a coincidental scatter match.
fn fuzzy_score_term(haystack: &str, needle: &str) -> Option<i32> {
    let hay: Vec<char> = haystack.chars().collect();
    let hay_lower: Vec<char> = haystack.to_lowercase().chars().collect();

    let mut score = 0i32;
    let mut search_from = 0usize;
    let mut prev_match: Option<usize> = None;

    for needle_char in needle.to_lowercase().chars() {
        let found = hay_lower[search_from..]
            .iter()
            .position(|&c| c == needle_char)
            .map(|i| i + search_from)?;

        score += 1;
        if prev_match == Some(found.wrapping_sub(1)) {
            score += 5; // consecutive characters
        }
        if found == 0 {
            score += 8; // start of string
        } else if matches!(hay[found - 1], ' ' | '_' | '-' | '.' | '/' | '\\') {
            score += 4; // start of a word
        }

        prev_match = Some(found);
        search_from = found + 1;
    }

    // Mild penalty for longer haystacks, so tighter matches sort first.
    score -= (hay.len() as i32) / 20;
    Some(score)
}

/// Path for display, without Windows' `\\?\` verbatim prefix.
fn display_dir(path: &Path) -> String {
    let s = path.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}
