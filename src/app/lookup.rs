//! The online lookup (`crate::lookup`) in the app: run on the one device
//! chosen for it, an album at a time while the app's open, its matches
//! saved to the songs (and so synced); its preference and status in
//! Preferences > Songs; a song's match in Song info, with Wrong match.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use eframe::egui;

use super::App;
use crate::lookup::matching::{self, Album, Release};
use crate::lookup::{self, Answer, Outcome, State, Tried, Worker};
use crate::song_store::Online;

/// How long the lookup waits to look for work again, with none to do.
const IDLE: Duration = Duration::from_secs(60);

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
    /// Needs a look is open: the releases each unsure album might be, by
    /// its id (read from what was kept when it opened).
    review: Option<BTreeMap<String, Vec<Release>>>,
}

impl App {
    /// This device looks albums up.
    fn looks_up(&self) -> bool {
        crate::build_info::online_features() && self.config.online_lookup && self.config.lookup_device.as_deref() == Some(self.device_id())
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
        if let Some((album, outcome)) = answer {
            self.lookup_answered(&album, outcome);
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
        match next {
            Some(album) => {
                log::info!("looking up {:?} by {:?} ({} songs{})", album.name, album.artist, album.songs.len(), if fresh { ", anew" } else { "" });
                if let Some(worker) = &mut self.lookup.worker {
                    worker.ask(album, fresh);
                }
            }
            None => self.lookup.idle_until = Some((revision, Instant::now() + IDLE)),
        }
    }

    fn lookup_answered(&mut self, album: &Album, outcome: Outcome) {
        let at = crate::settings_sync::now();
        let tried = match outcome {
            Outcome::Matched(release, tracks) => {
                log::info!("{:?} is release {} ({:?})", album.name, release.id, release.title);
                let matches = lookup::matches(album, &release, &tracks, false, &self.songs);
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
        if !crate::build_info::online_features() {
            ui.weak(crate::build_info::OFFLINE_NOTE);
            return;
        }
        let mut on = self.config.online_lookup;
        super::with_info(
            ui,
            "Looks your albums up at MusicBrainz, one at a time in the background, and shows the \
             Cover Art Archive's cover for those without art of their own. Album names, artists and \
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
        if on {
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
        let (mut matched, mut unsure, mut not_found, mut waiting) = (0, 0, 0, 0);
        for album in albums {
            if !lookup::wanted(&self.songs, album) {
                matched += 1;
                continue;
            }
            match tried.get(&album.id) {
                Some(t) if !t.due(now) && matches!(t.found, Answer::Unsure(_)) => unsure += 1,
                Some(t) if !t.due(now) && t.found == Answer::NotFound => not_found += 1,
                _ => waiting += 1,
            }
        }
        let total = albums.len();
        let busy = busy.and_then(|id| albums.iter().find(|a| a.id == id)).map(|a| a.name.clone());
        let mut review = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{matched} of {total} albums matched, {unsure} unsure, {not_found} not found, {waiting} to look up."));
            review = unsure > 0 && ui.button("Needs a look").on_hover_text("Pick the release each unsure album is").clicked();
        });
        if review {
            self.open_review();
        }
        if let Some(name) = busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(format!("Looking up {name}..."));
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
        let Some(song) = self.songs.get(path) else {
            return;
        };
        let album = lookup::album_of(song, &self.config.group_ignore);
        let online = song.online.clone();
        if self.looks_up()
            && let Some(album) = album
        {
            ui.add_space(4.0);
            let asked = self.lookup.refresh.contains(&album) || self.lookup.worker.as_ref().is_some_and(|w| w.busy.as_ref() == Some(&album));
            let refresh = ui
                .add_enabled(!asked, egui::Button::new(if asked { "Looking up..." } else { "Look up its album again" }))
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

    /// Open Needs a look, with the releases kept for each unsure album.
    fn open_review(&mut self) {
        let tried = &self.lookup.state.get_or_insert_with(State::load).tried;
        let review = tried
            .iter()
            .filter_map(|(id, t)| match &t.found {
                Answer::Unsure(ids) => Some((id.clone(), ids.iter().filter_map(|r| lookup::kept_release(r)).collect())),
                _ => None,
            })
            .collect();
        self.lookup.review = Some(review);
    }

    /// The Needs a look window, while it's open: each unsure album, the
    /// releases it might be and how well each fits, to pick one or none.
    pub(super) fn review_ui(&mut self, ctx: &egui::Context) {
        let Some(review) = self.lookup.review.take() else { return };
        self.lookup_albums();
        let albums = self.lookup.albums.as_ref().map(|(_, a)| a.clone()).unwrap_or_default();
        let mut open = true;
        let mut picked: Option<(Album, Release)> = None;
        let mut none: Option<(Album, Vec<String>)> = None;
        egui::Window::new("Needs a look").id(egui::Id::new("lookup_review")).open(&mut open).default_width(560.0).show(ctx, |ui| {
            ui.weak("Albums the lookup wasn't sure of. Pick the release each one is, or none of them to leave it be.");
            egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                let mut shown = 0;
                for (id, releases) in &review {
                    let Some(album) = albums.iter().find(|a| &a.id == id) else { continue };
                    if !lookup::wanted(&self.songs, album) {
                        continue;
                    }
                    shown += 1;
                    ui.separator();
                    let by = album.artist.as_deref().map(|a| format!(" by {a}")).unwrap_or_default();
                    ui.strong(format!("{}{by} ({} songs)", album.name, album.songs.len()));
                    if releases.is_empty() {
                        ui.weak("What it might be wasn't kept: look it up again from Song info.");
                    }
                    egui::Grid::new(("lookup_review", id)).num_columns(3).striped(true).show(ui, |ui| {
                        for release in releases {
                            let fit = matching::fit(album, release);
                            let date = release.date.as_deref().unwrap_or("no date");
                            ui.hyperlink_to(format!("{} by {}", release.title, release.artist), format!("https://musicbrainz.org/release/{}", release.id))
                                .on_hover_text("Open on MusicBrainz");
                            ui.weak(format!(
                                "{date}, {} tracks, {} of {} songs on it",
                                release.tracks.len(),
                                fit.tracks.iter().flatten().count(),
                                album.songs.len()
                            ));
                            if ui.button("This one").clicked() {
                                picked = Some((album.clone(), release.clone()));
                            }
                            ui.end_row();
                        }
                    });
                    if ui.button("None of these").on_hover_text("Leave it unmatched, and never take these for it").clicked() {
                        none = Some((album.clone(), releases.iter().map(|r| r.id.clone()).collect()));
                    }
                }
                if shown == 0 {
                    ui.label("Nothing needs a look.");
                }
            });
        });
        if let Some((album, release)) = picked {
            let fit = matching::fit(&album, &release);
            log::info!("{:?} is release {} ({:?}), as picked", album.name, release.id, release.title);
            let matches = lookup::matches(&album, &release, &fit.tracks, true, &self.songs);
            self.songs.set_online(matches);
            self.songs_changed();
            self.lookup.state.get_or_insert_with(State::load).note(&album.id, None);
        }
        if let Some((album, rejected)) = none {
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
        if open {
            self.lookup.review = Some(review);
        }
    }
}
