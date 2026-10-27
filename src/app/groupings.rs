//! Groupings: the user's playlists, then the playlists each ruleset makes
//! (`crate::smart`: By Album, By Artist and the user's own), shown as a
//! grid of tiles or a list. On the desktop it's a tab with Playlists and
//! the rulesets along its top; on a phone each is a page of the menu. A
//! group opens on a page of its own (a pushed page, on a phone): its art,
//! title, play, shuffle, star them all, "find in" and its songs. A
//! playlist opens as itself, to play and edit (`playlist_panel`).
//!
//! A group's tile is its album's art when the ruleset splits by album and
//! a song of it with a picture is here (`album_art`); else, and while the
//! art's read, its initials on a color of its own.
//!
//! Each ruleset keeps its own state for the session (search, sort, grid or
//! list, the group open, the song picked, and its scroll, which egui keeps
//! by each scroll area's ID): nothing of it is saved or synced. Only the
//! tiles and rows in sight are drawn.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;

use super::{icon_button, is_midi_path, naming, toast, App, PrefTab};
use crate::album_art::{AlbumArt, Source as AlbumArtSource};
use crate::fields::{self, Facts, Field};
use crate::select_list;
use crate::smart::{Group, Ruleset, SplitBy};
use crate::song_info::{format_length, thousands};
use crate::song_list::DraggedFiles;
use crate::song_store::STARRED;

/// The built-in ruleset of artists, where an album's artist links to.
const ARTISTS: &str = "artists";
/// The user's playlists, the first of the Groupings (not a ruleset).
pub(super) const PLAYLISTS: &str = "playlists";
/// The songs that play together (`SongStore::together`), each set a group
/// in its order, after the playlists while there are any (not a ruleset).
pub(super) const TOGETHER: &str = "together";
/// The ruleset shown first.
const FIRST: &str = PLAYLISTS;
/// The built-in ruleset of albums, where the wishlist's albums show.
const ALBUMS: &str = "albums";
/// What a wished album's key starts with, among By Album's groups (it
/// stands in for an album none of is here: `App::with_wished`).
const WISHED: &str = "wish:";

/// The wish a group stands in for (its ID), if it does.
fn wished(group: &Group) -> Option<&str> {
    group.key.strip_prefix(WISHED)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Sort {
    #[default]
    Title,
    MostSongs,
    Added,
}

impl Sort {
    const ALL: [Sort; 3] = [Self::Title, Self::MostSongs, Self::Added];

    fn title(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::MostSongs => "Most songs",
            Self::Added => "Recently added",
        }
    }
}

/// One ruleset's view, for the session.
#[derive(Default)]
struct RulesetView {
    search: String,
    sort: Sort,
    /// Grid (`false`) or list, once picked: till then, a grid for albums.
    list: Option<bool>,
    /// The group open (its key), on its page.
    open: Option<String>,
    /// "Find in" on the group's page.
    find: String,
    picked: Option<PathBuf>,
    /// The groups shown, in order, and what that was worked out from.
    shown: Option<(u64, Vec<usize>)>,
}

/// A song on a group's page.
struct Track {
    path: PathBuf,
    name: String,
    artists: Vec<String>,
    album: Option<String>,
    disc: Option<u32>,
    number: Option<u32>,
    length: Option<f64>,
    starred: bool,
    /// Its file's here (it can play).
    here: bool,
}

/// The Groupings' state, for the session.
#[derive(Default)]
pub(super) struct Groupings {
    /// The ruleset shown (its ID); none yet: By Album.
    shown: Option<String>,
    views: HashMap<String, RulesetView>,
    pub(super) art: AlbumArt,
    /// The open group's songs, and what they were made from (the songs'
    /// revision, the ruleset and the group).
    page: Option<(u64, String, String, Arc<Vec<Track>>)>,
    /// The playlists as groups, and the fingerprint of the playlists
    /// they were made from.
    playlists: Option<(u64, Arc<Vec<Group>>)>,
    /// By Album's groups with the wished albums among them, and the
    /// fingerprint of what they were made from.
    with_wished: Option<(u64, Arc<Vec<Group>>)>,
}

impl Groupings {
    fn shown(&self) -> &str {
        self.shown.as_deref().unwrap_or(FIRST)
    }

    /// The ruleset a phone's page shows, by ID.
    pub(super) fn show(&mut self, id: &str) {
        self.shown = Some(id.to_string());
    }

    pub(super) fn shown_id(&self) -> String {
        self.shown().to_string()
    }


    fn view(&mut self) -> &mut RulesetView {
        let id = self.shown().to_string();
        self.views.entry(id).or_default()
    }
}

/// What the views asked for this frame, done once they're drawn.
enum Act {
    /// Play these songs from the one at the index.
    Play(Vec<PathBuf>, usize),
    /// Play the playlist with this ID.
    PlayPlaylist(String),
    Shuffle(Vec<PathBuf>),
    /// Queue these songs: next, or at the end.
    Queue(Vec<PathBuf>, bool),
    Star(Vec<PathBuf>, bool),
    Info(PathBuf),
    /// Open group `key` of ruleset `id` (a playlist, by its ID, for
    /// `PLAYLISTS`).
    Open(String, String),
    /// Make the set of songs that play together with this ID these, in
    /// order.
    Together(String, Vec<String>),
    Wish(super::wishlist::AlbumAsk),
    Unwish(String),
    GoToWishlist,
}

/// "3 songs": how many of `what` (singular) there are.
fn count(n: usize, what: &str) -> String {
    format!("{} {what}{}", thousands(n), if n == 1 { "" } else { "s" })
}

/// The songs that play together, shown like a ruleset's playlists.
fn together_ruleset() -> Ruleset {
    Ruleset { name: "Play together".into(), rules: Default::default() }
}

/// What a ruleset's playlists are of, one of them.
fn what(ruleset: &Ruleset) -> &'static str {
    ruleset.rules.split.as_ref().map_or("playlist", |s| match s.by {
        SplitBy::Album => "album",
        SplitBy::Artist | SplitBy::AlbumArtist => "artist",
        SplitBy::Composer => "composer",
        SplitBy::Genre => "genre",
        SplitBy::Year => "year",
        SplitBy::Tag => "tag",
    })
}

/// Whether a ruleset's playlists are albums (with their art).
fn albums(ruleset: &Ruleset) -> bool {
    ruleset.rules.split.as_ref().is_some_and(|s| s.by == SplitBy::Album)
}

/// How tall a row is.
fn row_height(ui: &egui::Ui) -> f32 {
    if select_list::touch(ui.ctx()) { 44.0 } else { ui.spacing().interact_size.y }
}

/// A smart playlist's mark on its tile at `rect`: its icon, in the
/// theme's accent, on a dimmed notch in the bottom right corner (its inner
/// corner rounded).
fn smart_badge(ui: &egui::Ui, rect: egui::Rect) {
    let size = (rect.height() * 0.3).clamp(18.0, 44.0);
    let notch = egui::Rect::from_min_max(rect.max - egui::vec2(size, size), rect.max);
    let corners = egui::CornerRadius { nw: (size * 0.35) as u8, ne: 0, sw: 0, se: 4 };
    let painter = ui.painter();
    painter.rect_filled(notch, corners, egui::Color32::from_black_alpha(150));
    let font = egui::FontId::new(size * 0.62, egui::FontFamily::Name(super::fonts::PHOSPHOR_FILL.into()));
    let color = ui.visuals().selection.stroke.color.gamma_multiply(0.85);
    painter.text(notch.center(), egui::Align2::CENTER_CENTER, super::icon(super::smart::SMART_ICON), font, color);
}

/// A wished album's mark on its tile at `rect`: dimmed, with a gift in the
/// corner, so it reads as one that isn't here.
fn wished_veil(ui: &egui::Ui, rect: egui::Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, ui.visuals().panel_fill.gamma_multiply(0.6));
    let size = (rect.height() * 0.3).clamp(14.0, 40.0);
    let font = egui::FontId::new(size * 0.7, egui::FontFamily::Name(super::fonts::PHOSPHOR_FILL.into()));
    let at = rect.right_top() + egui::vec2(-size * 0.6, size * 0.6);
    painter.text(at, egui::Align2::CENTER_CENTER, super::icon("gift"), font, ui.visuals().selection.stroke.color);
}

/// Draw only the rows of `heights` in sight, in a scroll area filling
/// `ui`, each by `draw` into the rect it's given.
fn virtual_rows(ui: &mut egui::Ui, id: impl egui::AsIdSalt, heights: &[f32], mut draw: impl FnMut(&mut egui::Ui, egui::Rect, usize)) {
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        let width = ui.available_width();
        let origin = ui.max_rect().min;
        ui.set_min_height(heights.iter().sum());
        let mut top = 0.0;
        for (i, &height) in heights.iter().enumerate() {
            if top + height >= viewport.min.y && top <= viewport.max.y {
                draw(ui, egui::Rect::from_min_size(origin + egui::vec2(0.0, top), egui::vec2(width, height)), i);
            }
            top += height;
        }
    });
}

impl App {
    /// The Groupings tab: the rulesets along the top, then the one picked.
    pub(super) fn groupings_ui(&mut self, ui: &mut egui::Ui) {
        let rulesets = self.all_rulesets();
        let mut pick = None;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(icon_button("gear")).on_hover_text("Rulesets: make your own, or see their rules (Preferences > Songs)").clicked() {
                    self.preferences_tab = PrefTab::Songs;
                    self.preferences_open = true;
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    egui::ScrollArea::horizontal().id_salt("ruleset_chips").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let chip = egui::Button::selectable(self.groupings.shown() == PLAYLISTS, "Playlists").corner_radius(12.0);
                            if ui.add(chip).clicked() {
                                pick = Some(PLAYLISTS.to_string());
                            }
                            if !self.songs.together().is_empty() {
                                let chip = egui::Button::selectable(self.groupings.shown() == TOGETHER, "Play together").corner_radius(12.0);
                                if ui.add(chip).clicked() {
                                    pick = Some(TOGETHER.to_string());
                                }
                            }
                            for (id, ruleset) in &rulesets {
                                let chip = egui::Button::selectable(self.groupings.shown() == id, &ruleset.name).corner_radius(12.0);
                                if ui.add(chip).clicked() {
                                    pick = Some(id.clone());
                                }
                            }
                        });
                    });
                });
            });
        });
        if let Some(id) = pick {
            self.groupings.shown = Some(id);
        }
        self.grouping_ui(ui);
    }

    /// The ruleset shown: its groups, or the one open.
    pub(super) fn grouping_ui(&mut self, ui: &mut egui::Ui) {
        self.groupings.art.frame(ui.ctx(), self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup));
        self.groupings.art.letters = self.config.tile_letters.unwrap_or(crate::album_art::DEFAULT_LETTERS);
        let act = if self.groupings.shown() == PLAYLISTS { self.playlists_grouping_ui(ui) } else { self.ruleset_ui(ui) };
        match act {
            Some(Act::Play(tracks, start)) => self.play_tracks(tracks, start),
            Some(Act::PlayPlaylist(key)) => {
                if let Some(list) = self.playlists.find(&key) {
                    self.play_playlist(list);
                }
            }
            Some(Act::Shuffle(tracks)) if !tracks.is_empty() => {
                self.set_shuffle(true);
                let start = (self.rng.next() % tracks.len() as u64) as usize;
                self.play_tracks(tracks, start);
            }
            Some(Act::Queue(paths, next)) => self.queue_songs(ui.ctx(), paths, next),
            Some(Act::Star(paths, on)) => self.tag_songs(super::tags::TagAction::Set(STARRED.into(), on), &paths),
            Some(Act::Info(path)) => self.details_for = Some(vec![path]),
            Some(Act::Open(id, key)) if id == PLAYLISTS => {
                if let Some(list) = self.playlists.find(&key) {
                    self.open_playlist(Some(list));
                }
            }
            Some(Act::Open(id, key)) => {
                self.groupings.shown = Some(id);
                let view = self.groupings.view();
                view.open = Some(key);
                view.find.clear();
                view.picked = None;
            }
            Some(Act::Together(id, keys)) => {
                self.songs.set_together(&id, keys);
                self.songs_changed();
            }
            Some(Act::Wish(ask)) => self.wish_for_album(ui.ctx(), ask),
            Some(Act::Unwish(id)) => self.unwish(&id),
            Some(Act::GoToWishlist) => self.go_to_wishlist(),
            Some(Act::Shuffle(_)) | None => {}
        }
    }

    /// The ruleset shown: its groups, or the one open.
    fn ruleset_ui(&mut self, ui: &mut egui::Ui) -> Option<Act> {
        let (id, ruleset, what, groups) = if self.groupings.shown() == TOGETHER && !self.songs.together().is_empty() {
            (TOGETHER.to_string(), together_ruleset(), "set", self.together_groups())
        } else {
            let rulesets = self.all_rulesets();
            let (id, ruleset) = rulesets.iter().find(|(id, _)| id == self.groupings.shown()).or(rulesets.first()).cloned()?;
            let mut groups = self.groups_of(&id, &ruleset.rules);
            if id == ALBUMS && !self.config.hide_wished {
                groups = self.with_wished(groups);
            }
            (id, ruleset.clone(), what(&ruleset), groups)
        };
        self.groupings.shown = Some(id.clone());
        let open = self.groupings.view().open.clone().and_then(|key| groups.iter().find(|g| g.key == key).cloned());
        match open {
            Some(group) => self.group_page_ui(ui, &id, &ruleset, &group),
            None => self.groups_ui(ui, &id, what, albums(&ruleset), self.songs.revision(), &groups),
        }
    }

    /// By Album's `groups` with the albums on the wishlist none of are
    /// here among them, by title, made again when either changes.
    fn with_wished(&mut self, groups: Arc<Vec<Group>>) -> Arc<Vec<Group>> {
        let wanted = self.wanted_albums();
        if wanted.is_empty() {
            return groups;
        }
        let fingerprint = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (Arc::as_ptr(&groups) as usize, groups.len(), self.config.sort_with_articles).hash(&mut hasher);
            for (id, wish) in &wanted {
                (id, &wish.title, &wish.artist).hash(&mut hasher);
            }
            hasher.finish()
        };
        if let Some((seen, made)) = &self.groupings.with_wished
            && *seen == fingerprint
        {
            return made.clone();
        }
        let mut all = (*groups).clone();
        all.extend(wanted.iter().map(|(id, wish)| Group {
            key: format!("{WISHED}{id}"),
            title: wish.title.clone(),
            sort: fields::sort_text(&wish.title, self.config.sort_with_articles),
            subtitle: wish.artist.clone(),
            artists: Vec::new(),
            songs: Vec::new(),
        }));
        all.sort_by(|a, b| a.sort.cmp(&b.sort));
        let all = Arc::new(all);
        self.groupings.with_wished = Some((fingerprint, all.clone()));
        all
    }

    /// The songs that play together as groups: each set titled by its
    /// first song, the rest of it beneath.
    fn together_groups(&mut self) -> Arc<Vec<Group>> {
        let naming = naming(&self.config, &self.songs);
        let cache = &mut self.song_info;
        let mut groups: Vec<Group> = self
            .songs
            .together()
            .iter()
            .map(|(id, keys)| {
                let names: Vec<String> = keys
                    .iter()
                    .map(|key| {
                        let path = self.songs.path(key);
                        let info = if self.songs.has(key) { cache.get(&path) } else { None };
                        naming.name(&path, info.as_deref())
                    })
                    .collect();
                Group {
                    key: id.clone(),
                    sort: fields::sort_text(&names[0], self.config.sort_with_articles),
                    title: names[0].clone(),
                    subtitle: Some(format!("then {}", names[1..].join(", "))),
                    artists: Vec::new(),
                    songs: keys.clone(),
                }
            })
            .collect();
        groups.sort_by(|a, b| a.sort.cmp(&b.sort));
        Arc::new(groups)
    }

    /// The playlists: as tiles or rows (dropping songs there makes a new
    /// one of them), or the one open.
    fn playlists_grouping_ui(&mut self, ui: &mut egui::Ui) -> Option<Act> {
        if let Some(list) = self.viewed_playlist.filter(|&l| l < self.playlists.lists.len()) {
            self.playlist_panel_ui(ui, list);
            return None;
        }
        self.viewed_playlist = None;
        let area = ui.max_rect();
        let (revision, groups) = self.playlist_groups();
        let act = self.groups_ui(ui, PLAYLISTS, "playlist", false, revision, &groups);
        if ui.rect_contains_pointer(area)
            && ui.input(|i| i.pointer.any_released())
            && let Some(files) = egui::DragAndDrop::take_payload::<DraggedFiles>(ui.ctx())
        {
            let name = self.unique_playlist_name("New playlist");
            self.create_playlist(&name, Vec::new());
            if let Some(list) = self.viewed_playlist {
                self.insert_songs(list, 0, files.paths.clone());
            }
        }
        act
    }

    /// The playlists as groups, by name, made again when they change.
    fn playlist_groups(&mut self) -> (u64, Arc<Vec<Group>>) {
        let fingerprint = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for stored in &self.playlists.lists {
                (&stored.id, &stored.playlist.name, stored.playlist.smart.is_some()).hash(&mut hasher);
                for entry in &stored.playlist.entries {
                    entry.path.hash(&mut hasher);
                }
            }
            hasher.finish()
        };
        if let Some((seen, groups)) = &self.groupings.playlists
            && *seen == fingerprint
        {
            return (fingerprint, groups.clone());
        }
        let dir = self.songs.dir();
        let mut groups: Vec<Group> = self
            .playlists
            .lists
            .iter()
            .map(|stored| Group {
                key: stored.id.clone(),
                title: stored.playlist.name.clone(),
                sort: fields::sort_text(&stored.playlist.name, self.config.sort_with_articles),
                subtitle: None,
                artists: Vec::new(),
                songs: stored.playlist.entries.iter().filter_map(|e| crate::song_store::key_in(dir, &e.path).map(str::to_string)).collect(),
            })
            .collect();
        groups.sort_by(|a, b| a.sort.cmp(&b.sort));
        let groups = Arc::new(groups);
        self.groupings.playlists = Some((fingerprint, groups.clone()));
        (fingerprint, groups)
    }

    /// Show playlist `list` on its page (`None`: back to them all).
    pub(super) fn open_playlist(&mut self, list: Option<usize>) {
        self.viewed_playlist = list;
        self.playlist_rename = None;
        self.confirm_delete_playlist = false;
        self.playlist_selection.clear();
    }

    /// Whether a group's (or a playlist's) page is open.
    pub(super) fn grouping_open(&self) -> bool {
        match self.groupings.shown() {
            PLAYLISTS => self.viewed_playlist.is_some(),
            id => self.groupings.views.get(id).is_some_and(|v| v.open.is_some()),
        }
    }

    /// Back from the open group's (or playlist's) page.
    pub(super) fn grouping_back(&mut self) {
        if self.groupings.shown() == PLAYLISTS {
            self.open_playlist(None);
        } else {
            self.groupings.view().open = None;
        }
    }

    /// What a phone's top bar names the Groupings page: the open group,
    /// with Back (`true`), else the ruleset.
    pub(super) fn grouping_title(&self) -> (String, bool) {
        let id = self.groupings.shown();
        if id == PLAYLISTS {
            return match self.viewed_playlist.and_then(|l| self.playlists.lists.get(l)) {
                Some(stored) => (stored.playlist.name.clone(), true),
                None => ("Playlists".into(), false),
            };
        }
        if id == TOGETHER {
            return ("Play together".into(), self.groupings.views.get(id).is_some_and(|v| v.open.is_some()));
        }
        let open = self.groupings.views.get(id).and_then(|v| v.open.as_ref());
        let group = open.and_then(|key| self.ruleset_groups.get(id)?.2.iter().find(|g| &g.key == key));
        match group {
            Some(group) => (group.title.clone(), true),
            None => (self.all_rulesets().into_iter().find(|(i, _)| i == id).map_or_else(|| "Groupings".into(), |(_, r)| r.name), false),
        }
    }

    /// The song files of `group` that are here, in its order: what
    /// playing it plays.
    fn playable(&self, group: &Group) -> Vec<PathBuf> {
        group.songs.iter().filter(|key| self.songs.has(key)).map(|key| self.songs.path(key)).collect()
    }

    /// Where `group`'s art comes from, when the ruleset splits by album:
    /// the first song of it here that isn't MIDI, and the first release
    /// one of its songs is on.
    fn art_from(&self, albums: bool, group: &Group) -> AlbumArtSource {
        if !albums {
            return AlbumArtSource::default();
        }
        if let Some(wish) = wished(group).and_then(|id| self.wishlist.list.wishes.get(id)) {
            return super::wishlist::source(wish);
        }
        let file = group.songs.iter().filter(|key| self.songs.has(key)).map(|key| self.songs.path(key)).find(|p| !is_midi_path(p));
        let release = group
            .songs
            .iter()
            .filter_map(|key| crate::lookup::release_of(self.songs.get_by_key(key)?))
            .map(|(release, group)| (release.to_string(), group.map(str::to_string)))
            .next();
        let chosen = group.songs.iter().find_map(|key| self.songs.get_by_key(key)?.online.as_ref()?.art.clone());
        AlbumArtSource { file, release, chosen }
    }

    /// Every group of `id` (each one `what`; `albums` have art), as tiles
    /// or rows, with Play all, search and sort (worked out again when
    /// `revision` changes), and + for a new one, for the playlists.
    fn groups_ui(&mut self, ui: &mut egui::Ui, id: &str, what: &str, albums: bool, revision: u64, groups: &[Group]) -> Option<Act> {
        let mut act = None;
        let mut play_all = false;
        let mut list = self.groupings.view().list.unwrap_or(!albums);
        ui.horizontal(|ui| {
            if id == PLAYLISTS {
                self.new_playlist_menu_ui(ui);
            }
            play_all = ui.add(icon_button("play")).on_hover_text("Play them all").clicked();
            ui.weak(count(groups.iter().filter(|g| wished(g).is_none()).count(), what));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if id == ALBUMS && !self.wishlist.list.wishes.is_empty() {
                    let shown = !self.config.hide_wished;
                    let hover = "Wished albums: show the albums on your wishlist that none of are here yet, dimmed";
                    if ui.add(icon_button("gift").selected(shown)).on_hover_text(hover).clicked() {
                        self.config.hide_wished = shown;
                        toast::show(ui.ctx(), if shown { "Wished albums: hidden" } else { "Wished albums: shown" });
                        if let Err(e) = self.config.save() {
                            log::warn!("saving the config: {e}");
                        }
                    }
                }
                let view = self.groupings.view();
                if ui.add(icon_button("list").selected(list)).on_hover_text("As a list").clicked() {
                    list = true;
                }
                if ui.add(icon_button("squares-four").selected(!list)).on_hover_text("As a grid").clicked() {
                    list = false;
                }
                let title = view.sort.title();
                let sorts = |ui: &mut egui::Ui| {
                    for sort in Sort::ALL {
                        ui.selectable_value(&mut view.sort, sort, sort.title());
                    }
                };
                // (On a phone, a button: the search gets the room.)
                if select_list::touch(ui.ctx()) {
                    egui::containers::menu::MenuButton::from_button(icon_button("sort-ascending")).config(crate::menu::config(ui.ctx())).ui(ui, sorts);
                } else {
                    egui::ComboBox::from_id_salt("grouping_sort").selected_text(title).show_ui(ui, sorts);
                }
                ui.add(egui::TextEdit::singleline(&mut view.search).hint_text(format!("search {what}s...")).desired_width(f32::INFINITY));
            });
        });
        let view = self.groupings.view();
        view.list = Some(list);
        if groups.is_empty() {
            ui.weak(if id == PLAYLISTS { "No playlists yet: make one with +, or drag songs here from Songs." } else { "Nothing here yet: these come from your songs' tags." });
            return None;
        }

        // The groups shown, in order: worked out again when the library,
        // the search or the sort change.
        let made_from = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (revision, groups.len(), view.search.trim(), view.sort as u8).hash(&mut hasher);
            hasher.finish()
        };
        if view.shown.as_ref().is_none_or(|(from, _)| *from != made_from) {
            let query = view.search.trim().to_lowercase();
            let words: Vec<&str> = query.split_whitespace().collect();
            let mut shown: Vec<usize> = (0..groups.len())
                .filter(|&i| {
                    let text = format!("{} {}", groups[i].title, groups[i].subtitle.as_deref().unwrap_or_default()).to_lowercase();
                    words.iter().all(|w| text.contains(w))
                })
                .collect();
            match view.sort {
                Sort::Title => {}
                Sort::MostSongs => shown.sort_by_key(|&i| std::cmp::Reverse(groups[i].songs.len())),
                Sort::Added => {
                    let added = |g: &Group| g.songs.iter().filter_map(|k| self.songs.get_by_key(k)).map(|s| s.imported).max().unwrap_or(0);
                    shown.sort_by_cached_key(|&i| std::cmp::Reverse(added(&groups[i])));
                }
            }
            self.groupings.view().shown = Some((made_from, shown));
        }
        let shown = self.groupings.view().shown.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        if play_all {
            let all: Vec<PathBuf> = shown.iter().flat_map(|&i| self.playable(&groups[i])).collect();
            act = Some(Act::Play(all, 0));
        }

        // Smart playlists have a badge on their tiles.
        let smart: HashSet<String> = if id == PLAYLISTS {
            self.playlists.lists.iter().filter(|l| l.playlist.smart.is_some()).map(|l| l.id.clone()).collect()
        } else {
            HashSet::new()
        };
        let tile_ui = |ui: &egui::Ui, art: &mut AlbumArt, rect: egui::Rect, from: AlbumArtSource, group: &Group| {
            art.tile(ui, rect, &from, &group.title);
            if smart.contains(&group.key) {
                smart_badge(ui, rect);
            }
            if wished(group).is_some() {
                wished_veil(ui, rect);
            }
        };
        // (A wished album's menu: what's on it is on the wishlist.)
        let wished_menu = |ui: &mut egui::Ui, id: &str| {
            if ui.button("Show in the wishlist").clicked() {
                return Some(Act::GoToWishlist);
            }
            ui.button("Take off the wishlist").clicked().then(|| Act::Unwish(id.to_string()))
        };

        // (Art's looked for only for the tiles in sight. Each row's drawn
        // in a child of its own, so what's put in it can't move the
        // cursor: the rows must take exactly the height `show_rows` was
        // told.)
        let mut opened = None;
        let mut queued = None;
        let mut menu_act = None;
        let online = self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup);
        if list {
            let height = 56.0;
            let rows = shown.len();
            let mut play = None;
            egui::ScrollArea::vertical().id_salt(("grouping_list", id)).auto_shrink([false, false]).show_rows(ui, height, rows, |ui, range| {
                for row in range {
                    let group = &groups[shown[row]];
                    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
                    let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    if response.hovered() {
                        ui.painter().rect_filled(rect, 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
                    } else if row % 2 == 1 {
                        ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
                    }
                    let tile = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 6.0), egui::vec2(44.0, 44.0));
                    let from = self.art_from(albums, group);
                    tile_ui(ui, &mut self.groupings.art, tile, from, group);
                    let button = egui::Rect::from_min_size(rect.right_center() - egui::vec2(36.0, 12.0), egui::vec2(28.0, 24.0));
                    if ui.put(button, icon_button("play")).on_hover_text("Play").clicked() {
                        play = Some(shown[row]);
                    }
                    let text = egui::Rect::from_min_max(egui::pos2(tile.right() + 10.0, rect.top() + 8.0), egui::pos2(button.left() - 8.0, rect.bottom() - 8.0));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(text).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
                        ui.style_mut().interaction.selectable_labels = false;
                        ui.add(egui::Label::new(egui::RichText::new(&group.title).strong()).truncate());
                        let mut detail = if wished(group).is_some() { "On your wishlist".to_string() } else { count(group.songs.len(), "song") };
                        if let Some(subtitle) = &group.subtitle {
                            detail = format!("{subtitle} \u{b7} {detail}");
                        }
                        ui.add(egui::Label::new(egui::RichText::new(detail).weak()).truncate());
                    });
                    if response.clicked() {
                        opened = Some(group.key.clone());
                    }
                    crate::menu::context(&response, |ui| {
                        if let Some(wish) = wished(group) {
                            menu_act = wished_menu(ui, wish);
                            return;
                        }
                        if let Some(next) = super::queue::menu(ui) {
                            queued = Some((shown[row], next));
                        }
                        if albums && let Some(wish) = super::wishlist::album_wish_menu(ui, &self.songs, &self.lookup.kept, &self.wishlist.list, online, &group.songs) {
                            menu_act = Some(Act::Wish(wish));
                        }
                    });
                }
            });
            if let Some(i) = play {
                act = Some(if id == PLAYLISTS { Act::PlayPlaylist(groups[i].key.clone()) } else { Act::Play(self.playable(&groups[i]), 0) });
            }
        } else {
            let gap = 12.0;
            let width = ui.available_width();
            let phone = select_list::touch(ui.ctx());
            let most = if phone { 220.0 } else { 150.0 };
            let columns = ((width + gap) / (most + gap)).ceil().max(2.0) as usize;
            let tile = (width - gap * (columns - 1) as f32) / columns as f32;
            let height = tile + 40.0 + gap;
            let rows = shown.len().div_ceil(columns);
            egui::ScrollArea::vertical().id_salt(("grouping_grid", id)).auto_shrink([false, false]).show_rows(ui, height, rows, |ui, range| {
                for row in range {
                    let (_, line) = ui.allocate_space(egui::vec2(width, height));
                    let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(line));
                    for (column, &i) in shown.iter().skip(row * columns).take(columns).enumerate() {
                        let group = &groups[i];
                        let min = line.min + egui::vec2(column as f32 * (tile + gap), 0.0);
                        let rect = egui::Rect::from_min_size(min, egui::vec2(tile, tile + 40.0));
                        let response = ui.interact(rect, ui.id().with(("group_tile", &group.key)), egui::Sense::click());
                        let cover = egui::Rect::from_min_size(rect.min, egui::vec2(tile, tile));
                        let from = self.art_from(albums, group);
                        tile_ui(ui, &mut self.groupings.art, cover, from, group);
                        if response.hovered() {
                            ui.painter().rect_stroke(cover, 4.0, ui.visuals().widgets.hovered.fg_stroke, egui::StrokeKind::Inside);
                        }
                        let text = egui::Rect::from_min_max(egui::pos2(rect.left(), cover.bottom() + 2.0), rect.max);
                        ui.scope_builder(egui::UiBuilder::new().max_rect(text).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.style_mut().interaction.selectable_labels = false;
                            ui.add(egui::Label::new(&group.title).truncate());
                            let detail = group.subtitle.clone().unwrap_or_else(|| count(group.songs.len(), "song"));
                            let detail = if wished(group).is_some() { format!("{detail} \u{b7} wished") } else { detail };
                            ui.add(egui::Label::new(egui::RichText::new(detail).weak()).truncate());
                        });
                        if response.clicked() {
                            opened = Some(group.key.clone());
                        }
                        crate::menu::context(&response, |ui| {
                            if let Some(wish) = wished(group) {
                                menu_act = wished_menu(ui, wish);
                                return;
                            }
                            if let Some(next) = super::queue::menu(ui) {
                                queued = Some((i, next));
                            }
                            if albums && let Some(wish) = super::wishlist::album_wish_menu(ui, &self.songs, &self.lookup.kept, &self.wishlist.list, online, &group.songs) {
                                menu_act = Some(Act::Wish(wish));
                            }
                        });
                    }
                }
            });
        }
        if let Some(key) = opened {
            act = Some(if key.starts_with(WISHED) { Act::GoToWishlist } else { Act::Open(id.to_string(), key) });
        }
        if menu_act.is_some() {
            act = menu_act;
        }
        if let Some((i, next)) = queued {
            act = Some(Act::Queue(self.playable(&groups[i]), next));
        }
        act
    }

    /// The songs of `group` as its page lists them, made again when the
    /// songs change.
    fn group_tracks(&mut self, id: &str, group: &Group) -> Arc<Vec<Track>> {
        let revision = self.songs_revision();
        if let Some((seen, ruleset, key, tracks)) = &self.groupings.page
            && *seen == revision
            && ruleset == id
            && *key == group.key
        {
            return tracks.clone();
        }
        let naming = naming(&self.config, &self.songs);
        let cache = &mut self.song_info;
        let tracks: Vec<Track> = group
            .songs
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let path = self.songs.path(key);
                let here = self.songs.has(key);
                let info = if here { cache.get(&path) } else { None };
                let facts = self.songs.get_by_key(key).map(Facts::of).unwrap_or_default();
                let text = |field| facts.text(field).map(|t| t.into_owned());
                Track {
                    name: naming.name(&path, info.as_deref()),
                    artists: facts.items(Field::Artist).into_iter().map(|a| a.into_owned()).collect(),
                    album: text(Field::Album),
                    disc: facts.number(Field::Disc).map(|n| n as u32),
                    // (A set's songs by their place in it.)
                    number: if id == TOGETHER { Some(i as u32 + 1) } else { facts.number(Field::Track).map(|n| n as u32) },
                    length: facts.number(Field::Length),
                    starred: facts.starred(),
                    here,
                    path,
                }
            })
            .collect();
        let tracks = Arc::new(tracks);
        self.groupings.page = Some((revision, id.to_string(), group.key.clone(), tracks.clone()));
        tracks
    }

    /// A group's page: its art, title, its artist (a link, for an album),
    /// count and length, play, shuffle and star them all, then its songs
    /// ("find in" narrows them), under their albums' or discs' names.
    fn group_page_ui(&mut self, ui: &mut egui::Ui, id: &str, ruleset: &Ruleset, group: &Group) -> Option<Act> {
        let tracks = self.group_tracks(id, group);
        let phone = self.mobile();
        let albums = albums(ruleset);
        let art_from = self.art_from(albums, group);
        let current = self.current_song.clone();
        let online = self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup);
        let mut wish = albums.then(|| super::wishlist::album_ask(&self.songs, &self.lookup.kept, &self.wishlist.list, online, &group.songs)).flatten();
        let links_to_artist = albums && id == "albums";
        let ignore = self.config.group_ignore.clone();
        let view = self.groupings.view();
        ui.horizontal(|ui| {
            // (A phone's top bar has Back.)
            if !phone && ui.add(icon_button("arrow-left")).on_hover_text(format!("Back to {}", ruleset.name)).clicked() {
                view.open = None;
            }
            ui.add(egui::TextEdit::singleline(&mut view.find).hint_text("find in here...").desired_width(f32::INFINITY));
        });
        if view.open.is_none() {
            return None;
        }

        enum Row {
            Header,
            Section(String),
            Track(usize),
        }
        let row = row_height(ui);
        let art = if phone { 112.0 } else { 160.0 };
        let mut rows = vec![Row::Header];
        let mut heights = vec![art + 16.0];
        let words: Vec<String> = view.find.to_lowercase().split_whitespace().map(str::to_string).collect();
        let discs = albums && tracks.iter().any(|t| t.disc.unwrap_or(1) != tracks[0].disc.unwrap_or(1));
        // Spellings of an album are one section, under the best of them.
        let album_key = |track: &Track| track.album.as_deref().map(|a| fields::group_key(a, &ignore));
        let mut spellings: HashMap<String, Vec<&str>> = HashMap::new();
        for track in tracks.iter() {
            if let (Some(key), Some(album)) = (album_key(track), &track.album) {
                spellings.entry(key).or_default().push(album);
            }
        }
        let best: HashMap<String, &str> = spellings.into_iter().filter_map(|(key, names)| Some((key, fields::best_spelling(names)?))).collect();
        let mut section: Option<String> = None;
        for (t, track) in tracks.iter().enumerate() {
            let name = track.name.to_lowercase();
            if !words.iter().all(|w| name.contains(w.as_str())) {
                continue;
            }
            // Albums under their discs, the rest under their albums (a set
            // of songs that play together as it is).
            let this = if id == TOGETHER {
                None
            } else if albums {
                discs.then(|| format!("Disc {}", track.disc.unwrap_or(1)))
            } else {
                Some(album_key(track).and_then(|k| best.get(&k).map(|b| b.to_string())).unwrap_or_else(|| "No album".into()))
            };
            if this.is_some() && this != section {
                section = this.clone();
                rows.push(Row::Section(this.unwrap_or_default()));
                heights.push(row + 6.0);
            }
            rows.push(Row::Track(t));
            heights.push(row);
        }

        let playable: Vec<PathBuf> = tracks.iter().filter(|t| t.here).map(|t| t.path.clone()).collect();
        let all_starred = tracks.iter().all(|t| t.starred);
        let length: f64 = tracks.iter().filter_map(|t| t.length).sum();
        let art_cache = &mut self.groupings.art;
        let view = self.groupings.views.entry(id.to_string()).or_default();
        let mut act = None;
        let mut choose_art = false;
        virtual_rows(ui, ("group_page", id, &group.key), &heights, |ui, rect, i| match &rows[i] {
            Row::Header => {
                let inner = rect.shrink(8.0);
                let cover = egui::Rect::from_min_size(inner.min, egui::vec2(art, art));
                art_cache.tile(ui, cover, &art_from, &group.title);
                let text = egui::Rect::from_min_max(egui::pos2(cover.right() + 12.0, inner.top()), inner.max);
                ui.scope_builder(egui::UiBuilder::new().max_rect(text), |ui| {
                    if !phone {
                        ui.add(egui::Label::new(egui::RichText::new(&group.title).heading()).truncate());
                    }
                    if let Some(subtitle) = &group.subtitle {
                        if links_to_artist && !group.artists.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for (i, artist) in group.artists.iter().enumerate() {
                                    if i > 0 {
                                        ui.label(" & ");
                                    }
                                    if ui.link(artist).on_hover_text("Show the artist").clicked() {
                                        act = Some(Act::Open(ARTISTS.into(), format!("{}\u{0}", fields::group_key(artist, &ignore))));
                                    }
                                }
                            });
                        } else {
                            ui.label(subtitle);
                        }
                    }
                    ui.weak(format!("{} \u{b7} {}", count(tracks.len(), "song"), format_length(length)));
                    ui.horizontal(|ui| {
                        if ui.add(icon_button("play")).on_hover_text("Play").clicked() {
                            act = Some(Act::Play(playable.clone(), 0));
                        }
                        if ui.add(icon_button("shuffle")).on_hover_text("Shuffle").clicked() {
                            act = Some(Act::Shuffle(playable.clone()));
                        }
                        if ui.add(icon_button("list-plus")).on_hover_text("Add them to the queue").clicked() {
                            act = Some(Act::Queue(playable.clone(), false));
                        }
                        let hover = if all_starred { "Unstar them all" } else { "Star them all" };
                        if ui.add(icon_button(select_list::STAR_ICON).selected(all_starred)).on_hover_text(hover).clicked() {
                            act = Some(Act::Star(tracks.iter().map(|t| t.path.clone()).collect(), !all_starred));
                        }
                        if albums && ui.add(icon_button("image")).on_hover_text("Choose art").clicked() {
                            choose_art = true;
                        }
                        if let Some(ask) = super::wishlist::album_wish_button(ui, wish.take()) {
                            act = Some(Act::Wish(ask));
                        }
                    });
                });
            }
            Row::Section(title) => {
                let font = egui::TextStyle::Button.resolve(ui.style());
                ui.painter().text(rect.left_bottom() + egui::vec2(4.0, -4.0), egui::Align2::LEFT_BOTTOM, title, font, ui.visuals().strong_text_color());
            }
            Row::Track(t) => {
                let track = &tracks[*t];
                // (A guest's name beside their track.)
                let named = |a: &str| fields::group_key(a, &ignore);
                let guests: Vec<&String> = track
                    .artists
                    .iter()
                    .filter(|a| named(a) != named(&group.title) && !group.artists.iter().any(|g| named(g) == named(a)))
                    .collect();
                let guest = (!guests.is_empty()).then(|| fields::Def::names(&guests));
                let guest = guest.as_deref();
                let playing = current.as_deref() == Some(track.path.as_path());
                let picked = view.picked.as_deref() == Some(track.path.as_path());
                // (A set's songs move and come out of it here.)
                let moves = (id == TOGETHER).then(|| (*t > 0, *t + 1 < tracks.len()));
                let output = track_row(ui, rect, track, guest, playing, picked, *t % 2 == 1, moves);
                if let Some(change) = output.change {
                    let mut keys = group.songs.clone();
                    match change {
                        Change::Sooner => keys.swap(*t - 1, *t),
                        Change::Later => keys.swap(*t, *t + 1),
                        Change::Out => {
                            keys.remove(*t);
                        }
                    }
                    act = Some(Act::Together(group.key.clone(), keys));
                }
                if output.picked {
                    view.picked = Some(track.path.clone());
                }
                if output.play && track.here {
                    let at = playable.iter().position(|p| *p == track.path).unwrap_or(0);
                    act = Some(Act::Play(playable.clone(), at));
                }
                if output.star {
                    act = Some(Act::Star(vec![track.path.clone()], !track.starred));
                }
                if output.info {
                    act = Some(Act::Info(track.path.clone()));
                }
                if let Some(next) = output.queue {
                    act = Some(Act::Queue(vec![track.path.clone()], next));
                }
            }
        });
        if choose_art {
            self.open_choose_art(&group.title, group.subtitle.clone(), group.songs.clone(), art_from);
        }
        act
    }
}

/// What a song not here yet says on hover.
const NOT_HERE: &str = "Not on this device yet: it's coming from your other devices";

/// What happened on a track's row.
#[derive(Default)]
struct RowOutput {
    picked: bool,
    play: bool,
    star: bool,
    info: bool,
    /// Queue it: next, or at the end.
    queue: Option<bool>,
    change: Option<Change>,
}

/// A change to where a song is in the set it plays together with.
#[derive(Clone, Copy)]
enum Change {
    Sooner,
    Later,
    Out,
}

/// One song's row in `rect`: its star, number, name, guest artist and
/// length, marked when it's `playing` or `picked`. A click (a tap, on a
/// phone) plays it and Ctrl+click picks it; on the desktop it drags onto
/// a playlist. One whose file isn't here yet (known from your other
/// devices) is faint, with a download cloud for its star, and does none of
/// that: there's nothing to play or pick yet. With `moves` (whether
/// it can go sooner, and later), it has buttons to move it in its set, or
/// take it out.
#[allow(clippy::too_many_arguments)]
fn track_row(ui: &mut egui::Ui, rect: egui::Rect, track: &Track, artist: Option<&str>, playing: bool, picked: bool, striped: bool, moves: Option<(bool, bool)>) -> RowOutput {
    let touch = select_list::touch(ui.ctx());
    let sense = match (track.here, touch) {
        (false, _) => egui::Sense::hover(),
        (true, true) => egui::Sense::click(),
        (true, false) => egui::Sense::click_and_drag(),
    };
    let mut response = ui.interact(rect, ui.id().with(("track", &track.path)), sense - egui::Sense::FOCUSABLE);
    if !track.here {
        response = response.on_hover_text(NOT_HERE);
    }
    let pick = !touch && ui.input(|i| i.modifiers.command || i.modifiers.shift);
    let mut output = RowOutput { picked: response.clicked() && pick, play: response.clicked() && !pick && !response.double_clicked(), ..RowOutput::default() };
    let visuals = ui.style().interact_selectable(&response, picked);
    let playing_color = crate::theme::playing_color(ui.ctx());
    if playing && !picked && !response.hovered() {
        ui.painter().rect_filled(rect, visuals.corner_radius, playing_color.gamma_multiply(0.15));
    } else if picked || (track.here && response.hovered()) {
        let stroke = if playing { egui::Stroke::NONE } else { visuals.bg_stroke };
        ui.painter().rect(rect, visuals.corner_radius, visuals.weak_bg_fill, stroke, egui::StrokeKind::Inside);
    } else if striped {
        ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
    }
    if playing {
        // Outlined, picked or not.
        ui.painter().rect_stroke(rect, visuals.corner_radius, egui::Stroke::new(1.0, playing_color), egui::StrokeKind::Inside);
    }
    if response.drag_started() {
        response.dnd_set_drag_payload(DraggedFiles { paths: vec![track.path.clone()], label: track.name.clone() });
    }
    if track.here {
        crate::menu::context(&response, |ui| {
            output.queue = super::queue::menu(ui);
            if ui.button("Song info").clicked() {
                output.info = true;
            }
        });
    }
    let builder = egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center));
    ui.scope_builder(builder, |ui| {
        ui.style_mut().interaction.selectable_labels = false;
        if picked {
            ui.visuals_mut().override_text_color = Some(visuals.text_color());
        } else if playing {
            ui.visuals_mut().override_text_color = Some(playing_color);
        } else if !track.here {
            let faint = ui.visuals().weak_text_color().gamma_multiply(0.45);
            ui.visuals_mut().override_text_color = Some(faint);
        }
        if track.here {
            output.star = select_list::star(ui, track.starred);
        } else {
            let (icon, _) = ui.allocate_exact_size(egui::vec2(select_list::STAR_WIDTH, ui.available_height()), egui::Sense::hover());
            let font = egui::TextStyle::Body.resolve(ui.style());
            let color = ui.visuals().weak_text_color();
            ui.painter().text(icon.center(), egui::Align2::CENTER_CENTER, super::icon("cloud-arrow-down"), font, color);
        }
        let number = track.number.map_or(String::new(), |n| n.to_string());
        ui.add_sized([24.0, rect.height()], egui::Label::new(egui::RichText::new(number).weak()));
        let scroll = track.here && (playing || response.contains_pointer());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(select_list::EDGE);
            if let Some((sooner, later)) = moves {
                if ui.button(super::icon("x")).on_hover_text("Take it out").clicked() {
                    output.change = Some(Change::Out);
                }
                if ui.add_enabled(later, egui::Button::new(super::icon("arrow-down"))).on_hover_text("Play it later").clicked() {
                    output.change = Some(Change::Later);
                }
                if ui.add_enabled(sooner, egui::Button::new(super::icon("arrow-up"))).on_hover_text("Play it sooner").clicked() {
                    output.change = Some(Change::Sooner);
                }
            }
            if let Some(length) = track.length {
                ui.label(egui::RichText::new(format_length(length)).weak());
            }
            match artist {
                Some(artist) if touch => select_list::two_lines(ui, std::iter::empty(), track.name.as_str(), Some(artist.to_string()), scroll),
                Some(artist) => {
                    ui.add_sized([160.0, rect.height()], egui::Label::new(egui::RichText::new(artist).weak()).truncate());
                    select_list::label(ui, track.name.as_str(), None, scroll);
                }
                None => {
                    select_list::label(ui, track.name.as_str(), None, scroll);
                }
            }
        });
    });
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_say_one_or_many() {
        assert_eq!(count(1, "song"), "1 song");
        assert_eq!(count(1200, "album"), "1,200 albums");
    }

    #[test]
    fn every_ruleset_keeps_its_own_view() {
        let mut groupings = Groupings::default();
        assert_eq!(groupings.shown(), PLAYLISTS, "the playlists come first");
        groupings.show("albums");
        groupings.view().open = Some("modal soul\u{0}nujabes".into());
        groupings.show(ARTISTS);
        assert!(groupings.view().open.is_none(), "another ruleset's view is its own");
        groupings.show("albums");
        assert!(groupings.view().open.is_some(), "the album's still open after switching away and back");
    }
}
