//! Recording the visualizer to a video (see `recorder.rs`): starting,
//! pausing and stopping (toolbar buttons, F9 and F10), feeding it each
//! frame, the REC overlay, getting ffmpeg the first time, and the
//! Recording section of Preferences.
//!
//! Besides a free recording (start it, stop it), a recording can be a
//! session that starts and stops itself: one song from its start to its
//! end, or a playlist from its first track to its last. A session's
//! recorder starts paused and begins once the song is actually playing,
//! so there's no lead-in while it loads.

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

use super::{open_in_file_manager, App};
use crate::audio::AudioCommand;
use crate::engine::EngineView;
use crate::ffmpeg::{self, DownloadState};
use crate::layout::Section;
use crate::recorder::{self, Format, Quality, Recorder, Resolution, FRAME_RATES};

/// Recording state the app keeps.
pub struct Recording {
    pub recorder: Option<Recorder>,
    /// Recordings being written out after Stop.
    finishing: Vec<recorder::Finishing>,
    last_feed: Option<Instant>,
    /// Seconds since the last feed, worked out before the frame is drawn.
    step: f64,
    /// The recorder's repeated-frame count, and when it last went up, for
    /// the "script too slow" warning.
    repeats_seen: u64,
    repeating_since: Option<Instant>,
    download: ffmpeg::Downloader,
    /// The "recording needs ffmpeg" modal is up.
    pub prompt_open: bool,
    /// Start recording once the ffmpeg download finishes.
    start_after_download: bool,
    /// The recording is a self-running session.
    session: Option<Session>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// The current song, from its start to its end.
    Song,
    /// The current (or shown) playlist, from its first track to its last.
    Playlist,
}

struct Session {
    kind: SessionKind,
    /// `generation` when the session asked for its song: it starts once
    /// that changes (the song has started from the top).
    asked_at: u64,
    started: bool,
    /// The song a Song session records.
    song: Option<PathBuf>,
    last_position: f64,
}

impl Recording {
    pub fn new() -> Self {
        Self {
            recorder: None,
            finishing: Vec::new(),
            last_feed: None,
            step: 0.0,
            repeats_seen: 0,
            repeating_since: None,
            download: ffmpeg::Downloader::new(),
            prompt_open: false,
            start_after_download: false,
            session: None,
        }
    }

    /// Block until recordings being saved are done (closing the app).
    pub fn wait_for_saves(&mut self) {
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
    /// hold (show the last frame instead of rendering).
    pub(super) fn pace_recording(&mut self) -> bool {
        let now = Instant::now();
        let step = self.recording.last_feed.map_or(0.0, |last| now.duration_since(last).as_secs_f64());
        self.recording.step = step;
        self.recording.last_feed = Some(now);
        let script = self.visualizer.visualizer_mut();
        let recorder = match &self.recording.recorder {
            Some(recorder) if !recorder.paused() && !self.preferences_open => recorder,
            _ => {
                script.set_fixed_timestep(None);
                return false;
            }
        };
        let due = recorder.frames_due(step, self.tap.ready_frames());
        if due == 0 {
            return true;
        }
        script.set_fixed_timestep(Some(due as f64 / f64::from(recorder.format().fps)));
        false
    }

    /// F9 / the Record and Stop buttons.
    pub(super) fn toggle_recording(&mut self) {
        if self.recording.recorder.is_some() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    pub(super) fn start_recording(&mut self) {
        if self.recording.recorder.is_some() {
            return;
        }
        if self.dedicated.is_none() && !self.is_open(Section::Visualizer) {
            self.status = "Open the visualizer (View > Visualizer) to record it.".to_string();
            return;
        }
        let Some(ffmpeg) = ffmpeg::find() else {
            self.recording.prompt_open = true;
            return;
        };
        let format = self.record_format();
        let output = recorder::output_path(&self.config.recordings_dir());
        match Recorder::start(&ffmpeg, format, self.sample_rate, output) {
            Ok(recorder) => {
                self.status = format!("Recording to {} (F9 stops, F10 pauses).", recorder.output().display());
                self.recording.recorder = Some(recorder);
                self.recording.last_feed = None;
                self.recording.repeats_seen = 0;
                self.recording.repeating_since = None;
            }
            Err(e) => self.status = format!("Couldn't start recording: {e:#}"),
        }
    }

    /// Record the current song from its start, or the current playlist from
    /// its first track, stopping by itself at the end.
    pub(super) fn start_session(&mut self, kind: SessionKind, view: &EngineView) {
        if self.recording.recorder.is_some() {
            return;
        }
        match kind {
            SessionKind::Song => {
                if self.current_song.is_none() || !(view.has_midi || view.has_audio_file) {
                    self.status = "Play a song first, then record it from the start.".to_string();
                    return;
                }
            }
            SessionKind::Playlist => {
                let list = self.now_playing.as_ref().map(|np| np.list).or(self.viewed_playlist);
                let Some(list) = list.filter(|&l| self.playlists.lists.get(l).is_some_and(|l| !l.playlist.entries.is_empty()))
                else {
                    self.status = "Open a playlist with songs in it (View > Playlists) to record it.".to_string();
                    return;
                };
                self.start_recording();
                if self.recording.recorder.is_none() {
                    return;
                }
                self.play_entry(list, 0);
            }
        }
        if kind == SessionKind::Song {
            self.start_recording();
            if self.recording.recorder.is_none() {
                return;
            }
            self.send(AudioCommand::Seek(0.0));
            if view.paused {
                self.send(AudioCommand::TogglePause);
            }
        }
        if let Some(recorder) = &mut self.recording.recorder {
            recorder.set_paused(true);
        }
        self.recording.session = Some(Session {
            kind,
            asked_at: view.generation,
            started: false,
            song: self.current_song.clone(),
            last_position: 0.0,
        });
        self.status = match kind {
            SessionKind::Song => "Recording this song from the start; it stops when the song ends.",
            SessionKind::Playlist => "Recording the playlist from the top; it stops after the last song.",
        }
        .to_string();
    }

    /// A session starts its recorder once its song is playing, and stops it
    /// at the end.
    fn session_tick(&mut self, view: &EngineView) {
        if self.recording.recorder.is_none() {
            self.recording.session = None;
            return;
        }
        let Some(session) = &mut self.recording.session else { return };
        if !session.started {
            if view.generation != session.asked_at && !view.paused && !view.finished {
                session.started = true;
                session.last_position = view.position;
                if session.kind == SessionKind::Playlist {
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
            SessionKind::Song => {
                view.finished
                    || self.current_song != session.song
                    || view.position + 0.5 < session.last_position
            }
            // The last track finished with nothing to follow it.
            SessionKind::Playlist => {
                view.finished
                    && self.pending_play.is_none()
                    && self.now_playing.as_ref().is_none_or(|np| np.upcoming.is_none())
            }
        };
        session.last_position = view.position;
        if done {
            self.recording.session = None;
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
            let (samples, pixels) = self.visualizer.recording_parts();
            recorder.feed(dt, samples, Some(pixels))
        } else {
            recorder.feed(dt, &[], None)
        };
        let (seconds, paused) = (recorder.seconds(), recorder.paused());
        // Pictures repeating: the script can't draw this size fast enough.
        let repeats = recorder.repeated_frames();
        if repeats > self.recording.repeats_seen + 2 {
            self.recording.repeats_seen = repeats;
            self.recording.repeating_since = Some(Instant::now());
        }
        let slow = self.recording.repeating_since.is_some_and(|t| t.elapsed().as_secs_f64() < 2.0);
        let waiting = self.recording.session.as_ref().is_some_and(|s| !s.started);
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
            let state = if waiting {
                "STARTING"
            } else if paused {
                "PAUSED"
            } else {
                "REC"
            };
            let mut text = format!("{state} {}", clock(seconds));
            if slow {
                text.push_str("  - frames repeating: the script is too slow at this size");
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
                    self.status = format!("Saved the recording: {}", path.display());
                    self.last_recording = Some(path);
                }
                Err(e) => {
                    log::warn!("recording failed: {e:#}");
                    self.status = format!("The recording couldn't be saved: {e:#}");
                }
            }
        }
        if let DownloadState::Done(_) = self.recording.download.state()
            && self.recording.start_after_download
        {
            self.recording.start_after_download = false;
            self.recording.prompt_open = false;
            self.start_recording();
        }
    }

    /// Record / Pause / Stop, next to "Fullscreen visualizer".
    pub(super) fn recording_buttons_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        let format = self.record_format();
        match &self.recording.recorder {
            None => {
                let label = egui::RichText::new("Record").color(egui::Color32::from_rgb(230, 70, 70));
                let mut choice = None;
                ui.menu_button(label, |ui| {
                    ui.weak(format!(
                        "{} at {} fps (Preferences > Recording)",
                        self.config.record_resolution.label(),
                        format.fps
                    ));
                    if ui
                        .button("This song, from the start")
                        .on_hover_text("Starts the current song over and records until it ends.")
                        .clicked()
                    {
                        choice = Some(Some(SessionKind::Song));
                    }
                    if ui
                        .button("This playlist, from the start")
                        .on_hover_text("Plays the current (or open) playlist from its first song and records until the last one ends.")
                        .clicked()
                    {
                        choice = Some(Some(SessionKind::Playlist));
                    }
                    if ui.button("Free: start now, stop it yourself (F9)").clicked() {
                        choice = Some(None);
                    }
                });
                match choice {
                    Some(Some(kind)) => self.start_session(kind, view),
                    Some(None) => self.start_recording(),
                    None => {}
                }
            }
            Some(recorder) => {
                let paused = recorder.paused();
                if ui.button(if paused { "Resume" } else { "Pause" }).on_hover_text("F10").clicked() {
                    self.toggle_recording_pause();
                }
                if ui.button("Stop").on_hover_text("Stop and save the video (F9)").clicked() {
                    self.stop_recording();
                }
            }
        }
        if !self.recording.finishing.is_empty() {
            ui.add(egui::Spinner::new());
            ui.weak("Saving...");
        } else if let Some(path) = self.last_recording.clone()
            && self.recording.recorder.is_none()
            && ui.small_button("Show last recording").on_hover_text(path.display().to_string()).clicked()
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
                    self.recording.start_after_download = true;
                }
            });
        });
        if close || response.should_close() {
            self.recording.prompt_open = false;
            self.recording.start_after_download = false;
        }
    }

    /// The Recording section of Preferences.
    pub(super) fn recording_preferences_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("Recording");
        let mut resolution = self.config.record_resolution;
        let mut fps = self.config.record_fps.filter(|f| FRAME_RATES.contains(f)).unwrap_or(recorder::DEFAULT_FPS);
        let mut quality = self.config.record_quality;
        let recording = self.recording.recorder.is_some();
        ui.add_enabled_ui(!recording, |ui| {
            ui.horizontal(|ui| {
                ui.label("Size");
                egui::ComboBox::from_id_salt("record_resolution")
                    .selected_text(resolution.label())
                    .show_ui(ui, |ui| {
                        for option in Resolution::ALL {
                            ui.selectable_value(&mut resolution, option, option.label());
                        }
                    });
                ui.label("at");
                egui::ComboBox::from_id_salt("record_fps").selected_text(format!("{fps} fps")).show_ui(ui, |ui| {
                    for option in FRAME_RATES {
                        ui.selectable_value(&mut fps, option, format!("{option} fps"));
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label("Quality");
                egui::ComboBox::from_id_salt("record_quality").selected_text(quality.label()).show_ui(ui, |ui| {
                    for option in Quality::ALL {
                        ui.selectable_value(&mut quality, option, option.label());
                    }
                });
            });
        });
        ui.weak(match quality {
            Quality::Standard => {
                "Plays everywhere (browsers, Discord, phones). Color is stored at half resolution, \
                 so single-pixel colored details soften slightly."
            }
            Quality::Sharp => {
                "Full color resolution: pixel edges stay exact. Plays in desktop players (VLC, mpv, \
                 Windows' Media Player), not reliably in browsers or Discord's player."
            }
            Quality::Lossless => {
                "Every pixel exactly, for editing: files are many times bigger, and few players \
                 besides VLC and mpv open them."
            }
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
        ui.weak(
            "Records just the visualizer, at this size whatever the window's size, with what you \
             hear (your own notes included, before the volume slider). F9 starts and stops, F10 \
             pauses. Bigger sizes cost the script more time per frame.",
        );

        let fps_setting = (fps != recorder::DEFAULT_FPS).then_some(fps);
        let mut changed = resolution != self.config.record_resolution
            || fps_setting != self.config.record_fps
            || quality != self.config.record_quality;
        if pick_folder && let Some(folder) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
            self.config.record_dir = Some(folder);
            changed = true;
        }
        if changed {
            self.config.record_resolution = resolution;
            self.config.record_fps = fps_setting;
            self.config.record_quality = quality;
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
    }
}
