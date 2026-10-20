//! The Songs tab: the song library's songs (`song_store.rs`), by their
//! names (`song_title`), in columns of what's known of them (`View`,
//! synced like the other settings): sorted by any of them, and grouped by
//! one on the fly, with a fuzzy search over the names, file names and tags.
//! Double-clicking a song plays it (and marks it as playing); rows can be
//! selected several at a time and dragged out together (onto a playlist).
//! For touch, the columns go on a second line, and sorting and grouping
//! are in a menu.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::select_list::{self, Selection};

/// How many ranked matches are shown for a query.
const MAX_SEARCH_RESULTS: usize = 200;
/// Room at the end of each row for its own widgets (`trailing`).
const TRAILING_WIDTH: f32 = 36.0;
/// The least the name keeps: columns that don't fit beside it are left out.
const MIN_NAME_WIDTH: f32 = 120.0;

/// Draws a row's right-click menu, given the songs it applies to (the
/// selection, if the row's in it).
pub type RowMenu<'a> = &'a mut dyn FnMut(&mut egui::Ui, &[PathBuf]);

/// The list's columns, sorting and grouping (`Config::songs_view`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    /// Shown after the name, in order (`COLUMNS`' keys).
    pub columns: Vec<String>,
    /// `"name"`, or a column's key.
    pub sort: String,
    pub descending: bool,
    /// A column's key, or empty for no groups.
    pub group: String,
    /// For touch: the column on each row's second line, or empty for none.
    pub second_line: String,
}

impl Default for View {
    fn default() -> Self {
        Self { columns: vec!["artist".into(), "album".into(), "length".into()], sort: "name".into(), descending: false, group: String::new(), second_line: "artist".into() }
    }
}

/// A column the list can show: a song title field (`song_title::FIELDS`)
/// or when the song was imported.
pub struct Column {
    pub key: &'static str,
    pub title: &'static str,
    width: f32,
}

pub const COLUMNS: &[Column] = &[
    Column { key: "artist", title: "Artist", width: 140.0 },
    Column { key: "album", title: "Album", width: 140.0 },
    Column { key: "album_artist", title: "Album artist", width: 140.0 },
    Column { key: "composer", title: "Composer", width: 140.0 },
    Column { key: "genre", title: "Genre", width: 100.0 },
    Column { key: "track", title: "#", width: 36.0 },
    Column { key: "year", title: "Year", width: 48.0 },
    Column { key: "length", title: "Length", width: 56.0 },
    Column { key: "bpm", title: "BPM", width: 48.0 },
    Column { key: "notes", title: "Notes", width: 72.0 },
    Column { key: "file_extension", title: "Type", width: 48.0 },
    Column { key: "imported", title: "Imported", width: 88.0 },
    Column { key: "file_name", title: "File", width: 160.0 },
];

fn column(key: &str) -> Option<&'static Column> {
    COLUMNS.iter().find(|c| c.key == key)
}

/// A column's value for a song: its text, and its number for those that
/// sort as numbers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Value {
    pub text: String,
    pub number: Option<f64>,
}

/// `secs` (Unix seconds) as a date, `2026-10-20` (UTC).
pub fn date(secs: i64) -> String {
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// A song as the list shows it: its name, its original file's name, the
/// values of the columns asked for, in order, and its tags (names and
/// colors).
pub struct Described {
    pub name: String,
    pub file_name: String,
    pub values: Vec<Option<Value>>,
    pub tags: Vec<(String, [u8; 3])>,
}

struct Entry {
    path: PathBuf,
    song: Described,
}

impl Entry {
    fn tags(&self) -> impl DoubleEndedIterator<Item = (&str, [u8; 3])> {
        self.song.tags.iter().map(|(name, color)| (name.as_str(), *color))
    }
}

enum Item {
    /// A group's header: its value (and that in lowercase, which tells
    /// groups apart), how many songs it has, and whether they're hidden.
    Group { label: String, key: String, count: usize, collapsed: bool },
    Song(Entry),
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
    /// The groups closed, by their headers' text (lowercase).
    collapsed: BTreeSet<String>,
    /// Each row's song as of the last `ui` call (`None` for a group).
    listed: Vec<Option<PathBuf>>,
}

/// What [`SongList::ui`] reports.
#[derive(Default)]
pub struct Output {
    /// The song double-clicked.
    pub play: Option<PathBuf>,
    /// Delete was pressed with these selected.
    pub delete: Vec<PathBuf>,
    /// The view changed (to save).
    pub view_changed: bool,
}

/// How two values compare for sorting: numbers as numbers, text without
/// case, a song without one last.
fn compare(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => match (a.number, b.number) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => a.text.to_lowercase().cmp(&b.text.to_lowercase()),
        },
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The group a song with `value` in column `key` is in: the value, or the
/// month for the import date.
fn group_label(key: &str, value: Option<&Value>) -> String {
    match value {
        Some(value) if key == "imported" => value.text.get(..7).unwrap_or(&value.text).to_string(),
        Some(value) => value.text.clone(),
        None => "Unknown".to_string(),
    }
}

/// The menu of the view: what to sort and group by, and which columns (for
/// touch, which one goes on the second line).
fn view_menu(ui: &mut egui::Ui, view: &mut View, touch: bool) -> bool {
    let before = view.clone();
    ui.label("Sort by");
    ui.radio_value(&mut view.sort, "name".to_string(), "Name");
    for c in COLUMNS {
        ui.radio_value(&mut view.sort, c.key.to_string(), c.title);
    }
    ui.checkbox(&mut view.descending, "Descending");
    ui.separator();
    ui.menu_button("Group by", |ui| {
        ui.radio_value(&mut view.group, String::new(), "Nothing");
        for c in COLUMNS.iter().filter(|c| c.key != "file_name") {
            ui.radio_value(&mut view.group, c.key.to_string(), c.title);
        }
    });
    if touch {
        ui.menu_button("Second line", |ui| {
            ui.radio_value(&mut view.second_line, String::new(), "Nothing");
            for c in COLUMNS {
                ui.radio_value(&mut view.second_line, c.key.to_string(), c.title);
            }
        });
        return *view != before;
    }
    ui.menu_button("Columns", |ui| {
        for c in COLUMNS {
            let mut shown = view.columns.iter().any(|k| k == c.key);
            if ui.checkbox(&mut shown, c.title).changed() {
                if shown {
                    view.columns.push(c.key.to_string());
                } else {
                    view.columns.retain(|k| k != c.key);
                }
            }
        }
    });
    *view != before
}

/// A cell `width` wide, its text cut short to fit.
fn cell(ui: &mut egui::Ui, width: f32, text: impl Into<egui::WidgetText>) -> egui::Response {
    let size = egui::vec2(width, ui.available_height());
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(width);
        ui.add(egui::Label::new(text).truncate().selectable(false))
    })
    .inner
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

    /// The one song selected, if just one is.
    pub fn selected_one(&self) -> Option<PathBuf> {
        match self.selection.rows() {
            [one] => self.listed.get(*one).cloned().flatten(),
            _ => None,
        }
    }

    /// The library changed: what was selected may be gone.
    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Draw the list (toolbar, search box, column headers, rows) into
    /// `ui`, as `view` says. `songs` are the songs to list; `describe`
    /// gives one's name, file name and the values of the columns asked
    /// for. `tools` draws buttons at the start of the toolbar, `trailing`
    /// extra widgets at the right end of each row (right to left, in
    /// `TRAILING_WIDTH`), and `menu` a row's right-click menu.
    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        songs: &[PathBuf],
        view: &mut View,
        tools: &mut dyn FnMut(&mut egui::Ui),
        describe: &mut dyn FnMut(&Path, &[&str]) -> Described,
        trailing: &mut dyn FnMut(&mut egui::Ui, &Path),
        menu: RowMenu,
    ) -> Output {
        let touch = select_list::touch(ui.ctx());
        let mut view_changed = false;
        ui.horizontal(|ui| {
            // Fixed-size widgets first, then the text edit last with
            // `desired_width(INFINITY)` to soak up whatever's left, sizing it
            // from `ui.available_width()` instead fed back into the layout
            // and made the row (and its containing panel) grow every frame.
            tools(ui);
            if touch {
                let button = crate::app::icon_button("sort-ascending");
                egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| view_changed |= view_menu(ui, view, true));
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

        // The columns that fit beside the name (for touch, the one on the
        // second line).
        let room = ui.available_width() - TRAILING_WIDTH - MIN_NAME_WIDTH;
        let mut used = 0.0;
        let shown: Vec<&Column> = if touch {
            column(&view.second_line).into_iter().collect()
        } else {
            view.columns
                .iter()
                .filter_map(|k| column(k))
                .take_while(|c| {
                    used += c.width + ui.spacing().item_spacing.x;
                    used <= room
                })
                .collect()
        };
        let mut keys: Vec<&str> = shown.iter().map(|c| c.key).collect();
        let sort_at = column(&view.sort).map(|c| {
            keys.push(c.key);
            keys.len() - 1
        });
        let group_at = column(&view.group).map(|c| {
            keys.push(c.key);
            keys.len() - 1
        });

        if !touch {
            view_changed |= self.header_ui(ui, view, &shown);
        }

        let query = self.search_query.trim();
        let items: Vec<Item> = if query.is_empty() {
            let mut entries: Vec<Entry> = songs.iter().map(|p| Entry { song: describe(p, &keys), path: p.clone() }).collect();
            let by_sort = |a: &Entry, b: &Entry| {
                let order = match sort_at {
                    Some(i) => compare(a.song.values[i].as_ref(), b.song.values[i].as_ref()),
                    None => a.song.name.to_lowercase().cmp(&b.song.name.to_lowercase()),
                };
                if view.descending { order.reverse() } else { order }
            };
            match group_at {
                None => {
                    entries.sort_by(by_sort);
                    entries.into_iter().map(Item::Song).collect()
                }
                Some(g) => {
                    let label = |e: &Entry| group_label(&view.group, e.song.values[g].as_ref());
                    entries.sort_by(|a, b| {
                        let (la, lb) = (label(a).to_lowercase(), label(b).to_lowercase());
                        let group = match (la == "Unknown", lb == "Unknown") {
                            (true, false) => Ordering::Greater,
                            (false, true) => Ordering::Less,
                            _ => compare(a.song.values[g].as_ref(), b.song.values[g].as_ref()).then_with(|| la.cmp(&lb)),
                        };
                        if la == lb { by_sort(a, b) } else { group }
                    });
                    self.grouped(entries, label)
                }
            }
        } else {
            ranked_matches(songs, query, &keys, describe).into_iter().map(Item::Song).collect()
        };
        let items = items.as_slice();
        if songs.is_empty() {
            ui.weak("No songs yet: import some, or download them from the Library.");
        }
        let active = self.active.as_deref();
        let mut toggle = None;
        let path_of = |i: usize| match items.get(i) {
            Some(Item::Song(entry)) => Some(entry.path.clone()),
            _ => None,
        };
        let output = select_list::show(ui, "songs", items, &mut self.selection, |ui, row, item| {
            let entry = match item {
                Item::Group { label, key, count, collapsed } => {
                    let arrow = crate::app::icon(if *collapsed { "caret-right" } else { "caret-down" });
                    select_list::label(ui, egui::RichText::new(format!("{arrow} {label} ({count})")).strong(), None);
                    if row.response.clicked() {
                        toggle = Some(key.clone());
                    }
                    return;
                }
                Item::Song(entry) => entry,
            };
            let rows = || row.rows().into_iter().filter_map(path_of).collect::<Vec<_>>();
            if row.response.drag_started() {
                let paths = rows();
                let label = match paths.len() {
                    1 => entry.song.name.clone(),
                    n => format!("{} and {} more", entry.song.name, n - 1),
                };
                row.response.dnd_set_drag_payload(DraggedFiles { paths, label });
            }
            row.response.context_menu(|ui| menu(ui, &rows()));
            let text = egui::RichText::new(&entry.song.name);
            let text = if active == Some(entry.path.as_path()) { text.strong() } else { text };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let size = egui::vec2(TRAILING_WIDTH, ui.available_height());
                ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.set_width(TRAILING_WIDTH);
                    trailing(ui, &entry.path);
                });
                if touch {
                    let second = shown.first().and(entry.song.values.first()).cloned().flatten();
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.add_space(4.0);
                        select_list::label(ui, text, None);
                        ui.horizontal(|ui| {
                            select_list::chips(ui, entry.tags());
                            if let Some(second) = second {
                                select_list::label(ui, egui::RichText::new(second.text).small().weak(), None);
                            }
                        });
                    });
                    return;
                }
                for (c, value) in shown.iter().zip(&entry.song.values).rev() {
                    cell(ui, c.width, egui::RichText::new(value.as_ref().map_or("", |v| v.text.as_str())).weak());
                }
                select_list::chips(ui, entry.tags());
                select_list::label(ui, text, None);
            });
        });
        if let Some(key) = toggle {
            if !self.collapsed.remove(&key) {
                self.collapsed.insert(key);
            }
            self.selection.clear();
        }
        if view_changed {
            self.selection.clear();
        }

        self.hovered = output.hovered.and_then(path_of);
        self.listed = (0..items.len()).map(path_of).collect();
        Output {
            play: output.activated.and_then(path_of),
            delete: if output.delete { self.selection.rows().iter().filter_map(|&i| path_of(i)).collect() } else { Vec::new() },
            view_changed,
        }
    }

    /// The column headers: click one to sort by it (again to turn it
    /// around), right-click for the view's menu.
    fn header_ui(&mut self, ui: &mut egui::Ui, view: &mut View, shown: &[&Column]) -> bool {
        let mut changed = false;
        let mut sort_by = None;
        let header = ui
            .horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(TRAILING_WIDTH + ui.spacing().item_spacing.x);
                    let arrow = format!(" {}", crate::app::icon(if view.descending { "caret-down" } else { "caret-up" }));
                    let title = |key: &str, title: &str| if view.sort == key { format!("{title}{arrow}") } else { title.to_string() };
                    let hint = "Click to sort by it (again to turn it around); right-click for columns and groups";
                    for c in shown.iter().rev() {
                        let text = egui::RichText::new(title(c.key, c.title)).small().strong();
                        if cell(ui, c.width, text).interact(egui::Sense::click()).on_hover_text(hint).clicked() {
                            sort_by = Some(c.key);
                        }
                    }
                    let text = egui::RichText::new(title("name", "Name")).small().strong();
                    let name = ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(text).truncate().selectable(false).sense(egui::Sense::click()))
                    });
                    if name.inner.on_hover_text(hint).clicked() {
                        sort_by = Some("name");
                    }
                });
            })
            .response;
        header.context_menu(|ui| changed |= view_menu(ui, view, false));
        if let Some(key) = sort_by {
            if view.sort == key {
                view.descending = !view.descending;
            } else {
                view.sort = key.to_string();
                view.descending = false;
            }
            changed = true;
        }
        changed
    }

    /// `entries`, sorted by group, with each group's header before them
    /// (and them left out if it's closed).
    fn grouped(&self, entries: Vec<Entry>, label: impl Fn(&Entry) -> String) -> Vec<Item> {
        let mut items = Vec::new();
        let mut current: Option<(usize, String)> = None;
        for entry in entries {
            let label = label(&entry);
            let group = label.to_lowercase();
            if current.as_ref().is_none_or(|(_, key)| *key != group) {
                let collapsed = self.collapsed.contains(&group);
                items.push(Item::Group { label, key: group.clone(), count: 0, collapsed });
                current = Some((items.len() - 1, group));
            }
            let (at, group) = current.as_ref().expect("a group was just started");
            if let Some(Item::Group { count, .. }) = items.get_mut(*at) {
                *count += 1;
            }
            if !self.collapsed.contains(group) {
                items.push(Item::Song(entry));
            }
        }
        items
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

/// `songs` ranked against `query`, by their names, their original files'
/// names or their tags, whichever matches best: the top matches.
fn ranked_matches(songs: &[PathBuf], query: &str, keys: &[&str], describe: &mut dyn FnMut(&Path, &[&str]) -> Described) -> Vec<Entry> {
    let mut scored: Vec<(i32, Entry)> = songs
        .iter()
        .filter_map(|path| {
            let song = describe(path, keys);
            let tagged = song.tags.iter().filter_map(|(name, _)| fuzzy_score(name, query)).max();
            let score = fuzzy_score(&song.name, query).max(fuzzy_score(&song.file_name, query)).max(tagged)?;
            Some((score, Entry { song, path: path.clone() }))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.song.name.to_lowercase().cmp(&b.1.song.name.to_lowercase())));
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
