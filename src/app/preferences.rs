//! Preferences: the sections, and what's in each. The desktop shows them in
//! a modal, the sections down the side; the mobile layout as a page, the
//! sections as a list that opens each one (`mobile.rs`). Both use
//! `preference_tab_ui` for a section's contents, so they're the same
//! everywhere.

use eframe::egui;

use super::{categories_body, fit_width, nice_name, with_info, App};
use crate::audio::{AudioCommand, DEFAULT_BUFFER_MS, MAX_BUFFER_MS, MIN_BUFFER_MS};
use crate::config::{self, DEFAULT_PRELOAD_EXPIRY_SECS, PRELOAD_EXPIRY_RANGE};

/// The sections of Preferences, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PrefTab {
    #[default]
    Appearance,
    Playback,
    Songs,
    Visualizers,
    Library,
    Updates,
    App,
    Recording,
}

impl PrefTab {
    const ALL: [PrefTab; 8] = [
        Self::Appearance,
        Self::Playback,
        Self::Songs,
        Self::Visualizers,
        Self::Library,
        Self::Updates,
        Self::App,
        Self::Recording,
    ];

    /// The sections there are on this platform.
    pub(super) fn available() -> Vec<PrefTab> {
        Self::ALL.into_iter().filter(|tab| !(cfg!(target_os = "android") && *tab == Self::Recording)).collect()
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Playback => "Playback",
            Self::Songs => "Songs",
            Self::Visualizers => "Visualizers",
            Self::Library => "Online Library",
            Self::Updates => "Updates",
            Self::App => "App",
            Self::Recording => "Recording",
        }
    }

    pub(super) fn icon(self) -> &'static str {
        match self {
            Self::Appearance => "palette",
            Self::Playback => "speaker-high",
            Self::Songs => "music-notes",
            Self::Visualizers => "waveform",
            Self::Library => "storefront",
            Self::Updates => "arrows-clockwise",
            Self::App => "app-window",
            Self::Recording => "record",
        }
    }

    /// What's in it, in a few words (under its title in the mobile list).
    pub(super) fn summary(self) -> &'static str {
        match self {
            Self::Appearance => "Theme, UI scale, the seek bar, full screen, song names",
            Self::Playback => "Audio buffer, what's kept from last time",
            Self::Songs => "Audio files, loading",
            Self::Visualizers => "Script logs",
            Self::Library => "Servers and identity",
            Self::Updates => "Checking for updates, canary builds",
            Self::App => "Tray, welcome and starter pack",
            Self::Recording => "Video size, quality, ffmpeg",
        }
    }
}

/// Something a Preferences section's button asks for, which takes the
/// place of Preferences.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PrefJump {
    Welcome,
    Tour,
    Themes,
}

impl App {
    /// The desktop's Preferences, as a modal over everything else.
    pub(super) fn preferences_ui(&mut self, ctx: &egui::Context) {
        if !self.preferences_open {
            return;
        }
        let tabs = PrefTab::available();
        let mut close = false;
        let mut jump = None;
        let response = egui::Modal::new(egui::Id::new("preferences")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 600.0));
            ui.heading("Preferences");
            ui.add_space(6.0);
            let titles: Vec<&str> = tabs.iter().map(|t| t.title()).collect();
            let mut index = tabs.iter().position(|&t| t == self.preferences_tab).unwrap_or(0);
            categories_body(ui, "preferences", &titles, &mut index, |ui, index| {
                jump = self.preference_tab_ui(ui, tabs[index]);
            });
            self.preferences_tab = tabs[index];
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if close || jump.is_some() || response.should_close() {
            self.preferences_open = false;
        }
        if let Some(jump) = jump {
            self.preference_jump(jump);
        }
    }

    pub(super) fn preference_jump(&mut self, jump: PrefJump) {
        match jump {
            PrefJump::Welcome => self.open_welcome(),
            PrefJump::Tour => self.start_tour(),
            PrefJump::Themes if self.mobile() => self.open_mobile_themes(),
            PrefJump::Themes => self.themes.open = true,
        }
    }

    /// One section's preferences, applied (and saved) as they change.
    pub(super) fn preference_tab_ui(&mut self, ui: &mut egui::Ui, tab: PrefTab) -> Option<PrefJump> {
        let mut separate = self.config.separate_fullscreen_layout;
        let mut expiry = self.config.preload_expiry_secs.unwrap_or(DEFAULT_PRELOAD_EXPIRY_SECS);
        let mut hover_preload = self.config.preload_on_hover;
        let mut check_updates = self.config.check_updates_on_launch.unwrap_or(true);
        let mut canary = self.config.canary_updates();
        let mut close_to_tray = self.config.close_to_tray.unwrap_or(false);
        let mut warn_heavy = self.config.warn_heavy_midi.unwrap_or(true);
        let mut beat_pulse = self.config.beat_pulse.unwrap_or(config::DEFAULT_BEAT_PULSE) * 100.0;
        let mut pulse_length = self.config.beat_pulse_length.unwrap_or(config::DEFAULT_BEAT_PULSE_LENGTH) * 100.0;
        let mut script_log = self.config.script_log_to_app;
        let mut audio_files = self.config.show_audio_files;
        let mut channel_chips = self.config.channel_chips;
        let mut buffer_ms = self.buffer_ms;
        let mut remember = self.config.remember;
        let mut install_updates = self.config.android_install_updates;
        let mut over_cutout = self.config.fullscreen_over_cutout;
        let mut mini_player = self.config.mini_player.unwrap_or(true);
        let mut mini_player_shape = self.config.mini_player_shape;
        let mut jump = None;
        match tab {
            PrefTab::Appearance => {
                let current = self.current_theme().name;
                let mut picked = None;
                ui.horizontal(|ui| {
                    ui.label("Theme");
                    egui::ComboBox::from_id_salt("preferences_theme").selected_text(&current).show_ui(ui, |ui| {
                        for name in self.theme_names() {
                            if ui.selectable_label(name == current, &name).clicked() {
                                picked = Some(name);
                            }
                        }
                    });
                    if ui.button("Edit themes...").on_hover_text("Make your own themes").clicked() {
                        jump = Some(PrefJump::Themes);
                    }
                });
                if let Some(name) = picked {
                    self.pick_theme(ui.ctx(), name);
                }
                self.ui_scale_ui(ui);
                with_info(
                    ui,
                    "The channel toggles above the visualizer as chips in each channel's color, \
                     saying what it plays (\"3 \u{b7} Violin\"): click one to turn its channel off \
                     (it goes grey) or back on, right-click to hear only that one. Off: small \
                     numbered checkboxes, with the instrument when you hover one.",
                    |ui| ui.checkbox(&mut channel_chips, "Channel toggles as chips with instrument names"),
                );
                with_info(
                    ui,
                    "The seek bar is colored by the song's tempo (slow songs blue, fast ones pink), \
                     and flashes toward white on each beat while the song plays: 100% is white, \
                     0% doesn't flash. Each bar's first beat flashes for longer.",
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Pulse with the beat");
                            ui.add(egui::Slider::new(&mut beat_pulse, 0.0..=100.0).suffix("%").fixed_decimals(0))
                        })
                        .inner
                    },
                );
                with_info(
                    ui,
                    "How long each beat's flash takes to fade, as a share of the beat: short is a \
                     quick tick, 100% fades right up to the next beat.",
                    |ui| {
                        ui.add_enabled_ui(beat_pulse > 0.0, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Flash length");
                                ui.add(egui::Slider::new(&mut pulse_length, 10.0..=100.0).suffix("% of a beat").fixed_decimals(0))
                            })
                            .inner
                        })
                        .inner
                    },
                );
                if cfg!(target_os = "android") {
                    with_info(
                        ui,
                        "With the controls hidden, the visualizer fills the screen. On: it goes \
                         under the notch or camera hole as well. Off: that strip stays black.",
                        |ui| ui.checkbox(&mut over_cutout, "Full screen covers the notch"),
                    );
                } else {
                    with_info(
                        ui,
                        "Off: fullscreen shows the same tabs as the window, and rearranging them in \
                         either changes both. On: fullscreen remembers its own arrangement (starting \
                         with just the visualizer). Turning this off keeps that arrangement saved for \
                         if you turn it back on.",
                        |ui| ui.checkbox(&mut separate, "Separate layout for fullscreen"),
                    );
                }
                ui.separator();
                self.song_names_ui(ui);
            }
            PrefTab::Playback => {
                with_info(
                    ui,
                    &format!(
                        "How much sound is made ahead of what's heard. More holds up better in busy \
                         songs and on a busy computer (crackles and gaps mean it's too low); less \
                         makes pausing, seeking and the channel toggles answer sooner. Visualizers \
                         stay in step with the sound either way. {DEFAULT_BUFFER_MS} ms unless changed."
                    ),
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Audio buffer");
                            ui.add(egui::Slider::new(&mut buffer_ms, MIN_BUFFER_MS..=MAX_BUFFER_MS).suffix(" ms"))
                        })
                        .inner
                    },
                );
                with_info(
                    ui,
                    &format!(
                        "Over {} notes: visualizer scripts that go through every note can stall on \
                         them. Asks before playing one (they're marked with a red ! in the lists).",
                        crate::song_info::thousands(crate::song_info::HEAVY_MIDI_NOTES)
                    ),
                    |ui| ui.checkbox(&mut warn_heavy, "Warn before playing a very large MIDI file"),
                );
                ui.add_space(6.0);
                ui.strong("Start with what was set last time");
                for (on, label, info) in [
                    (&mut remember.volume, "Volume", "Off: full volume every time."),
                    (
                        &mut remember.soundfont,
                        "Soundfont",
                        "The one selected in Soundfonts (not a song's own). Off: the first in the list.",
                    ),
                    (&mut remember.script, "Visualizer script", "Off: the first script in the list."),
                    (&mut remember.speed, "Speed", "Off: normal speed every time."),
                    (&mut remember.modes, "Loop and shuffle", "Off: both off every time."),
                ] {
                    with_info(ui, info, |ui| ui.checkbox(on, label));
                }
            }
            PrefTab::Songs => {
                with_info(
                    ui,
                    "synththing is built around MIDI. Audio files play, and visualizers that only \
                     use the sound (waveform, spectrum) work with them, but there are no notes in \
                     them: channel muting, per-song soundfonts, and visualizers and games that read \
                     the notes don't do anything with them. Off: only MIDI files are listed, and \
                     adding a folder adds only its MIDI files (an audio file dragged in by itself is \
                     still added).",
                    |ui| ui.checkbox(&mut audio_files, "Show audio files (MP3, WAV, OGG, FLAC, ...)"),
                );
                // (No pointer to rest on a touch screen.)
                if !cfg!(target_os = "android") {
                    with_info(
                        ui,
                        "Start loading a song once the pointer rests on it in Songs or a playlist, \
                         so it starts sooner when clicked. Uses more memory while browsing.",
                        |ui| ui.checkbox(&mut hover_preload, "Preload songs on hover"),
                    );
                }
                with_info(
                    ui,
                    "Songs and soundfonts load in the background, and the next playlist track is \
                     loaded ahead of time. Anything loaded that nothing has needed for this long \
                     (not playing, not up next, not hovered) is unloaded to free memory.",
                    |ui| {
                        let label = ui.label("Unload preloaded files after");
                        label | ui.add(egui::Slider::new(&mut expiry, PRELOAD_EXPIRY_RANGE).suffix(" s").logarithmic(true))
                    },
                );
                let loaded = self.assets.summary();
                egui::CollapsingHeader::new(format!("Loaded right now ({})", loaded.len()))
                    .id_salt("preferences_loaded_files")
                    .show(ui, |ui| {
                        if loaded.is_empty() {
                            ui.weak("(nothing)");
                        }
                        for (path, kind, state) in &loaded {
                            ui.label(format!("{} ({kind:?}, {state})", nice_name(path)))
                                .on_hover_text(path.display().to_string());
                        }
                    });
            }
            PrefTab::Visualizers => {
                with_info(
                    ui,
                    "What visualizer scripts log() shows in their Debug window; with this \
                     on, it also goes to Help > Log... (and the log file), labelled with the \
                     script's name. A script can turn it on for itself (script_options), or for \
                     single messages (log_app).",
                    |ui| ui.checkbox(&mut script_log, "Copy script logs to the app log"),
                );
                if cfg!(target_os = "android") {
                    with_info(
                        ui,
                        "Leaving synththing while a song plays (home, or switching apps) shrinks it \
                         into a small floating window of the visualizer, with play, pause and skip. \
                         Drag it around, pinch to resize it, or swipe it away. Off: it plays on in \
                         the background, with only the notification.",
                        |ui| ui.checkbox(&mut mini_player, "Mini player when leaving the app"),
                    );
                    ui.add_enabled_ui(mini_player, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Mini player shape");
                            egui::ComboBox::from_id_salt("mini_player_shape").selected_text(mini_player_shape.name()).show_ui(
                                ui,
                                |ui| {
                                    for shape in crate::config::MiniPlayerShape::ALL {
                                        ui.selectable_value(&mut mini_player_shape, shape, shape.name());
                                    }
                                },
                            );
                        });
                    });
                }
            }
            PrefTab::Library => self.library_preferences_ui(ui),
            PrefTab::Updates => {
                with_info(
                    ui,
                    "Looks for a newer release on GitHub and asks before installing anything. \
                     Help > Check for updates does it any time.",
                    |ui| ui.checkbox(&mut check_updates, "Check for updates when synththing starts"),
                );
                // (A branch build follows its branch.)
                if !matches!(crate::build_info::channel(), crate::build_info::Channel::Branch(_)) {
                    with_info(
                        ui,
                        "Canary: the newest build from master, minutes after every change, and \
                         releases once they're out. For testing new things early: it can be broken. \
                         Turning it off keeps the build you have until the next release.",
                        |ui| ui.checkbox(&mut canary, "Canary updates (test builds)"),
                    );
                }
                if cfg!(target_os = "android") {
                    with_info(
                        ui,
                        "Download new versions and hand them to Android to install (it asks you \
                         first). Android has to allow synththing to install apps: Settings > Apps > \
                         Special app access > Install unknown apps. Off: a new version is only \
                         announced, once, with a link to get it.",
                        |ui| ui.checkbox(&mut install_updates, "Install updates in the app"),
                    );
                }
            }
            PrefTab::App => {
                if self.tray.is_some() {
                    with_info(
                        ui,
                        "On: closing the window hides synththing in the tray (by the clock), still \
                         playing. Click its icon for the mini player, right-click it to bring the \
                         window back. Quit, in the synththing menu or the mini player, closes it \
                         completely. Off: closing the window quits.",
                        |ui| ui.checkbox(&mut close_to_tray, "Keep running in the tray when the window's closed"),
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button("Welcome & starter pack...")
                        .on_hover_text("The first-run window: starter songs and soundfonts")
                        .clicked()
                    {
                        jump = Some(PrefJump::Welcome);
                    }
                    // (The tour shows the desktop's panels.)
                    if !self.mobile() && ui.button("Take the tour").on_hover_text("A walk through the app").clicked() {
                        jump = Some(PrefJump::Tour);
                    }
                });
                ui.separator();
                self.storage_ui(ui);
            }
            PrefTab::Recording => self.recording_preferences_ui(ui),
        }

        let mut changed = false;
        let expiry = Some(expiry).filter(|&s| s != DEFAULT_PRELOAD_EXPIRY_SECS);
        if expiry != self.config.preload_expiry_secs
            || hover_preload != self.config.preload_on_hover
            || check_updates != self.config.check_updates_on_launch.unwrap_or(true)
            || canary != self.config.canary_updates()
            || (self.tray.is_some() && Some(close_to_tray) != self.config.close_to_tray && (close_to_tray || self.config.close_to_tray.is_some()))
            || warn_heavy != self.config.warn_heavy_midi.unwrap_or(true)
            || (beat_pulse / 100.0 - self.config.beat_pulse.unwrap_or(config::DEFAULT_BEAT_PULSE)).abs() > 1e-4
            || (pulse_length / 100.0 - self.config.beat_pulse_length.unwrap_or(config::DEFAULT_BEAT_PULSE_LENGTH)).abs() > 1e-4
            || script_log != self.config.script_log_to_app
            || audio_files != self.config.show_audio_files
            || channel_chips != self.config.channel_chips
            || buffer_ms != self.buffer_ms
            || remember != self.config.remember
            || install_updates != self.config.android_install_updates
            || over_cutout != self.config.fullscreen_over_cutout
            || mini_player != self.config.mini_player.unwrap_or(true)
            || mini_player_shape != self.config.mini_player_shape
        {
            self.config.mini_player = (!mini_player).then_some(false);
            self.config.mini_player_shape = mini_player_shape;
            self.config.remember = remember;
            self.config.android_install_updates = install_updates;
            self.config.fullscreen_over_cutout = over_cutout;
            if buffer_ms != self.buffer_ms {
                self.buffer_ms = buffer_ms;
                self.send(AudioCommand::SetBufferMs(buffer_ms));
            }
            self.config.buffer_ms = (buffer_ms != DEFAULT_BUFFER_MS).then_some(buffer_ms);
            self.config.channel_chips = channel_chips;
            self.config.show_audio_files = audio_files;
            self.config.script_log_to_app = script_log;
            self.config.warn_heavy_midi = (!warn_heavy).then_some(false);
            self.config.beat_pulse = Some(beat_pulse / 100.0).filter(|&v| (v - config::DEFAULT_BEAT_PULSE).abs() > 1e-4);
            self.config.beat_pulse_length =
                Some(pulse_length / 100.0).filter(|&v| (v - config::DEFAULT_BEAT_PULSE_LENGTH).abs() > 1e-4);
            self.config.check_updates_on_launch = (!check_updates).then_some(false);
            if canary != self.config.canary_updates() {
                self.config.canary_updates = Some(canary);
            }
            if self.tray.is_some() && (close_to_tray || self.config.close_to_tray.is_some()) {
                self.config.close_to_tray = Some(close_to_tray);
            }
            self.config.preload_expiry_secs = expiry;
            self.config.preload_on_hover = hover_preload;
            changed = true;
        }
        // (A layout preference change saves the config itself, below.)
        if changed
            && separate == self.config.separate_fullscreen_layout
            && let Err(e) = self.config.save()
        {
            self.status = format!("Couldn't save preferences: {e}");
        }
        if separate != self.config.separate_fullscreen_layout {
            self.config.separate_fullscreen_layout = separate;
            // Saves the config (with the new preference) along the way when
            // the layout actually switches; otherwise save it directly.
            self.sync_layout_slot();
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
        jump
    }
}
