//! A minimal file browser widget: the Songs tab, where double-clicking a song
//! plays it and marks it as playing. [`FileBrowser::ui`] returns `Some(path)`
//! when a *file* row is double-clicked; double-clicking a directory navigates
//! into it internally. Rows can be selected several at a time and dragged
//! out together (onto a playlist).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use eframe::egui;

use crate::select_list::{self, Selection};

/// How many files a search scans below the current directory before giving up.
const MAX_SEARCH_CANDIDATES: usize = 4000;
/// How many subdirectory levels a search descends into.
const MAX_SEARCH_DEPTH: u32 = 12;
/// How many ranked matches are kept (and shown) for a query.
const MAX_SEARCH_RESULTS: usize = 200;

struct Entry {
    label: String,
    path: PathBuf,
    is_dir: bool,
}

/// Draws a file row's right-click menu.
pub type RowMenu<'a> = &'a mut dyn FnMut(&mut egui::Ui, &Path);

pub struct FileBrowser {
    title: String,
    /// Allowed lowercase extensions (without the dot). Empty = accept any file.
    extensions: Vec<String>,
    cwd: PathBuf,
    entries: Vec<Entry>,
    show_hidden: bool,
    error: Option<String>,
    /// Path currently marked with a dot (the playing file), if any.
    active: Option<PathBuf>,
    /// Live text in the search box; empty means "just show `entries`".
    search_query: String,
    /// Every matching file under the directory the search started in, built
    /// once when the query goes from empty to non-empty and re-scored on
    /// every keystroke rather than re-walking the disk each time.
    search_candidates: Option<Vec<(String, PathBuf)>>,
    /// The file row under the pointer as of the last `ui` call.
    hovered: Option<PathBuf>,
    /// Selected rows of what's listed (the folder, or the search results).
    selection: Selection,
    /// The folder it's kept inside, if any (the mobile layout's song
    /// library): no going above it, and no browsing elsewhere.
    root: Option<PathBuf>,
}

impl FileBrowser {
    pub fn new(title: impl Into<String>, start_dir: PathBuf, extensions: &[&str]) -> Self {
        let cwd = start_dir.canonicalize().unwrap_or(start_dir);
        let mut browser = Self {
            title: title.into(),
            extensions: extensions.iter().map(|e| e.to_lowercase()).collect(),
            cwd,
            entries: Vec::new(),
            show_hidden: false,
            error: None,
            active: None,
            search_query: String::new(),
            search_candidates: None,
            hovered: None,
            selection: Selection::default(),
            root: None,
        };
        browser.refresh();
        browser
    }

    pub fn set_active(&mut self, path: Option<PathBuf>) {
        self.active = path;
    }

    /// The file (not folder) row the pointer is over, if any.
    pub fn hovered(&self) -> Option<&Path> {
        self.hovered.as_deref()
    }

    /// Forget the hovered row; call once a frame before drawing, so a
    /// browser that isn't drawn (its tab closed or hidden) reports nothing.
    pub fn clear_hovered(&mut self) {
        self.hovered = None;
    }

    /// Re-read the current folder (and the search results, mid-search),
    /// e.g. because files in it changed on disk.
    pub fn rescan(&mut self) {
        self.refresh();
        if !self.search_query.trim().is_empty() {
            self.search_candidates = Some(self.collect_candidates());
        }
    }

    /// The directory currently being listed.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Jump straight to `dir` (e.g. from the native folder dialog or a saved
    /// default), clearing any in-progress search.
    pub fn navigate_to(&mut self, dir: PathBuf) {
        self.cwd = dir.canonicalize().unwrap_or(dir);
        self.search_query.clear();
        self.search_candidates = None;
        self.refresh();
    }

    /// Keep the browser inside `root` (or let it go anywhere, `None`),
    /// starting there when that's new. Whether it changed.
    pub fn set_root(&mut self, root: Option<PathBuf>) -> bool {
        let root = root.map(|r| r.canonicalize().unwrap_or(r));
        if root == self.root {
            return false;
        }
        self.root = root.clone();
        if let Some(root) = root {
            self.navigate_to(root);
        }
        true
    }

    /// Open the OS's native folder-picker (drives, network shares, everything
    /// the system browser can reach) and jump there if the user picks one.
    #[cfg(not(target_os = "android"))]
    fn browse_dialog(&mut self) {
        // Point the dialog at the *parent* of the current directory, not the
        // current directory itself. `set_directory` opens straight into that
        // folder's contents, so starting it at `self.cwd` left the current
        // folder with nothing in the list to click "Select Folder" on;
        // starting one level up puts it in the list as a normal, selectable
        // entry instead.
        let start = self.cwd.parent().unwrap_or(&self.cwd);
        if let Some(dir) = rfd::FileDialog::new().set_directory(start).pick_folder() {
            self.navigate_to(dir);
        }
    }

    /// Show only files with these extensions from now on.
    pub fn set_extensions(&mut self, extensions: &[&str]) {
        let extensions: Vec<String> = extensions.iter().map(|e| e.to_lowercase()).collect();
        if extensions != self.extensions {
            self.extensions = extensions;
            self.refresh();
        }
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
        self.selection.clear();
        self.entries.clear();
        self.error = None;

        let at_root = self.root.as_ref().is_some_and(|root| !self.cwd.starts_with(root) || self.cwd == *root);
        if let Some(parent) = self.cwd.parent().filter(|_| !at_root) {
            self.entries.push(Entry {
                label: "..".to_string(),
                path: parent.to_path_buf(),
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

    /// Draw the browser (search box + scrollable list) into `ui`. Returns
    /// `Some(path)` the frame a file row is double-clicked; double-clicking a
    /// directory row navigates into it internally and returns `None`.
    /// `trailing` draws extra widgets at the right end of each file row
    /// (right to left). `menu`, if given, draws a file row's right-click menu.
    pub fn ui(&mut self, ui: &mut egui::Ui, trailing: &mut dyn FnMut(&mut egui::Ui, &Path), mut menu: Option<RowMenu>) -> Option<PathBuf> {
        match &self.root {
            Some(root) => {
                let inside = self.cwd.strip_prefix(root).unwrap_or(Path::new(""));
                if !inside.as_os_str().is_empty() {
                    ui.weak(display_dir(inside));
                }
            }
            None => {
                ui.weak(display_dir(&self.cwd));
            }
        }

        let mut hidden_changed = false;
        ui.horizontal(|ui| {
            // Fixed-size widgets first, then the text edit last with
            // `desired_width(INFINITY)` to soak up whatever's left, sizing it
            // from `ui.available_width()` instead fed back into the layout
            // and made the row (and its containing panel) grow every frame.
            if self.root.is_none() {
                #[cfg(not(target_os = "android"))]
                if ui.button("Browse...").clicked() {
                    self.browse_dialog();
                }
                hidden_changed = ui.checkbox(&mut self.show_hidden, "hidden").changed();
            }

            let response = ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("fuzzy search...")
                    .desired_width(f32::INFINITY),
            );
            if response.changed() {
                self.selection.clear();
                if self.search_query.trim().is_empty() {
                    self.search_candidates = None;
                } else if self.search_candidates.is_none() {
                    self.search_candidates = Some(self.collect_candidates());
                }
            }
        });
        if hidden_changed {
            self.rescan();
        }

        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), err);
        }

        let query = self.search_query.trim();
        let matches;
        let entries: &[Entry] = if query.is_empty() {
            &self.entries
        } else {
            matches = self.search_candidates.as_deref().map(|c| ranked_matches(c, query)).unwrap_or_default();
            &matches
        };
        let active = self.active.as_deref();
        let output = select_list::show(ui, &self.title, entries, &mut self.selection, |ui, row, entry| {
            let is_active = active == Some(entry.path.as_path());
            // ".." stays out of drags: it would add the parent folder.
            if entry.label != ".." && row.response.drag_started() {
                let paths = row
                    .rows()
                    .into_iter()
                    .filter_map(|i| entries.get(i).filter(|e| e.label != ".."))
                    .map(|e| e.path.clone())
                    .collect();
                row.response.dnd_set_drag_payload(DraggedFiles(paths));
            }
            if let Some(menu) = menu.as_mut().filter(|_| !entry.is_dir) {
                row.response.context_menu(|ui| menu(ui, &entry.path));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !entry.is_dir {
                    trailing(ui, &entry.path);
                }
                // Directories already carry a trailing "/" from `refresh`,
                // so the label alone tells them apart.
                let text = egui::RichText::new(&entry.label);
                select_list::label(ui, if is_active { text.strong() } else { text }, None)
            });
        });

        let picked = output.activated.and_then(|i| entries.get(i)).map(|e| (e.path.clone(), e.is_dir));
        self.hovered = output.hovered.and_then(|i| entries.get(i)).filter(|e| !e.is_dir).map(|e| e.path.clone());
        match picked {
            Some((path, true)) => {
                self.navigate_to(path);
                None
            }
            Some((path, false)) => Some(path),
            None => None,
        }
    }
}

/// Drag-and-drop payload for file and folder rows dragged out of the
/// browser (e.g. onto a playlist): the selection if the dragged row is part
/// of it, else just that row.
pub struct DraggedFiles(pub Vec<PathBuf>);

/// Re-rank `candidates` against `query` and return the top matches as `Entry`s.
fn ranked_matches(candidates: &[(String, PathBuf)], query: &str) -> Vec<Entry> {
    let mut scored: Vec<(i32, usize)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, (label, _))| fuzzy_score(label, query).map(|score| (score, i)))
        .collect();
    // Stable sort: ties keep the walk order, which groups by folder.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(MAX_SEARCH_RESULTS);

    scored
        .into_iter()
        .map(|(_, i)| {
            let (label, path) = &candidates[i];
            Entry {
                label: label.clone(),
                path: path.clone(),
                is_dir: false,
            }
        })
        .collect()
}

/// Case-insensitive fuzzy match. A query with spaces is split into terms
/// (e.g. "chrono corridor") and every term must independently subsequence-match
/// `haystack`, this catches concatenated names like "ChronoTrigger/Corridor..."
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
