//! The Songs tab: the song library's songs (`song_store.rs`), by their
//! names (`song_title`), sorted by them or by their original files' names,
//! with a fuzzy search over both. Double-clicking a song plays it (and
//! marks it as playing); rows can be selected several at a time and
//! dragged out together (onto a playlist).

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::select_list::{self, Selection};

/// How many ranked matches are shown for a query.
const MAX_SEARCH_RESULTS: usize = 200;

/// Draws a row's right-click menu, given the songs it applies to (the
/// selection, if the row's in it).
pub type RowMenu<'a> = &'a mut dyn FnMut(&mut egui::Ui, &[PathBuf]);

struct Entry {
    label: String,
    path: PathBuf,
}

#[derive(Default)]
pub struct SongList {
    /// The song marked as playing, if any.
    active: Option<PathBuf>,
    /// Live text in the search box; empty means every song.
    search_query: String,
    /// The row under the pointer as of the last `ui` call.
    hovered: Option<PathBuf>,
    /// Selected rows of what's listed (every song, or the search results).
    selection: Selection,
    /// Sorted by the original files' names, not the names shown.
    sort_by_file_name: bool,
}

/// What [`SongList::ui`] reports.
#[derive(Default)]
pub struct Output {
    /// The song double-clicked.
    pub play: Option<PathBuf>,
    /// Delete was pressed with these selected.
    pub delete: Vec<PathBuf>,
}

impl SongList {
    pub fn set_active(&mut self, path: Option<PathBuf>) {
        self.active = path;
    }

    /// The song row the pointer is over, if any.
    pub fn hovered(&self) -> Option<&Path> {
        self.hovered.as_deref()
    }

    /// Forget the hovered row; call once a frame before drawing, so a
    /// list that isn't drawn (its tab closed or hidden) reports nothing.
    pub fn clear_hovered(&mut self) {
        self.hovered = None;
    }

    /// The library changed: what was selected may be gone.
    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Draw the list (toolbar, search box, rows) into `ui`. `songs` are
    /// the songs to list; `title` names one, `file_name` gives its
    /// original file's name. `tools` draws buttons at the start of the
    /// toolbar, `trailing` extra widgets at the right end of each row
    /// (right to left), and `menu` a row's right-click menu.
    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        songs: &[PathBuf],
        tools: &mut dyn FnMut(&mut egui::Ui),
        title: &mut dyn FnMut(&Path) -> String,
        file_name: &dyn Fn(&Path) -> String,
        trailing: &mut dyn FnMut(&mut egui::Ui, &Path),
        menu: RowMenu,
    ) -> Output {
        ui.horizontal(|ui| {
            // Fixed-size widgets first, then the text edit last with
            // `desired_width(INFINITY)` to soak up whatever's left, sizing it
            // from `ui.available_width()` instead fed back into the layout
            // and made the row (and its containing panel) grow every frame.
            tools(ui);
            let (sort, hint) = if self.sort_by_file_name {
                ("Sort: file", "Sorted by the files' names. Click to sort by the songs' names.")
            } else {
                ("Sort: name", "Sorted by the songs' names. Click to sort by the files' names.")
            };
            if ui.button(sort).on_hover_text(hint).clicked() {
                self.sort_by_file_name = !self.sort_by_file_name;
                self.selection.clear();
            }
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("fuzzy search...")
                    .desired_width(f32::INFINITY),
            );
            if response.changed() {
                self.selection.clear();
            }
        });

        let query = self.search_query.trim();
        let entries: Vec<Entry> = if query.is_empty() {
            let mut entries: Vec<Entry> = songs.iter().map(|p| Entry { label: title(p), path: p.clone() }).collect();
            if self.sort_by_file_name {
                entries.sort_by_cached_key(|e| file_name(&e.path).to_lowercase());
            } else {
                entries.sort_by_cached_key(|e| e.label.to_lowercase());
            }
            entries
        } else {
            ranked_matches(songs, query, title, file_name)
        };
        let entries = entries.as_slice();
        if songs.is_empty() {
            ui.weak("No songs yet: import some, or download them from the Library.");
        }
        let active = self.active.as_deref();
        let output = select_list::show(ui, "songs", entries, &mut self.selection, |ui, row, entry| {
            let rows = || row.rows().into_iter().filter_map(|i| entries.get(i)).map(|e| e.path.clone()).collect::<Vec<_>>();
            if row.response.drag_started() {
                let paths = rows();
                let label = match paths.len() {
                    1 => entry.label.clone(),
                    n => format!("{} and {} more", entry.label, n - 1),
                };
                row.response.dnd_set_drag_payload(DraggedFiles { paths, label });
            }
            row.response.context_menu(|ui| menu(ui, &rows()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                trailing(ui, &entry.path);
                let text = egui::RichText::new(&entry.label);
                select_list::label(ui, if active == Some(entry.path.as_path()) { text.strong() } else { text }, None)
            });
        });

        self.hovered = output.hovered.and_then(|i| entries.get(i)).map(|e| e.path.clone());
        Output {
            play: output.activated.and_then(|i| entries.get(i)).map(|e| e.path.clone()),
            delete: if output.delete {
                self.selection.rows().iter().filter_map(|&i| entries.get(i)).map(|e| e.path.clone()).collect()
            } else {
                Vec::new()
            },
        }
    }
}

/// Drag-and-drop payload for song rows dragged out of the list (onto a
/// playlist): the selection if the dragged row is part of it, else just
/// that row.
pub struct DraggedFiles {
    pub paths: Vec<PathBuf>,
    /// What's being dragged, for the label following the pointer.
    pub label: String,
}

/// `songs` ranked against `query`, by their names (`title`) or their
/// original files' names, whichever matches better: the top matches.
fn ranked_matches(
    songs: &[PathBuf],
    query: &str,
    title: &mut dyn FnMut(&Path) -> String,
    file_name: &dyn Fn(&Path) -> String,
) -> Vec<Entry> {
    let mut scored: Vec<(i32, Entry)> = songs
        .iter()
        .filter_map(|path| {
            let label = title(path);
            let score = fuzzy_score(&label, query).max(fuzzy_score(&file_name(path), query))?;
            Some((score, Entry { label, path: path.clone() }))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.to_lowercase().cmp(&b.1.label.to_lowercase())));
    scored.truncate(MAX_SEARCH_RESULTS);
    scored.into_iter().map(|(_, entry)| entry).collect()
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
