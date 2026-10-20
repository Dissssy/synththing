//! The library's views: Songs (the list in `song_list`), Artists and
//! Albums, picked in a sidebar on the desktop and as pages of their own on
//! a phone. Artists is a searchable list of artists
//! beside the open one's albums, each a section that folds away. Albums is
//! a grid of their art, and an album opens on a page of its own (pushed on
//! a phone), with Back to the grid.
//!
//! Each view keeps its own state for the session (search, the artist or
//! album open, folded sections, the track picked, and its scroll, which
//! egui keeps by each scroll area's ID): switching views changes only
//! which is drawn. Nothing of it is saved or synced. Only the rows and
//! tiles in sight are drawn, and art is read off the UI thread
//! (`album_art`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;

use super::{icon, icon_button, is_midi_path, listed_song_extensions, naming, App};
use crate::album_art::AlbumArt;
use crate::library_index::{matches, Album, Index, Track};
use crate::select_list;
use crate::song_info::{format_length, leading_number, thousands};
use crate::song_list::DraggedFiles;
use crate::song_store::{self, STARRED};

/// The library's views.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum View {
    #[default]
    Songs,
    Artists,
    Albums,
}

impl View {
    pub(super) const ALL: [View; 3] = [Self::Songs, Self::Artists, Self::Albums];

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Songs => "Songs",
            Self::Artists => "Artists",
            Self::Albums => "Albums",
        }
    }

    pub(super) fn icon(self) -> &'static str {
        match self {
            Self::Songs => "music-notes",
            Self::Artists => "microphone-stage",
            Self::Albums => "vinyl-record",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AlbumSort {
    #[default]
    Title,
    Artist,
    Year,
    Added,
}

impl AlbumSort {
    const ALL: [AlbumSort; 4] = [Self::Title, Self::Artist, Self::Year, Self::Added];

    fn title(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::Year => "Year",
            Self::Added => "Recently added",
        }
    }
}

#[derive(Default)]
struct ArtistsState {
    search: String,
    /// The artist open (their key).
    open: Option<String>,
    /// The sections folded away: albums' keys, and `SINGLES`.
    collapsed: HashSet<String>,
    picked: Option<PathBuf>,
}

#[derive(Default)]
struct AlbumsState {
    search: String,
    sort: AlbumSort,
    /// The album open (its key), on its page.
    open: Option<String>,
    /// "Find in album".
    find: String,
    picked: Option<PathBuf>,
}

/// The views' state, for the session.
#[derive(Default)]
pub(super) struct LibraryViews {
    /// The view the desktop's sidebar has picked.
    pub(super) view: View,
    /// A link went to this view (an album's artist): the phone turns to
    /// its page.
    pub(super) jump: Option<View>,
    /// What the views are made from, and what that was made from.
    index: Option<(u64, Arc<Index>)>,
    art: AlbumArt,
    artists: ArtistsState,
    albums: AlbumsState,
}

impl LibraryViews {
    /// What a phone's top bar names the page pushed over `view`, if one is.
    pub(super) fn pushed_title(&self, view: View) -> Option<String> {
        let index = &self.index.as_ref()?.1;
        match view {
            View::Songs => None,
            View::Artists => Some(index.artist(self.artists.open.as_deref()?)?.name.clone()),
            View::Albums => Some(index.album(self.albums.open.as_deref()?)?.title.clone()),
        }
    }

    /// Back from the page pushed over `view` (the phone's): whether there
    /// was one.
    pub(super) fn back(&mut self, view: View) -> bool {
        match view {
            View::Songs => false,
            View::Artists => self.artists.open.take().is_some(),
            View::Albums => self.albums.open.take().is_some(),
        }
    }
}

/// The key the songs on no album fold away by.
const SINGLES: &str = "\u{0}singles";

/// A section's header in an artist's pane, and the pane's own.
const ARTIST_HEADER: f32 = 64.0;
const ALBUM_HEADER: f32 = 76.0;
/// An album page's art, on the desktop and on a phone.
const PAGE_ART: f32 = 160.0;
const PHONE_PAGE_ART: f32 = 112.0;
/// The albums grid's tiles: the art's width, and the most a phone's take.
const TILE: f32 = 150.0;
/// The two lines of text under a tile's art.
const TILE_TEXT: f32 = 38.0;

/// What the views asked for this frame, done once they're drawn.
enum Act {
    /// Play these songs (each with whether it's starred) from the one at
    /// the index.
    Play(Vec<(PathBuf, bool)>, usize),
    /// Play them shuffled.
    Shuffle(Vec<(PathBuf, bool)>),
    Star(Vec<PathBuf>, bool),
    Info(PathBuf),
    OpenArtist(String),
}

/// What playing a list of tracks takes.
fn playable<'a>(tracks: impl IntoIterator<Item = &'a Track>) -> Vec<(PathBuf, bool)> {
    tracks.into_iter().map(|t| (t.path.clone(), t.starred)).collect()
}

/// "3 albums, 30 songs": how many of `what` (singular) there are.
fn count(n: usize, what: &str) -> String {
    format!("{} {what}{}", thousands(n), if n == 1 { "" } else { "s" })
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
                let rect = egui::Rect::from_min_size(origin + egui::vec2(0.0, top), egui::vec2(width, height));
                draw(ui, rect, i);
            }
            top += height;
        }
    });
}

/// How tall a track's row is.
fn row_height(ui: &egui::Ui) -> f32 {
    if select_list::touch(ui.ctx()) { 44.0 } else { ui.spacing().interact_size.y }
}

/// What happened on a track's row.
#[derive(Default)]
struct RowOutput {
    picked: bool,
    play: bool,
    star: bool,
    info: bool,
}

/// One track's row in `rect`: its star, number, name and length, marked
/// when it's `playing` or `picked`. A click picks it and a double-click
/// plays it (a tap, on a phone), and on the desktop it drags onto a
/// playlist.
#[allow(clippy::too_many_arguments)]
fn track_row(ui: &mut egui::Ui, rect: egui::Rect, track: &Track, number: Option<u32>, artist: Option<&str>, playing: bool, picked: bool, striped: bool) -> RowOutput {
    let touch = select_list::touch(ui.ctx());
    let sense = if touch { egui::Sense::click() } else { egui::Sense::click_and_drag() };
    let response = ui.interact(rect, ui.id().with(("track", &track.path)), sense - egui::Sense::FOCUSABLE);
    let mut output = RowOutput { picked: response.clicked(), play: response.double_clicked() || (touch && response.clicked()), ..RowOutput::default() };
    let visuals = ui.style().interact_selectable(&response, picked);
    let playing_color = crate::theme::playing_color(ui.ctx());
    if playing && !picked && !response.hovered() {
        ui.painter().rect(rect, visuals.corner_radius, playing_color.gamma_multiply(0.15), egui::Stroke::new(1.0, playing_color), egui::StrokeKind::Inside);
    } else if picked || response.hovered() {
        ui.painter().rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
    } else if striped {
        ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
    }
    if response.drag_started() {
        response.dnd_set_drag_payload(DraggedFiles { paths: vec![track.path.clone()], label: track.name.clone() });
    }
    response.context_menu(|ui| {
        if ui.button("Song info").clicked() {
            output.info = true;
        }
    });
    let builder = egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center));
    ui.scope_builder(builder, |ui| {
        ui.style_mut().interaction.selectable_labels = false;
        if picked {
            ui.visuals_mut().override_text_color = Some(visuals.text_color());
        } else if playing {
            ui.visuals_mut().override_text_color = Some(playing_color);
        }
        output.star = select_list::star(ui, track.starred);
        let number = number.map_or(String::new(), |n| n.to_string());
        ui.add_sized([24.0, rect.height()], egui::Label::new(egui::RichText::new(number).weak()));
        let scroll = playing || response.contains_pointer();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            if let Some(length) = track.length {
                ui.label(egui::RichText::new(format_length(length)).weak());
            }
            match artist {
                Some(artist) if touch => select_list::two_lines(ui, std::iter::empty(), track.name.as_str(), Some(artist.to_string()), scroll),
                Some(artist) => {
                    ui.add_sized([140.0, rect.height()], egui::Label::new(egui::RichText::new(artist).weak()).truncate());
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

/// Play, shuffle and (with `starred` given) star them all. What was clicked: 0 play, 1 shuffle, 2 star.
fn play_buttons(ui: &mut egui::Ui, starred: Option<bool>) -> Option<u8> {
    let mut clicked = None;
    if ui.add(icon_button("play")).on_hover_text("Play").clicked() {
        clicked = Some(0);
    }
    if ui.add(icon_button("shuffle")).on_hover_text("Shuffle").clicked() {
        clicked = Some(1);
    }
    if let Some(starred) = starred {
        let hover = if starred { "Unstar them all" } else { "Star them all" };
        if ui.add(icon_button(select_list::STAR_ICON).selected(starred)).on_hover_text(hover).clicked() {
            clicked = Some(2);
        }
    }
    clicked
}

impl App {
    /// The revision of what the songs are listed from: it changes whenever
    /// the songs, what's been read of them, or how they're named do.
    pub(super) fn songs_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.songs.revision(), self.song_info.revision(), &self.config.title_patterns, listed_song_extensions(&self.config)).hash(&mut hasher);
        hasher.finish()
    }

    /// The artists and albums of the songs here, made again when they
    /// change.
    fn library_index(&mut self) -> Arc<Index> {
        let revision = self.songs_revision();
        if let Some((built, index)) = &self.library_views.index
            && *built == revision
        {
            return index.clone();
        }
        let _timing = crate::frame_timing::scope("library index");
        let extensions = listed_song_extensions(&self.config);
        let naming = naming(&self.config, &self.songs);
        let cache = &mut self.song_info;
        let tracks: Vec<Track> = self
            .songs
            .songs()
            .map(|(path, _)| path)
            .filter(|p| song_store::has_extension(p, extensions))
            .map(|path| {
                let info = cache.get(&path);
                let song = naming.song(&path, info.as_deref());
                let text = |key: &str| song.field(key).filter(|v| !v.trim().is_empty());
                let number = |key: &str| song.field(key).as_deref().and_then(leading_number);
                Track {
                    name: naming.name(&path, info.as_deref()),
                    artist: text("artist"),
                    album_artist: text("album_artist"),
                    album: text("album"),
                    year: number("year"),
                    disc: number("disc"),
                    track: number("track"),
                    length: song.number("length"),
                    starred: self.songs.is_starred(&path),
                    imported: naming.imported(&path).unwrap_or(0),
                    midi: is_midi_path(&path),
                    path,
                }
            })
            .collect();
        let index = Arc::new(Index::build(tracks));
        self.library_views.index = Some((revision, index.clone()));
        index
    }

    /// The Songs tab on the desktop: the sidebar, and the view it picked.
    pub(super) fn library_section_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(view) = self.library_views.jump.take() {
            self.library_views.view = view;
        }
        egui::Panel::left("library_sidebar").resizable(true).default_size(150.0).min_size(110.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.library_sidebar_ui(ui));
        });
        let view = self.library_views.view;
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.library_view_ui(ui, view));
    }

    /// One of the views, filling `ui`.
    pub(super) fn library_view_ui(&mut self, ui: &mut egui::Ui, view: View) {
        match view {
            View::Songs => self.songs_ui(ui),
            View::Artists => self.artists_ui(ui),
            View::Albums => self.albums_ui(ui),
        }
    }

    /// Songs, Artists and Albums. (Rulesets join them later, as views of
    /// their own.)
    fn library_sidebar_ui(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().button_padding = egui::vec2(6.0, 4.0);
        for view in View::ALL {
            let text = format!("{} {}", icon(view.icon()), view.title());
            let selected = self.library_views.view == view;
            if ui.add_sized([ui.available_width(), 0.0], egui::Button::selectable(selected, text)).clicked() {
                self.library_views.view = view;
            }
        }
    }

    /// Do what the views asked.
    fn library_act(&mut self, act: Act) {
        match act {
            Act::Play(tracks, start) => self.play_tracks(tracks, start),
            Act::Shuffle(tracks) => {
                if tracks.is_empty() {
                    return;
                }
                self.set_shuffle(true);
                let start = (self.rng.next() % tracks.len() as u64) as usize;
                self.play_tracks(tracks, start);
            }
            Act::Star(paths, on) => self.tag_songs(super::tags::TagAction::Set(STARRED.into(), on), &paths),
            Act::Info(path) => self.song_info_open = Some(path),
            Act::OpenArtist(key) => {
                self.library_views.artists.open = Some(key);
                self.library_views.jump = Some(View::Artists);
            }
        }
    }

    // --- Artists -----------------------------------------------------------

    fn artists_ui(&mut self, ui: &mut egui::Ui) {
        let index = self.library_index();
        self.library_views.art.frame(ui.ctx());
        if index.artists.is_empty() {
            ui.weak("No artists yet: songs are listed here by their files' artist tags.");
            return;
        }
        let phone = self.mobile();
        let open = self.library_views.artists.open.clone().filter(|key| index.artist(key).is_some());
        let mut act = None;
        if phone {
            match open {
                Some(key) => act = self.artist_pane_ui(ui, &index, &key),
                None => self.artist_list_ui(ui, &index),
            }
        } else {
            egui::Panel::left("artist_list").resizable(true).default_size(200.0).min_size(120.0).show(ui, |ui| self.artist_list_ui(ui, &index));
            egui::CentralPanel::default().show(ui, |ui| match open {
                Some(key) => act = self.artist_pane_ui(ui, &index, &key),
                None => {
                    ui.weak("Pick an artist.");
                }
            });
        }
        if let Some(act) = act {
            self.library_act(act);
        }
    }

    fn artist_list_ui(&mut self, ui: &mut egui::Ui, index: &Index) {
        let state = &mut self.library_views.artists;
        ui.add(egui::TextEdit::singleline(&mut state.search).hint_text("search artists...").desired_width(f32::INFINITY));
        let query = state.search.trim();
        let shown: Vec<usize> = (0..index.artists.len()).filter(|&i| query.is_empty() || matches(&index.artists[i].name, query)).collect();
        let height = row_height(ui);
        let mut open = None;
        egui::ScrollArea::vertical().id_salt("artist_list").auto_shrink([false, false]).show_rows(ui, height, shown.len(), |ui, rows| {
            for &i in &shown[rows] {
                let artist = &index.artists[i];
                let selected = state.open.as_deref() == Some(artist.key.as_str());
                let button = egui::Button::selectable(selected, artist.name.as_str()).truncate();
                if ui.add_sized([ui.available_width(), height], button).clicked() {
                    open = Some(artist.key.clone());
                }
            }
        });
        if let Some(key) = open {
            state.open = Some(key);
            state.picked = None;
        }
    }

    /// An artist's pane: their name, counts and buttons, then each album
    /// as a section (art, title, year, tracks) that folds away, and their
    /// songs on no album last.
    fn artist_pane_ui(&mut self, ui: &mut egui::Ui, index: &Index, key: &str) -> Option<Act> {
        let artist = index.artist(key)?;
        let current = self.current_song.clone();
        let views = &mut self.library_views;
        let state = &mut views.artists;
        let art = &mut views.art;
        let row = row_height(ui);

        enum Row {
            Header,
            Album(usize),
            Track(Option<usize>, usize),
            Singles,
        }
        let mut rows = vec![Row::Header];
        let mut heights = vec![ARTIST_HEADER];
        for &a in &artist.albums {
            rows.push(Row::Album(a));
            heights.push(ALBUM_HEADER);
            if !state.collapsed.contains(&index.albums[a].key) {
                for t in 0..index.albums[a].tracks.len() {
                    rows.push(Row::Track(Some(a), t));
                    heights.push(row);
                }
            }
        }
        if !artist.singles.is_empty() {
            rows.push(Row::Singles);
            heights.push(row + 8.0);
            if !state.collapsed.contains(SINGLES) {
                for t in 0..artist.singles.len() {
                    rows.push(Row::Track(None, t));
                    heights.push(row);
                }
            }
        }

        let all = || playable(artist.tracks(index));
        let mut act = None;
        let mut toggle = None;
        virtual_rows(ui, ("artist_pane", key), &heights, |ui, rect, i| match rows[i] {
            Row::Header => {
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink(4.0)), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&artist.name).heading()).truncate());
                    ui.horizontal(|ui| {
                        let starred = artist.tracks(index).all(|t| t.starred);
                        match play_buttons(ui, Some(starred)) {
                            Some(0) => act = Some(Act::Play(all(), 0)),
                            Some(1) => act = Some(Act::Shuffle(all())),
                            Some(_) => act = Some(Act::Star(artist.tracks(index).map(|t| t.path.clone()).collect(), !starred)),
                            None => {}
                        }
                        let songs = artist.song_count(index);
                        ui.weak(format!("{}, {}", count(artist.albums.len(), "album"), count(songs, "song")));
                    });
                });
            }
            Row::Album(a) => {
                let album = &index.albums[a];
                let collapsed = state.collapsed.contains(&album.key);
                if album_section_header(ui, rect, album, collapsed, art) {
                    toggle = Some(album.key.clone());
                }
                let play = egui::Rect::from_min_size(rect.right_center() - egui::vec2(36.0, 12.0), egui::vec2(28.0, 24.0));
                if ui.put(play, icon_button("play")).on_hover_text("Play the album").clicked() {
                    act = Some(Act::Play(playable(&album.tracks), 0));
                }
            }
            Row::Singles => {
                let collapsed = state.collapsed.contains(SINGLES);
                let response = ui.interact(rect, ui.id().with("singles"), egui::Sense::click());
                let arrow = icon(if collapsed { "caret-right" } else { "caret-down" });
                let text = format!("{arrow} Songs on no album ({})", artist.singles.len());
                ui.painter().text(rect.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, text, egui::TextStyle::Button.resolve(ui.style()), ui.visuals().text_color());
                if response.clicked() {
                    toggle = Some(SINGLES.to_string());
                }
            }
            Row::Track(album, t) => {
                let (tracks, number) = match album {
                    Some(a) => (&index.albums[a].tracks, index.albums[a].tracks[t].track),
                    None => (&artist.singles, None),
                };
                let track = &tracks[t];
                // (A guest's name beside their track.)
                let guest = track.artist.as_deref().filter(|a| !a.eq_ignore_ascii_case(&artist.name));
                let playing = current.as_deref() == Some(track.path.as_path());
                let picked = state.picked.as_deref() == Some(track.path.as_path());
                let output = track_row(ui, rect, track, number, guest, playing, picked, t % 2 == 1);
                if output.picked {
                    state.picked = Some(track.path.clone());
                }
                if output.play {
                    act = Some(Act::Play(playable(tracks), t));
                }
                if output.star {
                    act = Some(Act::Star(vec![track.path.clone()], !track.starred));
                }
                if output.info {
                    act = Some(Act::Info(track.path.clone()));
                }
            }
        });
        if let Some(key) = toggle
            && !state.collapsed.remove(&key)
        {
            state.collapsed.insert(key);
        }
        act
    }

    // --- Albums ------------------------------------------------------------

    fn albums_ui(&mut self, ui: &mut egui::Ui) {
        let index = self.library_index();
        self.library_views.art.frame(ui.ctx());
        let open = self.library_views.albums.open.clone().filter(|key| index.album(key).is_some());
        let act = match open {
            Some(key) => self.album_page_ui(ui, &index, &key),
            None => {
                self.albums_grid_ui(ui, &index);
                None
            }
        };
        if let Some(act) = act {
            self.library_act(act);
        }
    }

    /// Every album as a tile of its art, its title and artist under it,
    /// with search and sort; clicking one opens its page.
    fn albums_grid_ui(&mut self, ui: &mut egui::Ui, index: &Index) {
        let views = &mut self.library_views;
        let state = &mut views.albums;
        let art = &mut views.art;
        if index.albums.is_empty() {
            ui.weak("No albums yet: songs are listed here by their files' album tags.");
            return;
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("album_sort").selected_text(state.sort.title()).show_ui(ui, |ui| {
                for sort in AlbumSort::ALL {
                    ui.selectable_value(&mut state.sort, sort, sort.title());
                }
            });
            ui.add(egui::TextEdit::singleline(&mut state.search).hint_text("search albums...").desired_width(f32::INFINITY));
        });
        let query = state.search.trim();
        let mut shown: Vec<&Album> = index.albums.iter().filter(|a| query.is_empty() || matches(&format!("{} {}", a.title, a.artist), query)).collect();
        match state.sort {
            // (They're by title already.)
            AlbumSort::Title => {}
            AlbumSort::Artist => shown.sort_by_cached_key(|a| a.artist.to_lowercase()),
            AlbumSort::Year => shown.sort_by_key(|a| std::cmp::Reverse(a.year)),
            AlbumSort::Added => shown.sort_by_key(|a| std::cmp::Reverse(a.added)),
        }

        let gap = 12.0;
        let width = ui.available_width();
        let columns = ((width + gap) / (TILE + gap)).floor().max(2.0) as usize;
        let tile = ((width - gap * (columns - 1) as f32) / columns as f32).min(TILE * 1.4);
        let height = tile + TILE_TEXT + gap;
        let rows = shown.len().div_ceil(columns);
        let mut open = None;
        egui::ScrollArea::vertical().id_salt("albums_grid").auto_shrink([false, false]).show_rows(ui, height, rows, |ui, range| {
            for row in range {
                let (_, line) = ui.allocate_space(egui::vec2(width, height));
                for (column, album) in shown.iter().skip(row * columns).take(columns).enumerate() {
                    let min = line.min + egui::vec2(column as f32 * (tile + gap), 0.0);
                    let rect = egui::Rect::from_min_size(min, egui::vec2(tile, tile + TILE_TEXT));
                    if album_tile(ui, rect, album, art).clicked() {
                        open = Some(album.key.clone());
                    }
                }
            }
        });
        if let Some(key) = open {
            state.open = Some(key);
            state.find.clear();
            state.picked = None;
        }
    }

    /// An album's page: its art, title, artist (a link to them), year,
    /// play and shuffle, then its tracks by disc and number, which "find
    /// in album" narrows.
    fn album_page_ui(&mut self, ui: &mut egui::Ui, index: &Index, key: &str) -> Option<Act> {
        let album = index.album(key)?;
        let phone = self.mobile();
        let current = self.current_song.clone();
        let views = &mut self.library_views;
        let state = &mut views.albums;
        let art = &mut views.art;
        ui.horizontal(|ui| {
            // (A phone's top bar has Back.)
            if !phone && ui.add(icon_button("arrow-left")).on_hover_text("Back to the albums").clicked() {
                state.open = None;
            }
            ui.add(egui::TextEdit::singleline(&mut state.find).hint_text("find in album...").desired_width(f32::INFINITY));
        });
        if state.open.is_none() {
            return None;
        }

        enum Row {
            Header,
            Disc(u32),
            Track(usize),
        }
        let row = row_height(ui);
        let art_size = if phone { PHONE_PAGE_ART } else { PAGE_ART };
        let mut rows = vec![Row::Header];
        let mut heights = vec![art_size + 16.0];
        let query = state.find.trim();
        let discs = album.discs();
        let mut disc = None;
        for (t, track) in album.tracks.iter().enumerate() {
            if !query.is_empty() && !matches(&track.name, query) {
                continue;
            }
            let this = track.disc.unwrap_or(1);
            if discs && disc != Some(this) {
                disc = Some(this);
                rows.push(Row::Disc(this));
                heights.push(row + 4.0);
            }
            rows.push(Row::Track(t));
            heights.push(row);
        }

        let mut act = None;
        virtual_rows(ui, ("album_page", key), &heights, |ui, rect, i| match rows[i] {
            Row::Header => {
                let inner = rect.shrink(8.0);
                let cover = egui::Rect::from_min_size(inner.min, egui::vec2(art_size, art_size));
                art.tile(ui, cover, album.art_from().map(PathBuf::as_path), &album.title);
                let text = egui::Rect::from_min_max(egui::pos2(cover.right() + 12.0, inner.top()), inner.max);
                ui.scope_builder(egui::UiBuilder::new().max_rect(text), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&album.title).heading()).wrap());
                    if ui.link(&album.artist).on_hover_text("Show the artist").clicked() {
                        act = Some(Act::OpenArtist(album.artist.to_lowercase()));
                    }
                    let mut facts = vec![count(album.tracks.len(), "song"), format_length(album.length())];
                    if let Some(year) = album.year {
                        facts.insert(0, year.to_string());
                    }
                    ui.weak(facts.join(" · "));
                    ui.horizontal(|ui| match play_buttons(ui, None) {
                        Some(0) => act = Some(Act::Play(playable(&album.tracks), 0)),
                        Some(_) => act = Some(Act::Shuffle(playable(&album.tracks))),
                        None => {}
                    });
                });
            }
            Row::Disc(disc) => {
                let font = egui::TextStyle::Button.resolve(ui.style());
                ui.painter().text(rect.left_bottom() + egui::vec2(4.0, -4.0), egui::Align2::LEFT_BOTTOM, format!("Disc {disc}"), font, ui.visuals().strong_text_color());
            }
            Row::Track(t) => {
                let track = &album.tracks[t];
                let guest = track.artist.as_deref().filter(|a| !a.eq_ignore_ascii_case(&album.artist));
                let playing = current.as_deref() == Some(track.path.as_path());
                let picked = state.picked.as_deref() == Some(track.path.as_path());
                let output = track_row(ui, rect, track, track.track, guest, playing, picked, t % 2 == 1);
                if output.picked {
                    state.picked = Some(track.path.clone());
                }
                if output.play {
                    act = Some(Act::Play(playable(&album.tracks), t));
                }
                if output.star {
                    act = Some(Act::Star(vec![track.path.clone()], !track.starred));
                }
                if output.info {
                    act = Some(Act::Info(track.path.clone()));
                }
            }
        });
        act
    }
}

/// An album's section header in an artist's pane, in `rect`: its fold
/// arrow, art, title, and year, song count and length. Whether it was
/// clicked (to fold it or open it out).
fn album_section_header(ui: &mut egui::Ui, rect: egui::Rect, album: &Album, collapsed: bool, art: &mut AlbumArt) -> bool {
    let response = ui.interact(rect, ui.id().with(("album_section", &album.key)), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let inner = rect.shrink2(egui::vec2(4.0, 6.0));
    let arrow = icon(if collapsed { "caret-right" } else { "caret-down" });
    let font = egui::TextStyle::Button.resolve(ui.style());
    ui.painter().text(egui::pos2(inner.left(), inner.center().y), egui::Align2::LEFT_CENTER, arrow, font, ui.visuals().text_color());
    let cover = egui::Rect::from_min_size(egui::pos2(inner.left() + 20.0, inner.top() + 6.0), egui::vec2(inner.height() - 6.0, inner.height() - 6.0));
    art.tile(ui, cover, album.art_from().map(PathBuf::as_path), &album.title);
    let text = egui::Rect::from_min_max(egui::pos2(cover.right() + 10.0, cover.top()), egui::pos2(inner.right() - 44.0, cover.bottom()));
    ui.scope_builder(egui::UiBuilder::new().max_rect(text).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.style_mut().interaction.selectable_labels = false;
        ui.add(egui::Label::new(egui::RichText::new(&album.title).strong()).truncate());
        let mut facts = vec![count(album.tracks.len(), "song"), format_length(album.length())];
        if let Some(year) = album.year {
            facts.insert(0, year.to_string());
        }
        ui.add(egui::Label::new(egui::RichText::new(facts.join(" · ")).weak()).truncate());
    });
    response.clicked()
}

/// An album's tile in the grid, in `rect`: its art, and its title and
/// artist under it.
fn album_tile(ui: &mut egui::Ui, rect: egui::Rect, album: &Album, art: &mut AlbumArt) -> egui::Response {
    let response = ui.interact(rect, ui.id().with(("album_tile", &album.key)), egui::Sense::click());
    let cover = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), rect.width()));
    art.tile(ui, cover, album.art_from().map(PathBuf::as_path), &album.title);
    if response.hovered() {
        ui.painter().rect_stroke(cover, 4.0, ui.visuals().widgets.hovered.fg_stroke, egui::StrokeKind::Inside);
    }
    let text = egui::Rect::from_min_max(egui::pos2(rect.left(), cover.bottom() + 2.0), rect.max);
    ui.scope_builder(egui::UiBuilder::new().max_rect(text).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.style_mut().interaction.selectable_labels = false;
        ui.add(egui::Label::new(&album.title).truncate());
        ui.add(egui::Label::new(egui::RichText::new(&album.artist).weak()).truncate());
    });
    response.on_hover_text(format!("{}\n{}", album.title, album.artist))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_say_one_or_many() {
        assert_eq!(count(1, "song"), "1 song");
        assert_eq!(count(1200, "album"), "1,200 albums");
    }
}
