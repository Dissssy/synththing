//! The online lookup (`crate::lookup`) in the app: run on the one device
//! chosen for it, an album at a time while the app's open, its matches
//! saved to the songs (and so synced); its preference and status in
//! Preferences > Songs; a song's match in Song info, with Wrong match; the
//! albums it wasn't sure of, for Clean up's Needs a look rows; artists'
//! names as MusicBrainz has them, for its Same name rows.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;

use super::App;
use crate::build_info;
use crate::cleanup;
use crate::fields::{Facts, Field};
use crate::lookup::artists::Tied;
use crate::lookup::matching::{self, Album, Release};
use crate::lookup::{self, Answer, Done, Fill, Kept, Outcome, Plan, State, Tried, Worker, details};
use crate::song_store::{Online, Song, SongStore};

/// How long the lookup waits to look for work again, with none to do.
const IDLE: Duration = Duration::from_secs(60);
/// How long a release that couldn't be asked for again waits to be.
const REFRESH_RETRY: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
pub(super) struct LookupState {
    worker: Option<Worker>,
    state: Option<State>,
    /// The library's albums, as of a songs revision (as the lookup
    /// thread last worked them out).
    albums: Option<(u64, Arc<Vec<Album>>)>,
    /// When to look for work again, with none found.
    idle_until: Option<(u64, Instant)>,
    /// Why the last lookup failed.
    error: Option<String>,
    /// Albums (by id) to look up again first, asking MusicBrainz anew.
    refresh: Vec<String>,
    /// Releases and artists as the lookup kept them.
    pub kept: Kept,
    /// Releases (and artists, as "artist:{id}") that couldn't be asked
    /// for, and when to try again.
    refresh_failed: HashMap<String, Instant>,
    /// How many artists have been asked for (so Clean up's made anew).
    pub artists_asked: u64,
}

impl App {
    /// This device looks albums up.
    fn looks_up(&self) -> bool {
        build_info::online(build_info::Online::Lookup) && self.config.online_lookup && self.config.lookup_device.as_deref() == Some(self.device_id())
    }

    /// Take in the lookup thread's answers, and have it plan the next
    /// ask (from a copy of the songs) once it's done. Once a frame.
    pub(super) fn lookup_tick(&mut self, ctx: &egui::Context) {
        if !self.looks_up() {
            return;
        }
        if self.lookup.worker.is_none() {
            match Worker::start(self.lookup.kept.clone(), ctx.clone()) {
                Ok(worker) => self.lookup.worker = Some(worker),
                Err(e) => {
                    log::warn!("couldn't start the online lookup: {e}");
                    self.config.online_lookup = false;
                    return;
                }
            }
        }
        while let Some(answer) = self.lookup.worker.as_mut().and_then(Worker::answer) {
            match answer {
                Done::Planned(planned) => {
                    self.fill_blanks(planned.fills);
                    if planned.asking.is_none() {
                        self.lookup.idle_until = Some((planned.revision, Instant::now() + IDLE));
                    }
                    self.lookup.albums = Some((planned.revision, planned.albums));
                }
                Done::Album(album, outcome, fills) => self.lookup_answered(&album, outcome, fills),
                Done::Release(id, release) => self.release_refreshed(&id, release),
                Done::Artist(_, Ok(artist)) => {
                    self.lookup.kept.keep_artist(artist);
                    self.lookup.artists_asked += 1;
                    self.lookup.error = None;
                }
                Done::Artist(id, Err(e)) => {
                    log::warn!("couldn't ask for artist {id}: {e}");
                    self.lookup.refresh_failed.insert(format!("artist:{id}"), Instant::now() + REFRESH_RETRY);
                    self.lookup.error = Some(e);
                }
            }
        }
        if self.lookup.worker.as_ref().is_some_and(Worker::working) {
            return;
        }
        let revision = self.songs.revision();
        if self.lookup.idle_until.is_some_and(|(r, until)| r == revision && Instant::now() < until) {
            return;
        }
        let plan = Plan {
            store: self.songs.clone(),
            tried: self.lookup.state.get_or_insert_with(State::load).tried.clone(),
            refresh: self.lookup.refresh.pop(),
            failed: self.lookup.refresh_failed.clone(),
            ignore: self.config.group_ignore.clone(),
        };
        if let Some(worker) = &mut self.lookup.worker {
            worker.plan(plan);
        }
    }

    /// Take in a matched release as MusicBrainz has it now: its matches
    /// are as of now.
    fn release_refreshed(&mut self, id: &str, release: Result<Release, String>) {
        match release {
            Ok(release) => {
                self.lookup.kept.keep_release(release);
                let at = crate::settings_sync::now();
                let changes = self
                    .songs
                    .known()
                    .filter_map(|(key, song)| {
                        let online = song.online.as_ref().filter(|o| o.release.as_deref() == Some(id))?;
                        Some((key.to_string(), Some(Online { at, ..online.clone() })))
                    })
                    .collect();
                self.songs.set_online(changes);
                self.songs_changed();
                self.lookup.error = None;
            }
            Err(e) => {
                log::warn!("couldn't ask for release {id} again: {e}");
                self.lookup.refresh_failed.insert(id.to_string(), Instant::now() + REFRESH_RETRY);
                self.lookup.error = Some(e);
            }
        }
    }

    fn lookup_answered(&mut self, album: &Album, outcome: Outcome, fills: Vec<Fill>) {
        let at = crate::settings_sync::now();
        let tried = match outcome {
            Outcome::Matched(release, tracks) => {
                log::info!("{:?} is release {} ({:?})", album.name, release.id, release.title);
                let matches = lookup::matches(album, &release, &tracks, false, &self.songs);
                self.lookup.kept.keep_release(*release);
                self.songs.set_online(matches);
                self.songs_changed();
                self.fill_blanks(fills);
                self.lookup.error = None;
                None
            }
            Outcome::Unsure(releases) => {
                log::info!("{:?} might be {} releases", album.name, releases.len());
                self.lookup.error = None;
                Some(Tried { at, found: Answer::Unsure(releases.into_iter().map(|r| r.id).collect()) })
            }
            Outcome::NotFound => {
                log::info!("{:?} wasn't found", album.name);
                self.lookup.error = None;
                Some(Tried { at, found: Answer::NotFound })
            }
            Outcome::Failed(e) => {
                log::warn!("couldn't look up {:?}: {e}", album.name);
                self.lookup.error = Some(e);
                Some(Tried { at, found: Answer::Failed })
            }
        };
        self.lookup.state.get_or_insert_with(State::load).note(&album.id, tried);
    }

    /// Preferences > Songs: whether albums are looked up, on which device,
    /// and how far it's got.
    pub(super) fn lookup_preferences_ui(&mut self, ui: &mut egui::Ui) {
        let mut on = self.config.online_lookup;
        super::with_info(
            ui,
            "Looks your albums up at MusicBrainz, one at a time in the background, and shows the \
             Cover Art Archive's cover for those without art of their own. Details your songs leave blank are \
             filled in from their album's release, and those it has otherwise are offered in Clean up. Album names, artists and \
             song titles are sent to musicbrainz.org; covers come from coverartarchive.org. Art in \
             your files always comes first, and your files are never changed. One of your devices \
             does the looking up, and its matches sync to the others.",
            |ui| ui.checkbox(&mut on, "Album art and details from MusicBrainz"),
        );
        let mut changed = false;
        if on != self.config.online_lookup {
            self.config.online_lookup = on;
            if on && self.config.lookup_device.is_none() {
                self.config.lookup_device = Some(self.device_id().to_string());
            }
            changed = true;
        }
        if on && self.online_ui(ui, build_info::Online::Lookup) {
            ui.indent("online_lookup", |ui| {
                if self.looks_up() {
                    self.lookup_status_ui(ui);
                } else {
                    ui.horizontal(|ui| {
                        ui.weak("Another of your devices looks albums up.");
                        if ui.button("Use this device").clicked() {
                            self.config.lookup_device = Some(self.device_id().to_string());
                            changed = true;
                        }
                    });
                }
            });
        }
        if changed && let Err(e) = self.config.save() {
            self.status = format!("Couldn't save preferences: {e}");
        }
    }

    fn lookup_status_ui(&mut self, ui: &mut egui::Ui) {
        let now = crate::settings_sync::now();
        let tried = self.lookup.state.get_or_insert_with(State::load).tried.clone();
        let busy = self.lookup.worker.as_ref().and_then(|w| w.busy.clone());
        let albums = self.lookup.albums.as_ref().map(|(_, a)| Arc::clone(a)).unwrap_or_default();
        let albums = albums.as_slice();
        // (Albums, then songs on no album: matched, unsure, not found,
        // to look up.)
        let mut counts = [[0; 4]; 2];
        for album in albums {
            let counts = &mut counts[usize::from(album.loose)];
            let at = if !lookup::wanted(&self.songs, album) {
                0
            } else {
                match tried.get(&album.id) {
                    Some(t) if !t.due(now) && matches!(t.found, Answer::Unsure(_)) => 1,
                    Some(t) if !t.due(now) && t.found == Answer::NotFound => 2,
                    _ => 3,
                }
            };
            counts[at] += 1;
        }
        let busy = busy.and_then(|id| albums.iter().find(|a| a.id == id)).map(|a| a.name.clone());
        let line = |name: &str, [matched, unsure, not_found, waiting]: [usize; 4]| {
            let mut parts = vec![format!("{name}: {matched} of {} matched", matched + unsure + not_found + waiting)];
            if unsure > 0 {
                parts.push(format!("{unsure} unsure"));
            }
            parts.push(format!("{not_found} not found"));
            parts.push(format!("{waiting} to look up"));
            parts.join(", ")
        };
        ui.label(line("Albums", counts[0]));
        ui.label(line("Songs on no album", counts[1]));
        if counts[0][1] + counts[1][1] > 0 && ui.button("Needs a look").on_hover_text("Pick the release each unsure album or song is, in Clean up").clicked() {
            self.cleanup = Some(Default::default());
        }
        if let Some(name) = busy {
            ui.horizontal(|ui| {
                ui.spinner();
                let text = format!("Looking up {name}...");
                ui.add(egui::Label::new(egui::RichText::new(&text).weak()).truncate()).on_hover_text(text);
            });
        }
        if let Some(error) = &self.lookup.error {
            ui.colored_label(ui.visuals().warn_fg_color, format!("The last lookup failed: {error}"));
        }
    }

    /// Song info > File: what the song at `path` was matched to, with a
    /// way to say it's wrong (for every song of its album matched so),
    /// and, on the device that looks albums up, to look its album up
    /// again, asking MusicBrainz anew.
    pub(super) fn online_match_ui(&mut self, ui: &mut egui::Ui, path: &std::path::Path) {
        let Some((key, song)) = self.songs.key(path).and_then(|k| Some((k.clone(), self.songs.get_by_key(&k)?))) else {
            return;
        };
        let album = lookup::album_of(&key, song, &self.config.group_ignore);
        let online = song.online.clone();
        if self.looks_up() {
            ui.add_space(4.0);
            let asked = self.lookup.refresh.contains(&album) || self.lookup.worker.as_ref().is_some_and(|w| w.busy.as_ref() == Some(&album));
            let again = if album.starts_with("song:") { "Look it up again" } else { "Look up its album again" };
            let refresh = ui
                .add_enabled(!asked, egui::Button::new(if asked { "Looking up..." } else { again }))
                .on_hover_text("Ask MusicBrainz about its album again, rather than going by what was kept from last time");
            if refresh.clicked() {
                self.lookup.refresh.push(album);
                self.lookup.idle_until = None;
            }
        }
        let Some(online) = online else {
            return;
        };
        let Some(release) = online.release.clone() else {
            if online.by_user {
                ui.add_space(4.0);
                ui.weak("Not matched: it'll be looked up again, leaving out the releases you've said it isn't.");
            }
            return;
        };
        ui.add_space(4.0);
        let mut wrong = false;
        egui::Grid::new("online_match").num_columns(2).show(ui, |ui| {
            ui.strong("Matched to");
            ui.horizontal(|ui| {
                ui.hyperlink_to("Release on MusicBrainz", format!("https://musicbrainz.org/release/{release}"));
                wrong = ui.button("Wrong match").on_hover_text("It isn't this album: look it up again, leaving this one out").clicked();
            });
            ui.end_row();
            if let Some(recording) = &online.recording {
                ui.strong("Recording");
                ui.hyperlink_to("Open on MusicBrainz", format!("https://musicbrainz.org/recording/{recording}"));
                ui.end_row();
            }
            ui.strong("Matched");
            ui.label(format!("{} on {}", if online.by_user { "By you" } else { "By the lookup" }, crate::fields::date(online.at)));
            ui.end_row();
        });
        if wrong {
            self.wrong_match(&release);
        }
    }

    /// Every song matched to `release` isn't on it: unmatch them, and
    /// keep it from being matched again.
    fn wrong_match(&mut self, release: &str) {
        let at = crate::settings_sync::now();
        // (What the match filled in, and is still as it filled it, goes.)
        let unfilled: Vec<(String, Field, Option<String>)> = self
            .songs
            .known()
            .filter_map(|(key, song)| Some((key, song.online.as_ref().filter(|o| o.release.as_deref() == Some(release))?.filled.as_ref()?)))
            .flat_map(|(key, filled)| filled.iter().map(move |field| (key.to_string(), *field, None)))
            .collect();
        self.songs.edit(unfilled);
        let changes: Vec<(String, Option<Online>)> = self
            .songs
            .known()
            .filter_map(|(key, song)| {
                let online = song.online.as_ref().filter(|o| o.release.as_deref() == Some(release))?;
                let mut rejected = online.rejected.clone();
                rejected.push(release.to_string());
                Some((key.to_string(), Some(Online { rejected, by_user: true, at, art: online.art.clone(), ..Online::default() })))
            })
            .collect();
        log::info!("{} songs aren't on release {release}", changes.len());
        self.songs.set_online(changes);
        self.songs_changed();
        self.lookup.idle_until = None;
    }

    /// What Clean up's rows from the lookup are made from, besides the
    /// songs: what it's tried, on the device that looks albums up.
    pub(super) fn lookup_tried(&mut self) -> Option<BTreeMap<String, Tried>> {
        self.looks_up().then(|| self.lookup.state.get_or_insert_with(State::load).tried.clone())
    }

    /// Fill in matched songs' blank fields from their releases (as the
    /// lookup thread read them), for those still matched to them and not
    /// filled in yet: once for each match.
    fn fill_blanks(&mut self, fills: Vec<Fill>) {
        let ignore = &self.config.group_ignore;
        let fills: Vec<_> = fills
            .into_iter()
            .filter_map(|fill| {
                let song = self.songs.get_by_key(&fill.key)?;
                let unfilled = song.online.as_ref().is_some_and(|o| o.release.as_deref() == Some(fill.release.as_str()) && o.filled.is_none());
                unfilled.then(|| (fill.key, details::compare(&Facts::of(song), &fill.said, ignore).blank))
            })
            .collect();
        if !fills.is_empty() {
            log::info!("filling in {} matched songs' blanks", fills.len());
            self.songs.fill(fills);
            self.songs_changed();
        }
    }

    /// Take what was picked for an unsure album.
    pub(super) fn reviewed(&mut self, reviewed: Reviewed) {
        match reviewed {
            Reviewed::This(album, release) => {
                let fit = matching::fit(&album, &release);
                log::info!("{:?} is release {} ({:?}), as picked", album.name, release.id, release.title);
                let matches = lookup::matches(&album, &release, &fit.tracks, true, &self.songs);
                self.songs.set_online(matches);
                self.songs_changed();
                self.lookup.state.get_or_insert_with(State::load).note(&album.id, None);
            }
            Reviewed::NoneOf(album, rejected) => {
                let at = crate::settings_sync::now();
                let changes = album
                    .songs
                    .iter()
                    .map(|song| {
                        let mut online = self.songs.get_by_key(&song.key).and_then(|s| s.online.clone()).unwrap_or(Online { at, ..Online::default() });
                        online.rejected.extend(rejected.iter().filter(|r| !online.rejected.contains(r)).cloned().collect::<Vec<_>>());
                        online.by_user = true;
                        (song.key.clone(), Some(online))
                    })
                    .collect();
                self.songs.set_online(changes);
                self.songs_changed();
                self.lookup.state.get_or_insert_with(State::load).note(&album.id, Some(Tried { at, found: Answer::NotFound }));
            }
        }
    }
}

/// `store`'s matched songs, with their releases as `kept` and recordings.
fn matched_releases<'s>(store: &'s SongStore, kept: &Kept) -> Vec<(String, &'s Song, Arc<Release>, Option<String>)> {
    lookup::matched(store, |_| true)
        .into_iter()
        .filter_map(|(key, release, recording)| Some((key.clone(), store.get_by_key(&key)?, kept.release(&release)?, recording)))
        .collect()
}

/// What MusicBrainz says otherwise of `store`'s matched songs (their
/// releases as `kept` on this device), for Clean up.
pub(super) fn musicbrainz_suggestions(store: &SongStore, kept: &Kept, ignore: &str) -> Vec<cleanup::Suggestion> {
    let genres = details::Genres::of(store, ignore);
    let mut offers = Vec::new();
    for (key, song, release, recording) in matched_releases(store, kept) {
        let facts = Facts::of(song);
        let track = details::track(&release, recording.as_deref());
        let said = details::said(&release, track, &genres, ignore);
        let compared = details::compare(&facts, &said, ignore);
        let title = facts.text(Field::Title).map_or_else(|| song.name.clone(), |t| t.into_owned());
        let length = track.and_then(|t| Some((facts.number(Field::Length)?, t.length?)));
        offers.push(cleanup::Offered { key, title, release: release.id.clone(), album: release.title.clone(), differs: compared.differs, length });
    }
    cleanup::from_musicbrainz(offers, details::LENGTH_LEEWAY)
}

/// Clean up's Same name rows for artists tied to MusicBrainz
/// (`lookup::artists`), in place of the offline ones (in `suggestions`)
/// they overlap.
pub(super) fn artist_suggestions(store: &SongStore, kept: &Kept, suggestions: &mut Vec<cleanup::Suggestion>, ignore: &str) {
    let matched = matched_releases(store, kept);
    let mut tied = Tied::of(matched.iter().map(|(_, song, release, recording)| (*song, &**release, details::track(release, recording.as_deref()))), ignore);
    let artists: Vec<_> = tied.own.keys().filter_map(|id| kept.artist(id)).collect();
    tied.add_aliases(artists.iter().map(|a| &**a), ignore);
    tied.offer(store, suggestions, ignore);
}

/// The albums the lookup wasn't sure of (as it `tried` them) and still
/// has to match, with the releases kept for each: Clean up's Needs a
/// look rows.
pub(super) fn unsure_albums(store: &SongStore, kept: &Kept, tried: &BTreeMap<String, Tried>, ignore: &str) -> Vec<Unsure> {
    lookup::albums(store, ignore)
        .into_iter()
        .filter(|album| lookup::wanted(store, album))
        .filter_map(|album| {
            let Answer::Unsure(ids) = &tried.get(&album.id)?.found else { return None };
            let releases: Vec<Release> = ids.iter().filter_map(|id| kept.release(id)).map(|r| (*r).clone()).collect();
            (!releases.is_empty()).then(|| Unsure::new(album, releases))
        })
        .collect()
}

/// An album the lookup wasn't sure of, and the releases it might be.
pub(super) struct Unsure {
    pub album: Album,
    pub releases: Vec<Release>,
    /// Each release as one line (`Unsure::describe`).
    pub lines: Vec<String>,
}

/// What was picked for an unsure album: a release, or none of them.
pub(super) enum Reviewed {
    This(Album, Release),
    NoneOf(Album, Vec<String>),
}

impl Unsure {
    fn new(album: Album, releases: Vec<Release>) -> Self {
        let lines = releases.iter().map(|r| describe(&album, r)).collect();
        Self { album, releases, lines }
    }
}

/// A release `album` might be, as one line: its name, date and how many
/// of the album's songs are on it.
fn describe(album: &Album, release: &Release) -> String {
    let fit = matching::fit(album, release);
    {
        format!(
            "{} by {} ({}, {} of {} songs on it)",
            release.title,
            release.artist,
            release.date.as_deref().unwrap_or("no date"),
            fit.tracks.iter().flatten().count(),
            album.songs.len()
        )
    }
}
