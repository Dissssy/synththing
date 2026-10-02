//! Starting songs and soundfonts without blocking the UI: requests go to
//! the background `AssetCache`, and the actual "play it" / "use it" happens
//! on whichever later frame finds the load finished. Also decides each
//! frame what to keep loaded and what to preload: the playing track, the
//! next playlist track, the soundfonts in use, and (experimental) whatever
//! song is being hovered.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;

use super::{is_midi_path, App};
use crate::audio::AudioCommand;
use crate::config::nice_name;
use crate::loader::{Asset, AssetKind, LoadState, Priority};
use crate::playlist::{LoopMode, NowPlaying};

/// How long the pointer has to rest on a song before hover preloading
/// starts on it, so sweeping the mouse down a list doesn't start a load for
/// every row it crosses.
const HOVER_PRELOAD_DWELL_SECS: f64 = 0.3;

/// How often to check back while something is loading (or a hover is
/// waiting out its dwell), since a finished load in the background doesn't
/// wake the UI by itself.
const LOADING_REPAINT: Duration = Duration::from_millis(50);

/// A song that's been asked to play, waiting on its file (and soundfont).
pub(super) struct PendingPlay {
    path: PathBuf,
    /// The soundfont it should play with (MIDI only): its playlist override,
    /// else the selected one at the time it was asked for.
    soundfont: Option<PathBuf>,
    /// The playlist position it was started from, if any.
    entry: Option<NowPlaying>,
    /// Failing to load moves on to the next playlist track (auto-advance,
    /// Next) rather than just reporting it (a click on that track).
    skip_on_failure: bool,
}

fn song_kind(path: &Path) -> AssetKind {
    if is_midi_path(path) { AssetKind::Midi } else { AssetKind::Audio }
}

impl App {
    pub(super) fn selected_soundfont(&self) -> Option<PathBuf> {
        self.active_sf.and_then(|i| self.config.soundfonts.get(i)).cloned()
    }

    /// The soundfont `song` should play with: none for plain audio, else
    /// its override, else the selected one.
    fn soundfont_for(&self, song: &Path, soundfont_override: Option<&Path>) -> Option<PathBuf> {
        if !is_midi_path(song) {
            return None;
        }
        soundfont_override.map(Path::to_path_buf).or_else(|| self.selected_soundfont())
    }

    /// Start loading `path` to play it as soon as it (and its soundfont) is
    /// ready, which may be this very frame if it was preloaded. Replaces any
    /// earlier song still waiting to load.
    pub(super) fn request_play(
        &mut self,
        path: &Path,
        soundfont_override: Option<&Path>,
        entry: Option<NowPlaying>,
        skip_on_failure: bool,
    ) {
        let now = Instant::now();
        self.assets.request(path, song_kind(path), Priority::Now, now);
        let soundfont = self.soundfont_for(path, soundfont_override);
        if let Some(sf) = &soundfont
            && self.loaded_sf.as_ref() != Some(sf)
        {
            self.assets.request(sf, AssetKind::SoundFont, Priority::Now, now);
        }
        self.pending_play = Some(PendingPlay { path: path.to_path_buf(), soundfont, entry, skip_on_failure });
        self.advance_armed = false;
        self.status = format!("Loading {}...", nice_name(path));
        self.poll_pending_play();
    }

    /// Make soundfont `idx` the selected one, once it's loaded.
    pub(super) fn activate_soundfont(&mut self, idx: usize) {
        let Some(path) = self.config.soundfonts.get(idx).cloned() else {
            return;
        };
        if self.loaded_sf.as_ref() == Some(&path) {
            self.active_sf = Some(idx);
            self.pending_soundfont = None;
            return;
        }
        self.swap_soundfont(path, Some(idx));
    }

    /// Switch the engine to soundfont `path` once it's loaded (live, if a
    /// song is playing). `select`: also make it the selected one.
    fn swap_soundfont(&mut self, path: PathBuf, select: Option<usize>) {
        self.assets.request(&path, AssetKind::SoundFont, Priority::Now, Instant::now());
        self.status = format!("Loading soundfont {}...", nice_name(&path));
        self.pending_soundfont = Some((select, path));
        self.poll_pending_soundfont();
    }

    /// The playing song's soundfont should be something else now (its
    /// playlist override was set or cleared): swap to it live. Does nothing
    /// for plain audio, or when it's already the one in use.
    pub(super) fn refresh_playing_soundfont(&mut self, soundfont_override: Option<&Path>) {
        let Some(song) = self.current_song.clone() else {
            return;
        };
        let Some(wanted) = self.soundfont_for(&song, soundfont_override) else {
            return;
        };
        if self.loaded_sf.as_ref() != Some(&wanted) {
            self.swap_soundfont(wanted, None);
        }
    }

    /// Pick up finished loads and act on whatever was waiting for them.
    pub(super) fn poll_loads(&mut self) {
        self.assets.poll();
        self.poll_pending_soundfont();
        self.poll_pending_play();
    }

    fn poll_pending_soundfont(&mut self) {
        let Some((idx, path)) = self.pending_soundfont.clone() else {
            return;
        };
        match self.assets.get(&path).cloned() {
            Some(LoadState::Loading) => {}
            None => self.assets.request(&path, AssetKind::SoundFont, Priority::Now, Instant::now()),
            Some(LoadState::Ready(Asset::SoundFont(soundfont))) => {
                self.send(AudioCommand::SetSoundFont(soundfont, nice_name(&path)));
                self.status = format!("Soundfont loaded: {}", nice_name(&path));
                self.loaded_sf = Some(path);
                if let Some(idx) = idx {
                    self.active_sf = Some(idx);
                }
                self.pending_soundfont = None;
            }
            Some(LoadState::Ready(_)) => self.pending_soundfont = None,
            Some(LoadState::Failed(e)) => {
                self.status = format!("Soundfont not loaded: {e}");
                self.pending_soundfont = None;
            }
        }
    }

    fn poll_pending_play(&mut self) {
        let Some(pending) = &self.pending_play else {
            return;
        };
        let path = pending.path.clone();
        let song = match self.assets.get(&path).cloned() {
            None => {
                self.assets.request(&path, song_kind(&path), Priority::Now, Instant::now());
                return;
            }
            Some(LoadState::Loading) => return,
            Some(LoadState::Failed(e)) => {
                self.fail_pending_play(e);
                return;
            }
            Some(LoadState::Ready(asset)) => asset,
        };

        // Swap soundfonts first, so a MIDI file never starts on the wrong
        // one. A soundfont that fails doesn't hold the song up.
        let mut sf_error = None;
        if let Some(sf) = pending.soundfont.clone()
            && self.loaded_sf.as_ref() != Some(&sf)
        {
            match self.assets.get(&sf).cloned() {
                None => {
                    self.assets.request(&sf, AssetKind::SoundFont, Priority::Now, Instant::now());
                    return;
                }
                Some(LoadState::Loading) => return,
                Some(LoadState::Ready(Asset::SoundFont(soundfont))) => {
                    self.send(AudioCommand::SetSoundFont(soundfont, nice_name(&sf)));
                    self.loaded_sf = Some(sf);
                }
                Some(LoadState::Ready(_)) => sf_error = Some("not a soundfont".to_string()),
                Some(LoadState::Failed(e)) => sf_error = Some(e),
            }
        }

        self.pending_play = None;
        let name = nice_name(&path);
        match song {
            Asset::Midi(midi) => {
                self.send(AudioCommand::LoadMidi(midi.file, name.clone()));
                self.visualizer.visualizer_mut().set_note_list(midi.notes);
                self.status = match sf_error {
                    Some(e) => format!("Playing {name}, but its soundfont didn't load: {e}"),
                    None if self.loaded_sf.is_none() => format!("Loaded {name}, pick a soundfont to hear it."),
                    None => format!("Playing: {name}"),
                };
            }
            Asset::Audio(audio) => {
                self.send(AudioCommand::LoadAudioFile(audio, name.clone()));
                self.visualizer.visualizer_mut().set_note_list(Default::default());
                self.status = format!("Playing: {name}");
            }
            Asset::SoundFont(_) => {
                self.status = format!("'{name}' is a soundfont, not a song.");
                return;
            }
        }
        self.browser.set_active(Some(path.clone()));
        self.current_song = Some(path);
        self.advance_armed = false;
        self.skips_in_a_row = 0;
    }

    fn fail_pending_play(&mut self, error: String) {
        let Some(pending) = self.pending_play.take() else {
            return;
        };
        self.status = format!("Failed to read '{}': {error}", nice_name(&pending.path));
        let Some(np) = pending.entry else {
            return;
        };
        let len = match self.playlists.lists.get_mut(np.list) {
            Some(stored) => {
                if let Some(entry) = stored.playlist.entries.get_mut(np.entry) {
                    entry.missing = !entry.path.exists();
                }
                stored.playlist.entries.len()
            }
            None => 0,
        };
        if pending.skip_on_failure {
            self.skips_in_a_row += 1;
            if self.skips_in_a_row < len {
                self.next_track();
            } else {
                self.skips_in_a_row = 0;
                self.status = "Nothing left in this playlist could be played.".to_string();
            }
        }
    }

    /// Say what should stay loaded this frame, start any preloads, and
    /// unload whatever's gone unneeded for longer than the expiry.
    pub(super) fn keep_loaded(&mut self, ctx: &egui::Context) {
        let now = Instant::now();

        // Needed right now, never unloaded while so: anything waiting to
        // play, the playing song, and the soundfont in use (the engine holds
        // those last two anyway, so keeping them costs no extra memory, and
        // replaying or going back to them is instant).
        let mut needed: Vec<PathBuf> = Vec::new();
        if let Some(p) = &self.pending_play {
            needed.push(p.path.clone());
            needed.extend(p.soundfont.clone());
        }
        if let Some((_, p)) = &self.pending_soundfont {
            needed.push(p.clone());
        }
        needed.extend(self.current_song.clone());
        needed.extend(self.loaded_sf.clone());
        for path in &needed {
            self.assets.touch(path, now);
        }

        // Preloads. The selected soundfont, so switching back to it after a
        // playlist override is instant; the next playlist track and its
        // soundfont.
        if let Some(sf) = self.selected_soundfont() {
            self.assets.request(&sf, AssetKind::SoundFont, Priority::Background, now);
        }
        if let Some((song, sf)) = self.upcoming_song() {
            self.assets.request(&song, song_kind(&song), Priority::Background, now);
            if let Some(sf) = sf {
                self.assets.request(&sf, AssetKind::SoundFont, Priority::Background, now);
            }
        }
        self.preload_hovered(ctx, now);

        self.assets.evict(now, self.config.preload_expiry());

        if self.assets.any_loading() || self.pending_play.is_some() || self.pending_soundfont.is_some() {
            ctx.request_repaint_after(LOADING_REPAINT);
        }
    }

    /// The playlist track that'll play after this one, with its soundfont,
    /// if there is one worth preloading. Looping one track has no next.
    fn upcoming_song(&self) -> Option<(PathBuf, Option<PathBuf>)> {
        if self.loop_mode == LoopMode::One {
            return None;
        }
        let np = self.now_playing.as_ref()?;
        let entry = self.playlists.lists.get(np.list)?.playlist.entries.get(np.upcoming?)?;
        if entry.missing {
            return None;
        }
        Some((entry.path.clone(), self.soundfont_for(&entry.path, entry.soundfont.as_deref())))
    }

    /// Experimental: once the pointer has rested on a song (in Songs or a
    /// playlist) for a moment, start loading it, so clicking it plays
    /// sooner.
    fn preload_hovered(&mut self, ctx: &egui::Context, now: Instant) {
        let hovered = self
            .browser
            .hovered()
            .map(|p| (p.to_path_buf(), None))
            .or_else(|| self.hovered_playlist_entry.clone());
        let Some((song, sf_override)) = hovered.filter(|_| self.config.preload_on_hover) else {
            self.hover_since = None;
            return;
        };
        let time = ctx.input(|i| i.time);
        let since = match &self.hover_since {
            Some((p, t)) if *p == song => *t,
            _ => {
                self.hover_since = Some((song.clone(), time));
                time
            }
        };
        let waited = time - since;
        if waited < HOVER_PRELOAD_DWELL_SECS {
            ctx.request_repaint_after(Duration::from_secs_f64(HOVER_PRELOAD_DWELL_SECS - waited));
            return;
        }
        self.assets.request(&song, song_kind(&song), Priority::Background, now);
        if let Some(sf) = self.soundfont_for(&song, sf_override.as_deref()) {
            self.assets.request(&sf, AssetKind::SoundFont, Priority::Background, now);
        }
    }
}
