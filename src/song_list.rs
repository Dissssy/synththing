//! The Songs tab: the song library's songs (`song_store.rs`), by their
//! names (`song_title`), in columns of what's known of them (`View`,
//! synced like the other settings): sorted by any of them, and grouped by
//! one on the fly (or by tag, a song in each of its tags' groups), with a
//! fuzzy search over the names, file names and tags, and filters
//! (`type:mid`, `search.rs`): with only filters, the list stays sorted and
//! grouped.
//! Double-clicking a song plays it (and marks it as playing); rows can be
//! selected several at a time and dragged out together (onto a playlist).
//! The alien at the left of each row stars it, or unstars it.
//! For touch, the column sorted by goes on a second line, and sorting and
//! grouping are in a menu.
//! Autoplay plays on down the list as it is when each song ends (`next`).

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::fields::{self, Def, Field, Value};
use crate::select_list::{self, Selection};
use crate::together::{self, Member};

/// How many ranked matches are shown for a query.
const MAX_SEARCH_RESULTS: usize = 200;
/// Room at the end of each row for its own widgets (`trailing`).
const TRAILING_WIDTH: f32 = 36.0;
/// The least the name keeps: columns that don't fit beside it are left out.
const MIN_NAME_WIDTH: f32 = 120.0;

/// What a song started from the list plays on through when it ends
/// (`Config::autoplay`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Autoplay {
    /// Just the song.
    #[default]
    Off,
    /// The rest of its group (the whole list, when it isn't grouped).
    Group,
    /// The rest of the list, group after group.
    All,
}

impl Autoplay {
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Group,
            Self::Group => Self::All,
            Self::All => Self::Off,
        }
    }
}

/// A song's place in the list: the song, the group it's in there (its
/// header's key; `None` when the list isn't grouped), and its row in the
/// list's play order, to carry on from if it leaves the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Position {
    pub path: PathBuf,
    pub group: Option<String>,
    at: usize,
}

/// Playing on down the list (`Autoplay`), or through an album's or
/// artist's songs: the song playing, the songs that have had their turn
/// this pass through (oldest first, the one playing last), so none plays
/// twice, and the one picked to come next (so it can be preloaded, and a
/// shuffle's pick holds).
#[derive(Clone, Debug)]
pub struct Playing {
    pub at: Position,
    /// The songs played through, when they aren't the list's.
    tracks: Option<Arc<[Queued]>>,
    pub history: Vec<PathBuf>,
    /// With whether it starts a new pass (see [`SongList::next`]).
    pub upcoming: Option<(Position, bool)>,
    /// The list's `generation` `upcoming` was picked from.
    pub planned: u64,
}

impl Playing {
    pub fn new(at: Position) -> Self {
        Self { history: vec![at.path.clone()], at, tracks: None, upcoming: None, planned: 0 }
    }

    /// Playing through `tracks` (each with how it plays) from the one at
    /// `start`, whatever Autoplay is set to.
    pub fn tracks(tracks: impl IntoIterator<Item = (PathBuf, Plays)>, start: usize) -> Option<Self> {
        let tracks: Arc<[Queued]> = tracks.into_iter().map(|(path, plays)| Queued { path, group: None, collapsed: false, plays }).collect();
        let at = position(&tracks, start.min(tracks.len().checked_sub(1)?));
        Some(Self { tracks: Some(tracks), ..Self::new(at) })
    }

    /// Whether it plays through songs of its own rather than the list.
    pub fn has_tracks(&self) -> bool {
        self.tracks.is_some()
    }

    /// The songs it goes through and the rules it goes by: its own (to
    /// their end, whatever Autoplay says), else the list's.
    fn source<'a>(&'a self, list: Option<&'a [Queued]>, rules: Rules) -> Option<(&'a [Queued], Rules)> {
        match &self.tracks {
            Some(tracks) => Some((tracks, Rules { autoplay: Autoplay::All, skip_collapsed: false, ..rules })),
            None => Some((list?, rules)),
        }
    }
}

/// How the next song's picked: how far autoplay goes, whether it goes
/// round again at the end, shuffles, skips the closed groups, and plays
/// only starred songs.
#[derive(Clone, Copy, Debug)]
pub struct Rules {
    pub autoplay: Autoplay,
    pub loop_all: bool,
    pub shuffle: bool,
    pub skip_collapsed: bool,
    pub only_starred: bool,
}

/// A song in the list's play order: every song listed, the closed groups'
/// too, each in the group it's listed in.
#[derive(Clone, Debug)]
struct Queued {
    path: PathBuf,
    group: Option<String>,
    collapsed: bool,
    plays: Plays,
}

/// What of a song decides when it plays: whether it's starred, whether
/// shuffling passes over it (`Field::SkipShuffle`), and the songs it plays
/// together with (`SongStore::together_of`).
#[derive(Clone, Debug, Default)]
pub struct Plays {
    pub starred: bool,
    pub skip_shuffle: bool,
    pub together: Option<InSet>,
}

/// Where a song is in the set it plays together with: the set's ID, its
/// place in it, and how many songs it has.
#[derive(Clone, Debug)]
pub struct InSet {
    pub set: String,
    pub at: usize,
    pub of: usize,
}

/// Draws a row's right-click menu, given the songs it applies to (the
/// selection, if the row's in it).
pub type RowMenu<'a> = &'a mut dyn FnMut(&mut egui::Ui, &[PathBuf]);

/// The list's columns, sorting and grouping (`Config::songs_view`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    /// Shown after the name, in order (columns' keys).
    pub columns: Vec<String>,
    /// `"name"`, or a column's key.
    pub sort: String,
    pub descending: bool,
    /// A groupable column's key, `"tag"`, or empty for no groups.
    pub group: String,
}

impl Default for View {
    fn default() -> Self {
        Self { columns: vec!["artist".into(), "album".into(), "length".into()], sort: "name".into(), descending: false, group: String::new() }
    }
}

/// A column the list can show: a field with a width (`fields::columns`).
fn column(key: &str) -> Option<&'static Def> {
    Field::by_key(key).map(Field::def).filter(|d| d.width > 0.0)
}

/// Group by the songs' tags (`View::group`): a song is in each of its
/// tags' groups.
const TAG: &str = "tag";

/// What the list's grouped by, for the groups' headers.
fn group_title(key: &str) -> Option<&'static str> {
    if key == TAG { Some("Tag") } else { column(key).map(|c| c.label) }
}

/// A song as the list shows it: its name, its original file's name, the
/// values of the columns asked for, in order, its tags (names and
/// colors), and how it plays.
#[derive(Clone)]
pub struct Described {
    pub name: String,
    pub file_name: String,
    pub values: Vec<Option<Value>>,
    pub tags: Vec<(String, [u8; 3])>,
    pub plays: Plays,
}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    song: Described,
}

impl Entry {
    fn tags(&self) -> impl DoubleEndedIterator<Item = (&str, [u8; 3])> {
        self.song.tags.iter().map(|(name, color)| (name.as_str(), *color))
    }
}

/// The rows as last made, and what they were made from: made again only
/// when that changes, not every frame.
struct Built {
    revision: u64,
    view: View,
    keys: Vec<&'static str>,
    query: String,
    /// There are no songs at all.
    empty: bool,
    items: Vec<Item>,
    /// What autoplay goes through, in order.
    queue: Vec<Queued>,
}

enum Item {
    /// A group's header: its value (and that in lowercase, which tells
    /// groups apart), how many songs it has, and whether they're hidden.
    Group { label: String, key: String, count: usize, collapsed: bool },
    Song(Entry),
}

#[derive(Default)]
pub struct SongList {
    /// The song marked as playing, if any, and the group it's playing in
    /// (when autoplay knows it; else its first).
    active: Option<(PathBuf, Option<String>)>,
    /// The row of `active` in the rows as made (worked out once a time).
    active_row: Option<Option<usize>>,
    /// Live text in the search box; empty means every song.
    search_query: String,
    /// The row under the pointer as of the last `ui` call.
    hovered: Option<PathBuf>,
    /// Selected rows of what's listed (every song, or the search results).
    selection: Selection,
    /// The groups closed, by their headers' text (lowercase).
    collapsed: BTreeSet<String>,
    built: Option<Built>,
    /// Counts the times the rows were made, so autoplay knows when to
    /// look again at what comes next.
    generation: u64,
}

/// What [`SongList::ui`] reports.
#[derive(Default)]
pub struct Output {
    /// The song double-clicked, where it was.
    pub play: Option<Position>,
    /// Delete was pressed with these selected.
    pub delete: Vec<PathBuf>,
    /// The view changed (to save).
    pub view_changed: bool,
    /// The song's note was clicked: star it (`true`) or unstar it.
    pub star: Option<(PathBuf, bool)>,
}

/// What a song sorts by for `value`: what it sorts as, else its text
/// (`fields::sort_text`, past a leading article unless `articles`),
/// made once for [`compare`] rather than each time two songs are.
fn sort_value(value: Option<&Value>, articles: bool) -> Option<Value> {
    value.map(|v| Value {
        text: match &v.sort {
            Some(sort) => sort.to_lowercase(),
            None => fields::sort_text(&v.text, articles),
        },
        number: v.number,
        sort: None,
    })
}

/// How two values ([`sort_value`]s) compare for sorting: numbers as
/// numbers, text without case, a song without one last.
fn compare(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => match (a.number, b.number) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => a.text.cmp(&b.text),
        },
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The group a song with `value` in column `key` is in: the value, or the
/// month for the import date; `None` for the songs without one.
fn group_label(key: &str, value: Option<&Value>) -> Option<String> {
    let value = value?;
    Some(if key == "imported" { value.text.get(..7).unwrap_or(&value.text).to_string() } else { value.text.clone() })
}

/// `paths` without repeats (a song can be listed in several groups).
fn unique(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    paths.into_iter().filter(|p| seen.insert(p.clone())).collect()
}

/// The menu of the view: what to sort and group by, and (not for touch)
/// which columns.
fn view_menu(ui: &mut egui::Ui, view: &mut View, touch: bool) -> bool {
    let before = view.clone();
    ui.menu_button("Sort by", |ui| {
        ui.radio_value(&mut view.sort, "name".to_string(), "Name");
        for c in fields::columns() {
            ui.radio_value(&mut view.sort, c.key.to_string(), c.label);
        }
        ui.separator();
        ui.checkbox(&mut view.descending, "Descending");
    });
    ui.menu_button("Group by", |ui| {
        ui.radio_value(&mut view.group, String::new(), "Nothing");
        ui.separator();
        for c in fields::columns().filter(|c| c.groups) {
            ui.radio_value(&mut view.group, c.key.to_string(), c.label);
        }
        ui.radio_value(&mut view.group, TAG.to_string(), "Tag");
    });
    if !touch {
        ui.menu_button("Columns", |ui| {
            for c in fields::columns() {
                let mut shown = view.columns.iter().any(|k| k == c.key);
                if ui.checkbox(&mut shown, c.label).changed() {
                    if shown {
                        view.columns.push(c.key.to_string());
                    } else {
                        view.columns.retain(|k| k != c.key);
                    }
                }
            }
        });
    }
    *view != before
}

/// A column's cell `width` wide (in a right-to-left layout), its text cut
/// short to fit (or scrolling, while `scroll` is on), with a line before it.
fn cell(ui: &mut egui::Ui, width: f32, text: impl Into<egui::WidgetText>, scroll: bool) -> egui::Response {
    let size = egui::vec2(width, ui.available_height());
    let response = ui
        .allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(width);
            crate::marquee::show(ui, text, scroll)
        })
        .inner;
    let x = response.rect.left() - ui.spacing().item_spacing.x / 2.0;
    ui.painter().vline(x, ui.max_rect().y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
    response
}

impl SongList {
    pub fn set_active(&mut self, active: Option<(PathBuf, Option<String>)>) {
        self.active = active;
        self.active_row = None;
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

    /// The songs selected, each once.
    pub fn selected(&self) -> Vec<PathBuf> {
        let Some(built) = &self.built else { return Vec::new() };
        unique(self.selection.rows().iter().filter_map(|&i| match built.items.get(i) {
            Some(Item::Song(entry)) => Some(entry.path.clone()),
            _ => None,
        }))
    }

    /// The library changed: what was selected may be gone.
    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Goes up each time the rows are made again (the songs, the view, the
    /// search or the groups closed changed).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// How many songs autoplay could go through (the closed groups' too).
    pub fn queue_len(&self) -> usize {
        self.built.as_ref().map_or(0, |b| b.queue.len())
    }

    /// Where the song on row `row` is: its group is the header above it.
    fn position_of_row(&self, row: usize) -> Option<Position> {
        let built = self.built.as_ref()?;
        let Some(Item::Song(entry)) = built.items.get(row) else { return None };
        let group = built.items[..row].iter().rev().find_map(|item| match item {
            Item::Group { key, .. } => Some(key.clone()),
            Item::Song(_) => None,
        });
        let at = built.queue.iter().position(|q| q.path == entry.path && q.group == group).unwrap_or(0);
        Some(Position { path: entry.path.clone(), group, at })
    }

    /// Where `path` is in the list (in its first group, if it's in several).
    pub fn position_of(&self, path: &Path) -> Option<Position> {
        let queue = &self.built.as_ref()?.queue;
        let at = queue.iter().position(|q| q.path == path)?;
        Some(Position { path: path.to_path_buf(), group: queue[at].group.clone(), at })
    }

    /// The song to play after `playing`'s, in the list as it is now, and
    /// whether it starts a new pass through (the history starts again).
    pub fn next(&self, playing: &Playing, rules: Rules, random: u64) -> Option<(Position, bool)> {
        let (queue, rules) = playing.source(self.built.as_ref().map(|b| &b.queue[..]), rules)?;
        next_in(queue, playing, rules, random)
    }

    /// The song "Previous" goes to from `playing`'s.
    pub fn previous(&self, playing: &Playing, rules: Rules) -> Option<Position> {
        let (queue, rules) = playing.source(self.built.as_ref().map(|b| &b.queue[..]), rules)?;
        previous_in(queue, playing, rules)
    }

    /// Draw the list (toolbar, search box, column headers, rows) into
    /// `ui`, as `view` says. `songs` gives the songs a query's filters
    /// keep and the text left to search for, and
    /// `describe` one's name, file name and the values of the columns
    /// asked for; they're asked only when the rows are made again: when
    /// `revision` (which changes whenever the songs or what's said of them
    /// do, `ignore` included), the view, the search or the groups closed
    /// change. Groups go by `fields::group_key`, past the characters in
    /// `ignore`, and text sorts past a leading article unless `articles`.
    /// `tools` draws buttons at the start of the toolbar, `trailing`
    /// extra widgets at the right end of each row (right to left, in
    /// `TRAILING_WIDTH`), and `menu` a row's right-click menu.
    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        songs: &mut dyn FnMut(&str) -> (Vec<PathBuf>, String),
        revision: u64,
        view: &mut View,
        ignore: &str,
        articles: bool,
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
                    .hint_text("search, or filter: type:mid...")
                    .desired_width(f32::INFINITY),
            )
            .on_hover_text(crate::search::HELP);
            if response.changed() {
                self.selection.clear();
            }
        });

        // The columns that fit beside the name, and the one sorted by
        // (for touch, just that, on the second line).
        let room = ui.available_width() - TRAILING_WIDTH - MIN_NAME_WIDTH;
        let mut used = 0.0;
        let sorted = column(&view.sort);
        let shown: Vec<&'static Def> = if touch {
            sorted.into_iter().collect()
        } else {
            let hidden = sorted.filter(|c| !view.columns.iter().any(|k| k == c.key));
            // (One that isn't a column of the user's goes first, to fit.)
            hidden
                .into_iter()
                .chain(view.columns.iter().filter_map(|k| column(k)))
                .take_while(|c| {
                    used += c.width + ui.spacing().item_spacing.x;
                    used <= room
                })
                .collect()
        };
        let mut keys: Vec<&'static str> = shown.iter().map(|c| c.key).collect();
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
        let stale = self
            .built
            .as_ref()
            .is_none_or(|b| b.revision != revision || b.view != *view || b.keys != keys || b.query != query);
        if stale {
            let (songs, text) = songs(query);
            let (items, queue) = if text.is_empty() {
                self.sorted(&songs, view, &keys, sort_at, group_at, ignore, articles, describe)
            } else {
                let found = ranked_matches(&songs, &text, &keys, describe);
                let queue = found.iter().map(|e| Queued { path: e.path.clone(), group: None, collapsed: false, plays: e.song.plays.clone() }).collect();
                (found.into_iter().map(Item::Song).collect(), queue)
            };
            let empty = songs.is_empty() && query.is_empty();
            let query = query.to_string();
            self.generation += 1;
            self.built = Some(Built { revision, view: view.clone(), keys: keys.clone(), query, empty, items, queue });
            self.active_row = None;
        }
        let built = self.built.as_ref().expect("the rows were just made");
        let items = built.items.as_slice();
        if built.empty {
            ui.weak("No songs yet: import some, or download them from the Library.");
        }
        let active = *self.active_row.get_or_insert_with(|| self.active.as_ref().and_then(|(path, group)| active_row(items, path, group.as_deref())));
        let mut toggle = None;
        let mut star = None;
        let path_of = |i: usize| match items.get(i) {
            Some(Item::Song(entry)) => Some(entry.path.clone()),
            _ => None,
        };
        let grouped_by = group_title(&view.group);
        let output = select_list::show(ui, "songs", items, &mut self.selection, active, |ui, row, item| {
            let entry = match item {
                Item::Group { label, key, count, collapsed } => {
                    let arrow = crate::app::icon(if *collapsed { "caret-right" } else { "caret-down" });
                    let rect = ui.max_rect();
                    ui.painter().rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
                    ui.painter().hline(rect.x_range(), rect.bottom(), ui.visuals().widgets.noninteractive.bg_stroke);
                    ui.label(egui::RichText::new(arrow).strong());
                    if let Some(title) = grouped_by {
                        ui.label(egui::RichText::new(format!("{title}:")).weak());
                    }
                    // (The count at the end, so a long name can't push it
                    // off the row.)
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!("({count})")).weak());
                        select_list::label(ui, egui::RichText::new(label).strong(), None, select_list::scrolls(row, false));
                    });
                    if row.response.clicked() {
                        toggle = Some(key.clone());
                    }
                    return;
                }
                Item::Song(entry) => entry,
            };
            let rows = || unique(row.rows().into_iter().filter_map(path_of));
            if row.response.drag_started() {
                let paths = rows();
                let label = match paths.len() {
                    1 => entry.song.name.clone(),
                    n => format!("{} and {} more", entry.song.name, n - 1),
                };
                row.response.dnd_set_drag_payload(DraggedFiles { paths, label });
            }
            row.response.context_menu(|ui| menu(ui, &rows()));
            if select_list::star(ui, entry.song.plays.starred) {
                star = Some((entry.path.clone(), !entry.song.plays.starred));
            }
            let text = egui::RichText::new(&entry.song.name);
            let scroll = select_list::scrolls(row, active == Some(row.idx));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let size = egui::vec2(TRAILING_WIDTH, ui.available_height());
                ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.set_width(TRAILING_WIDTH);
                    trailing(ui, &entry.path);
                });
                // (Linked, if it plays together with others.)
                let linked = |ui: &mut egui::Ui| {
                    if let Some(set) = &entry.song.plays.together {
                        ui.label(egui::RichText::new(crate::app::icon("link")).weak())
                            .on_hover_text(format!("Plays together with others: {} of {} (Song info > Options lists them)", set.at + 1, set.of));
                    }
                };
                if touch {
                    linked(ui);
                    // The name and the tags, and under them the value
                    // sorted by.
                    let second = shown.first().and(entry.song.values.first()).cloned().flatten();
                    select_list::two_lines(ui, entry.tags(), text, second.map(|v| v.text), scroll);
                    return;
                }
                for (c, value) in shown.iter().zip(&entry.song.values).rev() {
                    cell(ui, c.width, egui::RichText::new(value.as_ref().map_or("", |v| v.text.as_str())).weak(), scroll);
                }
                select_list::chips(ui, entry.tags());
                linked(ui);
                select_list::label(ui, text, None, scroll);
            });
        });
        self.hovered = output.hovered.and_then(path_of);
        let output = Output {
            play: output.activated.and_then(|i| self.position_of_row(i)),
            delete: if output.delete { unique(self.selection.rows().iter().filter_map(|&i| path_of(i))) } else { Vec::new() },
            view_changed,
            star,
        };
        if let Some(key) = toggle {
            if !self.collapsed.remove(&key) {
                self.collapsed.insert(key);
            }
            self.built = None;
            self.selection.clear();
        }
        if view_changed {
            self.selection.clear();
        }
        output
    }

    /// The column headers: click one to sort by it (again to turn it
    /// around), right-click for the view's menu.
    fn header_ui(&mut self, ui: &mut egui::Ui, view: &mut View, shown: &[&Def]) -> bool {
        let height = ui.spacing().interact_size.y;
        // The whole row first, under the headers, for right-clicks between them.
        let (rect, background) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
        ui.painter().rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        ui.painter().hline(rect.x_range(), rect.bottom(), ui.visuals().widgets.noninteractive.bg_stroke);
        let arrow = format!(" {}", crate::app::icon(if view.descending { "caret-down" } else { "caret-up" }));
        let title = |key: &str, title: &str| {
            let text = if view.sort == key { format!("{title}{arrow}") } else { title.to_string() };
            egui::RichText::new(text).strong()
        };
        let hint = "Click to sort by it (again to turn it around); right-click for columns and groups";
        let mut headers: Vec<(&str, egui::Response)> = Vec::new();
        let builder = egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center));
        ui.scope_builder(builder, |ui| {
            ui.add_space(TRAILING_WIDTH + ui.spacing().item_spacing.x);
            for c in shown.iter().rev() {
                let response = cell(ui, c.width, title(c.key, c.label), false).interact(egui::Sense::click());
                headers.push((c.key, response));
            }
            let name = ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // (Over the rows' notes.)
                ui.add_space(select_list::STAR_WIDTH + ui.spacing().item_spacing.x);
                ui.add(egui::Label::new(title("name", "Name")).truncate().selectable(false).sense(egui::Sense::click()))
            });
            headers.push(("name", name.inner));
        });
        let mut changed = false;
        let mut sort_by = None;
        for (key, response) in &headers {
            let response = response.clone().on_hover_text(hint);
            if response.clicked() {
                sort_by = Some(*key);
            }
            response.context_menu(|ui| changed |= view_menu(ui, view, false));
        }
        background.context_menu(|ui| changed |= view_menu(ui, view, false));
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

    /// Every song in `songs`, sorted as `view` says, by the value at
    /// `sort_at` (or the name), and grouped by the one at `group_at` (or by
    /// tag, a song in each of its tags' groups), each group's header before
    /// its songs (left out if it's closed). The songs without a value (or
    /// tag) are a group of their own, last.
    fn sorted(
        &self,
        songs: &[PathBuf],
        view: &View,
        keys: &[&str],
        sort_at: Option<usize>,
        group_at: Option<usize>,
        ignore: &str,
        articles: bool,
        describe: &mut dyn FnMut(&Path, &[&str]) -> Described,
    ) -> (Vec<Item>, Vec<Queued>) {
        /// A song in one of its groups, and what it sorts by: the group
        /// (its header's text and that in lowercase, `None` for the songs
        /// without a value, and the value) and its place in it.
        struct Sorting {
            label: Option<String>,
            group: Option<String>,
            value: Option<Value>,
            order: Option<Value>,
            entry: Entry,
        }
        let by_tag = view.group == TAG;
        let mut sorting: Vec<Sorting> = Vec::with_capacity(songs.len());
        for path in songs {
            let entry = Entry { song: describe(path, keys), path: path.clone() };
            let order = match sort_at {
                Some(i) => sort_value(entry.song.values[i].as_ref(), articles),
                None => Some(Value { text: fields::sort_text(&entry.song.name, articles), ..Value::default() }),
            };
            let groups: Vec<(Option<String>, Option<Value>)> = match group_at {
                _ if by_tag && !entry.song.tags.is_empty() => {
                    entry.song.tags.iter().map(|(name, _)| (Some(name.clone()), Some(Value { text: name.to_lowercase(), ..Value::default() }))).collect()
                }
                Some(g) => {
                    let value = entry.song.values[g].as_ref();
                    // (Sorted by what tells the groups apart, so their
                    // songs stay together.)
                    let group = |label: String| {
                        let key = Value { text: fields::group_key(&label, ignore), number: value.and_then(|v| v.number), sort: None };
                        (Some(label), Some(key))
                    };
                    match group_label(&view.group, value) {
                        // (A song in each of a list's items' groups.)
                        Some(label) if let Some(def) = column(&view.group).filter(|c| c.is_list()) => {
                            let items: Vec<_> = def.split(&label).map(|i| group(i.to_string())).collect();
                            if items.is_empty() { vec![(None, None)] } else { items }
                        }
                        Some(label) => vec![group(label)],
                        None => vec![(None, None)],
                    }
                }
                None => vec![(None, None)],
            };
            let mut seen = BTreeSet::new();
            for (label, value) in groups {
                let group = value.as_ref().map(|v| v.text.clone());
                // (Once, even with two tags that differ only in case.)
                if seen.insert(group.clone()) {
                    sorting.push(Sorting { label, group, value, order: order.clone(), entry: entry.clone() });
                }
            }
        }
        sorting.sort_by(|a, b| {
            if a.group == b.group {
                let order = compare(a.order.as_ref(), b.order.as_ref());
                if view.descending { order.reverse() } else { order }
            } else {
                compare(a.value.as_ref(), b.value.as_ref()).then_with(|| a.group.cmp(&b.group))
            }
        });
        if group_at.is_none() && !by_tag {
            let queue = sorting.iter().map(|s| Queued { path: s.entry.path.clone(), group: None, collapsed: false, plays: s.entry.song.plays.clone() }).collect();
            return (sorting.into_iter().map(|s| Item::Song(s.entry)).collect(), queue);
        }
        // Each group's header shows the best of its songs' spellings.
        let mut spellings: HashMap<&str, Vec<&str>> = HashMap::new();
        for s in &sorting {
            if let (Some(group), Some(label)) = (&s.group, &s.label) {
                spellings.entry(group).or_default().push(label);
            }
        }
        let best: HashMap<String, String> =
            spellings.into_iter().filter_map(|(group, labels)| Some((group.to_string(), fields::best_spelling(labels)?.to_string()))).collect();
        let missing = if by_tag { "Untagged" } else { "Unknown" };
        let mut items = Vec::new();
        let mut queue = Vec::with_capacity(sorting.len());
        let mut current: Option<(usize, Option<String>)> = None;
        for Sorting { label, group, entry, .. } in sorting {
            let at = match &current {
                Some((at, key)) if *key == group => *at,
                _ => {
                    let key = group.clone().unwrap_or_default();
                    let collapsed = self.collapsed.contains(&key);
                    let label = best.get(&key).cloned().or(label).unwrap_or_else(|| missing.to_string());
                    items.push(Item::Group { label, key, count: 0, collapsed });
                    current = Some((items.len() - 1, group));
                    items.len() - 1
                }
            };
            let Item::Group { key, count, collapsed, .. } = &mut items[at] else { unreachable!("`current` is a group's header") };
            *count += 1;
            queue.push(Queued { path: entry.path.clone(), group: Some(key.clone()), collapsed: *collapsed, plays: entry.song.plays.clone() });
            if !*collapsed {
                items.push(Item::Song(entry));
            }
        }
        (items, queue)
    }
}

/// The row of `path` in `items`: under the header of `group` if it's
/// listed there, else its first.
fn active_row(items: &[Item], path: &Path, group: Option<&str>) -> Option<usize> {
    let mut header: Option<&str> = None;
    let mut first = None;
    for (i, item) in items.iter().enumerate() {
        match item {
            Item::Group { key, .. } => header = Some(key),
            Item::Song(entry) if entry.path == path => {
                if group.is_none() || header == group {
                    return Some(i);
                }
                first = first.or(Some(i));
            }
            Item::Song(_) => {}
        }
    }
    first
}

/// Where `at` is in `queue` now, and the group it's in there: its own
/// group, else (the list grouped another way) its first, else (it's left
/// the list) `None`, in the group of the song now in its old place.
fn locate(queue: &[Queued], at: &Position) -> (Option<usize>, Option<String>) {
    let found = queue
        .iter()
        .position(|q| q.path == at.path && q.group == at.group)
        .or_else(|| queue.iter().position(|q| q.path == at.path));
    match found {
        Some(i) => (Some(i), queue[i].group.clone()),
        None => (None, queue.get(at.at.min(queue.len().saturating_sub(1))).and_then(|q| q.group.clone())),
    }
}

/// The rows of `queue` autoplay goes through from a song in `group`.
fn scope(queue: &[Queued], group: &Option<String>, rules: Rules) -> Vec<usize> {
    (0..queue.len())
        .filter(|&i| rules.autoplay == Autoplay::All || queue[i].group == *group)
        .filter(|&i| !(rules.skip_collapsed && queue[i].collapsed))
        .filter(|&i| !rules.only_starred || queue[i].plays.starred)
        .filter(|&i| !(rules.shuffle && queue[i].plays.skip_shuffle))
        .collect()
}

fn position(queue: &[Queued], i: usize) -> Position {
    Position { path: queue[i].path.clone(), group: queue[i].group.clone(), at: i }
}

/// The song after `playing`'s in `queue` (see [`SongList::next`]): the
/// next row down that hasn't played this pass (any of them, shuffling,
/// songs that play together as one), going round to the top only when
/// looping. `None` with autoplay off.
fn next_in(queue: &[Queued], playing: &Playing, rules: Rules, random: u64) -> Option<(Position, bool)> {
    if rules.autoplay == Autoplay::Off || queue.is_empty() {
        return None;
    }
    let (here, group) = locate(queue, &playing.at);
    let rows = scope(queue, &group, rules);
    let played: HashSet<&Path> = playing.history.iter().map(PathBuf::as_path).collect();
    let fresh = |i: &&usize| !played.contains(queue[**i].path.as_path());
    let members: Vec<Member> = queue.iter().map(|q| q.plays.together.as_ref().map(|t| (t.set.as_str(), t.at))).collect();
    let next = if rules.shuffle {
        let unplayed: Vec<usize> = rows.iter().filter(fresh).copied().collect();
        here.and_then(|i| together::next_in_set(&members, &unplayed, i)).or_else(|| {
            let unplayed = together::firsts(&members, &unplayed);
            (!unplayed.is_empty()).then(|| unplayed[(random % unplayed.len() as u64) as usize])
        })
    } else {
        let start = here.map_or(playing.at.at, |i| i + 1);
        rows.iter().filter(fresh).find(|&&i| i >= start).copied()
    };
    if let Some(i) = next {
        return Some((position(queue, i), false));
    }
    if !rules.loop_all || rows.is_empty() {
        return None;
    }
    // A new pass: from the top, or shuffled, anything but the song that
    // just finished (unless it's the only one).
    let i = if rules.shuffle {
        let others: Vec<usize> = together::firsts(&members, &rows).into_iter().filter(|&i| queue[i].path != playing.at.path).collect();
        if others.is_empty() { rows[0] } else { others[(random % others.len() as u64) as usize] }
    } else {
        rows[0]
    };
    Some((position(queue, i), true))
}

/// The song "Previous" goes to from `playing`'s in `queue`: the one before
/// it this pass when shuffling, else the row above it (going round to the
/// bottom only when looping).
fn previous_in(queue: &[Queued], playing: &Playing, rules: Rules) -> Option<Position> {
    if rules.shuffle {
        let path = playing.history.iter().rev().nth(1)?;
        let at = queue.iter().position(|q| &q.path == path && q.group == playing.at.group).or_else(|| queue.iter().position(|q| &q.path == path));
        return Some(match at {
            Some(i) => position(queue, i),
            None => Position { path: path.clone(), group: playing.at.group.clone(), at: playing.at.at },
        });
    }
    let (here, group) = locate(queue, &playing.at);
    let rows = scope(queue, &group, rules);
    let here = here.unwrap_or(playing.at.at);
    let above = rows.iter().rev().find(|&&i| i < here).copied();
    let i = above.or_else(|| rules.loop_all.then(|| rows.last().copied()).flatten())?;
    Some(position(queue, i))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws `list` for one frame of `songs`, grouped by artist, counting
    /// the songs described into `described`.
    fn frame(ctx: &egui::Context, list: &mut SongList, songs: &[PathBuf], revision: u64, described: &mut usize) {
        let mut view = View { group: "artist".into(), ..View::default() };
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let mut describe = |path: &Path, keys: &[&str]| {
                *described += 1;
                let n: usize = path.to_str().unwrap().parse().unwrap();
                let values = keys.iter().map(|&k| (k == "artist").then(|| Value { text: format!("Artist {}", n % 3), ..Value::default() })).collect();
                Described { name: format!("Song {n}"), file_name: String::new(), values, tags: Vec::new(), plays: Plays::default() }
            };
            list.ui(ui, &mut |query| (songs.to_vec(), query.to_string()), revision, &mut view, "", false, &mut |_| {}, &mut describe, &mut |_, _| {}, &mut |_, _| {});
        });
        output.textures_delta.clear();
    }

    #[test]
    fn rows_are_made_again_only_when_something_changes() {
        let ctx = egui::Context::default();
        let songs: Vec<PathBuf> = (0..30).map(|n| PathBuf::from(n.to_string())).collect();
        let mut list = SongList::default();
        let mut described = 0;
        frame(&ctx, &mut list, &songs, 1, &mut described);
        assert_eq!(described, 30);
        for _ in 0..5 {
            frame(&ctx, &mut list, &songs, 1, &mut described);
        }
        assert_eq!(described, 30, "nothing changed, so nothing was described again");
        frame(&ctx, &mut list, &songs, 2, &mut described);
        assert_eq!(described, 60, "a new revision makes the rows again");
    }

    #[test]
    fn groups_sort_and_collapse() {
        let ctx = egui::Context::default();
        let songs: Vec<PathBuf> = (0..6).map(|n| PathBuf::from(n.to_string())).collect();
        let mut list = SongList::default();
        let mut described = 0;
        frame(&ctx, &mut list, &songs, 1, &mut described);
        let rows = |list: &SongList| -> Vec<String> {
            list.built
                .as_ref()
                .unwrap()
                .items
                .iter()
                .map(|item| match item {
                    Item::Group { label, count, .. } => format!("{label} ({count})"),
                    Item::Song(entry) => entry.song.name.clone(),
                })
                .collect()
        };
        assert_eq!(rows(&list), ["Artist 0 (2)", "Song 0", "Song 3", "Artist 1 (2)", "Song 1", "Song 4", "Artist 2 (2)", "Song 2", "Song 5"]);
        list.collapsed.insert("artist1".into());
        list.built = None;
        frame(&ctx, &mut list, &songs, 1, &mut described);
        assert_eq!(rows(&list), ["Artist 0 (2)", "Song 0", "Song 3", "Artist 1 (2)", "Artist 2 (2)", "Song 2", "Song 5"]);
    }

    /// The headers and songs of `items`, as text.
    fn shape(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                Item::Group { label, count, .. } => format!("{label} ({count})"),
                Item::Song(entry) => entry.song.name.clone(),
            })
            .collect()
    }

    /// `songs` (name, album, tags) as `list` lists them, grouped by `group`.
    fn grouped(list: &SongList, group: &str, songs: &[(&str, Option<&str>, &[&str])]) -> Vec<String> {
        let paths: Vec<PathBuf> = songs.iter().map(|(name, ..)| PathBuf::from(name)).collect();
        let mut describe = |path: &Path, _: &[&str]| {
            let (name, album, tags) = songs.iter().find(|(name, ..)| Path::new(name) == path).unwrap();
            let values = vec![album.map(|a| Value { text: a.to_string(), ..Value::default() })];
            let tags = tags.iter().map(|t| (t.to_string(), [0; 3])).collect();
            Described { name: name.to_string(), file_name: name.to_string(), values, tags, plays: Plays::default() }
        };
        let view = View { group: group.into(), ..View::default() };
        let group_at = (group == "album").then_some(0);
        shape(&list.sorted(&paths, &view, &["album"], None, group_at, "", false, &mut describe).0)
    }

    #[test]
    fn songs_with_several_tags_are_in_each_group() {
        let songs: &[(&str, Option<&str>, &[&str])] = &[("c", None, &[]), ("b", None, &["Rock", "chill", "ROCK"]), ("a", None, &["rock"])];
        let rows = grouped(&SongList::default(), TAG, songs);
        assert_eq!(rows, ["chill (1)", "b", "Rock (2)", "a", "b", "Untagged (1)", "c"], "under the best spelling");
    }

    #[test]
    fn closed_tag_groups_keep_their_counts() {
        let mut list = SongList::default();
        list.collapsed.insert("rock".into());
        list.collapsed.insert(String::new());
        let songs: &[(&str, Option<&str>, &[&str])] = &[("c", None, &[]), ("b", None, &["rock", "chill"]), ("a", None, &["rock"])];
        assert_eq!(grouped(&list, TAG, songs), ["chill (1)", "b", "rock (2)", "Untagged (1)"]);
    }

    #[test]
    fn songs_without_a_value_go_last_apart_from_one_named_unknown() {
        let songs: &[(&str, Option<&str>, &[&str])] = &[("a", None, &[]), ("b", Some("Unknown"), &[]), ("c", Some("Abc"), &[])];
        assert_eq!(grouped(&SongList::default(), "album", songs), ["Abc (1)", "c", "Unknown (1)", "b", "Unknown (1)", "a"]);
    }

    #[test]
    fn songs_are_in_each_of_their_genres() {
        let songs: &[(&str, Option<&str>, &[&str])] = &[("a", Some("Avant-Garde, Rock"), &[]), ("b", Some("rock; Blues"), &[])];
        let paths: Vec<PathBuf> = songs.iter().map(|(name, ..)| PathBuf::from(name)).collect();
        let mut describe = |path: &Path, _: &[&str]| {
            let (name, genre, _) = songs.iter().find(|(name, ..)| Path::new(name) == path).unwrap();
            let values = vec![genre.map(|a| Value { text: a.to_string(), ..Value::default() })];
            Described { name: name.to_string(), file_name: name.to_string(), values, tags: Vec::new(), plays: Plays::default() }
        };
        let view = View { group: "genre".into(), ..View::default() };
        let rows = shape(&SongList::default().sorted(&paths, &view, &["genre"], None, Some(0), "", false, &mut describe).0);
        assert_eq!(rows, ["Avant-Garde (1)", "a", "Blues (1)", "b", "Rock (2)", "a", "b"]);
    }

    #[test]
    fn spellings_of_a_name_are_one_group_under_the_best() {
        let songs: &[(&str, Option<&str>, &[&str])] = &[
            ("a", Some("JUSTCORE EXTRAVAGANZA"), &[]),
            ("b", Some("<OST>"), &[]),
            ("c", Some("Justcore Extravaganza"), &[]),
            ("d", Some("Modal Soul"), &[]),
            ("e", Some("OST"), &[]),
            ("f", Some("justcore  extravaganza"), &[]),
        ];
        let rows = grouped(&SongList::default(), "album", songs);
        assert_eq!(rows, ["Justcore Extravaganza (3)", "a", "c", "f", "Modal Soul (1)", "d", "<OST> (2)", "b", "e"]);
    }

    /// A play order of (song, group, closed).
    fn queue(rows: &[(&str, Option<&str>, bool)]) -> Vec<Queued> {
        rows.iter().map(|(p, g, c)| Queued { path: PathBuf::from(p), group: g.map(str::to_string), collapsed: *c, plays: Plays::default() }).collect()
    }

    /// Playing `path` (in `group`, at row `at`), with `history` played before it this pass.
    fn playing(path: &str, group: Option<&str>, at: usize, history: &[&str]) -> Playing {
        let mut playing = Playing::new(Position { path: PathBuf::from(path), group: group.map(str::to_string), at });
        playing.history = history.iter().chain([&path]).map(PathBuf::from).collect();
        playing
    }

    fn rules(autoplay: Autoplay, loop_all: bool) -> Rules {
        Rules { autoplay, loop_all, shuffle: false, skip_collapsed: false, only_starred: false }
    }

    fn next_path(queue: &[Queued], playing: &Playing, rules: Rules) -> Option<(String, bool)> {
        next_in(queue, playing, rules, 0).map(|(at, new_pass)| (at.path.to_string_lossy().into_owned(), new_pass))
    }

    #[test]
    fn autoplay_stays_in_the_group_or_goes_on_through_the_list() {
        let q = queue(&[("a", Some("x"), false), ("b", Some("x"), false), ("c", Some("y"), false)]);
        let at_b = playing("b", Some("x"), 1, &["a"]);
        assert_eq!(next_path(&q, &playing("a", Some("x"), 0, &[]), rules(Autoplay::Group, false)), Some(("b".into(), false)));
        assert_eq!(next_path(&q, &at_b, rules(Autoplay::Group, false)), None, "the end of the group");
        assert_eq!(next_path(&q, &at_b, rules(Autoplay::Group, true)), Some(("a".into(), true)), "round the group again");
        assert_eq!(next_path(&q, &at_b, rules(Autoplay::All, false)), Some(("c".into(), false)), "into the next group");
        assert_eq!(next_path(&q, &playing("c", Some("y"), 2, &["a", "b"]), rules(Autoplay::All, true)), Some(("a".into(), true)));
        assert_eq!(next_path(&q, &at_b, rules(Autoplay::Off, true)), None);
    }

    #[test]
    fn autoplay_plays_a_song_in_several_groups_once_a_pass() {
        let q = queue(&[("a", Some("t1"), false), ("b", Some("t1"), false), ("a", Some("t2"), false), ("c", Some("t2"), false)]);
        assert_eq!(next_path(&q, &playing("b", Some("t1"), 1, &["a"]), rules(Autoplay::All, false)), Some(("c".into(), false)));
        // (Played from its second group: on from there.)
        assert_eq!(next_path(&q, &playing("a", Some("t2"), 2, &[]), rules(Autoplay::All, false)), Some(("c".into(), false)));
    }

    #[test]
    fn autoplay_follows_the_list_as_it_is_now() {
        // "b" was playing at row 1 and has left the list: on from where it was.
        let q = queue(&[("a", None, false), ("c", None, false), ("d", None, false)]);
        assert_eq!(next_path(&q, &playing("b", None, 1, &["a"]), rules(Autoplay::All, false)), Some(("c".into(), false)));
        // Re-sorted: on from where "a" is now.
        let q = queue(&[("c", None, false), ("a", None, false), ("d", None, false)]);
        assert_eq!(next_path(&q, &playing("a", None, 0, &[]), rules(Autoplay::All, false)), Some(("d".into(), false)));
    }

    #[test]
    fn autoplay_can_skip_closed_groups() {
        let q = queue(&[("a", Some("x"), false), ("b", Some("y"), true), ("c", Some("z"), false)]);
        let at_a = playing("a", Some("x"), 0, &[]);
        assert_eq!(next_path(&q, &at_a, rules(Autoplay::All, false)), Some(("b".into(), false)));
        let skip = Rules { skip_collapsed: true, ..rules(Autoplay::All, false) };
        assert_eq!(next_path(&q, &at_a, skip), Some(("c".into(), false)));
    }

    #[test]
    fn autoplay_can_play_only_starred_songs() {
        let mut q = queue(&[("a", None, false), ("b", None, false), ("c", None, false), ("d", None, false)]);
        q[2].plays.starred = true;
        let only = Rules { only_starred: true, ..rules(Autoplay::All, true) };
        // Picked by hand, "a" played; after it, only the starred ones.
        assert_eq!(next_path(&q, &playing("a", None, 0, &[]), only), Some(("c".into(), false)));
        assert_eq!(next_path(&q, &playing("c", None, 2, &["a"]), only), Some(("c".into(), true)), "round again");
        assert_eq!(next_path(&q, &playing("a", None, 0, &[]), Rules { shuffle: true, ..only }), Some(("c".into(), false)));
    }

    #[test]
    fn an_album_plays_through_whatever_autoplay_says() {
        let tracks = ["a", "b", "c"].map(|p| (PathBuf::from(p), Plays { starred: p != "b", ..Plays::default() }));
        let playing = Playing::tracks(tracks.clone(), 0).expect("there are tracks");
        let list = SongList::default();
        let next = list.next(&playing, rules(Autoplay::Off, false), 0).map(|(at, _)| at.path);
        assert_eq!(next, Some(PathBuf::from("b")), "past autoplay being off, and not in the list");
        let starred = Rules { only_starred: true, ..rules(Autoplay::Off, false) };
        assert_eq!(list.next(&playing, starred, 0).map(|(at, _)| at.path), Some(PathBuf::from("c")));
        let last = Playing::tracks(tracks, 2).expect("there are tracks");
        assert!(list.next(&last, rules(Autoplay::Off, false), 0).is_none(), "it ends with the album");
        assert!(Playing::tracks([], 0).is_none());
        // Shuffling passes over a song that says to.
        let tracks = ["a", "b", "c"].map(|p| (PathBuf::from(p), Plays { starred: true, skip_shuffle: p == "b", ..Plays::default() }));
        let playing = Playing::tracks(tracks, 0).expect("there are tracks");
        let shuffled = Rules { shuffle: true, ..rules(Autoplay::Off, false) };
        for random in 0..4 {
            assert_eq!(list.next(&playing, shuffled, random).map(|(at, _)| at.path), Some(PathBuf::from("c")));
        }
    }

    #[test]
    fn autoplay_previous_is_the_row_above() {
        let q = queue(&[("a", Some("x"), false), ("b", Some("x"), false), ("c", Some("y"), false)]);
        let path = |at: Option<Position>| at.map(|at| at.path.to_string_lossy().into_owned());
        assert_eq!(path(previous_in(&q, &playing("b", Some("x"), 1, &["a"]), rules(Autoplay::Group, false))), Some("a".into()));
        assert_eq!(path(previous_in(&q, &playing("a", Some("x"), 0, &[]), rules(Autoplay::Group, false))), None);
        assert_eq!(path(previous_in(&q, &playing("a", Some("x"), 0, &[]), rules(Autoplay::Group, true))), Some("b".into()), "round the group");
        assert_eq!(path(previous_in(&q, &playing("c", Some("y"), 2, &[]), rules(Autoplay::All, false))), Some("b".into()));
    }

    #[test]
    fn autoplay_shuffles_each_song_once_a_pass() {
        let q = queue(&[("a", None, false), ("b", None, false), ("c", None, false)]);
        let shuffle = Rules { shuffle: true, ..rules(Autoplay::All, true) };
        for random in 0..6 {
            let (at, new_pass) = next_in(&q, &playing("b", None, 1, &["a"]), shuffle, random).unwrap();
            assert_eq!((at.path, new_pass), (PathBuf::from("c"), false));
            let (at, new_pass) = next_in(&q, &playing("c", None, 2, &["a", "b"]), shuffle, random).unwrap();
            assert!(new_pass && at.path != Path::new("c"), "a new pass, not the same song twice in a row");
        }
    }

    #[test]
    fn autoplay_shuffles_songs_together_as_one() {
        let mut q = queue(&[("a", None, false), ("x1", None, false), ("x0", None, false)]);
        q[1].plays.together = Some(InSet { set: "x".into(), at: 1, of: 2 });
        q[2].plays.together = Some(InSet { set: "x".into(), at: 0, of: 2 });
        let shuffle = Rules { shuffle: true, ..rules(Autoplay::All, false) };
        for random in 0..6 {
            assert_eq!(next_in(&q, &playing("a", None, 0, &["a"]), shuffle, random).map(|(at, _)| at.path), Some(PathBuf::from("x0")), "from its first");
            assert_eq!(next_in(&q, &playing("x0", None, 2, &["x0"]), shuffle, random).map(|(at, _)| at.path), Some(PathBuf::from("x1")), "then in order");
        }
    }

    #[test]
    fn the_playing_row_is_in_the_group_it_plays_in() {
        let songs: &[(&str, Option<&str>, &[&str])] = &[("a", None, &["rock", "chill"]), ("b", None, &["rock"])];
        let paths: Vec<PathBuf> = songs.iter().map(|(name, ..)| PathBuf::from(name)).collect();
        let mut describe = |path: &Path, _: &[&str]| {
            let (name, _, tags) = songs.iter().find(|(name, ..)| Path::new(name) == path).unwrap();
            Described { name: name.to_string(), file_name: name.to_string(), values: vec![None], tags: tags.iter().map(|t| (t.to_string(), [0; 3])).collect(), plays: Plays::default() }
        };
        let view = View { group: TAG.into(), ..View::default() };
        let (items, _) = SongList::default().sorted(&paths, &view, &["album"], None, None, "", false, &mut describe);
        assert_eq!(shape(&items), ["chill (1)", "a", "rock (2)", "a", "b"]);
        assert_eq!(active_row(&items, Path::new("a"), Some("rock")), Some(3));
        assert_eq!(active_row(&items, Path::new("a"), None), Some(1), "its first, with no group known");
        assert_eq!(active_row(&items, Path::new("a"), Some("jazz")), Some(1));
        assert_eq!(active_row(&items, Path::new("c"), None), None);
    }

    #[test]
    fn repeated_songs_are_listed_once() {
        let paths = ["a", "b", "a"].map(PathBuf::from);
        assert_eq!(unique(paths), ["a", "b"].map(PathBuf::from));
    }
}
