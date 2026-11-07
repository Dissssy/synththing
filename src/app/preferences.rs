//! Preferences: the sections, and what's in each. The desktop shows them in
//! a modal, the sections down the side; the mobile layout as a page, the
//! sections as a list that opens each one (`mobile.rs`). Both use
//! `preference_tab_ui` for a section's contents, so they're the same
//! everywhere.

use eframe::egui;

use super::reorder::{reorder_list, trash_ui, ListEntry};
use super::{page_ui, row_icon_button, row_icon_button_room, nice_name, control_width, setting_label, setting_combo, setting_row, setting_slider, settings_group, toggle_chip, with_info, App};
use crate::audio::{AudioCommand, DEFAULT_BUFFER_MS, MAX_BUFFER_MS, MIN_BUFFER_MS};
use crate::config::{self, DEFAULT_PRELOAD_EXPIRY_SECS, PRELOAD_EXPIRY_RANGE};
use crate::loudness;

/// The sections of Preferences, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PrefTab {
    #[default]
    Appearance,
    Playback,
    Browsing,
    Library,
    Visualizer,
    Account,
    Keyboard,
    Integrations,
    App,
}

impl PrefTab {
    const ALL: [PrefTab; 9] = [
        Self::Appearance,
        Self::Playback,
        Self::Browsing,
        Self::Library,
        Self::Visualizer,
        Self::Account,
        Self::Keyboard,
        Self::Integrations,
        Self::App,
    ];

    /// The sections there are on this platform.
    pub(super) fn available() -> Vec<PrefTab> {
        Self::ALL
            .into_iter()
            .filter(|tab| !(cfg!(target_os = "android") && matches!(tab, Self::Keyboard | Self::Integrations)))
            .collect()
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Playback => "Playback",
            Self::Browsing => "Browsing",
            Self::Library => "Library",
            Self::Visualizer => "Visualizer",
            Self::Account => "Account",
            Self::Keyboard => "Keyboard",
            Self::Integrations => "Integrations",
            Self::App => "App",
        }
    }

    pub(super) fn icon(self) -> &'static str {
        match self {
            Self::Appearance => "palette",
            Self::Playback => "speaker-high",
            Self::Browsing => "rows",
            Self::Library => "books",
            Self::Visualizer => "waveform",
            Self::Account => "user-circle",
            Self::Keyboard => "keyboard",
            Self::Integrations => "plugs-connected",
            Self::App => "app-window",
        }
    }

    pub(super) fn summary(self) -> &'static str {
        match self {
            Self::Appearance => "Theme, UI scale, the seek bar",
            Self::Playback => "Sound, what plays, what's kept from last time, loading",
            Self::Browsing => "Song names, sorting and grouping, search",
            Self::Library => "Adding songs, album art and details, tags, rulesets, storage",
            Self::Visualizer => "Pixel scale, channel toggles, scripts, recording",
            Self::Account => "Identity, your devices, sync, the Online Library",
            Self::Keyboard => "Keyboard shortcuts",
            Self::Integrations => "Media controls, letting other programs control synththing",
            Self::App => "The window, updates, getting started",
        }
    }
}

/// Something a Preferences section's button asks for, which takes the
/// place of Preferences.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PrefJump {
    Welcome,
    Themes,
}

/// A tag's icon and name in its color, as lists of buttons show it.
fn tag_entry_text(tag: &crate::song_store::Tag) -> egui::RichText {
    egui::RichText::new(format!("{}  {}", super::icon(tag.icon_or_default()), tag.name)).color(egui::Color32::from_rgb(tag.color[0], tag.color[1], tag.color[2]))
}

impl App {
    /// The desktop's Preferences, as a page in the panels' place: the title
    /// bar and the bottom bar stay, and playback goes on.
    pub(super) fn preferences_ui(&mut self, ui: &mut egui::Ui) {
        let tabs = PrefTab::available();
        let mut jump = None;
        let titles: Vec<String> = tabs.iter().map(|t| format!("{}  {}", super::icon(t.icon()), t.title())).collect();
        let mut index = tabs.iter().position(|&t| t == self.preferences_tab).unwrap_or(0);
        // (Esc ends waiting for keys first.)
        let esc = self.keymap_capture.is_none();
        let close = page_ui(ui, "Preferences", &titles, &mut index, esc, |_| true, |ui, index| {
            jump = self.preference_tab_ui(ui, tabs[index]);
        });
        self.preferences_tab = tabs[index];
        if close || jump.is_some() {
            self.toggle_preferences();
        }
        if let Some(jump) = jump {
            self.preference_jump(jump);
        }
    }

    /// Open Preferences at `tab` (on a phone, its page at that section).
    pub(super) fn open_preferences(&mut self, tab: PrefTab) {
        if self.mobile() {
            self.open_mobile_preferences(tab);
        } else {
            self.preferences_tab = tab;
            self.preferences_open = true;
        }
    }

    /// Open Preferences, or close it (letting go of what it was doing).
    pub(super) fn toggle_preferences(&mut self) {
        self.preferences_open = !self.preferences_open;
        self.help = None;
        self.cleanup = None;
        self.recording.window = None;
        if !self.preferences_open {
            self.keymap_capture = None;
            self.api_preferences_closed();
        }
    }

    /// Playback > Media controls (Android): the buttons beside
    /// play/pause, in order, each dragged to its place or onto the heading's trash, and more
    /// added.
    fn media_buttons_ui(&self, ui: &mut egui::Ui, buttons: &mut config::MediaButtons) {
        use config::MediaButton;
        let tags = self.songs.tags();
        // Its icon and name (`None`: a tag since deleted).
        let describe = |button: &MediaButton| -> Option<egui::RichText> {
            Some(match button {
                MediaButton::Previous => egui::RichText::new(format!("{}  Previous", super::icon("skip-back"))),
                MediaButton::Next => egui::RichText::new(format!("{}  Next", super::icon("skip-forward"))),
                MediaButton::Star => egui::RichText::new(format!("{}  Star", super::icon(self.songs.star().icon_name()))),
                MediaButton::Tag(id) => tag_entry_text(tags.get(id)?),
            })
        };
        buttons.0.retain(|b| describe(b).is_some());
        let info = "The buttons beside play/pause in the notification, on the lock screen and in the mini player, \
                    most wanted first: each place shows as many as it has room for, as the lines say. Star and tag \
                    buttons give the song playing its star or the tag, or take it off: to sort songs out as they \
                    play (a \"Don't keep\" tag, say). They show only the tag's icon, set in Song info's Tags. \
                    Previous and next always sit either side of play/pause.";
        // Where each place's room runs out: the lines drawn there.
        #[cfg(target_os = "android")]
        let mini_player = crate::android::picture_in_picture_room();
        #[cfg(not(target_os = "android"))]
        let mini_player = 2;
        let cutoff = |i: usize| -> Vec<String> {
            let places: Vec<&str> =
                [(config::NOTIFICATION_BUTTONS, "notification"), (mini_player, "mini player")].into_iter().filter(|(room, _)| *room == i).map(|(_, p)| p).collect();
            (!places.is_empty()).then(|| format!("The {} shows the ones above", places.join(" and the "))).into_iter().collect()
        };
        let len = buttons.0.len();
        let mut trash = |ui: &mut egui::Ui| trash_ui(ui, "media_buttons", len);
        settings_group("Media controls").icon("bell-simple").hint("Buttons beside play/pause, most wanted first").buttons(&mut trash).show(ui, |ui| {
            let entry = |i: usize, _: &MediaButton| ListEntry { lines: cutoff(i), row: true };
            let row = |ui: &mut egui::Ui, button: &MediaButton| {
                if let Some(text) = describe(button) {
                    ui.add(egui::Label::new(text).selectable(false).truncate());
                }
            };
            if let Some(i) = reorder_list(ui, "media_buttons", &mut buttons.0, entry, row) {
                buttons.0.remove(i);
            }
            let mut sorted: Vec<(&String, &crate::song_store::Tag)> = tags.iter().collect();
            sorted.sort_by_key(|(_, tag)| tag.name.to_lowercase());
            let unused: Vec<MediaButton> = [MediaButton::Previous, MediaButton::Next, MediaButton::Star]
                .into_iter()
                .chain(sorted.into_iter().map(|(id, _)| MediaButton::Tag(id.clone())))
                .filter(|b| !buttons.0.contains(b))
                .collect();
            with_info(ui, info, |ui| {
                ui.add_enabled_ui(!unused.is_empty(), |ui| {
                    setting_combo(ui, "media_button_add", 0.0).selected_text("Add a button...").show_ui(ui, |ui| {
                        for button in unused {
                            if let Some(text) = describe(&button)
                                && ui.selectable_label(false, text).clicked()
                            {
                                buttons.0.push(button);
                            }
                        }
                    });
                })
                .response
            });
        });
    }

    /// Appearance > Controls bar: the bar's line of buttons and sliders as
    /// a list, its left, middle and right sections under their lines, each
    /// dragged to its place (past a line into the next section) or onto the heading's trash;
    /// the heading's + adds more at the left's end, and ↺ puts back the
    /// usual ones.
    fn controls_bar_list_ui(&self, ui: &mut egui::Ui, bar: &mut config::ControlsBar) {
        use config::BarItem;
        let tags = self.songs.tags();
        // Its icon and name (`None`: a section's start, or a tag since
        // deleted).
        let describe = |item: &BarItem| -> Option<egui::RichText> {
            let (glyph, name) = match item {
                BarItem::Middle | BarItem::Right => return None,
                BarItem::Tag(id) => return tags.get(id).map(tag_entry_text),
                BarItem::Devices => ("devices", "Devices"),
                BarItem::Shuffle => ("shuffle", "Shuffle"),
                BarItem::Stop => ("stop", "Stop"),
                BarItem::Previous => ("skip-back", "Previous"),
                BarItem::PlayPause => ("play", "Play/pause"),
                BarItem::Next => ("skip-forward", "Next"),
                BarItem::Loop => ("repeat", "Loop"),
                BarItem::Volume => ("speaker-high", "Volume"),
                BarItem::Speed => ("gauge", "Speed"),
                BarItem::Star => (self.songs.star().icon_name(), "Star"),
            };
            Some(egui::RichText::new(format!("{}  {name}", super::icon(glyph))))
        };
        bar.0.retain(|item| matches!(item, BarItem::Middle | BarItem::Right) || describe(item).is_some());
        let entry = |i: usize, item: &BarItem| {
            let mut lines = Vec::new();
            if i == 0 {
                lines.push("Left".to_string());
            }
            match item {
                BarItem::Middle => lines.push("Middle (kept centred)".to_string()),
                BarItem::Right => lines.push("Right".to_string()),
                _ => {}
            }
            ListEntry { lines, row: !matches!(item, BarItem::Middle | BarItem::Right) }
        };
        let row = |ui: &mut egui::Ui, item: &BarItem| {
            if let Some(text) = describe(item) {
                ui.add(egui::Label::new(text).selectable(false).truncate());
            }
        };
        let mut sorted: Vec<(&String, &crate::song_store::Tag)> = tags.iter().collect();
        sorted.sort_by_key(|(_, tag)| tag.name.to_lowercase());
        let unused: Vec<BarItem> = BarItem::SINGLE
            .into_iter()
            .chain(sorted.into_iter().map(|(id, _)| BarItem::Tag(id.clone())))
            .filter(|item| !bar.0.contains(item))
            .collect();
        let info = "The buttons and sliders under the seek bar, left to right. Moving one past a line moves it into \
                    that section: the middle stays centred, the left and right hug their ends. Star and tag buttons \
                    give the song playing its star or the tag, or take it off. When the window gets narrow, speed \
                    and then volume become buttons that open their slider, and then star and tag buttons fold into \
                    a list.";
        let mut added = None;
        let mut reset = false;
        let usual = *bar == config::ControlsBar::default();
        let len = bar.0.len();
        let mut buttons = |ui: &mut egui::Ui| {
            super::info_icon(ui, info);
            if ui.add_enabled(!usual, row_icon_button(ui, "arrow-counter-clockwise")).on_hover_text("Back to the usual buttons").clicked() {
                reset = true;
            }
            let add = ui.add_enabled(!unused.is_empty(), row_icon_button(ui, "plus")).on_hover_text("Add a button");
            egui::Popup::menu(&add).show(|ui| {
                for item in &unused {
                    if let Some(text) = describe(item)
                        && ui.button(text).clicked()
                    {
                        added = Some(item.clone());
                    }
                }
            });
            trash_ui(ui, "controls_bar", len);
        };
        settings_group("Controls bar").icon("faders-horizontal").hint("Left to right, the middle kept centred").buttons(&mut buttons).show(ui, |ui| {
            if let Some(i) = reorder_list(ui, "controls_bar", &mut bar.0, entry, row) {
                bar.0.remove(i);
            }
        });
        if let Some(item) = added {
            let at = bar.0.iter().position(|i| *i == BarItem::Middle).unwrap_or(0);
            bar.0.insert(at, item);
        }
        if reset {
            *bar = config::ControlsBar::default();
        }
    }

    /// Playback > Loaded right now: each file in memory, with what it takes
    /// and how long since anything needed it.
    fn loaded_files_ui(&self, ui: &mut egui::Ui) {
        let loaded = self.assets.summary(std::time::Instant::now());
        let total: usize = loaded.iter().filter_map(|l| l.memory).sum();
        let title = match loaded.len() {
            0 => "Loaded right now".to_string(),
            n => format!("Loaded right now ({n}, {})", crate::storage::size_text(total as u64)),
        };
        settings_group(&title).icon("memory").closable("loaded_files").show(ui, |ui| {
            if loaded.is_empty() {
                setting_row(ui, |ui| ui.weak("Nothing"));
            }
            for file in &loaded {
                setting_row(ui, |ui| {
                    let kind = match file.kind {
                        crate::loader::AssetKind::Midi => "music-notes",
                        crate::loader::AssetKind::Audio => "waveform",
                        crate::loader::AssetKind::SoundFont => "piano-keys",
                    };
                    ui.label(super::icon(kind)).on_hover_text(format!("{:?}", file.kind));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let idle = file.idle.as_secs();
                        if idle < 2 {
                            ui.label("in use");
                        } else {
                            let text = if idle < 60 { format!("unused {idle} s") } else { format!("unused {} min", idle / 60) };
                            ui.weak(text)
                                .on_hover_text("Since anything needed it: playing, up next or hovered");
                        }
                        let size = match file.memory {
                            Some(bytes) => crate::storage::size_text(bytes as u64),
                            None => file.state.to_string(),
                        };
                        ui.add_sized([80.0, ui.available_height()], egui::Label::new(size));
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(nice_name(&file.path)).truncate()).on_hover_text(file.path.display().to_string());
                        });
                    });
                });
            }
        });
    }

    pub(super) fn preference_jump(&mut self, jump: PrefJump) {
        match jump {
            PrefJump::Welcome => self.open_welcome(),
            PrefJump::Themes if self.mobile() => self.open_mobile_themes(),
            PrefJump::Themes => self.themes.open = true,
        }
    }

    /// One section's preferences, applied (and saved) as they change.
    pub(super) fn preference_tab_ui(&mut self, ui: &mut egui::Ui, tab: PrefTab) -> Option<PrefJump> {
        let mut expiry = self.config.preload_expiry_secs.unwrap_or(DEFAULT_PRELOAD_EXPIRY_SECS);
        let mut hover_preload = self.config.preload_on_hover;
        let mut skip_collapsed = self.config.autoplay_skip_collapsed;
        let mut only_starred = self.config.only_starred;
        let mut coming_up = self.config.coming_up.unwrap_or(config::DEFAULT_COMING_UP);
        let mut play_counts = self.config.play_counts;
        let mut dropped_songs = self.config.dropped_songs;
        let mut tile_letters = self.config.tile_letters.unwrap_or(crate::album_art::DEFAULT_LETTERS);
        let mut group_ignore = self.config.group_ignore.clone();
        let mut past_articles = !self.config.sort_with_articles;
        let mut unsearched = self.config.songs_view.unsearched.clone();
        let mut check_updates = self.config.check_updates_on_launch.unwrap_or(true);
        let mut canary = self.config.canary_updates();
        let mut close_to_tray = self.config.close_to_tray.unwrap_or(false);
        let mut native_frame = self.config.native_frame;
        let mut playing_fps = self.config.playing_fps.unwrap_or(config::DEFAULT_PLAYING_FPS);
        let mut warn_heavy = self.config.warn_heavy_midi.unwrap_or(true);
        let mut beat_pulse = self.config.beat_pulse.unwrap_or(config::DEFAULT_BEAT_PULSE) * 100.0;
        let mut pulse_length = self.config.beat_pulse_length.unwrap_or(config::DEFAULT_BEAT_PULSE_LENGTH) * 100.0;
        let mut script_log = self.config.script_log_to_app;
        let mut visualizer_scale = config::visualizer_scale_down(self.config.visualizer_scale.unwrap_or(config::DEFAULT_VISUALIZER_SCALE));
        let mut channel_chips = self.config.channel_chips;
        let mut controls_bar = self.config.controls_bar.clone();
        let mut buffer_ms = self.buffer_ms;
        let mut even_volume = self.config.even_volume.unwrap_or(true);
        let mut volume_target = self.config.volume_target.unwrap_or(loudness::DEFAULT_TARGET);
        let mut album_volume = self.config.album_volume;
        let mut remember = self.config.remember;
        let mut install_updates = self.config.android_install_updates;
        let mut over_cutout = self.config.fullscreen_over_cutout;
        let mut mini_player = self.config.mini_player.unwrap_or(true);
        let mut mini_player_shape = self.config.mini_player_shape;
        let mut media_buttons = self.config.media_buttons.clone();
        let mut jump = None;
        match tab {
            PrefTab::Appearance => {
                settings_group("Look").icon("palette").show(ui, |ui| {
                    let current = self.current_theme().name;
                    let mut picked = None;
                    setting_row(ui, |ui| {
                        setting_label(ui, "Theme");
                        setting_combo(ui, "preferences_theme", row_icon_button_room(ui)).selected_text(&current).show_ui(ui, |ui| {
                            for name in self.theme_names() {
                                if ui.selectable_label(name == current, &name).clicked() {
                                    picked = Some(name);
                                }
                            }
                        });
                        if ui.add(row_icon_button(ui, "pencil-simple")).on_hover_text("Edit themes: make your own").clicked() {
                            jump = Some(PrefJump::Themes);
                        }
                    });
                    if let Some(name) = picked {
                        self.pick_theme(ui.ctx(), name);
                    }
                    self.ui_scale_ui(ui);
                });
                if !self.mobile() {
                    self.controls_bar_list_ui(ui, &mut controls_bar);
                }
                settings_group("Seek bar").icon("sliders-horizontal").show(ui, |ui| {
                    with_info(
                        ui,
                        "The seek bar is colored by the song's tempo (slow songs blue, fast ones pink), \
                         and flashes toward white on each beat while the song plays: 100% is white, \
                         0% doesn't flash. Each bar's first beat flashes for longer.",
                        |ui| {
                            let label = setting_label(ui, "Pulse with the beat");
                            label | setting_slider(ui, egui::Slider::new(&mut beat_pulse, 0.0..=100.0).suffix("%").fixed_decimals(0), 0.0)
                        },
                    );
                    with_info(
                        ui,
                        "How long each beat's flash takes to fade, as a share of the beat: short is a \
                         quick tick, 100% fades right up to the next beat.",
                        |ui| {
                            ui.add_enabled_ui(beat_pulse > 0.0, |ui| {
                                let label = setting_label(ui, "Flash length");
                                label | setting_slider(ui, egui::Slider::new(&mut pulse_length, 10.0..=100.0).suffix("%").fixed_decimals(0), 0.0)
                            })
                            .inner
                        },
                    );
                });
                if cfg!(target_os = "android") {
                    settings_group("Full screen").icon("corners-out").show(ui, |ui| {
                        with_info(
                            ui,
                            "With the controls hidden, the visualizer fills the screen. On: it goes \
                             under the notch or camera hole as well. Off: that strip stays black.",
                            |ui| ui.checkbox(&mut over_cutout, "Cover the notch"),
                        );
                    });
                }
            }
            PrefTab::Playback => {
                settings_group("Sound").icon("speaker-high").show(ui, |ui| {
                    with_info(
                        ui,
                        &format!(
                            "How much sound is made ahead of what's heard. More holds up better in busy \
                             songs and on a busy computer (crackles and gaps mean it's too low); less \
                             makes pausing, seeking and the channel toggles answer sooner. Visualizers \
                             stay in step with the sound either way. {DEFAULT_BUFFER_MS} ms unless changed."
                        ),
                        |ui| {
                            let label = setting_label(ui, "Audio buffer");
                            label | setting_slider(ui, egui::Slider::new(&mut buffer_ms, MIN_BUFFER_MS..=MAX_BUFFER_MS).suffix(" ms"), 0.0)
                        },
                    );
                    with_info(
                        ui,
                        "Plays every song about as loud as the next: each is turned up or down by how \
                         loud it is, as its ReplayGain tags say, else as measured (once, in the background, \
                         by your hub, or a computer without one; your other devices use that). A song's own volume \
                         adjust (Song info) goes on top. Quiet songs are turned up only as far as they \
                         go without clipping. MIDI files aren't evened out yet.",
                        |ui| ui.checkbox(&mut even_volume, "Even out volume"),
                    );
                    with_info(
                        ui,
                        &format!(
                            "How loud songs are brought to, in LUFS: higher is louder. {} LUFS (what most \
                             streaming services use) unless changed.",
                            loudness::DEFAULT_TARGET
                        ),
                        |ui| {
                            ui.add_enabled_ui(even_volume, |ui| {
                                let label = setting_label(ui, "Bring songs to");
                                label | setting_slider(ui, egui::Slider::new(&mut volume_target, loudness::TARGET_RANGE).suffix(" LUFS").step_by(1.0), 0.0)
                            })
                            .inner
                        },
                    );
                    with_info(
                        ui,
                        "Turns each album up or down as a whole, by how loud all of it is, instead of song \
                         by song: its quiet songs stay quieter than its loud ones, as it was mixed. Songs on \
                         no album are evened out by themselves.",
                        |ui| ui.add_enabled(even_volume, egui::Checkbox::new(&mut album_volume, "Keep albums' own balance")),
                    );
                });
                settings_group("What plays").icon("playlist").show(ui, |ui| {
                    with_info(
                        ui,
                        &format!(
                            "Shuffle, autoplay and playlists playing on go only to songs you've starred (the {} at the left of \
                             a song). A song you pick yourself still plays.",
                            super::icon(self.songs.star().icon_name())
                        ),
                        |ui| ui.checkbox(&mut only_starred, "Only play starred songs"),
                    );
                    with_info(
                        ui,
                        "Autoplay (the button at the top of Songs) goes on to the next song in the list when \
                         one ends. On: it leaves out the songs in groups you've closed.",
                        |ui| ui.checkbox(&mut skip_collapsed, "Autoplay skips closed groups"),
                    );
                    with_info(
                        ui,
                        "How many songs the queue shows coming up: what's queued, filled out with what the \
                         playlist, album or list playing goes on to. All of the queue shows, however long.",
                        |ui| {
                            let label = setting_label(ui, "Songs coming up");
                            label | setting_slider(ui, egui::Slider::new(&mut coming_up, 0..=20), 0.0)
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
                });
                settings_group("Play counts").icon("chart-bar").show(ui, |ui| {
                    let counts = &mut play_counts;
                    cutoff_row(ui, "Played after", &mut counts.played, "How far into a song it counts as played (Plays, Last played). Reaching its end always does.");
                    cutoff_row(ui, "Skipped from", &mut counts.skip_from, "Moving on to another song from this far in, before it counts as played, counts as skipping it.");
                    cutoff_row(ui, "Skipped until", &mut counts.skip_until, "Moving on later than this isn't a skip: you listened to enough of it.");
                });
                settings_group("Start with what was set last time").icon("clock-counter-clockwise").show(ui, |ui| {
                    for (on, label, info) in [
                        (&mut remember.volume, "Volume", "Off: full volume every time."),
                        (
                            &mut remember.soundfont,
                            "Soundfont",
                            "The one selected in Soundfonts (not a song's own). Off: the first in the list.",
                        ),
                        (&mut remember.script, "Visualizer script", "Off: the first script in the list."),
                        (&mut remember.speed, "Speed", "Off: normal speed every time."),
                        (&mut remember.modes, "Loop, shuffle and autoplay", "Off: all three off every time."),
                    ] {
                        with_info(ui, info, |ui| ui.checkbox(on, label));
                    }
                });
                if cfg!(target_os = "android") {
                    settings_group("Leaving the app").icon("picture-in-picture").show(ui, |ui| {
                        with_info(
                            ui,
                            "Leaving synththing while a song plays (home, or switching apps) shrinks it \
                             into a small floating window of the visualizer, with play, pause and skip. \
                             Drag it around, pinch to resize it, or swipe it away. Off: it plays on in \
                             the background, with only the notification.",
                            |ui| ui.checkbox(&mut mini_player, "Mini player"),
                        );
                        ui.add_enabled_ui(mini_player, |ui| {
                            setting_row(ui, |ui| {
                                setting_label(ui, "Mini player shape");
                                setting_combo(ui, "mini_player_shape", 0.0)
                                    .selected_text(mini_player_shape.name())
                                    .show_ui(ui, |ui| {
                                        for shape in crate::config::MiniPlayerShape::ALL {
                                            ui.selectable_value(&mut mini_player_shape, shape, shape.name());
                                        }
                                    });
                            });
                        });
                    });
                    self.media_buttons_ui(ui, &mut media_buttons);
                }
                settings_group("Loading").icon("hourglass").show(ui, |ui| {
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
                            let label = setting_label(ui, "Unload unused files after");
                            label | setting_slider(ui, egui::Slider::new(&mut expiry, PRELOAD_EXPIRY_RANGE).suffix(" s").logarithmic(true), 0.0)
                        },
                    );
                });
                self.loaded_files_ui(ui);
            }
            PrefTab::Browsing => {
                self.song_names_ui(ui);
                settings_group("Sorting and grouping").icon("sort-ascending").show(ui, |ui| {
                    with_info(
                        ui,
                        "Sorting by a name (the Songs list, smart playlists, Groupings) passes over a leading \
                         \"The\", \"A\" or \"An\", so The Beatles are with the Bs. A song's own \"sort as\" \
                         (Song info) goes first.",
                        |ui| ui.checkbox(&mut past_articles, "Sort past a leading The, A or An"),
                    );
                    with_info(
                        ui,
                        "Grouping by a name (the Songs list's groups, the playlists of Artists, Albums and \
                         other rulesets) already sees past case, spacing, punctuation and symbols, so \
                         \"<OST>\" and \"OST\" are one group, shown as its best spelling. These characters \
                         are passed over too.",
                        |ui| {
                            let label = setting_label(ui, "Also ignore when grouping");
                            label | ui.add(egui::TextEdit::singleline(&mut group_ignore).hint_text("e.g. ー").desired_width(control_width(ui, 0.0)))
                        },
                    );
                    with_info(
                        ui,
                        "A playlist in Groupings with no art (or while its art is read) shows the first \
                         letters of its name's words on a color of its own: one to three letters in a row, \
                         or four in a square.",
                        |ui| {
                            let label = setting_label(ui, "Letters on tiles without art");
                            label | setting_slider(ui, egui::Slider::new(&mut tile_letters, 1..=4), 0.0)
                        },
                    );
                });
                let hint = "What searching Songs looks in, besides each song's name. A filter (artist:...) goes by what's off too.";
                settings_group("Search looks in").icon("funnel").hint(hint).show(ui, |ui| {
                    let fields = || crate::fields::columns().filter(|d| d.key != "file_name");
                    let info = fields().filter(|d| d.editable && !d.key.starts_with("sort_")).map(|d| (d.key, d.label));
                    let sort = fields().filter(|d| d.key.starts_with("sort_")).map(|d| (d.key, d.label));
                    let file = fields().filter(|d| !d.editable).map(|d| (d.key, d.label));
                    let sections: [(&str, Vec<(&str, &str)>); 3] = [
                        ("Song info", [("tag", "Tags")].into_iter().chain(info).collect()),
                        ("Sort names", sort.collect()),
                        ("The file", [("file_name", "File name")].into_iter().chain(file).collect()),
                    ];
                    for (title, chips) in sections {
                        let accent = ui.visuals().selection.bg_fill;
                        setting_row(ui, |ui| {
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                                setting_label(ui, title);
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    for (key, label) in chips {
                                        let on = !unsearched.iter().any(|k| k == key);
                                        if toggle_chip(ui, on, label, accent).clicked() {
                                            unsearched.retain(|k| k != key);
                                            if on {
                                                unsearched.push(key.to_string());
                                            }
                                        }
                                    }
                                });
                            });
                        });
                    }
                });
            }
            PrefTab::Library => {
                settings_group("Adding songs").icon("plus-circle").show(ui, |ui| {
                    with_info(
                        ui,
                        "Songs dragged onto the window are always imported into the library. Then they're queued, \
                         or, just importing, added to the playlist on screen if one is.",
                        |ui| {
                            let label = setting_label(ui, "Dropped songs");
                            let combo = setting_combo(ui, "dropped_songs", 0.0)
                                .selected_text(dropped_songs.name())
                                .show_ui(ui, |ui| {
                                    for choice in crate::config::DroppedSongs::ALL {
                                        ui.selectable_value(&mut dropped_songs, choice, choice.name());
                                    }
                                });
                            label | combo.response
                        },
                    );
                });
                settings_group("Album art and details").icon("vinyl-record").show(ui, |ui| {
                    self.lookup_preferences_ui(ui);
                });
                self.tags_preferences_ui(ui);
                self.rulesets_ui(ui);
                self.storage_ui(ui);
            }
            PrefTab::Visualizer => {
                settings_group("Display").icon("monitor").show(ui, |ui| {
                    with_info(
                        ui,
                        "How many screen pixels each visualizer pixel covers, across and down. 1x draws at \
                         your screen's full resolution; higher draws fewer, chunkier pixels, so scripts run \
                         faster (useful fullscreen on a large or high-DPI screen). 4x by default on phones, \
                         1x elsewhere. Kept on this device only.",
                        |ui| {
                            let label = setting_label(ui, "Pixel scale");
                            let combo = setting_combo(ui, "visualizer_pixel_scale", 0.0)
                                .selected_text(format!("{visualizer_scale}x"))
                                .show_ui(ui, |ui| {
                                    for scale in config::VISUALIZER_SCALES {
                                        ui.selectable_value(&mut visualizer_scale, scale, format!("{scale}x"));
                                    }
                                });
                            label | combo.response
                        },
                    );
                    with_info(
                        ui,
                        "The channel toggles above the visualizer as chips in each channel's color, \
                         saying what it plays (\"3 \u{b7} Violin\"): click one to turn its channel off \
                         (it goes grey) or back on, right-click to hear only that one. Off: small \
                         numbered checkboxes, with the instrument when you hover one.",
                        |ui| ui.checkbox(&mut channel_chips, "Channel toggles as chips with instrument names"),
                    );
                });
                settings_group("Scripts").icon("code").show(ui, |ui| {
                    with_info(
                        ui,
                        "What visualizer scripts log() shows in their Debug window; with this \
                         on, it also goes to Help > Log (and the log file), labelled with the \
                         script's name. A script can turn it on for itself (script_options), or for \
                         single messages (log_app).",
                        |ui| ui.checkbox(&mut script_log, "Copy script logs to the app log"),
                    );
                });
                if !cfg!(target_os = "android") {
                    settings_group("Recording").icon("record").show(ui, |ui| self.recording_preferences_ui(ui));
                }
            }
            PrefTab::Account => self.library_preferences_ui(ui),
            PrefTab::Keyboard => self.keyboard_preferences_ui(ui),
            PrefTab::Integrations => self.api_preferences_ui(ui),
            PrefTab::App => {
                if !cfg!(target_os = "android") {
                    settings_group("Window").icon("app-window").show(ui, |ui| {
                        with_info(
                            ui,
                            "On: the window has your system's title bar and borders. Off: synththing \
                             draws its own, the menu bar doubling as the title bar (drag its empty \
                             middle to move the window, double-click it to maximize).",
                            |ui| ui.checkbox(&mut native_frame, "Use the system's window frame"),
                        );
                        with_info(
                            ui,
                            &format!(
                                "How many times a second the window redraws while a song plays, when \
                                 nothing else (the mouse, a visualizer) is redrawing it already. Higher \
                                 makes the seek bar and time move smoothly; lower uses less CPU and \
                                 battery. Hidden or minimized, it stays at {} a second. {} unless changed.",
                                config::DEFAULT_PLAYING_FPS,
                                config::DEFAULT_PLAYING_FPS,
                            ),
                            |ui| {
                                let label = setting_label(ui, "Redraws while playing");
                                label | setting_slider(ui, egui::Slider::new(&mut playing_fps, config::PLAYING_FPS_RANGE).suffix(" / s").logarithmic(true), 0.0)
                            },
                        );
                        if self.has_tray() {
                            with_info(
                                ui,
                                "On: closing the window hides synththing in the tray (by the clock), still \
                                 playing. Click its icon for the mini player, right-click it to bring the \
                                 window back. Quit, in the synththing menu or the mini player, closes it \
                                 completely. Off: closing the window quits.",
                                |ui| ui.checkbox(&mut close_to_tray, "Keep running in the tray when the window's closed"),
                            );
                        }
                    });
                    #[cfg(windows)]
                    self.install_preferences_ui(ui);
                    #[cfg(target_os = "linux")]
                    self.desktop_entry_preferences_ui(ui);
                }
                settings_group("Updates").icon("arrows-clockwise").show(ui, |ui| {
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
                });
                self.usage_preferences_ui(ui);
                // (The desktop has these in Help.)
                if self.mobile() {
                    settings_group("Getting started").icon("rocket-launch").show(ui, |ui| {
                        setting_row(ui, |ui| {
                            if ui.button("Welcome & starter pack...").on_hover_text("The first-run window: starter songs and soundfonts").clicked() {
                                jump = Some(PrefJump::Welcome);
                            }
                        });
                    });
                }
            }
        }

        let mut changed = false;
        let expiry = Some(expiry).filter(|&s| s != DEFAULT_PRELOAD_EXPIRY_SECS);
        if expiry != self.config.preload_expiry_secs
            || hover_preload != self.config.preload_on_hover
            || skip_collapsed != self.config.autoplay_skip_collapsed
            || only_starred != self.config.only_starred
            || coming_up != self.config.coming_up.unwrap_or(config::DEFAULT_COMING_UP)
            || play_counts != self.config.play_counts
            || dropped_songs != self.config.dropped_songs
            || tile_letters != self.config.tile_letters.unwrap_or(crate::album_art::DEFAULT_LETTERS)
            || group_ignore != self.config.group_ignore
            || past_articles == self.config.sort_with_articles
            || unsearched != self.config.songs_view.unsearched
            || check_updates != self.config.check_updates_on_launch.unwrap_or(true)
            || canary != self.config.canary_updates()
            || (self.has_tray() && Some(close_to_tray) != self.config.close_to_tray && (close_to_tray || self.config.close_to_tray.is_some()))
            || warn_heavy != self.config.warn_heavy_midi.unwrap_or(true)
            || native_frame != self.config.native_frame
            || playing_fps != self.config.playing_fps.unwrap_or(config::DEFAULT_PLAYING_FPS)
            || (beat_pulse / 100.0 - self.config.beat_pulse.unwrap_or(config::DEFAULT_BEAT_PULSE)).abs() > 1e-4
            || (pulse_length / 100.0 - self.config.beat_pulse_length.unwrap_or(config::DEFAULT_BEAT_PULSE_LENGTH)).abs() > 1e-4
            || script_log != self.config.script_log_to_app
            || visualizer_scale != self.config.visualizer_scale.unwrap_or(config::DEFAULT_VISUALIZER_SCALE)
            || channel_chips != self.config.channel_chips
            || controls_bar != self.config.controls_bar
            || buffer_ms != self.buffer_ms
            || even_volume != self.config.even_volume.unwrap_or(true)
            || volume_target != self.config.volume_target.unwrap_or(loudness::DEFAULT_TARGET)
            || album_volume != self.config.album_volume
            || remember != self.config.remember
            || install_updates != self.config.android_install_updates
            || over_cutout != self.config.fullscreen_over_cutout
            || mini_player != self.config.mini_player.unwrap_or(true)
            || mini_player_shape != self.config.mini_player_shape
            || media_buttons != self.config.media_buttons
        {
            self.config.media_buttons = media_buttons;
            self.config.mini_player = (!mini_player).then_some(false);
            self.config.mini_player_shape = mini_player_shape;
            self.config.remember = remember;
            self.config.play_counts = play_counts;
            self.config.android_install_updates = install_updates;
            self.config.fullscreen_over_cutout = over_cutout;
            if buffer_ms != self.buffer_ms {
                self.buffer_ms = buffer_ms;
                self.send(AudioCommand::SetBufferMs(buffer_ms));
            }
            self.config.buffer_ms = (buffer_ms != DEFAULT_BUFFER_MS).then_some(buffer_ms);
            self.config.even_volume = (!even_volume).then_some(false);
            self.config.volume_target = Some(volume_target).filter(|&t| t != loudness::DEFAULT_TARGET);
            self.config.album_volume = album_volume;
            self.send_volume();
            self.config.channel_chips = channel_chips;
            self.config.controls_bar = controls_bar;
            self.config.script_log_to_app = script_log;
            self.config.visualizer_scale = (visualizer_scale != config::DEFAULT_VISUALIZER_SCALE).then_some(visualizer_scale);
            self.config.warn_heavy_midi = (!warn_heavy).then_some(false);
            self.config.native_frame = native_frame;
            self.config.playing_fps = Some(playing_fps).filter(|&f| f != config::DEFAULT_PLAYING_FPS);
            self.config.beat_pulse = Some(beat_pulse / 100.0).filter(|&v| (v - config::DEFAULT_BEAT_PULSE).abs() > 1e-4);
            self.config.beat_pulse_length =
                Some(pulse_length / 100.0).filter(|&v| (v - config::DEFAULT_BEAT_PULSE_LENGTH).abs() > 1e-4);
            self.config.check_updates_on_launch = (!check_updates).then_some(false);
            if canary != self.config.canary_updates() {
                self.config.canary_updates = Some(canary);
            }
            if self.has_tray() && (close_to_tray || self.config.close_to_tray.is_some()) {
                self.config.close_to_tray = Some(close_to_tray);
            }
            self.config.preload_expiry_secs = expiry;
            self.config.preload_on_hover = hover_preload;
            self.config.autoplay_skip_collapsed = skip_collapsed;
            self.config.dropped_songs = dropped_songs;
            self.config.tile_letters = Some(tile_letters).filter(|&n| n != crate::album_art::DEFAULT_LETTERS);
            self.config.group_ignore = group_ignore;
            self.config.sort_with_articles = !past_articles;
            self.config.songs_view.unsearched = unsearched;
            let coming_up = Some(coming_up).filter(|&n| n != config::DEFAULT_COMING_UP);
            if self.config.only_starred != only_starred || self.config.coming_up != coming_up {
                self.config.only_starred = only_starred;
                self.config.coming_up = coming_up;
                self.plan_upcoming();
                self.plan_listed();
            }
            changed = true;
        }
        if changed && let Err(e) = self.config.save() {
            self.status = format!("Couldn't save preferences: {e}");
        }
        jump
    }
}

/// A point in a song (`play_stats::Cutoff`), as a slider with a button
/// switching it between a share of the song and seconds into it.
fn cutoff_row(ui: &mut egui::Ui, label: &str, cutoff: &mut crate::play_stats::Cutoff, info: &str) {
    use crate::play_stats::Cutoff;
    with_info(ui, info, |ui| {
        let label = setting_label(ui, label);
        let (switch, to) = match *cutoff {
            Cutoff::Percent(p) => ("%", Cutoff::Seconds(p.min(MAX_CUTOFF_SECS))),
            Cutoff::Seconds(s) => ("s", Cutoff::Percent(s.min(100.0))),
        };
        let button = ui.button(switch).on_hover_text(match to {
            Cutoff::Seconds(_) => "A share of the song: click for seconds into it",
            Cutoff::Percent(_) => "Seconds into the song: click for a share of it",
        });
        if button.clicked() {
            *cutoff = to;
        }
        let beside = button.rect.width() + ui.spacing().item_spacing.x;
        let slider = match cutoff {
            Cutoff::Percent(p) => egui::Slider::new(p, 0.0..=100.0).suffix("%").fixed_decimals(0),
            Cutoff::Seconds(s) => egui::Slider::new(s, 0.0..=MAX_CUTOFF_SECS).suffix(" s").fixed_decimals(0),
        };
        label | button | setting_slider(ui, slider, beside)
    });
}

/// The most seconds a play count's cutoff can be.
const MAX_CUTOFF_SECS: f64 = 300.0;
