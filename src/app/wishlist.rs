//! The Wishlist tab (and the mobile layout's Wishlist page): music wished
//! for (`crate::wishlist`), found by a MusicBrainz search or put in by
//! name, in three parts: Wanted, Partly got and Got it, worked out from
//! the songs here as they're matched. A wish that comes true says so.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::mpsc;

use eframe::egui;

use super::{icon, toast, App};
use crate::album_art::{AlbumArt, Source};
use crate::build_info::{self, Online};
use crate::lookup::Kept;
use crate::lookup::musicbrainz::MusicBrainz;
use crate::select_list::{self, Selection};
use crate::song_store::SongStore;
use crate::wishlist::{Kind, Progress, State, Wish, Wishlist};

/// What a MusicBrainz search found.
struct Found {
    albums: Vec<Wish>,
    songs: Vec<Wish>,
}

/// A search being made, or what came of it.
enum Search {
    Waiting(mpsc::Receiver<Result<Found, String>>),
    Done(Result<Found, String>),
}

pub(super) struct WishlistState {
    pub list: Wishlist,
    query: String,
    search: Option<Search>,
    /// A wish by name being written: its title and artist.
    by_name: Option<(String, String)>,
    /// A wish's note being written: its ID and the note.
    note: Option<(String, String)>,
    /// What of each wish is here, as of the wishlist's and the songs'
    /// revisions.
    progress: Option<((u64, u64), BTreeMap<String, Progress>)>,
    /// The wishes got, as last worked out (none yet: nothing to say).
    got: Option<HashSet<String>>,
    /// The parts folded away.
    folded: BTreeSet<State>,
    /// An album's release asked of MusicBrainz, to wish for the rest of it.
    fetching: Option<mpsc::Receiver<Result<crate::lookup::matching::Release, String>>>,
    selection: Selection,
    art: AlbumArt,
}

impl Default for WishlistState {
    fn default() -> Self {
        Self {
            list: Wishlist::load(),
            query: String::new(),
            search: None,
            by_name: None,
            note: None,
            progress: None,
            got: None,
            folded: BTreeSet::new(),
            fetching: None,
            selection: Selection::default(),
            art: AlbumArt::default(),
        }
    }
}

/// A row of the list: a part's heading, or a wish (by ID).
enum Row {
    Heading(State, usize),
    Wish(String),
}

/// What's asked of the list's rows.
enum Act {
    Toggle(State),
    Play(String),
    Note(String),
    Remove(Vec<String>),
}

impl App {
    /// Once a frame: work out what of each wish is here when the wishlist
    /// or the songs changed, and say which wishes just came true.
    pub(super) fn wishlist_tick(&mut self, ctx: &egui::Context) {
        self.take_fetched_album(ctx);
        let revisions = (self.wishlist.list.revision(), self.songs.revision());
        if self.wishlist.progress.as_ref().is_some_and(|(seen, _)| *seen == revisions) {
            return;
        }
        let progress = crate::wishlist::progress(&self.wishlist.list, &self.songs, &self.lookup.kept);
        let got: HashSet<String> = progress.iter().filter(|(_, p)| p.state() == State::Got).map(|(id, _)| id.clone()).collect();
        if let Some(before) = &self.wishlist.got {
            for id in got.difference(before) {
                // (Not one just wished for that's here already.)
                let Some(wish) = self.wishlist.list.wishes.get(id).filter(|w| crate::settings_sync::now() - w.added > 60) else { continue };
                toast::show(ctx, format!("From your wishlist: {}, wished {}", wish.title, crate::library::ago(wish.added)));
            }
        }
        self.wishlist.got = Some(got);
        self.wishlist.progress = Some((revisions, progress));
    }

    /// Wish for `wish`, and send it to the other devices.
    pub(super) fn wish_for(&mut self, wish: Wish) {
        self.wishlist.list.add(wish);
        self.wishlist_changed();
    }

    /// Wish for the rest of an album from somewhere else in the app, and
    /// say so: once MusicBrainz says what its tracks are, if it's to be
    /// asked.
    pub(super) fn wish_for_album(&mut self, ctx: &egui::Context, ask: AlbumAsk) {
        match ask {
            AlbumAsk::Wish(wish) => {
                toast::show(ctx, format!("Wished for {}", wish.title));
                self.wish_for(wish);
            }
            AlbumAsk::Fetch(release) => {
                let (send, receive) = mpsc::channel();
                let repaint = ctx.clone();
                let started = std::thread::Builder::new().name("wishlist album".into()).spawn(move || {
                    let _ = send.send(MusicBrainz::new().fresh_release(&release).map_err(|e| format!("{e:#}")));
                    repaint.request_repaint();
                });
                match started {
                    Ok(_) => self.wishlist.fetching = Some(receive),
                    Err(e) => toast::show(ctx, format!("Couldn't ask MusicBrainz: {e}")),
                }
            }
        }
    }

    /// An album asked about for the wishlist, once MusicBrainz answered.
    fn take_fetched_album(&mut self, ctx: &egui::Context) {
        let Some(answer) = self.wishlist.fetching.as_ref().and_then(|r| r.try_recv().ok()) else { return };
        self.wishlist.fetching = None;
        match answer {
            Ok(release) => {
                self.lookup.kept.keep_release(release.clone());
                self.groupings.tracks_changed();
                match crate::wishlist::rest_of(&self.songs, &release) {
                    Some(wish) => self.wish_for_album(ctx, AlbumAsk::Wish(wish)),
                    None => toast::show(ctx, format!("All of {} is here already", release.title)),
                }
            }
            Err(e) => toast::show(ctx, format!("Couldn't ask MusicBrainz about its album: {e}")),
        }
    }

    /// The wishes for albums none of are here, by ID: By Album shows them
    /// dimmed.
    pub(super) fn wanted_albums(&self) -> Vec<(&str, &Wish)> {
        let Some((_, progress)) = &self.wishlist.progress else { return Vec::new() };
        self.wishlist
            .list
            .wishes
            .iter()
            .filter(|(id, wish)| wish.kind == Kind::Album && progress.get(*id).is_none_or(|p| p.state() == State::Wanted))
            .map(|(id, wish)| (id.as_str(), wish))
            .collect()
    }

    /// Show the Wishlist: its tab, or its page on a phone.
    pub(super) fn go_to_wishlist(&mut self) {
        if self.mobile() {
            self.show_mobile_page(super::mobile::Page::Wishlist);
        } else {
            self.set_open(crate::layout::Section::Wishlist, true);
            super::layout::focus(&mut self.dock, crate::layout::Section::Wishlist);
        }
    }

    /// Take the wish `id` off the wishlist.
    pub(super) fn unwish(&mut self, id: &str) {
        self.wishlist.list.remove(&[id.to_string()]);
        self.wishlist_changed();
    }

    fn start_wishlist_search(&mut self, ctx: &egui::Context) {
        let words = self.wishlist.query.trim().to_string();
        if words.is_empty() {
            return;
        }
        let (send, receive) = mpsc::channel();
        let ctx = ctx.clone();
        let started = std::thread::Builder::new().name("wishlist search".into()).spawn(move || {
            let mut musicbrainz = MusicBrainz::new();
            let found = musicbrainz
                .find_albums(&words)
                .and_then(|albums| Ok(Found { albums, songs: musicbrainz.find_songs(&words)? }))
                .map_err(|e| format!("{e:#}"));
            let _ = send.send(found);
            ctx.request_repaint();
        });
        match started {
            Ok(_) => self.wishlist.search = Some(Search::Waiting(receive)),
            Err(e) => self.wishlist.search = Some(Search::Done(Err(e.to_string()))),
        }
    }

    pub(super) fn wishlist_ui(&mut self, ui: &mut egui::Ui) {
        self.wishlist_tick(ui.ctx());
        let online = self.config.online_lookup && build_info::online(Online::Lookup);
        self.wishlist.art.letters = self.config.tile_letters.unwrap_or(crate::album_art::DEFAULT_LETTERS);
        self.wishlist.art.frame(ui.ctx(), online);
        if let Some(Search::Waiting(receive)) = &self.wishlist.search
            && let Ok(found) = receive.try_recv()
        {
            self.wishlist.search = Some(Search::Done(found));
        }

        self.wishlist_search_ui(ui, online);
        self.wishlist_by_name_ui(ui);
        ui.separator();
        self.wishlist_list_ui(ui);
        self.wishlist_note_ui(ui.ctx());
    }

    /// The search box, and what it found.
    fn wishlist_search_ui(&mut self, ui: &mut egui::Ui, online: bool) {
        if !self.config.online_lookup {
            ui.weak("Searching MusicBrainz needs Online album art and details on (Preferences > Songs). You can still wish for music by name.");
        } else {
            // (Off in a branch build: it says so, with a button.)
            self.online_ui(ui, Online::Lookup);
        }
        let mut search = false;
        ui.horizontal(|ui| {
            let room = (ui.available_width() - 120.0).max(80.0);
            let edit = egui::TextEdit::singleline(&mut self.wishlist.query).hint_text("Search MusicBrainz for albums and songs").desired_width(room);
            let response = ui.add_enabled(online, edit);
            search = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let waiting = matches!(self.wishlist.search, Some(Search::Waiting(_)));
            search |= ui.add_enabled(online && !waiting, egui::Button::new("Search")).clicked();
            if ui.add(egui::Button::new(icon("plus"))).on_hover_text("Wish for something by name (not on MusicBrainz, or offline)").clicked() {
                self.wishlist.by_name = Some((self.wishlist.query.trim().to_string(), String::new()));
            }
        });
        if search {
            self.start_wishlist_search(ui.ctx());
        }
        let mut close = false;
        let mut add = None;
        match &self.wishlist.search {
            None => {}
            Some(Search::Waiting(_)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Searching MusicBrainz...");
                });
            }
            Some(Search::Done(Err(e))) => {
                ui.horizontal_wrapped(|ui| {
                    ui.weak(format!("The search failed: {e}"));
                    close = ui.small_button(icon("x")).clicked();
                });
            }
            Some(Search::Done(Ok(found))) => {
                let (list, art) = (&self.wishlist.list, &mut self.wishlist.art);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong("Found at MusicBrainz");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.small_button(icon("x")).on_hover_text("Put the results away").clicked();
                        });
                    });
                    egui::ScrollArea::vertical().id_salt("wishlist_found").max_height(ui.ctx().content_rect().height() * 0.4).show(ui, |ui| {
                        for (heading, wishes) in [("Albums", &found.albums), ("Songs", &found.songs)] {
                            ui.label(egui::RichText::new(heading).weak());
                            if wishes.is_empty() {
                                ui.weak("Nothing found.");
                            }
                            for wish in wishes {
                                if found_row_ui(ui, art, wish, list.find(wish).is_some()) {
                                    add = Some(wish.clone());
                                }
                            }
                        }
                    });
                });
            }
        }
        if close {
            self.wishlist.search = None;
        }
        if let Some(wish) = add {
            toast::show(ui.ctx(), format!("Wished for {}", wish.title));
            self.wish_for(wish);
        }
    }

    /// The form a wish by name is written in, while it's open.
    fn wishlist_by_name_ui(&mut self, ui: &mut egui::Ui) {
        let Some((title, artist)) = &mut self.wishlist.by_name else { return };
        let mut done = None;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("Wish for something by name");
            ui.weak("An album or a song named so (and by the artist, if you give one) counts as got. Leave the title empty for anything by the artist.");
            egui::Grid::new("wish_by_name").num_columns(2).show(ui, |ui| {
                ui.label("Title");
                ui.text_edit_singleline(title);
                ui.end_row();
                ui.label("Artist");
                ui.text_edit_singleline(artist);
                ui.end_row();
            });
            ui.horizontal(|ui| {
                let (title, artist) = (title.trim(), artist.trim());
                if ui.add_enabled(!title.is_empty() || !artist.is_empty(), egui::Button::new("Add")).clicked() {
                    let mut wish = Wish::new(Kind::Text, title);
                    wish.artist = (!artist.is_empty()).then(|| artist.to_string());
                    done = Some(Some(wish));
                }
                if ui.button("Cancel").clicked() {
                    done = Some(None);
                }
            });
        });
        if let Some(wish) = done {
            self.wishlist.by_name = None;
            if let Some(wish) = wish {
                self.wish_for(wish);
            }
        }
    }

    /// The wishes, under the part each is in.
    fn wishlist_list_ui(&mut self, ui: &mut egui::Ui) {
        let Some((_, progress)) = &self.wishlist.progress else { return };
        let wishes = &self.wishlist.list.wishes;
        let state_of = |id: &String| progress.get(id).map_or(State::Wanted, Progress::state);
        let got = wishes.keys().filter(|id| state_of(id) == State::Got).count();
        let mut act = None;
        ui.horizontal(|ui| {
            ui.weak(match wishes.len() {
                0 => "Nothing wished for yet. Search MusicBrainz above, or add something by name with +.".to_string(),
                1 => "1 wish".to_string(),
                n => format!("{n} wishes"),
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let clear = ui.add_enabled(got > 0, egui::Button::new(format!("Clear got it ({got})")));
                if clear.on_hover_text("Take the wishes that came true off the wishlist").clicked() {
                    act = Some(Act::Remove(wishes.keys().filter(|id| state_of(id) == State::Got).cloned().collect()));
                }
            });
        });
        if wishes.is_empty() {
            return self.wishlist_act(ui.ctx(), act);
        }

        let mut rows = Vec::new();
        for state in State::ALL {
            let mut ids: Vec<&String> = wishes.keys().filter(|id| state_of(id) == state).collect();
            if ids.is_empty() {
                continue;
            }
            // (Newest first.)
            ids.sort_by_key(|id| std::cmp::Reverse(wishes[*id].added));
            rows.push(Row::Heading(state, ids.len()));
            if !self.wishlist.folded.contains(&state) {
                rows.extend(ids.into_iter().cloned().map(Row::Wish));
            }
        }
        let touch = select_list::touch(ui.ctx());
        let art = &mut self.wishlist.art;
        let folded = &self.wishlist.folded;
        let ids = |rows: &[usize], all: &[Row]| -> Vec<String> {
            rows.iter().filter_map(|&i| if let Some(Row::Wish(id)) = all.get(i) { Some(id.clone()) } else { None }).collect()
        };
        let mut menu_act = None;
        let output = select_list::show(ui, "wishlist", &rows, &mut self.wishlist.selection, None, |ui, row, item| {
            let id = match item {
                Row::Heading(state, count) => {
                    let caret = icon(if folded.contains(state) { "caret-right" } else { "caret-down" });
                    ui.label(egui::RichText::new(format!("{caret} {} ({count})", state.name())).strong());
                    return;
                }
                Row::Wish(id) => id,
            };
            let wish = &wishes[id];
            let progress = progress.get(id).cloned().unwrap_or_default();
            crate::menu::context(&row.response, |ui| {
                if !progress.songs.is_empty() && ui.button("Play").clicked() {
                    menu_act = Some(Act::Play(id.clone()));
                }
                if ui.button(if wish.note.is_empty() { "Add a note..." } else { "Edit the note..." }).clicked() {
                    menu_act = Some(Act::Note(id.clone()));
                }
                if let Some(url) = musicbrainz_url(wish)
                    && ui.button("Open at MusicBrainz").clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
                let chosen = ids(&row.rows(), &rows);
                let count = if chosen.len() > 1 { format!(" ({})", chosen.len()) } else { String::new() };
                if ui.button(format!("Take off the wishlist{count}")).clicked() {
                    menu_act = Some(Act::Remove(chosen));
                }
            });
            let side = ui.available_height();
            let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
            art.tile(ui, rect.shrink(1.0), &source(wish), &wish.title);
            let title = egui::RichText::new(&wish.title);
            let title = if progress.state() == State::Got { title.strong() } else { title };
            let scroll = select_list::scrolls(row, false);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let kind = wish.release_type.clone().unwrap_or_else(|| wish.kind.name().to_string());
                if touch {
                    let detail = [wish.artist.clone(), Some(kind), Some(progress.text())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                    select_list::two_lines(ui, std::iter::empty(), title, Some(detail), scroll);
                    return;
                }
                // (The title keeps half the row: the narrower it is, the
                // fewer columns.)
                let room = ui.available_width();
                select_list::cell(ui, 70.0, egui::RichText::new(progress.text()).weak(), false);
                if room > 420.0 {
                    select_list::cell(ui, 60.0, egui::RichText::new(kind).weak(), false);
                }
                if room > 520.0 {
                    select_list::cell(ui, 40.0, egui::RichText::new(wish.year.clone().unwrap_or_default()).weak(), false);
                }
                select_list::cell(ui, (room * 0.25).clamp(60.0, 200.0), wish.artist.clone().unwrap_or_default(), scroll);
                let mut hover = format!("Wished for {}", crate::library::ago(wish.added));
                if let Some(by) = &wish.added_by {
                    hover.push_str(&format!(" by {by}"));
                }
                if !wish.note.is_empty() {
                    hover.push_str(&format!("\n{}", wish.note));
                }
                let title = if wish.note.is_empty() { title } else { title.italics() };
                select_list::label(ui, title, Some(hover), scroll);
            });
        });
        if let Some(i) = output.activated {
            match &rows[i] {
                Row::Heading(state, _) => act = Some(Act::Toggle(*state)),
                Row::Wish(id) => act = Some(Act::Play(id.clone())),
            }
        }
        if output.delete {
            act = Some(Act::Remove(ids(self.wishlist.selection.rows(), &rows)));
        }
        self.wishlist_act(ui.ctx(), menu_act.or(act));
    }

    fn wishlist_act(&mut self, ctx: &egui::Context, act: Option<Act>) {
        match act {
            None => {}
            Some(Act::Toggle(state)) => {
                if !self.wishlist.folded.remove(&state) {
                    self.wishlist.folded.insert(state);
                }
                self.wishlist.selection.clear();
            }
            Some(Act::Play(id)) => {
                let songs = self.wishlist.progress.as_ref().and_then(|(_, p)| p.get(&id)).map(|p| p.songs.clone()).unwrap_or_default();
                if songs.is_empty() {
                    toast::show(ctx, "Nothing of it is here yet");
                } else {
                    self.play_tracks(songs.iter().map(|key| self.songs.path(key)).collect(), 0);
                }
            }
            Some(Act::Note(id)) => {
                let note = self.wishlist.list.wishes.get(&id).map(|w| w.note.clone()).unwrap_or_default();
                self.wishlist.note = Some((id, note));
            }
            Some(Act::Remove(ids)) if !ids.is_empty() => {
                self.wishlist.list.remove(&ids);
                self.wishlist.selection.clear();
                self.wishlist_changed();
            }
            Some(Act::Remove(_)) => {}
        }
    }

    /// The window a wish's note is written in, while it's open.
    fn wishlist_note_ui(&mut self, ctx: &egui::Context) {
        let Some((id, note)) = &mut self.wishlist.note else { return };
        let title = self.wishlist.list.wishes.get(id.as_str()).map_or_else(String::new, |w| w.title.clone());
        let mut done = None;
        egui::Modal::new(egui::Id::new("wish_note")).show(ctx, |ui| {
            ui.strong(format!("A note on {title}"));
            ui.add(egui::TextEdit::multiline(note).hint_text("Which edition, who told you of it...").desired_rows(3));
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
        });
        if let Some(save) = done {
            let (id, note) = self.wishlist.note.take().expect("open");
            if save {
                self.wishlist.list.set_note(&id, note.trim());
                self.wishlist_changed();
            }
        }
    }
}

/// Where a wish's art comes from: the release it's on.
/// What "Wish for the rest of its album" asks for: the wish, or (its
/// release's tracks not known here yet) the release to ask MusicBrainz
/// about first.
pub(super) enum AlbumAsk {
    Wish(Wish),
    Fetch(String),
}

/// Whether to offer "Wish for the rest of its album" for `songs` (keys),
/// and what to do when it's taken: for the first of them on a release at
/// MusicBrainz that isn't all here (when `kept` knows its tracks; else,
/// `online`, to ask) and isn't on `wishlist` yet.
pub(super) fn album_ask(store: &SongStore, kept: &Kept, wishlist: &Wishlist, online: bool, songs: &[String]) -> Option<AlbumAsk> {
    let release = songs.iter().find_map(|key| crate::wishlist::release_of(store, key))?;
    if wishlist.wishes.values().any(|w| w.release.as_deref() == Some(release)) {
        return None;
    }
    match kept.release(release) {
        Some(known) => {
            let wish = crate::wishlist::rest_of(store, &known)?;
            wishlist.find(&wish).is_none().then_some(AlbumAsk::Wish(wish))
        }
        None if online => Some(AlbumAsk::Fetch(release.to_string())),
        None => None,
    }
}

impl AlbumAsk {
    fn hover(&self) -> String {
        match self {
            AlbumAsk::Wish(wish) => format!("Wish for the rest of {}: it shows how much of it is here", wish.title),
            AlbumAsk::Fetch(_) => "Wish for the rest of its album: asks MusicBrainz for its tracks first".into(),
        }
    }
}

/// [`album_ask`], as a menu item.
pub(super) fn album_wish_menu(ui: &mut egui::Ui, store: &SongStore, kept: &Kept, wishlist: &Wishlist, online: bool, songs: &[String]) -> Option<AlbumAsk> {
    let ask = album_ask(store, kept, wishlist, online, songs)?;
    ui.button("Wish for the rest of its album").on_hover_text(ask.hover()).clicked().then_some(ask)
}

/// [`album_ask`], as a gift button.
pub(super) fn album_wish_button(ui: &mut egui::Ui, ask: Option<AlbumAsk>) -> Option<AlbumAsk> {
    let ask = ask?;
    ui.add(super::icon_button("gift")).on_hover_text(ask.hover()).clicked().then_some(ask)
}

pub(super) fn source(wish: &Wish) -> Source {
    let group = (wish.kind == Kind::Album).then(|| wish.musicbrainz.clone()).flatten();
    Source { release: wish.release.clone().map(|release| (release, group)), ..Source::default() }
}

fn musicbrainz_url(wish: &Wish) -> Option<String> {
    let id = wish.musicbrainz.as_deref()?;
    match wish.kind {
        Kind::Album => Some(format!("https://musicbrainz.org/release-group/{id}")),
        Kind::Song => Some(format!("https://musicbrainz.org/recording/{id}")),
        Kind::Text => None,
    }
}

/// A search result: its cover, what it is, and Add (or that it's wished
/// for already). Whether Add was clicked.
fn found_row_ui(ui: &mut egui::Ui, art: &mut AlbumArt, wish: &Wish, wished: bool) -> bool {
    let mut add = false;
    ui.horizontal(|ui| {
        let side = 2.0 * ui.spacing().interact_size.y;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
        art.tile(ui, rect, &source(wish), &wish.title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if wished {
                ui.weak(format!("{} On your wishlist", icon("check")));
            } else {
                add = ui.button("Add").clicked();
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.add(egui::Label::new(&wish.title).truncate());
                let mut detail: Vec<String> = [wish.artist.clone(), wish.year.clone(), wish.release_type.clone()].into_iter().flatten().collect();
                if wish.tracks > 0 {
                    detail.push(if wish.tracks == 1 { "1 track".to_string() } else { format!("{} tracks", wish.tracks) });
                }
                ui.add(egui::Label::new(egui::RichText::new(detail.join(" · ")).small().weak()).truncate());
            });
        });
    });
    add
}
