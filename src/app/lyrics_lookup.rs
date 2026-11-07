//! Looking songs' lyrics up at LRCLIB (`lookup::lrclib`): on the device
//! that looks albums up, one song at a time while the app's open, for
//! songs with no lyrics of their own. A song found gets the record's id
//! (`song_store::Online::lyrics`, synced); one not found, or with lyrics
//! of its own, is noted here (`<config>/lrclib/tried.json`, on this device
//! only) and tried again later.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};

use super::App;
use crate::build_info;
use crate::fields::{Facts, Field};
use crate::latest::Latest;
use crate::lookup::lrclib;

/// How long the lookup waits to look for work again, with none to do.
const IDLE: Duration = Duration::from_secs(60);
/// How long a song not found (or with lyrics of its own) waits to be tried
/// again.
const RETRY: i64 = 90 * 24 * 60 * 60;
/// How long one whose lookup failed (offline, LRCLIB down) waits.
const RETRY_FAILED: i64 = 60 * 60;
/// The pause after each ask, to go easy on LRCLIB.
const PAUSE: Duration = Duration::from_secs(1);

/// A song tried and not given LRCLIB's lyrics, and when.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Tried {
    at: i64,
    /// Its lookup failed (rather than finding nothing, or finding it has
    /// lyrics of its own).
    failed: bool,
}

impl Tried {
    fn due(self, now: i64) -> bool {
        now - self.at >= if self.failed { RETRY_FAILED } else { RETRY }
    }
}

/// A song to look up.
struct Ask {
    key: String,
    path: PathBuf,
    song: lrclib::Song,
}

enum Found {
    /// It has timed lyrics of its own.
    Own,
    Record(i64),
    NotFound,
    Failed(String),
}

#[derive(Default)]
pub(super) struct LyricsLookup {
    worker: Option<Latest<(), Ask, (String, Found)>>,
    /// The song (by key) being looked up, and its title.
    busy: Option<(String, String)>,
    /// The songs tried (read from disk as first wanted).
    tried: Option<BTreeMap<String, Tried>>,
    /// When to look for work again, with none found (at a songs revision).
    idle_until: Option<(u64, Instant)>,
    /// Why the last lookup failed.
    error: Option<String>,
}

fn tried_path() -> Option<PathBuf> {
    Some(crate::config::config_dir().ok()?.join("lrclib").join("tried.json"))
}

/// Look `ask`'s song up (on the lookup's thread).
fn work(ask: Ask) -> (String, Found) {
    // (Untimed lyrics of its own are still looked up, for timed ones.)
    let found = if crate::lyrics::local(&ask.path).is_some_and(|l| l.timed) {
        Found::Own
    } else {
        log::info!("looking up the lyrics of {:?} by {:?}", ask.song.title, ask.song.artist);
        match lrclib::find(&ask.song) {
            Ok(Some(record)) => Found::Record(record.id),
            Ok(None) => Found::NotFound,
            Err(e) => Found::Failed(format!("{e:#}")),
        }
    };
    std::thread::sleep(PAUSE);
    (ask.key, found)
}

impl App {
    /// LRCLIB can be asked for lyrics (on this device).
    pub(super) fn fetches_lyrics(&self) -> bool {
        build_info::online(build_info::Online::Lookup) && self.config.online_lookup && self.config.lyrics_lookup
    }

    /// This device looks songs' lyrics up.
    pub(super) fn looks_up_lyrics(&self) -> bool {
        self.fetches_lyrics() && self.config.lookup_device.as_deref() == Some(self.device_id())
    }

    fn lyrics_tried(&mut self) -> &mut BTreeMap<String, Tried> {
        self.lyrics_lookup
            .tried
            .get_or_insert_with(|| tried_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default())
    }

    /// Take in the last song's answer, and ask about the next. Once a
    /// frame; while `held`, nothing new is asked.
    pub(super) fn lyrics_lookup_tick(&mut self, ctx: &egui::Context, held: bool) {
        if !self.looks_up_lyrics() {
            return;
        }
        let worker = self.lyrics_lookup.worker.get_or_insert_with(|| Latest::new("lyrics lookup", work));
        let answers: Vec<_> = worker.take().map(|(_, answer)| answer).collect();
        for (key, found) in answers {
            self.lyrics_lookup.busy = None;
            let now = crate::settings_sync::now();
            let tried = match found {
                Found::Record(id) => {
                    self.songs.set_lyrics(&key, Some(id));
                    self.songs_changed();
                    self.lyrics_lookup.error = None;
                    None
                }
                Found::Own | Found::NotFound => {
                    self.lyrics_lookup.error = None;
                    Some(Tried { at: now, failed: false })
                }
                Found::Failed(e) => {
                    log::warn!("couldn't look lyrics up at LRCLIB: {e}");
                    self.lyrics_lookup.error = Some(e);
                    Some(Tried { at: now, failed: true })
                }
            };
            let all = self.lyrics_tried();
            match tried {
                Some(tried) => all.insert(key, tried),
                None => all.remove(&key),
            };
            let saved = serde_json::to_vec(&*all).map_err(anyhow::Error::from).and_then(|bytes| {
                let path = tried_path().ok_or_else(|| anyhow::anyhow!("no config folder"))?;
                std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
                Ok(std::fs::write(path, bytes)?)
            });
            if let Err(e) = saved {
                log::warn!("couldn't save the lyrics looked up: {e:#}");
            }
        }
        let revision = self.songs.revision();
        if held || self.lyrics_lookup.busy.is_some() || self.lyrics_lookup.idle_until.is_some_and(|(r, until)| r == revision && Instant::now() < until) {
            return;
        }
        match self.next_lyrics_ask() {
            Some(ask) => {
                self.lyrics_lookup.busy = Some((ask.key.clone(), ask.song.title.clone()));
                if let Some(worker) = &self.lyrics_lookup.worker {
                    worker.ask((), ask, ctx);
                }
            }
            None => self.lyrics_lookup.idle_until = Some((revision, Instant::now() + IDLE)),
        }
    }

    /// The next song to look up: one here, not MIDI, with a title, artist
    /// and length, not given LRCLIB's lyrics and not tried lately.
    fn next_lyrics_ask(&mut self) -> Option<Ask> {
        let now = crate::settings_sync::now();
        let tried = self.lyrics_tried().clone();
        let songs = &self.songs;
        songs.known().filter(|(key, _)| songs.has(key) && !songs.is_blocked(key)).find_map(|(key, song)| {
            if song.online.as_ref().is_some_and(|o| o.lyrics.is_some()) || crate::midi_notes::is_midi(std::path::Path::new(&song.name)) {
                return None;
            }
            if tried.get(key).is_some_and(|t| !t.due(now)) {
                return None;
            }
            let facts = Facts::of(song);
            let song = lrclib::Song {
                title: facts.text(Field::Title)?.into_owned(),
                artist: facts.text(Field::Artist)?.into_owned(),
                album: facts.text(Field::Album).map(|a| a.into_owned()),
                length: facts.number(Field::Length)?,
            };
            Some(Ask { key: key.to_string(), path: songs.path(key), song })
        })
    }

    /// Preferences > Library: how far the lyrics lookup has got.
    pub(super) fn lyrics_lookup_status_ui(&mut self, ui: &mut egui::Ui) {
        let now = crate::settings_sync::now();
        let tried = self.lyrics_tried().clone();
        let (mut found, mut without) = (0, 0);
        for (key, song) in self.songs.known() {
            if song.online.as_ref().is_some_and(|o| o.lyrics.is_some()) {
                found += 1;
            } else if tried.get(key).is_some_and(|t| !t.due(now) && !t.failed) {
                without += 1;
            }
        }
        ui.label(format!("Lyrics: {found} found at LRCLIB, {without} not found or with their own"));
        if let Some((_, title)) = &self.lyrics_lookup.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Looking up the lyrics of {title}..."));
            });
        }
        if let Some(error) = &self.lyrics_lookup.error {
            ui.colored_label(ui.visuals().warn_fg_color, format!("The last lyrics lookup failed: {error}"));
        }
    }
}
