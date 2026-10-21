//! The online lookup (`crate::lookup`) in the app: run on the one device
//! chosen for it, an album at a time while the app's open, its matches
//! saved to the songs (and so synced); its preference and status in
//! Preferences > Songs; a song's match in Song info, with Wrong match; the
//! albums it wasn't sure of, for Clean up's Needs a look rows; artists'
//! names as MusicBrainz has them, for its Same name rows.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui;

use super::App;
use crate::build_info;
use crate::cleanup;
use crate::fields::{Facts, Field};
use crate::lookup::artists::{self, Artist, Tied};
use crate::lookup::matching::{self, Album, Release};
use crate::lookup::{self, Answer, Ask, Done, Outcome, State, Tried, Worker, details};
use crate::song_store::Online;

/// How long the lookup waits to look for work again, with none to do.
const IDLE: Duration = Duration::from_secs(60);
/// How long a release that couldn't be asked for again waits to be.
const REFRESH_RETRY: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
pub(super) struct LookupState {
    worker: Option<Worker>,
    state: Option<State>,
    /// The library's albums, as of a songs revision.
    albums: Option<(u64, Vec<Album>)>,
    /// When to look for work again, with none found.
    idle_until: Option<(u64, Instant)>,
    /// Why the last lookup failed.
    error: Option<String>,
    /// Albums (by id) to look up again first, asking MusicBrainz anew.
    refresh: Vec<String>,
    /// Releases as the lookup kept them (`None`: not kept), by id, read
    /// as they're needed.
    kept: HashMap<String, Option<Release>>,
    /// The songs revision matched songs' blanks were last filled at.
    filled_at: Option<u64>,
    /// Releases (and artists, as "artist:{id}") that couldn't be asked
    /// for, and when to try again.
    refresh_failed: HashMap<String, Instant>,
    /// Artists as the lookup kept them (`None`: not kept), by id.
    artists: HashMap<String, Option<Artist>>,
    /// How many artists have been asked for (so Clean up's made anew).
    pub artists_asked: u64,
}

impl App {
    /// This device looks albums up.
    fn looks_up(&self) -> bool {
        build_info::online(build_info::Online::Lookup) && self.config.online_lookup && self.config.lookup_device.as_deref() == Some(self.device_id())
    }

    /// Work out the library's albums again, if its songs changed.
    fn lookup_albums(&mut self) {
        let revision = self.songs.revision();
        if self.lookup.albums.as_ref().is_none_or(|(r, _)| *r != revision) {
            self.lookup.albums = Some((revision, lookup::albums(&self.songs, &self.config.group_ignore)));
        }
    }

    /// Take in a lookup's answer, and start the next album's. Once a frame.
    pub(super) fn lookup_tick(&mut self) {
        if !self.looks_up() {
            return;
        }
        if self.lookup.worker.is_none() {
            match Worker::start() {
                Ok(worker) => self.lookup.worker = Some(worker),
                Err(e) => {
                    log::warn!("couldn't start the online lookup: {e}");
                    self.config.online_lookup = false;
                    return;
                }
            }
        }
        let answer = self.lookup.worker.as_mut().and_then(Worker::answer);
        match answer {
            Some(Done::Album(album, outcome)) => self.lookup_answered(&album, outcome),
            Some(Done::Release(id, release)) => self.release_refreshed(&id, release),
            Some(Done::Artist(id, Ok(artist))) => {
                self.lookup.artists.insert(id, Some(artist));
                self.lookup.artists_asked += 1;
                self.lookup.error = None;
            }
            Some(Done::Artist(id, Err(e))) => {
                log::warn!("couldn't ask for artist {id}: {e}");
                self.lookup.refresh_failed.insert(format!("artist:{id}"), Instant::now() + REFRESH_RETRY);
                self.lookup.error = Some(e);
            }
            None => {}
        }
        if self.lookup.filled_at != Some(self.songs.revision()) {
            self.fill_blanks();
            self.lookup.filled_at = Some(self.songs.revision());
        }
        if self.lookup.worker.as_ref().is_some_and(|w| w.busy.is_some()) {
            return;
        }
        let revision = self.songs.revision();
        if self.lookup.idle_until.is_some_and(|(r, until)| r == revision && Instant::now() < until) {
            return;
        }
        let now = crate::settings_sync::now();
        self.lookup_albums();
        let tried = &self.lookup.state.get_or_insert_with(State::load).tried;
        let albums = self.lookup.albums.as_ref().map(|(_, a)| a.as_slice()).unwrap_or_default();
        let refresh = self.lookup.refresh.pop().and_then(|id| albums.iter().find(|a| a.id == id)).cloned();
        let fresh = refresh.is_some();
        let next = refresh.or_else(|| albums.iter().find(|a| tried.get(&a.id).is_none_or(|t| t.due(now)) && lookup::wanted(&self.songs, a)).cloned());
        // (Then artists not asked for yet, then matches due for a
        // refresh, the oldest first.)
        let ask = next.map(|album| Ask::Album(album, fresh)).or_else(|| self.artist_due().map(Ask::Artist)).or_else(|| {
            self.songs
                .known()
                .filter_map(|(_, s)| s.online.as_ref().filter(|o| now - o.at >= lookup::REFRESH).and_then(|o| Some((o.at, o.release.clone()?))))
                .filter(|(_, release)| self.lookup.refresh_failed.get(release).is_none_or(|until| Instant::now() >= *until))
                .min()
                .map(|(_, release)| Ask::Release(release))
        });
        match ask {
            Some(ask) => {
                match &ask {
                    Ask::Album(album, fresh) => {
                        log::info!("looking up {:?} by {:?} ({} songs{})", album.name, album.artist, album.songs.len(), if *fresh { ", anew" } else { "" })
                    }
                    Ask::Release(id) => log::info!("asking for release {id} again"),
                    Ask::Artist(id) => log::info!("asking for artist {id}"),
                }
                if let Some(worker) = &mut self.lookup.worker {
                    worker.ask(ask);
                }
            }
            None => self.lookup.idle_until = Some((revision, Instant::now() + IDLE)),
        }
    }

    /// An artist matched songs credit who isn't kept, to ask for.
    fn artist_due(&mut self) -> Option<String> {
        let matched = self.matched_with(|_| true);
        let kept = &self.lookup.kept;
        let credited = artists::credited(matched.iter().filter_map(|(_, r, _)| kept.get(r)?.as_ref()));
        let now = Instant::now();
        credited
            .into_iter()
            .find(|id| {
                self.lookup.artists.entry(id.to_string()).or_insert_with(|| lookup::kept_artist(id)).is_none()
                    && self.lookup.refresh_failed.get(&format!("artist:{id}")).is_none_or(|until| now >= *until)
            })
            .map(str::to_string)
    }

    /// Clean up's Same name rows for artists tied to MusicBrainz
    /// (`lookup::artists`), in place of the offline ones (in `suggestions`)
    /// they overlap.
    pub(super) fn artist_suggestions(&mut self, suggestions: &mut Vec<cleanup::Suggestion>) {
        let ignore = self.config.group_ignore.clone();
        let matched = self.matched_with(|_| true);
        let mut tied = Tied::of(
            matched.iter().filter_map(|(key, release, recording)| {
                let release = self.lookup.kept.get(release)?.as_ref()?;
                Some((self.songs.get_by_key(key)?, release, details::track(release, recording.as_deref())))
            }),
            &ignore,
        );
        for id in tied.own.keys() {
            self.lookup.artists.entry(id.clone()).or_insert_with(|| lookup::kept_artist(id));
        }
        tied.add_aliases(self.lookup.artists.values().flatten(), &ignore);
        tied.offer(&self.songs, suggestions, &ignore);
    }

    /// Take in a matched release as MusicBrainz has it now: its matches
    /// are as of now.
    fn release_refreshed(&mut self, id: &str, release: Result<Release, String>) {
        match release {
            Ok(release) => {
                self.lookup.kept.insert(id.to_string(), Some(release));
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

    fn lookup_answered(&mut self, album: &Album, outcome: Outcome) {
        let at = crate::settings_sync::now();
        let tried = match outcome {
            Outcome::Matched(release, tracks) => {
                log::info!("{:?} is release {} ({:?})", album.name, release.id, release.title);
                let matches = lookup::matches(album, &release, &tracks, false, &self.songs);
                self.lookup.kept.insert(release.id.clone(), Some(*release));
                self.songs.set_online(matches);
                self.songs_changed();
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
        self.lookup_albums();
        let albums = self.lookup.albums.as_ref().map(|(_, a)| a.as_slice()).unwrap_or_default();
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
        if counts[0][1] > 0 && ui.button("Needs a look").on_hover_text("Pick the release each unsure album is, in Clean up").clicked() {
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

    /// The albums the lookup wasn't sure of (and still has to match),
    /// with the releases kept for each: Clean up's Needs a look rows. None
    /// on a device that doesn't look up.
    pub(super) fn unsure_albums(&mut self) -> Vec<Unsure> {
        if !self.looks_up() {
            return Vec::new();
        }
        self.lookup_albums();
        let albums = self.lookup.albums.as_ref().map(|(_, a)| a.as_slice()).unwrap_or_default();
        let tried = &self.lookup.state.get_or_insert_with(State::load).tried;
        let unsure: Vec<(&Album, &Vec<String>)> = albums
            .iter()
            .filter(|album| lookup::wanted(&self.songs, album))
            .filter_map(|album| match &tried.get(&album.id)?.found {
                Answer::Unsure(ids) => Some((album, ids)),
                _ => None,
            })
            .collect();
        keep_releases(&mut self.lookup.kept, unsure.iter().flat_map(|(_, ids)| ids.iter().map(String::as_str)));
        unsure
            .into_iter()
            .filter_map(|(album, ids)| {
                let releases: Vec<Release> = ids.iter().filter_map(|id| self.lookup.kept.get(id)?.clone()).collect();
                (!releases.is_empty()).then(|| Unsure::new(album.clone(), releases))
            })
            .collect()
    }

    /// Matched songs (by key), with their releases (read into `kept`)
    /// and recordings: those `wanted`.
    fn matched_with(&mut self, wanted: impl Fn(&Online) -> bool) -> Vec<(String, String, Option<String>)> {
        let matched: Vec<(String, String, Option<String>)> = self
            .songs
            .known()
            .filter_map(|(key, song)| {
                let online = song.online.as_ref().filter(|o| wanted(o))?;
                Some((key.to_string(), online.release.clone()?, online.recording.clone()))
            })
            .collect();
        keep_releases(&mut self.lookup.kept, matched.iter().map(|(_, r, _)| r.as_str()));
        matched
    }

    /// Fill in matched songs' blank fields from their releases (as kept),
    /// once for each match.
    fn fill_blanks(&mut self) {
        let ignore = self.config.group_ignore.clone();
        if !self.songs.known().any(|(_, s)| s.online.as_ref().is_some_and(|o| o.release.is_some() && o.filled.is_none())) {
            return;
        }
        let genres = details::Genres::of(&self.songs, &ignore);
        let mut fills = Vec::new();
        for (key, release, recording) in self.matched_with(|o| o.filled.is_none()) {
            let Some(release) = self.lookup.kept.get(&release).and_then(Option::as_ref) else { continue };
            let Some(song) = self.songs.get_by_key(&key) else { continue };
            let said = details::said(release, details::track(release, recording.as_deref()), &genres, &ignore);
            fills.push((key, details::compare(&Facts::of(song), &said, &ignore).blank));
        }
        if !fills.is_empty() {
            log::info!("filling in {} matched songs' blanks", fills.len());
            self.songs.fill(fills);
            self.songs_changed();
        }
    }

    /// What MusicBrainz says otherwise of matched songs (their releases as
    /// kept on this device), for Clean up.
    pub(super) fn musicbrainz_suggestions(&mut self) -> Vec<cleanup::Suggestion> {
        let ignore = self.config.group_ignore.clone();
        let genres = details::Genres::of(&self.songs, &ignore);
        let mut offers = Vec::new();
        for (key, release, recording) in self.matched_with(|_| true) {
            let Some(release) = self.lookup.kept.get(&release).and_then(Option::as_ref) else { continue };
            let Some(song) = self.songs.get_by_key(&key) else { continue };
            let facts = Facts::of(song);
            let track = details::track(release, recording.as_deref());
            let said = details::said(release, track, &genres, &ignore);
            let compared = details::compare(&facts, &said, &ignore);
            let title = facts.text(Field::Title).map_or_else(|| song.name.clone(), |t| t.into_owned());
            let length = track.and_then(|t| Some((facts.number(Field::Length)?, t.length?)));
            offers.push(cleanup::Offered { key, title, release: release.id.clone(), album: release.title.clone(), differs: compared.differs, length });
        }
        cleanup::from_musicbrainz(offers, details::LENGTH_LEEWAY)
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

/// Read the releases `ids` into `kept`, those not read yet.
fn keep_releases<'i>(kept: &mut HashMap<String, Option<Release>>, ids: impl Iterator<Item = &'i str>) {
    for id in ids {
        if !kept.contains_key(id) {
            kept.insert(id.to_string(), lookup::kept_release(id));
        }
    }
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
