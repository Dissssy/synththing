//! The Wishlist tab (and the mobile layout's Wishlist page): music wished
//! for (`crate::wishlist`), found by a MusicBrainz search or put in by
//! name, in three parts: Wanted, Partly got and Got it, worked out from
//! the songs here as they're matched. A wish that comes true says so.
//! A watched artist's albums are looked up at MusicBrainz on each device
//! (one artist at a time, once), and listed on a page of their own in the
//! tab, where each can be wished for; an album's page picks its songs.
//! An artist or album found by a search opens on the same page, to wish
//! for it or for some of it.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::mpsc;

use eframe::egui;

use super::{icon, toast, App};
use crate::album_art::{AlbumArt, Source};
use crate::build_info::{self, Online};
use crate::lookup::Kept;
use crate::lookup::matching::{ArtistAlbum, Release};
use crate::lookup::musicbrainz::MusicBrainz;
use crate::search_box::{self, SearchBox};
use crate::select_list::{self, Selection};
use crate::wishlist_search::Filter;

/// The box the wishlist is filtered in.
const FILTER: search_box::Kind =
    search_box::Kind { id: "wishlist_filter", hint: "filter: kind:album got:no...", help: crate::wishlist_search::HELP, keys: false, save: false };
use crate::song_store::SongStore;
use crate::wishlist::{Kind, Progress, State, Wish, Wishlist};

/// What a MusicBrainz search found.
struct Found {
    artists: Vec<Wish>,
    albums: Vec<Wish>,
    songs: Vec<Wish>,
}

/// What was asked of MusicBrainz to wish for an album: a release, to wish
/// for the rest of it, or the wish itself, its tracks filled in; or a
/// wished album's release, to pick its songs.
enum Fetched {
    Release(Release),
    Album(Wish, Release),
    Tracks(Release),
    /// An album of an artist's (by its group) not wished for, to pick
    /// songs of: the wish it'd be.
    Preview(Wish, Release),
}

/// What the tab shows in place of the wishlist: an artist's albums, to
/// pick the ones wished for, or a wished album's songs, to skip some (by
/// the wish's ID); or either of an artist or album found at MusicBrainz
/// and not wished for, to wish for it (or for some of it).
#[derive(Clone)]
enum Page {
    Artist(String),
    Album(String),
    Found(Wish),
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
    /// Albums asked of MusicBrainz, to wish for them: each answer, by
    /// what was asked for (a release or a release group's ID), and those
    /// still to come.
    fetched: (mpsc::Sender<(String, Result<Fetched, String>)>, mpsc::Receiver<(String, Result<Fetched, String>)>),
    adding: HashSet<String>,
    /// A watched artist's albums being asked for: who, and the answer.
    discography: Option<(String, mpsc::Receiver<Result<Vec<ArtistAlbum>, String>>)>,
    /// The artists (and wished albums' releases) asked about this session
    /// (not asked again).
    looked_up: HashSet<String>,
    page: Option<Page>,
    page_selection: Selection,
    /// The albums (by group) opened on an artist's page, to pick songs of.
    expanded: HashSet<String>,
    /// The wishes the albums opened there that aren't wished for would be,
    /// by group, once MusicBrainz said what they are.
    previews: BTreeMap<String, Wish>,
    selection: Selection,
    found_selection: Selection,
    filter_box: SearchBox,
    /// The filter, and the query it's from.
    filter: Option<(String, Filter)>,
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
            fetched: mpsc::channel(),
            adding: HashSet::new(),
            discography: None,
            looked_up: HashSet::new(),
            page: None,
            page_selection: Selection::default(),
            expanded: HashSet::new(),
            previews: BTreeMap::new(),
            selection: Selection::default(),
            found_selection: Selection::default(),
            filter_box: SearchBox::default(),
            filter: None,
            art: AlbumArt::default(),
        }
    }
}

/// A row of the list: a part's heading, or a wish (by ID; one of an
/// artist's albums, under them).
enum Row {
    Heading(State, usize),
    Wish(String, bool),
}

/// What's asked of the list's rows.
enum Act {
    Toggle(State),
    Play(String),
    Note(String),
    Open(Page),
    Remove(Vec<String>),
}

impl App {
    /// Once a frame: work out what of each wish is here when the wishlist
    /// or the songs changed, and say which wishes just came true.
    pub(super) fn wishlist_tick(&mut self, ctx: &egui::Context) {
        self.take_fetched_album(ctx);
        self.look_up_artists(ctx);
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

    /// What of each wish is here, as last worked out for the wishlist's
    /// and the songs' `revisions` (none if they've changed since).
    pub(super) fn wishlist_progress(&self, revisions: (u64, u64)) -> Option<&BTreeMap<String, Progress>> {
        self.wishlist.progress.as_ref().filter(|(seen, _)| *seen == revisions).map(|(_, progress)| progress)
    }

    /// Wish for `wish`, and send it to the other devices.
    pub(super) fn wish_for(&mut self, wish: Wish) {
        self.wishlist.list.add(wish);
        self.wishlist_changed();
    }

    /// Wish for more by an artist from somewhere else in the app, and say
    /// so.
    pub(super) fn wish_for_artist(&mut self, ctx: &egui::Context, wish: Wish) {
        let name = if wish.kind == Kind::Artist { wish.title.clone() } else { wish.artist.clone().unwrap_or_default() };
        toast::show(ctx, format!("Wished for more by {name}"));
        self.wish_for(wish);
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
            AlbumAsk::Fetch(_) | AlbumAsk::Group(_) => {
                let asked = match &ask {
                    AlbumAsk::Fetch(release) => release.clone(),
                    AlbumAsk::Group(wish) => wish.musicbrainz.clone().unwrap_or_default(),
                    AlbumAsk::Wish(_) => unreachable!("wished above"),
                };
                if !self.wishlist.adding.insert(asked.clone()) {
                    return;
                }
                let send = self.wishlist.fetched.0.clone();
                let repaint = ctx.clone();
                let id = asked.clone();
                let started = std::thread::Builder::new().name("wishlist album".into()).spawn(move || {
                    let mut musicbrainz = MusicBrainz::new();
                    let fetched = match ask {
                        AlbumAsk::Fetch(release) => musicbrainz.fresh_release(&release).map(Fetched::Release),
                        AlbumAsk::Group(wish) => musicbrainz.album(wish.musicbrainz.as_deref().unwrap_or_default()).map(|(found, release)| {
                            // (As the artist's albums name it, and under them.)
                            Fetched::Album(found_as(wish, found), release)
                        }),
                        AlbumAsk::Wish(_) => unreachable!("wished above"),
                    };
                    // (The whole chain to the log; what failed to the toast.)
                    let _ = send.send((
                        id,
                        fetched.map_err(|e| {
                            log::warn!("asking MusicBrainz for an album to wish for: {e:#}");
                            e.to_string()
                        }),
                    ));
                    repaint.request_repaint();
                });
                if let Err(e) = started {
                    self.wishlist.adding.remove(&asked);
                    toast::show(ctx, format!("Couldn't ask MusicBrainz: {e}"));
                }
            }
        }
    }

    /// The albums asked about for the wishlist that MusicBrainz answered.
    fn take_fetched_album(&mut self, ctx: &egui::Context) {
        let answers: Vec<_> = self.wishlist.fetched.1.try_iter().collect();
        for (asked, answer) in answers {
            self.wishlist.adding.remove(&asked);
            self.take_album(ctx, answer);
        }
    }

    fn take_album(&mut self, ctx: &egui::Context, answer: Result<Fetched, String>) {
        match answer {
            Ok(Fetched::Album(wish, release)) => {
                self.lookup.kept.keep_release(release);
                self.wish_for_album(ctx, AlbumAsk::Wish(wish));
            }
            Ok(Fetched::Preview(wish, release)) => {
                self.lookup.kept.keep_release(release);
                if let Some(group) = wish.musicbrainz.clone() {
                    self.wishlist.previews.insert(group, wish);
                }
            }
            Ok(Fetched::Tracks(release)) => {
                self.lookup.kept.keep_release(release);
                self.wishlist.progress = None;
            }
            Ok(Fetched::Release(release)) => {
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

    /// Ask MusicBrainz for the albums of the next watched artist whose
    /// aren't kept here (once a session each), and take what it answers.
    fn look_up_artists(&mut self, ctx: &egui::Context) {
        if let Some((id, receive)) = &self.wishlist.discography {
            let Ok(answer) = receive.try_recv() else { return };
            let id = id.clone();
            self.wishlist.discography = None;
            match answer {
                Ok(albums) => {
                    self.lookup.kept.keep_discography(&id, albums);
                    self.wishlist.progress = None;
                    self.groupings.tracks_changed();
                }
                Err(e) => log::warn!("couldn't look up the albums of {id}: {e}"),
            }
        }
        if !(self.config.online_lookup && build_info::online(Online::Lookup)) {
            return;
        }
        let kept = &self.lookup.kept;
        let looked_up = &self.wishlist.looked_up;
        // (The one found at MusicBrainz being looked at first.)
        let found = match &self.wishlist.page {
            Some(Page::Found(wish)) => Some(wish),
            _ => None,
        };
        let next = found
            .into_iter()
            .chain(self.wishlist.list.wishes.values())
            .filter(|w| w.kind == Kind::Artist)
            .filter_map(|w| w.musicbrainz.clone())
            .find(|id| !looked_up.contains(id) && kept.discography(id).is_none());
        let Some(id) = next else { return };
        self.wishlist.looked_up.insert(id.clone());
        let (send, receive) = mpsc::channel();
        let repaint = ctx.clone();
        let artist = id.clone();
        let started = std::thread::Builder::new().name("wishlist artist".into()).spawn(move || {
            let _ = send.send(MusicBrainz::new().discography(&artist).map_err(|e| format!("{e:#}")));
            repaint.request_repaint();
        });
        match started {
            Ok(_) => self.wishlist.discography = Some((id, receive)),
            Err(e) => log::warn!("couldn't look up an artist's albums: {e}"),
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
            let found = (|| {
                Ok::<_, anyhow::Error>(Found {
                    artists: musicbrainz.find_artists(&words)?,
                    albums: musicbrainz.find_albums(&words)?,
                    songs: musicbrainz.find_songs(&words)?,
                })
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = send.send(found);
            ctx.request_repaint();
        });
        self.wishlist.found_selection.clear();
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

        // (One found, wished for now: its page as a wish.)
        if let Some(Page::Found(wish)) = &self.wishlist.page
            && let Some(id) = self.wishlist.list.find(wish)
        {
            let id = id.to_string();
            self.wishlist.page = Some(if wish.kind == Kind::Artist { Page::Artist(id) } else { Page::Album(id) });
        }
        if let Some(page) = self.wishlist.page.clone() {
            let wish = match &page {
                Page::Artist(id) | Page::Album(id) => self.wishlist.list.wishes.get(id).cloned(),
                Page::Found(wish) => Some(wish.clone()),
            };
            let id = match &page {
                Page::Artist(id) | Page::Album(id) => Some(id.as_str()),
                Page::Found(_) => None,
            };
            match wish {
                None => self.wishlist.page = None,
                Some(wish) if wish.kind == Kind::Artist => self.wishlist_artist_ui(ui, id, wish, online),
                Some(wish) => self.wishlist_album_ui(ui, id, wish, online),
            }
        } else {
            self.wishlist_search_ui(ui, online);
            self.wishlist_by_name_ui(ui);
            ui.separator();
            self.wishlist_list_ui(ui);
        }
        self.wishlist_note_ui(ui.ctx());
    }

    /// The search box, and what it found.
    fn wishlist_search_ui(&mut self, ui: &mut egui::Ui, online: bool) {
        if !self.config.online_lookup {
            ui.weak("Searching MusicBrainz needs Online album art and details on (Preferences > Library). You can still wish for music by name.");
        } else {
            // (Off in a branch build: it says so, with a button.)
            self.online_ui(ui, Online::Lookup);
        }
        let mut search = false;
        ui.horizontal(|ui| {
            let room = (ui.available_width() - 120.0).max(80.0);
            let edit = egui::TextEdit::singleline(&mut self.wishlist.query).hint_text("Search MusicBrainz for artists, albums and songs").desired_width(room);
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
        let mut open = None;
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
                let selection = &mut self.wishlist.found_selection;
                // (Artists, then albums, then songs.)
                let rows: Vec<&Wish> = found.artists.iter().chain(&found.albums).chain(&found.songs).collect();
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong("Found at MusicBrainz");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.small_button(icon("x")).on_hover_text("Put the results away").clicked();
                        });
                    });
                    if rows.is_empty() {
                        ui.weak("Nothing found.");
                        return;
                    }
                    let height = 2.0 * ui.spacing().interact_size.y;
                    let pitch = height + ui.spacing().item_spacing.y;
                    let room = egui::vec2(ui.available_width(), (rows.len() as f32 * pitch).min(ui.ctx().content_rect().height() * 0.4));
                    ui.allocate_ui(room, |ui| {
                        let output = select_list::show_tall(ui, "wishlist_found", &rows, selection, None, height, |ui, row, wish| {
                            if found_row_ui(ui, row, art, wish, list.find(wish).is_some()) {
                                add = Some((*wish).clone());
                            }
                        });
                        if let Some(i) = output.activated.filter(|&i| opens(rows[i])) {
                            open = Some(rows[i].clone());
                        }
                    });
                });
            }
        }
        if close {
            self.wishlist.search = None;
            self.wishlist.found_selection.clear();
        }
        if let Some(wish) = add {
            toast::show(ui.ctx(), format!("Wished for {}", wish.title));
            self.wish_for(wish);
        }
        if let Some(wish) = open {
            self.wishlist_act(ui.ctx(), Some(Act::Open(Page::Found(wish))));
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
        // let got = wishes.keys().filter(|id| state_of(id) == State::Got).count();
        let mut act = None;
        // (The filter, parsed again when the query changes.)
        let query = &self.wishlist.filter_box.query;
        if self.wishlist.filter.as_ref().is_none_or(|(q, _)| q != query) {
            self.wishlist.filter = Some((query.clone(), Filter::parse(query)));
        }
        let filter = &self.wishlist.filter.as_ref().expect("parsed").1;
        let now = crate::settings_sync::now();
        let kept: HashSet<&String> = wishes.iter().filter(|(id, wish)| filter.keeps(wish, state_of(id), now)).map(|(id, _)| id).collect();
        ui.horizontal(|ui| {
            ui.weak(match (wishes.len(), kept.len()) {
                (0, _) => "Nothing wished for yet. Search MusicBrainz above, or add something by name with +.".to_string(),
                (all, kept) if kept < all => format!("{kept} of {all}"),
                (1, _) => "1 wish".to_string(),
                (n, _) => format!("{n} wishes"),
            });
            if wishes.is_empty() {
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // let clear = ui.add_enabled(got > 0, egui::Button::new(format!("Clear got it ({got})")));
                // if clear.on_hover_text("Take the wishes that came true off the wishlist").clicked() {
                //     act = Some(Act::Remove(wishes.keys().filter(|id| state_of(id) == State::Got).cloned().collect()));
                // }
                let artists: Vec<String> = wishes.values().filter_map(|w| if w.kind == Kind::Artist { Some(&w.title) } else { w.artist.as_ref() }).cloned().collect::<BTreeSet<_>>().into_iter().collect();
                let programs: Vec<String> = wishes.values().filter_map(|w| w.added_by.clone()).collect::<BTreeSet<_>>().into_iter().collect();
                let mut complete = |query: &str, cursor, mode| crate::wishlist_search::complete(query, cursor, mode, &artists, &programs);
                // (How many it keeps is said before it.)
                self.wishlist.filter_box.ui(ui, &FILTER, &filter.problems, None, &mut complete);
            });
        });
        if wishes.is_empty() {
            return self.wishlist_act(ui.ctx(), act);
        }

        // (An artist's albums go under them, not in a part of their own.)
        let on_top = |wish: &Wish| wish.under.as_ref().is_none_or(|artist| !wishes.contains_key(artist));
        // (An artist's shown for any of their albums kept.)
        let shown = |id: &String| kept.contains(id) || self.wishlist.list.under(id).any(|(album, _)| kept.contains(album));
        let mut rows = Vec::new();
        for state in State::ALL {
            let mut ids: Vec<&String> = wishes.iter().filter(|(id, w)| on_top(w) && state_of(id) == state && shown(id)).map(|(id, _)| id).collect();
            if ids.is_empty() {
                continue;
            }
            // (Newest first.)
            ids.sort_by_key(|id| std::cmp::Reverse(wishes[*id].added));
            rows.push(Row::Heading(state, ids.len()));
            if self.wishlist.folded.contains(&state) {
                continue;
            }
            for id in ids {
                rows.push(Row::Wish(id.clone(), false));
                let mut albums: Vec<(&String, &Wish)> = self.wishlist.list.under(id).filter(|(album, _)| kept.contains(album)).collect();
                albums.sort_by(|(_, a), (_, b)| (&a.year, &a.title).cmp(&(&b.year, &b.title)));
                rows.extend(albums.into_iter().map(|(album, _)| Row::Wish(album.clone(), true)));
            }
        }
        let touch = select_list::touch(ui.ctx());
        let art = &mut self.wishlist.art;
        let folded = &self.wishlist.folded;
        let ids = |rows: &[usize], all: &[Row]| -> Vec<String> {
            rows.iter().filter_map(|&i| if let Some(Row::Wish(id, _)) = all.get(i) { Some(id.clone()) } else { None }).collect()
        };
        let mut menu_act = None;
        let output = select_list::show(ui, "wishlist", &rows, &mut self.wishlist.selection, None, |ui, row, item| {
            let id = match item {
                Row::Heading(state, count) => {
                    let caret = icon(if folded.contains(state) { "caret-right" } else { "caret-down" });
                    ui.label(egui::RichText::new(format!("{caret} {} ({count})", state.name())).strong());
                    return;
                }
                Row::Wish(id, under) => {
                    if *under {
                        ui.add_space(ui.available_height());
                    }
                    id
                }
            };
            let wish = &wishes[id];
            let progress = progress.get(id).cloned().unwrap_or_default();
            crate::menu::context(&row.response, |ui| {
                if !progress.songs.is_empty() && ui.button("Play").clicked() {
                    menu_act = Some(Act::Play(id.clone()));
                }
                if wish.kind == Kind::Artist && wish.musicbrainz.is_some() {
                    if ui.button("Pick their albums").clicked() {
                        menu_act = Some(Act::Open(Page::Artist(id.clone())));
                    }
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
                select_list::cell(ui, 70.0, egui::RichText::new(progress.text()).weak(), false, false);
                if room > 420.0 {
                    select_list::cell(ui, 60.0, egui::RichText::new(kind).weak(), false, false);
                }
                if room > 520.0 {
                    select_list::cell(ui, 40.0, egui::RichText::new(wish.year.clone().unwrap_or_default()).weak(), false, true);
                }
                select_list::cell(ui, (room * 0.25).clamp(60.0, 200.0), wish.artist.clone().unwrap_or_default(), scroll, false);
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
                Row::Wish(id, _) if wishes[id].kind == Kind::Artist && wishes[id].musicbrainz.is_some() => act = Some(Act::Open(Page::Artist(id.clone()))),
                Row::Wish(id, _) if wishes[id].kind == Kind::Album && wishes[id].release.is_some() => act = Some(Act::Open(Page::Album(id.clone()))),
                Row::Wish(id, _) => act = Some(Act::Play(id.clone())),
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
            Some(Act::Open(page)) => {
                self.wishlist.page = Some(page);
                self.wishlist.page_selection.clear();
            }
            Some(Act::Remove(ids)) if !ids.is_empty() => {
                self.wishlist.list.remove(&ids);
                self.wishlist.selection.clear();
                self.wishlist_changed();
            }
            Some(Act::Remove(_)) => {}
        }
    }

    /// The kinds of an artist's albums shown on their page.
    pub(super) fn wishlist_kinds(&self) -> Vec<&'static str> {
        let chosen = &self.config.wishlist_kinds;
        crate::wishlist::KINDS.into_iter().filter(|k| if chosen.is_empty() { crate::wishlist::SHOWN.contains(k) } else { chosen.iter().any(|c| c == k) }).collect()
    }

    /// An artist's albums, in the tab in place of the wishlist (those of
    /// the kinds shown): each ticked to wish for it, under them, with
    /// what's here of it. An artist not wished for (`id` none, found at
    /// MusicBrainz) is wished for with the first album ticked, or by
    /// themselves with the button by their name.
    fn wishlist_artist_ui(&mut self, ui: &mut egui::Ui, id: Option<&str>, artist: Wish, online: bool) {
        let mut back = false;
        let mut wish_them = false;
        let mut kinds = self.wishlist_kinds();
        let mut shown_changed = false;
        ui.horizontal(|ui| {
            back = ui.add(super::icon_button("arrow-left")).on_hover_text("Back to the wishlist").clicked();
            ui.add(egui::Label::new(egui::RichText::new(&artist.title).heading()).truncate());
            if id.is_none() {
                wish_them = ui.button(format!("{} Wish for them", icon("gift"))).on_hover_text("Watch over their albums: tick the ones you want").clicked();
            }
        });
        if let Some(about) = artist.artist.as_ref().filter(|_| id.is_none()) {
            ui.weak(about);
        }
        ui.horizontal_wrapped(|ui| {
            for kind in crate::wishlist::KINDS {
                let shown = kinds.contains(&kind);
                // (The last one shown stays: there's always something.)
                let last = shown && kinds.len() == 1;
                let chip = super::toggle_chip(ui, shown, kind, kind_color(kind));
                let hover = if last { "Show another kind to hide this one" } else if shown { "Shown: click to hide them" } else { "Hidden: click to show them" };
                if chip.on_hover_text(hover).clicked() && !last {
                    kinds.retain(|k| *k != kind);
                    if !shown {
                        kinds.push(kind);
                    }
                    shown_changed = true;
                }
            }
        });
        ui.weak("Tick the albums you want: they go on your wishlist under the artist.");
        ui.separator();
        let mut act = None;
        // (What to ask MusicBrainz, for an opened album's songs: the wish
        // it'd be, or the release of the one it is.)
        let mut fetch: Option<Result<Wish, String>> = None;
        let albums = artist.musicbrainz.as_deref().and_then(|a| self.lookup.kept.discography(a));
        // (Their albums wished for by themselves: theirs now, ticked here.)
        if let (Some(albums), Some(id)) = (&albums, id) {
            let groups: HashSet<&str> = albums.iter().map(|a| a.id.as_str()).collect();
            let alone: Vec<String> = self
                .wishlist
                .list
                .wishes
                .iter()
                .filter(|(_, w)| w.kind == Kind::Album && w.under.is_none() && w.musicbrainz.as_deref().is_some_and(|g| groups.contains(g)))
                .map(|(id, _)| id.clone())
                .collect();
            if self.wishlist.list.adopt(id, &alone.iter().map(String::as_str).collect::<Vec<_>>()) {
                self.wishlist_changed();
            }
        }
        match &albums {
            None => {
                ui.weak(if online { "Looking up their albums at MusicBrainz..." } else { "Their albums aren't looked up on this device yet: that needs Online album art and details on." });
            }
            Some(albums) => {
                let here = crate::wishlist::albums_here(&self.songs);
                let songs_here = crate::wishlist::songs_here(&self.songs);
                let picked: BTreeMap<&str, &String> =
                    self.wishlist.list.under(id.unwrap_or_default()).filter(|_| id.is_some()).filter_map(|(album, w)| Some((w.musicbrainz.as_deref()?, album))).collect();
                let wishes = &self.wishlist.list.wishes;
                let progress = self.wishlist.progress.as_ref().map(|(_, p)| p);
                let adding = &self.wishlist.adding;
                let kept = &self.lookup.kept;
                // (An opened album's songs: its wish's release's, or the one
                // it would be's, once known.)
                let mut rows: Vec<ArtistRow> = Vec::new();
                for album in albums.iter().filter(|a| kinds.contains(&a.kind)) {
                    rows.push(ArtistRow::Album(album));
                    if !self.wishlist.expanded.contains(&album.id) {
                        continue;
                    }
                    let wish = picked.get(album.id.as_str()).and_then(|w| wishes.get(*w)).or_else(|| self.wishlist.previews.get(&album.id));
                    match wish.and_then(|w| kept.release(w.release.as_deref()?)) {
                        Some(release) => rows.extend(release.tracks.iter().map(|t| ArtistRow::Track(album, t.clone()))),
                        None if adding.contains(&album.id) || wish.and_then(|w| w.release.as_ref()).is_some_and(|r| adding.contains(r)) => {
                            rows.push(ArtistRow::Note("Asking MusicBrainz for its songs...".into()))
                        }
                        None if online => {
                            let asked = self.wishlist.looked_up.contains(&album.id) || wish.and_then(|w| w.release.as_ref()).is_some_and(|r| self.wishlist.looked_up.contains(r));
                            let note = if asked { "Couldn't get its songs from MusicBrainz: try again later." } else { "Asking MusicBrainz for its songs..." };
                            rows.push(ArtistRow::Note(note.into()));
                            match wish {
                                Some(w) if w.release.is_some() => fetch = Some(Err(w.release.clone().unwrap_or_default())),
                                _ => fetch = Some(Ok(crate::wishlist::album_wish(album, &artist.title, id.unwrap_or_default()))),
                            }
                        }
                        None => rows.push(ArtistRow::Note("Its songs aren't known on this device yet.".into())),
                    }
                }
                let touch = select_list::touch(ui.ctx());
                let expanded = &self.wishlist.expanded;
                let previews = &self.wishlist.previews;
                let output = select_list::show(ui, "wishlist_albums", &rows, &mut self.wishlist.page_selection, None, |ui, row, item| {
                    let scroll = select_list::scrolls(row, false);
                    match item {
                        ArtistRow::Note(note) => {
                            ui.add_space(2.0 * ui.available_height());
                            ui.weak(note);
                        }
                        ArtistRow::Album(album) => {
                            let wished = picked.get(album.id.as_str()).copied();
                            let skipped = wished.and_then(|w| wishes.get(w)).is_some_and(|w| !w.skipped.is_empty());
                            let state = match wished {
                                Some(w) => progress.and_then(|p| p.get(w)).map_or_else(String::new, |p| p.text()),
                                None if adding.contains(&album.id) && !expanded.contains(&album.id) => "Adding...".to_string(),
                                None if here.contains(album.id.as_str()) => "Some here".to_string(),
                                None => String::new(),
                            };
                            crate::menu::context(&row.response, |ui| {
                                if ui.button("Open at MusicBrainz").clicked() {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab(format!("https://musicbrainz.org/release-group/{}", album.id)));
                                }
                            });
                            let open = expanded.contains(&album.id);
                            let caret = super::icon(if open { "caret-down" } else { "caret-right" });
                            if ui.add(egui::Button::new(caret).frame(false)).on_hover_text(if open { "Hide its songs" } else { "Pick its songs" }).clicked() {
                                act = Some(ArtistAct::Expand(album.id.clone()));
                            }
                            let busy = adding.contains(&album.id) && wished.is_none() && !expanded.contains(&album.id);
                            let mut ticked = wished.is_some() || busy;
                            let checkbox = egui::Checkbox::without_text(&mut ticked).indeterminate(skipped);
                            if ui.add_enabled(!busy, checkbox).changed() {
                                act = Some(ArtistAct::Album((*album).clone(), wished.cloned()));
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                select_list::cell(ui, 70.0, egui::RichText::new(state).weak(), false, false);
                                if !touch {
                                    select_list::cell(ui, 90.0, egui::RichText::new(album.kind).weak(), false, false);
                                }
                                select_list::cell(ui, 40.0, egui::RichText::new(album.year.clone().unwrap_or_default()).weak(), false, true);
                                select_list::label(ui, &album.title, None, scroll);
                            });
                        }
                        ArtistRow::Track(album, track) => {
                            ui.add_space(2.0 * ui.available_height());
                            let wished = picked.get(album.id.as_str()).and_then(|w| Some((*w, wishes.get(*w)?)));
                            let recording = track.recording.as_deref();
                            let mut ticked = match wished {
                                Some((_, w)) => recording.is_none_or(|r| !w.skipped.iter().any(|s| s == r)),
                                None => false,
                            };
                            if ui.add_enabled(recording.is_some(), egui::Checkbox::without_text(&mut ticked)).changed()
                                && let Some(recording) = recording
                            {
                                act = Some(match wished {
                                    Some((w, _)) => ArtistAct::Song(w.clone(), recording.to_string(), !ticked),
                                    None => ArtistAct::OnlySong(previews.get(&album.id).cloned(), recording.to_string()),
                                });
                            }
                            let is_here = recording.is_some_and(|r| songs_here.contains(r));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                here_cell(ui, is_here);
                                select_list::cell(ui, 50.0, egui::RichText::new(track.length.map_or_else(String::new, crate::song_info::format_length)).weak(), false, true);
                                let title = egui::RichText::new(format!("{}. {}", track.position, track.title));
                                let title = if ticked && !is_here { title } else { title.color(ui.visuals().weak_text_color()) };
                                select_list::label(ui, title, is_here.then(|| HERE.to_string()), scroll);
                            });
                        }
                    }
                });
                if let Some(i) = output.activated
                    && let ArtistRow::Album(album) = &rows[i]
                {
                    act = Some(ArtistAct::Expand(album.id.clone()));
                }
            }
        }
        if back {
            self.wishlist.page = None;
        }
        if wish_them {
            self.wish_for_artist(ui.ctx(), artist.clone());
        }
        if shown_changed {
            self.config.wishlist_kinds = if kinds.iter().copied().eq(crate::wishlist::SHOWN) {
                Vec::new()
            } else {
                crate::wishlist::KINDS.into_iter().filter(|k| kinds.contains(k)).map(str::to_string).collect()
            };
            if let Err(e) = self.config.save() {
                log::warn!("saving the config: {e}");
            }
        }
        match fetch {
            Some(Ok(wish)) => self.fetch_preview(ui.ctx(), wish),
            Some(Err(release)) => self.fetch_tracks(ui.ctx(), release),
            None => {}
        }
        // (Not wished for yet: they are, with what's picked under them.)
        let wishes = matches!(act, Some(ArtistAct::Album(_, None) | ArtistAct::OnlySong(Some(_), _)));
        let id = match id {
            Some(id) => id.to_string(),
            None if wishes => {
                let id = self.wishlist.list.add(artist.clone());
                self.wishlist_changed();
                id
            }
            None => String::new(),
        };
        match act {
            None => {}
            Some(ArtistAct::Expand(group)) => {
                if !self.wishlist.expanded.remove(&group) {
                    self.wishlist.expanded.insert(group);
                }
            }
            Some(ArtistAct::Album(_, Some(wished))) => self.unwish(&wished),
            Some(ArtistAct::Album(album, None)) => {
                let wish = crate::wishlist::album_wish(&album, &artist.title, &id);
                // (Its songs known already: wished for as it is.)
                match self.wishlist.previews.remove(&album.id) {
                    Some(preview) => self.wish_for_album(ui.ctx(), AlbumAsk::Wish(Wish { under: Some(id), ..preview })),
                    None => self.wish_for_album(ui.ctx(), if online { AlbumAsk::Group(wish) } else { AlbumAsk::Wish(wish) }),
                }
            }
            Some(ArtistAct::Song(wished, recording, skipped)) => {
                // (Its last song skipped: the album isn't wished for.)
                let tracks = self.wishlist.list.wishes.get(&wished).and_then(|w| self.lookup.kept.release(w.release.as_deref()?)).map(|r| r.tracks.len());
                let skipped_all = self.wishlist.list.wishes.get(&wished).is_some_and(|w| skipped && Some(w.skipped.len() + 1) >= tracks);
                if skipped_all {
                    self.unwish(&wished);
                } else {
                    self.wishlist.list.set_skipped(&wished, &recording, skipped);
                    self.wishlist_changed();
                }
            }
            Some(ArtistAct::OnlySong(Some(preview), recording)) => {
                // (Wished for with only that song.)
                let tracks = preview.release.as_deref().and_then(|r| self.lookup.kept.release(r));
                let skipped = tracks.iter().flat_map(|r| &r.tracks).filter_map(|t| t.recording.clone()).filter(|r| *r != recording).collect();
                if let Some(group) = &preview.musicbrainz {
                    self.wishlist.previews.remove(group);
                }
                self.wish_for_album(ui.ctx(), AlbumAsk::Wish(Wish { skipped, under: Some(id), ..preview }));
            }
            Some(ArtistAct::OnlySong(None, _)) => {}
        }
    }

    /// A wished album's songs, in the tab in place of the wishlist: each
    /// ticked unless it's skipped, with whether it's here. Its release's
    /// tracks are asked of MusicBrainz if they aren't kept here.
    /// An album not wished for (`id` none, found at MusicBrainz) is wished
    /// for with the songs ticked (the rest skipped), or all of it with the
    /// button by its name.
    fn wishlist_album_ui(&mut self, ui: &mut egui::Ui, id: Option<&str>, wish: Wish, online: bool) {
        let mut back = false;
        let mut wish_all = false;
        ui.horizontal(|ui| {
            back = ui.add(super::icon_button("arrow-left")).on_hover_text("Back").clicked();
            ui.add(egui::Label::new(egui::RichText::new(&wish.title).heading()).truncate());
            if id.is_none() {
                wish_all = ui.button(format!("{} Wish for all of it", icon("gift"))).clicked();
            }
        });
        if let Some(artist) = &wish.artist {
            ui.weak(artist);
        }
        ui.weak(if id.is_some() { "Untick the songs you don't want: the album's got once the rest are here." } else { "Tick the songs you want, or wish for all of it." });
        ui.separator();
        let release = wish.release.as_deref().and_then(|r| self.lookup.kept.release(r));
        let mut skip = None;
        match (&release, &wish.release) {
            (Some(release), _) => {
                let here = crate::wishlist::songs_here(&self.songs);
                let rows: Vec<&crate::lookup::matching::Track> = release.tracks.iter().collect();
                let output = select_list::show(ui, "wishlist_tracks", &rows, &mut self.wishlist.page_selection, None, |ui, row, track| {
                    let recording = track.recording.as_deref();
                    let mut ticked = id.is_some() && recording.is_none_or(|r| !wish.skipped.iter().any(|s| s == r));
                    if ui.add_enabled(recording.is_some(), egui::Checkbox::without_text(&mut ticked)).changed()
                        && let Some(recording) = recording
                    {
                        skip = Some((recording.to_string(), !ticked));
                    }
                    let scroll = select_list::scrolls(row, false);
                    let is_here = recording.is_some_and(|r| here.contains(r));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        here_cell(ui, is_here);
                        select_list::cell(ui, 50.0, egui::RichText::new(track.length.map_or_else(String::new, crate::song_info::format_length)).weak(), false, true);
                        let title = egui::RichText::new(format!("{}. {}", track.position, track.title));
                        let title = if ticked && !is_here { title } else { title.color(ui.visuals().weak_text_color()) };
                        select_list::label(ui, title, is_here.then(|| HERE.to_string()), scroll);
                    });
                });
                if let Some(recording) = output.activated.and_then(|i| rows[i].recording.clone()) {
                    let skipped = id.is_none() || wish.skipped.contains(&recording);
                    skip = Some((recording, !skipped));
                }
            }
            (None, Some(release)) if online && !self.wishlist.looked_up.contains(release) || self.wishlist.adding.contains(release) => {
                ui.weak("Asking MusicBrainz for its songs...");
                self.fetch_tracks(ui.ctx(), release.clone());
            }
            (None, Some(_)) if online => {
                ui.weak("Couldn't get its songs from MusicBrainz: try again later.");
            }
            _ => {
                ui.weak("Its songs aren't known on this device yet: that needs Online album art and details on.");
            }
        }
        if back {
            // (To the artist it's under, if it's one of theirs.)
            self.wishlist.page = wish.under.clone().filter(|artist| self.wishlist.list.wishes.contains_key(artist)).map(Page::Artist);
            self.wishlist.page_selection.clear();
        }
        if wish_all {
            toast::show(ui.ctx(), format!("Wished for {}", wish.title));
            self.wish_for(wish.clone());
        }
        match (skip, id) {
            (Some((recording, skipped)), Some(id)) => {
                self.wishlist.list.set_skipped(id, &recording, skipped);
                self.wishlist_changed();
            }
            // (Not wished for yet: it is, with only that song.)
            (Some((recording, false)), None) => {
                let skipped = release.iter().flat_map(|r| &r.tracks).filter_map(|t| t.recording.clone()).filter(|r| *r != recording).collect();
                toast::show(ui.ctx(), format!("Wished for {}", wish.title));
                self.wish_for(Wish { skipped, ..wish });
            }
            _ => {}
        }
    }

    /// Ask MusicBrainz what the album `wish` (not wished for yet) would be,
    /// to pick songs of, once a session.
    fn fetch_preview(&mut self, ctx: &egui::Context, wish: Wish) {
        let Some(group) = wish.musicbrainz.clone() else { return };
        if !self.wishlist.looked_up.insert(group.clone()) {
            return;
        }
        self.wishlist.adding.insert(group.clone());
        let send = self.wishlist.fetched.0.clone();
        let repaint = ctx.clone();
        let asked = group.clone();
        let started = std::thread::Builder::new().name("wishlist preview".into()).spawn(move || {
            let fetched = MusicBrainz::new().album(&group).map(|(found, release)| Fetched::Preview(found_as(wish, found), release)).map_err(|e| {
                log::warn!("asking MusicBrainz for an album's songs: {e:#}");
                e.to_string()
            });
            let _ = send.send((group, fetched));
            repaint.request_repaint();
        });
        if let Err(e) = started {
            self.wishlist.adding.remove(&asked);
            log::warn!("couldn't ask MusicBrainz for an album's songs: {e}");
        }
    }

    /// Ask MusicBrainz for the tracks of `release`, a wished album's, once
    /// a session.
    fn fetch_tracks(&mut self, ctx: &egui::Context, release: String) {
        if !self.wishlist.looked_up.insert(release.clone()) {
            return;
        }
        self.wishlist.adding.insert(release.clone());
        let send = self.wishlist.fetched.0.clone();
        let repaint = ctx.clone();
        let asked = release.clone();
        let started = std::thread::Builder::new().name("wishlist tracks".into()).spawn(move || {
            let fetched = MusicBrainz::new().fresh_release(&release).map(Fetched::Tracks).map_err(|e| {
                log::warn!("asking MusicBrainz for a wished album's songs: {e:#}");
                e.to_string()
            });
            let _ = send.send((release, fetched));
            repaint.request_repaint();
        });
        if let Err(e) = started {
            self.wishlist.adding.remove(&asked);
            log::warn!("couldn't ask MusicBrainz for an album's songs: {e}");
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

/// What wishing for an album asks for: the wish; the release to ask
/// MusicBrainz about first (its tracks not known here yet), for the rest
/// of it; or a wish for an album whose editions MusicBrainz is to be
/// asked for, to fill in its tracks.
pub(super) enum AlbumAsk {
    Wish(Wish),
    Fetch(String),
    Group(Wish),
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
            AlbumAsk::Group(wish) => format!("Wish for {}: asks MusicBrainz for its tracks first", wish.title),
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

/// A wish for more by `artist` (whose songs are `songs`, keys), unless
/// it's on `wishlist` already: what "Wish for more by this artist"
/// wishes for.
pub(super) fn artist_ask(store: &SongStore, kept: &Kept, wishlist: &Wishlist, artist: &str, songs: &[String]) -> Option<Wish> {
    let wish = crate::wishlist::more_by(store, kept, songs, artist);
    wishlist.find(&wish).is_none().then_some(wish)
}

fn artist_hover(wish: &Wish) -> String {
    match wish.kind {
        Kind::Artist => format!("Wish for more by {}: their albums at MusicBrainz, and how many of them are here", wish.title),
        _ => format!("Wish for anything by {} (they aren't matched at MusicBrainz)", wish.artist.as_deref().unwrap_or_default()),
    }
}

/// [`artist_ask`], as a menu item.
pub(super) fn artist_wish_menu(ui: &mut egui::Ui, store: &SongStore, kept: &Kept, wishlist: &Wishlist, artist: &str, songs: &[String]) -> Option<Wish> {
    let wish = artist_ask(store, kept, wishlist, artist, songs)?;
    ui.button("Wish for more by this artist").on_hover_text(artist_hover(&wish)).clicked().then_some(wish)
}

/// [`artist_ask`], as a gift button.
pub(super) fn artist_wish_button(ui: &mut egui::Ui, wish: Option<Wish>) -> Option<Wish> {
    let wish = wish?;
    ui.add(super::icon_button("gift")).on_hover_text(artist_hover(&wish)).clicked().then_some(wish)
}

/// The wish MusicBrainz `found` for an album asked for as `asked`: named
/// as the artist's albums name it, and under them.
fn found_as(asked: Wish, found: Wish) -> Wish {
    Wish { title: asked.title, artist: asked.artist, release_type: asked.release_type, under: asked.under, skipped: asked.skipped, ..found }
}

/// What's asked of an artist's page: open or close an album; wish for it
/// (or not: the wish under the artist it is); skip a song of the album
/// wished for (its wish, the song, whether it's skipped); or wish for an
/// album not wished for with only that song (the wish it'd be).
enum ArtistAct {
    Expand(String),
    Album(ArtistAlbum, Option<String>),
    Song(String, String, bool),
    OnlySong(Option<Wish>, String),
}

/// A row of an artist's page: one of their albums, or (it opened) a song
/// of it, or what's being waited on for its songs.
enum ArtistRow<'a> {
    Album(&'a ArtistAlbum),
    Track(&'a ArtistAlbum, crate::lookup::matching::Track),
    Note(String),
}

/// The color of a kind of album's chip.
fn kind_color(kind: &str) -> egui::Color32 {
    match kind {
        "Album" => egui::Color32::from_rgb(0x4c, 0x8b, 0xf5),
        "EP" => egui::Color32::from_rgb(0x2e, 0xb8, 0x8a),
        "Single" => egui::Color32::from_rgb(0xf2, 0xa3, 0x3a),
        "Compilation" => egui::Color32::from_rgb(0x9b, 0x6b, 0xe8),
        "Live" => egui::Color32::from_rgb(0xe8, 0x5d, 0x75),
        _ => egui::Color32::from_rgb(0x8a, 0x93, 0xa6),
    }
}

/// Where a wish's art comes from: the release it's on.
pub(super) fn source(wish: &Wish) -> Source {
    let group = (wish.kind == Kind::Album).then(|| wish.musicbrainz.clone()).flatten();
    Source { release: wish.release.clone().map(|release| (release, group)), ..Source::default() }
}

fn musicbrainz_url(wish: &Wish) -> Option<String> {
    let id = wish.musicbrainz.as_deref()?;
    match wish.kind {
        Kind::Album => Some(format!("https://musicbrainz.org/release-group/{id}")),
        Kind::Song => Some(format!("https://musicbrainz.org/recording/{id}")),
        Kind::Artist => Some(format!("https://musicbrainz.org/artist/{id}")),
        Kind::Text => None,
    }
}

/// A search result: its cover, what it is, and Add (or that it's wished
/// for already). Whether Add was clicked.
const HERE: &str = "You have this song";

/// The cell saying a song's here.
fn here_cell(ui: &mut egui::Ui, here: bool) {
    let text = if here { format!("{} Here", icon("check")) } else { String::new() };
    select_list::cell(ui, 70.0, egui::RichText::new(text).weak(), false, false);
}

/// Whether a search result has a page: an artist's albums, or an album's
/// songs, to pick from.
fn opens(wish: &Wish) -> bool {
    match wish.kind {
        Kind::Artist => wish.musicbrainz.is_some(),
        Kind::Album => wish.release.is_some(),
        Kind::Song | Kind::Text => false,
    }
}

/// A search result's row: whether its Add was clicked.
fn found_row_ui(ui: &mut egui::Ui, row: &select_list::Row, art: &mut AlbumArt, wish: &Wish, wished: bool) -> bool {
    if opens(wish) {
        row.response.clone().on_hover_text(if wish.kind == Kind::Artist { "Click to see their albums" } else { "Click to see its songs" });
    }
    crate::menu::context(&row.response, |ui| {
        if let Some(url) = musicbrainz_url(wish)
            && ui.button("Open at MusicBrainz").clicked()
        {
            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
        }
    });
    let side = ui.available_height() - 4.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    art.tile(ui, rect, &source(wish), &wish.title);
    let mut add = false;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if wished {
            ui.weak(format!("{} On your wishlist", icon("check")));
        } else {
            add = ui.button("Add").clicked();
        }
        let mut detail: Vec<String> = vec![wish.release_type.clone().unwrap_or_else(|| wish.kind.name().to_string())];
        detail.extend([wish.artist.clone(), wish.year.clone()].into_iter().flatten());
        if wish.tracks > 0 {
            detail.push(if wish.tracks == 1 { "1 track".to_string() } else { format!("{} tracks", wish.tracks) });
        }
        select_list::two_lines(ui, std::iter::empty(), egui::RichText::new(&wish.title), Some(detail.join(" · ")), select_list::scrolls(row, false));
    });
    add
}
