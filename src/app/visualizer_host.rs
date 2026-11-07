//! Running the visualizer script: its picture in the panels, the effects
//! it asks for, and the song and transport it sees.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::frame_timing;
use crate::audio::AudioCommand;
use crate::config;
use crate::engine::EngineView;
use crate::lua_visualizer::{
    self, PlaybackRequest,
};
use crate::visualizer::{DisplayMode, NotesSnapshot, VisualizerPanel};

use super::{App, naming, title_of, tour};

impl App {
    /// Script picker + channel toggles + error banner + the live preview.
    pub(super) fn visualizer_preview_ui(
        &mut self,
        ui: &mut egui::Ui,
        mut notes: NotesSnapshot,
        playback: EngineView,
    ) {
        self.script_bar_ui(ui, true);

        self.script_error_ui(ui, true);

        let channels = ui.scope(|ui| self.channels_ui(ui, &mut notes, playback.position)).response.rect;
        self.tour.mark(tour::Target::Channels, channels);

        self.show_visualizer(ui, &notes, &playback, DisplayMode::Window);
    }

    /// Draw (and run) the visualizer into the rest of `ui`, and pass on
    /// whatever it asked for.
    pub(super) fn show_visualizer(&mut self, ui: &mut egui::Ui, notes: &NotesSnapshot, playback: &EngineView, mode: DisplayMode) {
        let _timing = frame_timing::scope("visualizer");
        // A render has its own copy of the script: this one rests meanwhile.
        if self.recording.render.is_some() {
            self.render_progress_ui(ui);
            return;
        }
        let transport = self.script_transport();
        self.script.set_app_log(self.config.script_log_to_app);
        let recording = self.recording.recorder.as_ref().is_some_and(|r| !r.paused());
        let take = self.recording.take();
        self.script.set_recording(recording, take);
        self.script.set_theme(self.current_theme());
        self.sync_script_up_next();
        {
            let mut panel = lock_panel(&self.visualizer);
            panel.set_pads(self.pad_frame.clone());
            panel.set_mobile(self.mobile());
            panel.set_see_through(self.config.visualizer_see_through);
            panel.set_pixel_scale(self.config.visualizer_scale.unwrap_or(config::DEFAULT_VISUALIZER_SCALE));
        }
        // (A program watching through the API: drawn at the size it asked
        // for, unless a recording has a size already.)
        let watched = self.api_watched();
        self.script.set_streaming(watched.is_some());
        let recording_size = self.recording.frame_size();
        let fixed_size = recording_size.or(watched.flatten());
        lock_panel(&self.visualizer).set_off_screen(recording_size.is_none() && watched.flatten().is_some());
        let (hold, timestep) = self.pace_recording();
        self.visualizer_output = lock_panel(&self.visualizer).show(
            ui,
            &self.tap,
            notes,
            playback,
            transport,
            mode,
            fixed_size,
            hold,
            timestep,
        );
        self.feed_recording(ui);
        if self.visualizer_output.rendered && watched.is_some() {
            self.api_offer_frame(fixed_size);
        }
        self.visualizer_drawn = true;
        let effects = lock_panel(&self.visualizer).take_effects();
        self.apply_script_effects(effects, playback);
    }

    /// Carry out what the script asked for while drawing (in the main
    /// window, or the mini player).
    pub(super) fn apply_script_effects(&mut self, effects: crate::script_host::Effects, playback: &EngineView) {
        // A script can ask to mute/unmute a channel itself (e.g. a game
        // script silencing a dead player's channel), the same command the
        // GUI's own toggles send.
        for (channel, enabled) in effects.channel_requests {
            self.send(AudioCommand::SetChannelEnabled(channel, enabled));
        }

        // Notes the script played, for the live synth.
        for command in effects.live_commands {
            self.send(AudioCommand::Live(command));
        }

        // set_paused / seek from the script. `paused` tracks the effect of
        // earlier requests this frame, since the engine's view won't
        // reflect them until next frame.
        let mut paused = playback.paused;
        for request in effects.playback_requests {
            match request {
                PlaybackRequest::Pause(pause) if pause != paused => {
                    self.send(AudioCommand::TogglePause);
                    paused = pause;
                }
                PlaybackRequest::Pause(_) => {}
                PlaybackRequest::Seek(seconds) => self.send(AudioCommand::Seek(seconds)),
                PlaybackRequest::PlayTrack(entry) => {
                    if let Some(list) = self.script_playlist() {
                        self.play_entry(list, entry);
                    }
                }
                PlaybackRequest::NextTrack => self.next_track(),
                PlaybackRequest::PreviousTrack => self.previous_track(playback.position),
                PlaybackRequest::Speed(speed) => self.send(AudioCommand::SetSpeed(speed)),
                PlaybackRequest::Loop(mode) => self.set_loop_mode(mode),
                PlaybackRequest::Shuffle(on) => self.set_shuffle(on),
                PlaybackRequest::RecordingReady => self.recording_ready(),
                PlaybackRequest::RecordingDone => self.recording_take_done(),
            }
        }
    }

    /// A song was just sent to the audio thread: the script gets it (its
    /// notes, id, path and a new `song_loads`) once the audio thread reports
    /// it loaded, so everything `playback()` says changes in the same frame
    /// (`sync_script_song`).
    pub(super) fn song_for_script(&mut self, path: &Path, notes: Arc<crate::midi_notes::NoteList>, id: String) {
        self.loads_sent += 1;
        let (tags, art) = self.script_song(path);
        self.pending_script_song = Some((self.loads_sent, path.to_path_buf(), notes, id, tags));
        let online = self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup);
        let (sender, path) = (self.script_art.0.clone(), path.to_path_buf());
        let spawned = std::thread::Builder::new().name("script art".into()).spawn(move || {
            let art = crate::album_art::for_script(&art, online).map(Arc::new);
            let _ = sender.send((path, art));
        });
        if let Err(e) = spawned {
            log::warn!("couldn't start reading art for the script: {e}");
        }
    }

    /// What a script's told of the song at `path` (`playback()`): its
    /// tags, and where its art comes from (`album_art()`).
    pub(super) fn script_song(&self, path: &Path) -> (lua_visualizer::SongTags, crate::album_art::Source) {
        let song = self.songs.get(path);
        let facts = song.map(crate::fields::Facts::of);
        let text = |field| facts.as_ref().and_then(|f| f.text(field)).map(|t| t.into_owned());
        let tags = lua_visualizer::SongTags {
            title: text(crate::fields::Field::Title),
            artist: text(crate::fields::Field::Artist),
            album: text(crate::fields::Field::Album),
        };
        (tags, crate::album_art::Source::of_song(Some(path.to_path_buf()), song))
    }

    /// Hand the script the song the audio thread now reports as loaded.
    pub(super) fn sync_script_song(&mut self, view: &EngineView) {
        if self.pending_script_song.as_ref().is_some_and(|(target, ..)| view.loads >= *target)
            && let Some((_, path, notes, id, tags)) = self.pending_script_song.take()
        {
            self.song_notes = Some(Arc::clone(&notes));
            self.script_details = Some((path.clone(), u64::MAX));
            self.script.set_song(path, id, notes, tags);
        }
        // Its details again whenever the library changes (a star, a play,
        // an edit).
        let revision = self.songs.revision();
        if let Some((path, made)) = &mut self.script_details
            && *made != revision
        {
            *made = revision;
            self.script.set_song_details(crate::script_song::details(&self.songs, path));
        }
        while let Ok((path, art)) = self.script_art.1.try_recv() {
            self.script.set_art(path, art);
        }
    }

    /// What plays after the song playing (`up_next()`), made again when
    /// the queue, what's planned after it or the library changes.
    fn sync_script_up_next(&mut self) {
        let planned = self.planned();
        let revision = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (&self.queue, &planned, self.songs_revision()).hash(&mut hasher);
            hasher.finish()
        };
        if self.script_up_next == Some(revision) {
            return;
        }
        self.script_up_next = Some(revision);
        let naming = naming(&self.config, &self.songs);
        let queued = self.queue.iter().map(|path| (path, true));
        let songs = queued
            .chain(planned.iter().map(|path| (path, false)))
            .map(|(path, queued)| {
                let song = self.songs.get(path);
                let facts = song.map(crate::fields::Facts::of);
                let text = |field| facts.as_ref().and_then(|f| f.text(field)).map(|t| t.into_owned());
                crate::script_queue::UpNextSong {
                    name: title_of(&mut self.song_info, &naming, path),
                    title: text(crate::fields::Field::Title),
                    artist: text(crate::fields::Field::Artist),
                    album: text(crate::fields::Field::Album),
                    length: facts.as_ref().and_then(|f| f.number(crate::fields::Field::Length)),
                    queued,
                }
            })
            .collect();
        self.script.set_up_next(songs);
    }

    /// The playlist scripts see (`playlist()`): the one playing, else the
    /// one open in Groupings.
    pub(super) fn script_playlist(&self) -> Option<usize> {
        self.now_playing
            .as_ref()
            .map(|np| np.list)
            .or(self.viewed_playlist)
            .filter(|&l| l < self.playlists.lists.len())
    }

    /// What scripts see of the playlist and the loop/shuffle modes.
    pub(super) fn script_transport(&mut self) -> lua_visualizer::Transport {
        let naming = naming(&self.config, &self.songs);
        let playing_entry = self.playing_entry().map(|np| (np.list, np.entry));
        let playlist = self.script_playlist().map(|list| {
            let stored = &self.playlists.lists[list].playlist;
            let playing = playing_entry.filter(|(l, _)| *l == list).map(|(_, entry)| entry);
            let entries = stored
                .entries
                .iter()
                .map(|entry| lua_visualizer::PlaylistEntryView {
                    name: title_of(&mut self.song_info, &naming, &entry.path),
                    length: self.song_info.get(&entry.path).and_then(|info| info.meta.length),
                    missing: entry.missing,
                })
                .collect();
            lua_visualizer::PlaylistView {
                name: stored.name.clone(),
                playing: playing.is_some(),
                current: playing,
                entries,
            }
        });
        lua_visualizer::Transport { loop_mode: self.loop_mode, shuffle: self.shuffle, playlist }
    }
}

/// The visualizer panel, locked for a moment (`App::visualizer`), by the
/// main thread or the thread drawing it off screen for the local API
/// (`api.rs`). Locking it again on a thread that holds it already would
/// freeze the app: that panics instead, so it can't happen quietly.
pub(crate) fn lock_panel(panel: &Mutex<VisualizerPanel>) -> PanelGuard<'_> {
    assert!(!HOLDING_PANEL.replace(true), "the visualizer panel was locked twice at once");
    PanelGuard(panel.lock().unwrap_or_else(|p| p.into_inner()))
}

thread_local! {
    /// Whether this thread has the visualizer panel locked (`lock_panel`).
    static HOLDING_PANEL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The visualizer panel while it's locked (`lock_panel`).
pub(crate) struct PanelGuard<'a>(std::sync::MutexGuard<'a, VisualizerPanel>);

impl std::ops::Deref for PanelGuard<'_> {
    type Target = VisualizerPanel;
    fn deref(&self) -> &VisualizerPanel {
        &self.0
    }
}

impl std::ops::DerefMut for PanelGuard<'_> {
    fn deref_mut(&mut self) -> &mut VisualizerPanel {
        &mut self.0
    }
}

impl Drop for PanelGuard<'_> {
    fn drop(&mut self) {
        HOLDING_PANEL.set(false);
    }
}

/// Start the one-time import of the songs from before the library (the
/// Songs folder, or the starter songs' folder; on Android, the files
/// imported into the library's folder under their own names) and the
/// playlists naming them, on a thread of its own: its result comes back
/// on the returned channel.
/// A song's art read for the script: the song's file, and its art.
pub(super) type ScriptArtRead = (PathBuf, Option<Arc<crate::album_art::ScriptArt>>);
