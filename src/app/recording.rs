//! Recording the visualizer to a video (see `recorder.rs`): starting,
//! pausing and stopping (toolbar buttons, F9 and F10), feeding it each
//! frame, the REC overlay, getting ffmpeg the first time, and the
//! Recording section of Preferences.
//!
//! Every recording is a session: free (start it, stop it), one song from
//! its start to its end, or a playlist from its first track to its last.
//! A session's recorder starts paused and begins once the song is actually
//! playing, so there's no lead-in while it loads.
//!
//! A script with `record_prepare` decides instead: before each song it
//! prepares while nothing is recorded (its songs load paused), says
//! `recording_ready()` when the video should start, and `recording_done()`
//! when the song's take is over (after its own outro, say), or the take
//! ends `TAKE_TAIL` after the song does. A playlist moves on only then.
//! The Record window picks what to record, the script, the video format,
//! and for a script with `record_auto`, whether it plays itself; and Live
//! (recorded as it plays) or Render: made in the background, frame by
//! frame, by its own copy of the script and the synth (`offline::Render`),
//! so no frame can repeat whatever the size, with its progress shown in
//! the visualizer's place meanwhile.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::TextureHandle;

use eframe::egui;

use super::{is_midi_path, listed_song_extensions, open_in_file_manager, App};
use crate::config::nice_name;
use crate::audio::AudioCommand;
use crate::engine::EngineView;
use crate::ffmpeg::{self, DownloadState};
use crate::layout::Section;
use crate::lua_visualizer::{self, RecordKind, RecordPhase, RecordingTake};
use crate::offline::{Render, RenderJob, RenderSong, TAKE_TAIL};
use crate::recorder::{self, CUSTOM_SIZES, Format, Quality, Recorder, Resolution, FRAME_RATES};

/// Recording state the app keeps.
pub struct Recording {
    pub recorder: Option<Recorder>,
    /// Recordings being written out after Stop.
    finishing: Vec<recorder::Finishing>,
    last_feed: Option<Instant>,
    /// Seconds since the last feed, worked out before the frame is drawn.
    step: f64,
    /// The recorder's repeated-frame counts, and when and why they last
    /// went up, for the "frames repeating" warning.
    repeats_seen: recorder::Repeats,
    repeating_since: Option<(Instant, Lag)>,
    download: ffmpeg::Downloader,
    /// The "recording needs ffmpeg" modal is up.
    pub prompt_open: bool,
    /// The session to start once the ffmpeg download finishes.
    start_after_download: Option<RecordDraft>,
    /// The session the recorder belongs to.
    session: Option<Session>,
    /// The Record window is up, with what's picked in it.
    pub window: Option<RecordDraft>,
    /// A background render, while one is running.
    pub render: Option<Render>,
    /// Its preview, as a texture, and the revision it shows.
    render_preview: Option<(u64, TextureHandle)>,
}

/// Why a recording's frames are repeating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lag {
    /// Pictures arrive after their frame was due.
    Drawing,
    /// The encoder had no room for them.
    Encoder,
}

/// What to record, as picked in the Record window.
#[derive(Clone, Debug)]
pub struct RecordDraft {
    kind: RecordKind,
    /// The song a Song recording plays (the one playing, unless another
    /// was chosen).
    song: Option<PathBuf>,
    /// The soundfont a MIDI song plays with.
    soundfont: Option<PathBuf>,
    auto: bool,
    /// Rendered in the background rather than recorded live.
    render: bool,
}

struct Session {
    kind: RecordKind,
    /// The script plays itself (`recording().mode == "auto"`).
    auto: bool,
    /// The script prepares each song and ends each take
    /// (`record_prepare`), as it said when the session started.
    prepared: bool,
    phase: RecordPhase,
    /// `generation` when the session asked for its song: an unprepared
    /// session starts once that changes (the song has started from the top).
    asked_at: u64,
    /// The song being recorded (a Song session ends if another loads).
    song: Option<PathBuf>,
    last_position: f64,
    /// When the song was first seen finished, this take.
    finished_at: Option<Instant>,
    /// The script said this take is over (`recording_done`).
    take_over: bool,
}

impl Session {
    fn take(&self) -> RecordingTake {
        RecordingTake { phase: self.phase, kind: self.kind, auto: self.auto }
    }
}

impl Recording {
    pub fn new() -> Self {
        Self {
            recorder: None,
            finishing: Vec::new(),
            last_feed: None,
            step: 0.0,
            repeats_seen: recorder::Repeats::default(),
            repeating_since: None,
            download: ffmpeg::Downloader::new(),
            prompt_open: false,
            start_after_download: None,
            session: None,
            window: None,
            render: None,
            render_preview: None,
        }
    }

    /// The recording the script is part of, for `recording()`.
    pub fn take(&self) -> Option<RecordingTake> {
        self.session.as_ref().map(Session::take)
    }

    /// A prepared playlist session moves the playlist on itself, when each
    /// take ends, rather than when a song finishes.
    pub fn holds_advance(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.prepared && s.kind == RecordKind::Playlist)
    }

    /// Songs load paused for the script to prepare: each track of a
    /// prepared playlist session, or a prepared song session's song.
    pub fn loads_paused(&self) -> bool {
        self.holds_advance()
            || self.session.as_ref().is_some_and(|s| {
                s.prepared && s.kind == RecordKind::Song && s.phase == RecordPhase::Preparing
            })
    }

    /// Stop the recording without saving it (closing, when asked to).
    pub fn discard(&mut self) {
        self.session = None;
        if let Some(recorder) = self.recorder.take() {
            recorder.discard();
        }
    }

    /// Block until recordings being saved are done, and stop a render,
    /// keeping what it's done (closing the app).
    pub fn wait_for_saves(&mut self) {
        if let Some(render) = self.render.take() {
            render.stop_and_wait();
        }
        for finishing in self.finishing.drain(..) {
            if let Err(e) = finishing.wait() {
                log::warn!("recording failed: {e:#}");
            }
        }
    }

    /// The size frames must be while recording.
    pub fn frame_size(&self) -> Option<(usize, usize)> {
        self.recorder.as_ref().map(|r| (r.format().width, r.format().height))
    }
}

/// How big a saved video is, to say so ("160 MB").
fn file_size(path: &std::path::Path) -> String {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) as f64;
    if bytes >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", bytes / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024.0 * 1024.0 {
        format!("{:.0} MB", bytes / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KB", (bytes / 1024.0).max(1.0))
    }
}

fn fmt_clock(seconds: f64) -> String {
    clock(seconds.max(0.0))
}

fn clock(seconds: f64) -> String {
    let s = seconds as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

impl App {
    fn record_format(&self) -> Format {
        let (width, height) = self.config.record_resolution.size();
        let fps = self.config.record_fps.filter(|f| FRAME_RATES.contains(f)).unwrap_or(recorder::DEFAULT_FPS);
        Format { width, height, fps, quality: self.config.record_quality }
    }

    /// Before the visualizer draws: while recording, render only when a
    /// video frame is due, and step the script's time by exactly the video
    /// time that passed, so the video's motion is even. Returns whether to
    /// hold (show the last frame instead of rendering), and the timestep
    /// for the frame otherwise (`None`: the wall clock).
    pub(super) fn pace_recording(&mut self) -> (bool, Option<f64>) {
        let now = Instant::now();
        let step = self.recording.last_feed.map_or(0.0, |last| now.duration_since(last).as_secs_f64());
        self.recording.step = step;
        self.recording.last_feed = Some(now);
        let recorder = match &self.recording.recorder {
            Some(recorder) if !recorder.paused() && !self.preferences_open => recorder,
            _ => return (false, None),
        };
        let due = recorder.frames_due(step, self.tap.ready_frames());
        if due == 0 {
            return (true, None);
        }
        (false, Some(due as f64 / f64::from(recorder.format().fps)))
    }

    /// F9: stop, or start a free recording (in the mode last picked for
    /// this script).
    pub(super) fn toggle_recording(&mut self, view: &EngineView) {
        if self.recording.recorder.is_some() {
            self.stop_recording();
        } else {
            let auto = self.record_auto();
            self.start_session(RecordDraft { kind: RecordKind::Free, song: None, soundfont: None, auto, render: false }, view);
        }
    }

    /// Start the recorder itself (paused: the session unpauses it).
    fn start_recorder(&mut self, draft: &RecordDraft) -> bool {
        if self.recording.recorder.is_some() {
            return false;
        }
        if self.dedicated.is_none() && !self.is_open(Section::Visualizer) {
            self.status = "Open the visualizer (View > Visualizer) to record it.".to_string();
            return false;
        }
        let Some(ffmpeg) = ffmpeg::find() else {
            self.recording.prompt_open = true;
            self.recording.start_after_download = Some(draft.clone());
            return false;
        };
        let format = self.record_format();
        let output = recorder::output_path(&self.config.recordings_dir());
        match Recorder::start(&ffmpeg, format, self.sample_rate, output) {
            Ok(mut recorder) => {
                recorder.set_paused(true);
                self.recording.recorder = Some(recorder);
                self.recording.last_feed = None;
                self.recording.repeats_seen = recorder::Repeats::default();
                self.recording.repeating_since = None;
                true
            }
            Err(e) => {
                self.status = format!("Couldn't start recording: {e:#}");
                false
            }
        }
    }

    /// Whether the running script plays itself when recorded: the mode
    /// last picked for it (Auto unless Manual was), if it can.
    pub(super) fn record_auto(&self) -> bool {
        self.script.options().record_auto && self.script_record_key().is_none_or(|key| !self.config.record_manual.contains(&key))
    }

    /// The running script, as the Record window remembers its mode.
    fn script_record_key(&self) -> Option<String> {
        self.script.path().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned())
    }

    /// Record: a song from its start, the current playlist from its first
    /// track (both stopping by themselves at the end), or free.
    pub(super) fn start_session(&mut self, draft: RecordDraft, view: &EngineView) {
        if self.recording.recorder.is_some() {
            return;
        }
        if self.recording.render.is_some() {
            self.status = "A render is running: stop it first (synththing > Stop rendering).".to_string();
            return;
        }
        if draft.render && draft.kind != RecordKind::Free {
            self.start_render(draft);
            return;
        }
        let kind = draft.kind;
        // The song playing now, ready to go (with the soundfont asked for),
        // is restarted; another is loaded.
        let song_loaded = |app: &Self, song: &Path| {
            app.current_song.as_deref() == Some(song)
                && (view.has_midi || view.has_audio_file)
                && (!is_midi_path(song) || draft.soundfont.is_none() || app.loaded_sf == draft.soundfont)
        };
        let list = self.now_playing.as_ref().map(|np| np.list).or(self.viewed_playlist);
        let list = list.filter(|&l| self.playlists.lists.get(l).is_some_and(|l| !l.playlist.entries.is_empty()));
        match kind {
            RecordKind::Song if draft.song.is_none() => {
                self.status = "Choose a song to record.".to_string();
                return;
            }
            RecordKind::Playlist if list.is_none() => {
                self.status = "Open a playlist with songs in it (View > Playlists) to record it.".to_string();
                return;
            }
            _ => {}
        }
        if !self.start_recorder(&draft) {
            return;
        }
        let options = self.script.options();
        let prepared = options.record_prepare;
        let auto = draft.auto && options.record_auto;
        let song = if kind == RecordKind::Song { draft.song.clone() } else { self.current_song.clone() };
        // Free, unprepared: nothing to wait for.
        let at_once = kind == RecordKind::Free && !prepared;
        self.recording.session = Some(Session {
            kind,
            auto,
            prepared,
            phase: if at_once { RecordPhase::Recording } else { RecordPhase::Preparing },
            asked_at: view.generation,
            song: song.clone(),
            last_position: 0.0,
            finished_at: None,
            take_over: false,
        });
        if at_once && let Some(recorder) = &mut self.recording.recorder {
            recorder.set_paused(false);
        }
        match kind {
            RecordKind::Song => match song {
                Some(song) if song_loaded(self, &song) => {
                    self.send(AudioCommand::Seek(0.0));
                    // Prepared: it stays paused for the script to start;
                    // otherwise it plays from the top.
                    if view.paused != prepared {
                        self.send(AudioCommand::TogglePause);
                    }
                }
                // (Loaded paused, if prepared: see `loads_paused`.)
                Some(song) => self.request_play(&song, draft.soundfont.as_deref(), None, false),
                None => {}
            },
            // (Loaded paused, if prepared: see `loads_paused`.)
            RecordKind::Playlist => {
                if let Some(list) = list {
                    self.play_entry(list, 0);
                }
            }
            RecordKind::Free => {}
        }
        let how = match (kind, prepared) {
            (_, true) => "The script is getting ready; the video starts when it is.",
            (RecordKind::Song, false) => "Recording this song from the start; it stops when the song ends.",
            (RecordKind::Playlist, false) => "Recording the playlist from the top; it stops after the last song.",
            (RecordKind::Free, false) => "Recording (F9 stops, F10 pauses).",
        };
        self.status = match self.recording.recorder.as_ref() {
            Some(r) => format!("{how} Saving to {}.", r.output().display()),
            None => how.to_string(),
        };
    }

    /// Render `draft` in the background: the script's file as saved (edits
    /// waiting are saved first), its song or the playlist's, each with its
    /// soundfont.
    fn start_render(&mut self, draft: RecordDraft) {
        let Some(ffmpeg) = ffmpeg::find() else {
            self.recording.prompt_open = true;
            self.recording.start_after_download = Some(draft);
            return;
        };
        self.flush_editor();
        let Some(script) = self.script.path().map(PathBuf::from) else {
            self.status = "Save the script to a file first: a render runs its own copy of it.".to_string();
            return;
        };
        let songs: Vec<RenderSong> = match draft.kind {
            RecordKind::Song => draft
                .song
                .iter()
                .map(|path| RenderSong {
                    path: path.clone(),
                    soundfont: draft.soundfont.clone().or_else(|| self.soundfont_for(path, None)),
                })
                .collect(),
            RecordKind::Playlist => {
                let list = self.now_playing.as_ref().map(|np| np.list).or(self.viewed_playlist);
                list.and_then(|l| self.playlists.lists.get(l))
                    .map(|l| {
                        l.playlist
                            .entries
                            .iter()
                            .filter(|e| e.path.exists())
                            .map(|e| RenderSong {
                                path: e.path.clone(),
                                soundfont: self.soundfont_for(&e.path, e.soundfont.as_deref()),
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
            RecordKind::Free => Vec::new(),
        };
        if songs.is_empty() {
            self.status = "Nothing to render: pick a song, or a playlist with songs in it.".to_string();
            return;
        }
        let job = RenderJob {
            script,
            songs,
            kind: draft.kind,
            // (Nobody can play along with a render: a script that can play
            // itself does.)
            auto: true,
            format: self.record_format(),
            output: recorder::output_path(&self.config.recordings_dir()),
            fallback_soundfonts: self.config.soundfonts.clone(),
        };
        log::info!("rendering {} song(s) to {}", job.songs.len(), job.output.display());
        self.status = format!("Rendering to {} in the background.", job.output.display());
        self.recording.render_preview = None;
        self.recording.render = Some(Render::start(job, ffmpeg));
    }

    /// Where the visualizer would be, while a render runs: what it's on,
    /// how far, how fast, a glimpse of it, and Stop.
    pub(super) fn render_progress_ui(&mut self, ui: &mut egui::Ui) {
        let Some(render) = &self.recording.render else { return };
        let status = render.status();
        let elapsed = render.started().elapsed().as_secs_f64();
        let stopping = render.stopping();
        let mut stop = false;
        if let Some((w, h, pixels)) = &status.preview
            && self.recording.render_preview.as_ref().is_none_or(|(rev, _)| *rev != status.preview_revision)
        {
            let image = egui::ColorImage::new(
                [*w, *h],
                pixels
                    .iter()
                    .map(|&p| egui::Color32::from_rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
                    .collect(),
            );
            let texture = ui.ctx().load_texture("render_preview", image, egui::TextureOptions::NEAREST);
            self.recording.render_preview = Some((status.preview_revision, texture));
        }
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.heading("Rendering");
            let song = if status.songs > 1 {
                format!("{} ({} of {})", status.name, status.song + 1, status.songs)
            } else {
                status.name.clone()
            };
            ui.label(song);
            ui.add_space(6.0);
            let (fraction, text) = if status.finishing {
                (1.0, "finishing the video...".to_string())
            } else if status.preparing {
                (0.0, "the script is getting ready...".to_string())
            } else {
                let done = (status.position / status.length.max(0.001)).clamp(0.0, 1.0) as f32;
                (done, format!("{} of {}", fmt_clock(status.position), fmt_clock(status.length)))
            };
            ui.add(egui::ProgressBar::new(fraction).text(text).desired_width(ui.available_width().min(420.0)));
            let speed = if elapsed > 0.5 { status.seconds / elapsed } else { 0.0 };
            ui.weak(format!(
                "{} of video so far, {speed:.1}x real time. Every frame is drawn, however long it takes.",
                fmt_clock(status.seconds)
            ));
            ui.add_space(6.0);
            if let Some((_, texture)) = &self.recording.render_preview {
                let room = egui::vec2(ui.available_width() - 16.0, (ui.available_height() - 48.0).max(60.0));
                let size = texture.size_vec2();
                let scale = (room.x / size.x).min(room.y / size.y).min(2.0);
                ui.image((texture.id(), size * scale));
            }
            ui.add_space(6.0);
            let label = if stopping { "Stopping..." } else { "Stop (keeps what's done)" };
            if ui.add_enabled(!stopping, egui::Button::new(label)).clicked() {
                stop = true;
            }
        });
        if stop && let Some(render) = &self.recording.render {
            render.stop();
        }
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }

    /// `recording_ready()`, or "Start now": a prepared take starts.
    pub(super) fn recording_ready(&mut self) {
        let Some(session) = &mut self.recording.session else { return };
        if !session.prepared || session.phase != RecordPhase::Preparing {
            return;
        }
        session.phase = RecordPhase::Recording;
        session.finished_at = None;
        session.take_over = false;
        if let Some(recorder) = &mut self.recording.recorder {
            recorder.set_paused(false);
        }
    }

    /// `recording_done()`: a prepared take is over.
    pub(super) fn recording_take_done(&mut self) {
        if let Some(session) = &mut self.recording.session
            && session.prepared
            && session.phase == RecordPhase::Recording
        {
            session.take_over = true;
        }
    }

    /// A song was just loaded: in a prepared playlist session, a new take,
    /// prepared before anything's recorded.
    pub(super) fn recording_song_loaded(&mut self) {
        let Some(session) = &mut self.recording.session else { return };
        if !(session.prepared && session.kind == RecordKind::Playlist) {
            return;
        }
        session.phase = RecordPhase::Preparing;
        session.song = self.current_song.clone();
        session.finished_at = None;
        session.take_over = false;
        if let Some(recorder) = &mut self.recording.recorder {
            recorder.set_paused(true);
        }
    }

    /// Per frame: a session starts its recorder once its song is playing
    /// (unprepared), and stops it at the end.
    fn session_tick(&mut self, view: &EngineView) {
        if self.recording.recorder.is_none() {
            self.recording.session = None;
            return;
        }
        let Some(session) = &mut self.recording.session else { return };
        if session.prepared {
            self.prepared_tick(view);
            return;
        }
        if session.phase == RecordPhase::Preparing {
            let its_song = session.kind != RecordKind::Song || self.current_song == session.song;
            if its_song && view.generation != session.asked_at && !view.paused && !view.finished {
                session.phase = RecordPhase::Recording;
                session.last_position = view.position;
                if session.kind == RecordKind::Playlist {
                    session.song = self.current_song.clone();
                }
                if let Some(recorder) = &mut self.recording.recorder {
                    recorder.set_paused(false);
                }
            }
            return;
        }
        let done = match session.kind {
            // The end, another song, or starting over (looping, or a seek
            // back).
            RecordKind::Song => {
                view.finished || self.current_song != session.song || view.position + 0.5 < session.last_position
            }
            // The last track finished with nothing to follow it.
            RecordKind::Playlist => {
                view.finished
                    && self.pending_play.is_none()
                    && self.now_playing.as_ref().is_none_or(|np| np.upcoming.is_none())
            }
            RecordKind::Free => false,
        };
        session.last_position = view.position;
        if done {
            self.stop_recording();
        }
    }

    /// A prepared session's take ends when the script says, or a while
    /// after its song does; then the playlist moves on, or it's over.
    fn prepared_tick(&mut self, view: &EngineView) {
        let Some(session) = &mut self.recording.session else { return };
        // (Not while its song is still loading.)
        if session.kind == RecordKind::Song && self.current_song != session.song && self.pending_play.is_none() {
            self.stop_recording();
            return;
        }
        if session.phase != RecordPhase::Recording || session.kind == RecordKind::Free {
            return;
        }
        if view.finished {
            session.finished_at.get_or_insert_with(Instant::now);
        } else {
            session.finished_at = None;
        }
        if session.take_over || session.finished_at.is_some_and(|t| t.elapsed() >= TAKE_TAIL) {
            self.end_take();
        }
    }

    /// A prepared take is over: on to the next track, or the end.
    fn end_take(&mut self) {
        let Some(session) = &mut self.recording.session else { return };
        session.take_over = false;
        session.finished_at = None;
        let next = session.kind == RecordKind::Playlist
            && self.pending_play.is_none()
            && self.now_playing.as_ref().is_some_and(|np| np.upcoming.is_some());
        if next {
            if let Some(recorder) = &mut self.recording.recorder {
                recorder.set_paused(true);
            }
            if let Some(session) = &mut self.recording.session {
                session.phase = RecordPhase::Preparing;
            }
            self.next_track();
        } else {
            self.stop_recording();
        }
    }

    pub(super) fn stop_recording(&mut self) {
        self.recording.session = None;
        if let Some(recorder) = self.recording.recorder.take() {
            self.status = "Saving the recording...".to_string();
            self.recording.finishing.push(recorder.finish());
        }
    }

    /// F10 / the Pause button.
    pub(super) fn toggle_recording_pause(&mut self) {
        if let Some(recorder) = &mut self.recording.recorder {
            let paused = !recorder.paused();
            recorder.set_paused(paused);
            self.status = if paused { "Recording paused (F10 resumes)." } else { "Recording." }.to_string();
        }
    }

    /// After the visualizer drew (or skipped) a frame: hand it, and the
    /// audio it was given, to the recorder; and draw the REC badge.
    pub(super) fn feed_recording(&mut self, ui: &egui::Ui) {
        let Some(recorder) = &mut self.recording.recorder else { return };
        let dt = self.recording.step;
        // Frozen behind Preferences: nothing's happening, record nothing.
        let result = if self.preferences_open {
            Ok(())
        } else if self.visualizer_output.rendered {
            let mut panel = super::lock_panel(&self.visualizer);
            let (samples, pixels) = panel.recording_parts();
            recorder.feed(dt, samples, Some(pixels))
        } else {
            recorder.feed(dt, &[], None)
        };
        let (seconds, paused) = (recorder.seconds(), recorder.paused());
        // Pictures repeating: drawn too late (the script and the app's own
        // drawing together take longer than a video frame), or skipped (the
        // encoder can't keep up).
        let repeats = recorder.repeated_frames();
        let seen = self.recording.repeats_seen;
        if repeats.late > seen.late + 2 || repeats.behind > seen.behind + 2 {
            let cause = if repeats.behind > seen.behind + 2 { Lag::Encoder } else { Lag::Drawing };
            self.recording.repeats_seen = repeats;
            self.recording.repeating_since = Some((Instant::now(), cause));
        }
        let slow = self.recording.repeating_since.filter(|(t, _)| t.elapsed().as_secs_f64() < 2.0).map(|(_, c)| c);
        let waiting = self.recording.session.as_ref().filter(|s| s.phase == RecordPhase::Preparing).map(|s| s.prepared);
        if let Err(e) = result {
            self.status = format!("Recording stopped: {e:#}");
            if let Some(recorder) = self.recording.recorder.take() {
                self.recording.finishing.push(recorder.finish());
            }
            return;
        }

        // The badge goes on top of the picture on screen, not into it.
        let rect = self.visualizer_output.image_rect;
        if rect.is_positive() {
            let painter = ui.painter();
            let at = rect.left_top() + egui::vec2(12.0, 12.0);
            let state = if waiting == Some(true) {
                "PREPARING"
            } else if waiting == Some(false) {
                "STARTING"
            } else if paused {
                "PAUSED"
            } else {
                "REC"
            };
            let mut text = format!("{state} {}", clock(seconds));
            match slow {
                Some(Lag::Drawing) => text.push_str(
                    "  - frames repeating: drawing this size takes longer than a frame (script and app together)",
                ),
                Some(Lag::Encoder) => text.push_str("  - frames repeating: the encoder can't keep up at this size"),
                None => {}
            }
            let galley = painter.layout_no_wrap(text, egui::FontId::proportional(14.0), egui::Color32::WHITE);
            let badge = egui::Rect::from_min_size(at, galley.size() + egui::vec2(30.0, 8.0));
            painter.rect_filled(badge, 4.0, egui::Color32::from_black_alpha(170));
            let dot = if paused { egui::Color32::GRAY } else { egui::Color32::from_rgb(230, 40, 40) };
            painter.circle_filled(badge.left_center() + egui::vec2(12.0, 0.0), 5.0, dot);
            painter.galley(badge.left_top() + egui::vec2(22.0, 4.0), galley, egui::Color32::WHITE);
            ui.ctx().request_repaint();
        }
    }

    /// Per frame: sessions starting and stopping, and recordings that
    /// finished writing (say where they went).
    pub(super) fn poll_recordings(&mut self, view: &EngineView) {
        self.session_tick(view);
        if let Some(result) = self.recording.render.as_mut().and_then(Render::poll) {
            self.recording.render = None;
            self.recording.render_preview = None;
            match result {
                Ok(path) => {
                    log::info!("rendered {}", path.display());
                    self.status = format!("Rendered: {} ({})", path.display(), file_size(&path));
                    self.last_recording = Some(path);
                }
                Err(e) => {
                    log::warn!("render failed: {e}");
                    self.status = format!("The render didn't finish: {e}");
                }
            }
        }
        let mut done = Vec::new();
        self.recording.finishing.retain(|f| match f.poll() {
            Some(result) => {
                done.push(result);
                false
            }
            None => true,
        });
        for result in done {
            match result {
                Ok(path) => {
                    log::info!("saved recording {}", path.display());
                    self.status = format!("Saved the recording: {} ({})", path.display(), file_size(&path));
                    self.last_recording = Some(path);
                }
                Err(e) => {
                    log::warn!("recording failed: {e:#}");
                    self.status = format!("The recording couldn't be saved: {e:#}");
                }
            }
        }
        if let DownloadState::Done(_) = self.recording.download.state()
            && let Some(draft) = self.recording.start_after_download.take()
        {
            self.recording.prompt_open = false;
            self.start_session(draft, view);
        }
    }

    /// Open the Record window.
    pub(super) fn open_record_window(&mut self) {
        if self.recording.window.is_none() {
            let kind = if self.current_song.is_some() { RecordKind::Song } else { RecordKind::Free };
            let auto = self.record_auto();
            let soundfont = self.loaded_sf.clone().or_else(|| self.selected_soundfont());
            let render = self.recording.window.as_ref().is_some_and(|d| d.render);
            self.recording.window = Some(RecordDraft { kind, song: self.current_song.clone(), soundfont, auto, render });
        }
    }

    /// The synththing menu's recording items.
    pub(super) fn recording_menu_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(render) = &self.recording.render {
            if ui.add_enabled(!render.stopping(), egui::Button::new("Stop rendering")).clicked() {
                render.stop();
            }
            return;
        }
        match &self.recording.recorder {
            None => {
                if ui.button("Record...").on_hover_text("Record the visualizer to a video. F9 starts a free recording straight away.").clicked() {
                    self.open_record_window();
                }
            }
            Some(_) => {
                self.recording_controls_ui(ui);
            }
        }
    }

    /// Start now / Pause / Stop while recording: next to "Fullscreen
    /// visualizer", and in the synththing menu.
    fn recording_controls_ui(&mut self, ui: &mut egui::Ui) {
        let Some(recorder) = &self.recording.recorder else { return };
        let paused = recorder.paused();
        let preparing = self.recording.session.as_ref().is_some_and(|s| s.prepared && s.phase == RecordPhase::Preparing);
        if preparing {
            if ui
                .button("Start now")
                .on_hover_text("Start recording without waiting for the script to say it's ready.")
                .clicked()
            {
                self.recording_ready();
            }
        } else if ui.button(if paused { "Resume recording" } else { "Pause recording" }).on_hover_text("F10").clicked() {
            self.toggle_recording_pause();
        }
        if ui.button("Stop recording").on_hover_text("Stop and save the video (F9)").clicked() {
            self.stop_recording();
        }
    }

    /// Next to "Fullscreen visualizer": a recording's controls while there
    /// is one, saving, and "Show last recording".
    pub(super) fn recording_buttons_ui(&mut self, ui: &mut egui::Ui) {
        self.recording_controls_ui(ui);
        if !self.recording.finishing.is_empty() {
            ui.add(egui::Spinner::new());
            ui.weak("Saving...");
        } else if let Some(path) = self.last_recording.clone()
            && self.recording.recorder.is_none()
            && ui
                .small_button("Show last recording")
                .on_hover_text(format!("{} ({})", path.display(), file_size(&path)))
                .clicked()
        {
            open_in_file_manager(&path);
        }
    }

    /// "Recording needs ffmpeg": find it, or download it.
    pub(super) fn ffmpeg_prompt_ui(&mut self, ctx: &egui::Context) {
        if !self.recording.prompt_open {
            return;
        }
        let state = self.recording.download.state();
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("ffmpeg_prompt")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Recording needs ffmpeg");
            ui.add_space(6.0);
            ui.label(
                "synththing records with ffmpeg, the standard free video tool, which isn't \
                 part of synththing itself.",
            );
            ui.add_space(6.0);
            if !ffmpeg::CAN_DOWNLOAD {
                ui.label("Install it with your system's package manager (e.g. `apt install ffmpeg`), then record again.");
            } else {
                match &state {
                    DownloadState::Idle | DownloadState::Failed(_) => {
                        ui.label(
                            "Download it now? It's the \"release essentials\" build from gyan.dev \
                             (about 115 MB), checked against its published checksum and kept in \
                             synththing's own folder. One on your PATH works too.",
                        );
                        if let DownloadState::Failed(e) = &state {
                            ui.colored_label(ui.visuals().error_fg_color, format!("The download failed: {e}"));
                        }
                    }
                    DownloadState::Downloading { done, total } => {
                        let mb = |b: u64| b as f32 / 1_048_576.0;
                        match total {
                            Some(total) => {
                                ui.add(
                                    egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32)
                                        .text(format!("{:.0} / {:.0} MB", mb(*done), mb(*total))),
                                );
                            }
                            None => {
                                ui.label(format!("Downloaded {:.0} MB...", mb(*done)));
                            }
                        }
                        ui.ctx().request_repaint();
                    }
                    DownloadState::Done(path) => {
                        ui.label(format!("ffmpeg is ready ({}).", path.display()));
                    }
                }
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                let downloading = matches!(state, DownloadState::Downloading { .. });
                if ui.button(if downloading { "Hide" } else { "Cancel" }).clicked() {
                    close = true;
                }
                if ffmpeg::CAN_DOWNLOAD
                    && matches!(state, DownloadState::Idle | DownloadState::Failed(_))
                    && ui.button("Download and record").clicked()
                {
                    self.recording.download.start();
                    // (From Preferences, with nothing asked for yet: a free one.)
                    if self.recording.start_after_download.is_none() {
                        let auto = self.record_auto();
                        self.recording.start_after_download =
                            Some(RecordDraft { kind: RecordKind::Free, song: None, soundfont: None, auto, render: false });
                    }
                }
            });
        });
        if close || response.should_close() {
            self.recording.prompt_open = false;
            self.recording.start_after_download = None;
        }
    }

    /// The Recording section of Preferences.
    pub(super) fn recording_preferences_ui(&mut self, ui: &mut egui::Ui) {
        self.record_format_ui(ui);
        let dir = self.config.recordings_dir();
        let mut pick_folder = false;
        ui.horizontal(|ui| {
            ui.label("Saved to");
            ui.weak(dir.display().to_string());
        });
        ui.horizontal(|ui| {
            if ui.button("Change...").clicked() {
                pick_folder = true;
            }
            if ui.button("Open").clicked() {
                let _ = std::fs::create_dir_all(&dir);
                open_in_file_manager(&dir.join("."));
            }
        });
        match ffmpeg::find() {
            Some(path) => {
                ui.weak(format!("Encoding with {}", path.display()));
            }
            None => {
                ui.horizontal(|ui| {
                    ui.weak("ffmpeg isn't set up yet.");
                    if ffmpeg::CAN_DOWNLOAD && ui.small_button("Get ffmpeg...").clicked() {
                        self.recording.prompt_open = true;
                    }
                });
            }
        }
        if pick_folder && let Some(folder) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
            self.config.record_dir = Some(folder);
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
    }

    /// Size, frame rate and quality (Preferences, and the Record window).
    fn record_format_ui(&mut self, ui: &mut egui::Ui) {
        let mut resolution = self.config.record_resolution;
        let mut fps = self.config.record_fps.filter(|f| FRAME_RATES.contains(f)).unwrap_or(recorder::DEFAULT_FPS);
        let mut quality = self.config.record_quality;
        let recording = self.recording.recorder.is_some();
        if recording {
            ui.weak("(Can't be changed while recording.)");
        }
        ui.add_enabled_ui(!recording, |ui| {
            ui.horizontal(|ui| {
                ui.label("Size");
                egui::ComboBox::from_id_salt("record_resolution")
                    .selected_text(resolution.label())
                    .show_ui(ui, |ui| {
                        for option in Resolution::ALL {
                            if option == Resolution::V720 {
                                ui.separator();
                            }
                            ui.selectable_value(&mut resolution, option, option.label());
                        }
                        ui.separator();
                        let custom = matches!(resolution, Resolution::Custom(..));
                        if ui.selectable_label(custom, "Custom...").clicked() && !custom {
                            // (starting from the size picked so far)
                            let (w, h) = resolution.size();
                            resolution = Resolution::custom(w as u32, h as u32);
                        }
                    });
                ui.label("at");
                egui::ComboBox::from_id_salt("record_fps").selected_text(format!("{fps} fps")).show_ui(ui, |ui| {
                    for option in FRAME_RATES {
                        ui.selectable_value(&mut fps, option, format!("{option} fps"));
                    }
                });
                super::info_icon(ui).on_hover_text(
                    "Records just the visualizer, at this size whatever the window's size, with what you \
                     hear (your own notes included, before the volume slider). F9 starts and stops, F10 \
                     pauses. Bigger sizes cost the script more time per frame.",
                );
            });
            if let Resolution::Custom(w, h) = resolution {
                let (mut w, mut h) = (w, h);
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    let range = CUSTOM_SIZES;
                    ui.add(egui::DragValue::new(&mut w).range(range.clone()).speed(2.0).suffix(" wide"));
                    ui.label("x");
                    ui.add(egui::DragValue::new(&mut h).range(range).speed(2.0).suffix(" high"));
                    if ui.small_button("Swap").on_hover_text("Landscape to portrait, or back").clicked() {
                        std::mem::swap(&mut w, &mut h);
                    }
                });
                resolution = Resolution::custom(w, h);
            }
            let quality_info = match quality {
                Quality::Standard => {
                    "Standard plays everywhere (browsers, Discord, phones). Color is stored at half \
                     resolution, so single-pixel colored details soften slightly."
                }
                Quality::Sharp => {
                    "Sharp keeps full color resolution: pixel edges stay exact. Plays in desktop players \
                     (VLC, mpv, Windows' Media Player), not reliably in browsers or Discord's player."
                }
                Quality::Lossless => {
                    "Lossless keeps every pixel exactly, for editing: files are many times bigger, and few \
                     players besides VLC and mpv open them."
                }
            };
            super::with_info(ui, quality_info, |ui| {
                let label = ui.label("Quality");
                let combo = egui::ComboBox::from_id_salt("record_quality").selected_text(quality.label()).show_ui(
                    ui,
                    |ui| {
                        for option in Quality::ALL {
                            ui.selectable_value(&mut quality, option, option.label());
                        }
                    },
                );
                label | combo.response
            });
        });
        let (width, height) = resolution.size();
        if width * height > 1920 * 1080 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{:.1}x the pixels of 1080p: most scripts can't draw that {fps} times a second. \
                     When one can't keep up, the video repeats frames (the REC badge says so); \
                     try it with `run-script --video` first, or record at 30 fps.",
                    (width * height) as f32 / (1920.0 * 1080.0)
                ),
            );
        }
        let fps_setting = (fps != recorder::DEFAULT_FPS).then_some(fps);
        let changed = resolution != self.config.record_resolution
            || fps_setting != self.config.record_fps
            || quality != self.config.record_quality;
        if changed {
            self.config.record_resolution = resolution;
            self.config.record_fps = fps_setting;
            self.config.record_quality = quality;
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
    }

    /// The Record window: what to record, with which script, the video's
    /// format, and (for a script that can) whether it plays itself.
    pub(super) fn record_window_ui(&mut self, ctx: &egui::Context, view: &EngineView) {
        let Some(mut draft) = self.recording.window.clone() else { return };
        let mut start = false;
        let mut close = false;
        let mut picked_script = None;
        let options = self.script.options();
        let script_error = self.script.error().is_some();
        let mut choose_song = false;
        let has_playlist = self
            .now_playing
            .as_ref()
            .map(|np| np.list)
            .or(self.viewed_playlist)
            .is_some_and(|l| self.playlists.lists.get(l).is_some_and(|l| !l.playlist.entries.is_empty()));
        let response = egui::Modal::new(egui::Id::new("record_window")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Record");
            ui.add_space(6.0);
            egui::Grid::new("record_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("What");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut draft.kind, RecordKind::Song, "A song, from the start")
                            .on_hover_text("Plays the song from the top and records until it ends.");
                        let name = draft.song.as_deref().map(nice_name);
                        let playing = draft.song.is_some() && draft.song == self.current_song;
                        match name {
                            Some(name) if playing => ui.label(format!("{name} (playing)")),
                            Some(name) => ui.label(name),
                            None => ui.weak("(none yet)"),
                        };
                        if ui.button("Choose...").clicked() {
                            choose_song = true;
                        }
                    });
                    if draft.song.as_deref().is_some_and(is_midi_path) && !self.config.soundfonts.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            ui.label("Soundfont");
                            let shown = draft.soundfont.as_deref().map(nice_name).unwrap_or_else(|| "(none)".to_string());
                            egui::ComboBox::from_id_salt("record_soundfont").selected_text(shown).show_ui(ui, |ui| {
                                for sf in &self.config.soundfonts {
                                    ui.selectable_value(&mut draft.soundfont, Some(sf.clone()), nice_name(sf));
                                }
                            });
                        });
                    }
                    ui.add_enabled_ui(has_playlist, |ui| {
                        ui.radio_value(&mut draft.kind, RecordKind::Playlist, "This playlist, from the start")
                            .on_hover_text(
                                "Plays the current (or open) playlist from its first song and records until the last one ends.",
                            )
                            .on_disabled_hover_text("Open a playlist with songs in it (View > Playlists).");
                    });
                    ui.radio_value(&mut draft.kind, RecordKind::Free, "Free: start now, stop it yourself (F9)");
                });
                ui.end_row();

                ui.label("Script");
                ui.vertical(|ui| {
                    let current = self
                        .active_script
                        .and_then(|i| self.available_scripts.get(i))
                        .map(|p| lua_visualizer::display_name(p))
                        .unwrap_or_else(|| "(none)".to_string());
                    egui::ComboBox::from_id_salt("record_script").selected_text(current).height(f32::INFINITY).show_ui(ui, |ui| {
                        picked_script = self.script_list_ui(ui);
                    });
                    ui.weak("Picking one here switches the visualizer to it.");
                });
                ui.end_row();

                ui.label("How");
                ui.vertical(|ui| {
                    let free = draft.kind == RecordKind::Free;
                    ui.radio_value(&mut draft.render, false, "Live: recorded as it plays")
                        .on_hover_text("What you see and hear, as it happens; you can play along.");
                    ui.add_enabled_ui(!free, |ui| {
                        ui.radio_value(&mut draft.render, true, "Render: made in the background")
                            .on_hover_text(
                                "Every frame drawn, however long each takes, with the sound exactly in step: no \
                                 repeated frames at any size. Faster or slower than real time; the visualizer \
                                 shows its progress meanwhile. Nothing to play along with.",
                            )
                            .on_disabled_hover_text("A song or a playlist, not a free recording.");
                    });
                });
                ui.end_row();

                ui.label("Video");
                ui.vertical(|ui| {
                    self.record_format_ui(ui);
                    ui.weak(format!("Saved to {} (Preferences > Recording)", self.config.recordings_dir().display()));
                });
                ui.end_row();

                ui.label("Playing");
                ui.vertical(|ui| {
                    let rendering = draft.render && draft.kind != RecordKind::Free;
                    if options.record_auto && rendering {
                        ui.weak("Rendered, it plays itself (Auto).");
                    } else if options.record_auto {
                        ui.radio_value(&mut draft.auto, true, "Auto: the script plays itself");
                        ui.radio_value(&mut draft.auto, false, "Manual: you play it");
                    } else if rendering {
                        ui.weak("Rendered, it runs on its own: input isn't recorded.");
                    } else {
                        ui.weak("This script doesn't play itself: what you do is what's recorded.");
                    }
                    if options.record_prepare {
                        ui.weak("It gets ready before each song; the video starts when it is.");
                    }
                });
                ui.end_row();
            });
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                let ok = match draft.kind {
                    RecordKind::Song => draft.song.is_some(),
                    RecordKind::Playlist => has_playlist,
                    RecordKind::Free => true,
                };
                let rendering = draft.render && draft.kind != RecordKind::Free;
                let label = egui::RichText::new(if rendering { "Render" } else { "Record" })
                    .color(egui::Color32::from_rgb(230, 70, 70));
                if ui.add_enabled(ok && !script_error, egui::Button::new(label)).clicked() {
                    start = true;
                }
                if script_error {
                    ui.colored_label(ui.visuals().error_fg_color, "The script has an error.");
                }
            });
        });
        if choose_song
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Songs", listed_song_extensions(&self.config))
                .set_directory(self.config.browse_start_dir())
                .pick_file()
        {
            draft.song = Some(path);
            draft.kind = RecordKind::Song;
        }
        if options.record_auto && draft.auto != self.record_auto()
            && let Some(key) = self.script_record_key()
        {
            self.config.record_manual.retain(|k| *k != key);
            if !draft.auto {
                self.config.record_manual.push(key);
            }
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
        if let Some(idx) = picked_script {
            self.load_script(idx);
            // (The new script's own Auto/Manual, once it's compiled.)
            draft.auto = self.record_auto();
        }
        self.recording.window = Some(draft.clone());
        if start {
            self.recording.window = None;
            self.start_session(draft, view);
        } else if close || response.should_close() {
            self.recording.window = None;
        }
    }
}
