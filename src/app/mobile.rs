//! The mobile layout: one page at a time, sized for a phone's screen and
//! fingers. The Android app always uses it; on the desktop, View > Mobile
//! layout switches to it, so it can be tried without a phone.
//!
//! The Now playing page is the visualizer over the controls: the seek bar
//! as the line between them, the song's name (scrolling when it's too long
//! to fit), and the transport. The chevron folds the controls away, giving
//! the visualizer the whole screen, with a small handle left to bring them
//! back (touches on the visualizer go to the script, so they can't be what
//! brings them back). The menu at the top left opens the other pages, which
//! are the desktop's tabs, and the windows the desktop's menus open.

use std::time::{Duration, Instant};

use eframe::egui::{self, AtomExt as _};

use super::preferences::PrefTab;
use super::{fmt_time, icon, palette_color, status_color, status_outcome, App};
use crate::audio::{AudioCommand, PlaybackShared};
use crate::engine::EngineView;
use crate::lua_visualizer;
use crate::playlist::LoopMode;
use crate::visualizer::DisplayMode;

/// The window size the desktop switches to with the mobile layout: about
/// a phone's, standing up.
const PHONE_SIZE: egui::Vec2 = egui::vec2(420.0, 880.0);
/// And back to, without it (the size the app starts at).
const DESKTOP_SIZE: egui::Vec2 = egui::vec2(1100.0, 750.0);

/// The marquee's speed, in points a second, and the gap between the end of
/// the name and its next start.
const MARQUEE_SPEED: f32 = 40.0;
const MARQUEE_GAP: f32 = 48.0;

/// What the mobile layout shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Page {
    #[default]
    NowPlaying,
    Songs,
    Soundfonts,
    Playlists,
    Scripts,
    Library,
    Downloads,
    /// From the menu's lower part, not its pages.
    Preferences,
    Updates,
    /// What's new since `Mobile::changelog_since`, or all of it.
    Changelog,
    Log,
    Credits,
}

impl Page {
    const ALL: [Page; 7] = [
        Self::NowPlaying,
        Self::Songs,
        Self::Soundfonts,
        Self::Playlists,
        Self::Scripts,
        Self::Library,
        Self::Downloads,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::NowPlaying => "Now playing",
            Self::Songs => "Songs",
            Self::Soundfonts => "Soundfonts",
            Self::Playlists => "Playlists",
            Self::Scripts => "Visualizers",
            Self::Library => "Script library",
            Self::Downloads => "Downloaded songs",
            Self::Preferences => "Preferences",
            Self::Updates => "Updates",
            Self::Changelog => "Changelog",
            Self::Log => "Log",
            Self::Credits => "Credits & licenses",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::NowPlaying => "monitor-play",
            Self::Songs => "music-notes",
            Self::Soundfonts => "piano-keys",
            Self::Playlists => "playlist",
            Self::Scripts => "waveform",
            Self::Library => "storefront",
            Self::Downloads => "download-simple",
            Self::Preferences => "gear",
            Self::Updates => "arrows-clockwise",
            Self::Changelog => "clock-counter-clockwise",
            Self::Log => "notepad",
            Self::Credits => "info",
        }
    }
}

/// The mobile layout's own state (for the session; nothing's saved).
#[derive(Default)]
pub(super) struct Mobile {
    page: Page,
    drawer_open: bool,
    /// The Now playing page's controls folded away.
    collapsed: bool,
    /// What the media session was last told, and when.
    #[cfg(target_os = "android")]
    told: Option<(crate::android::Playback, std::time::Instant)>,
    /// What picture-in-picture was last set to: whether leaving goes into
    /// it, and its shape.
    #[cfg(target_os = "android")]
    picture_in_picture: Option<(bool, crate::config::MiniPlayerShape)>,
    /// Audio files just imported with audio files turned off: whether to
    /// turn them on (`audio_import_ui`).
    audio_import: Option<Vec<std::path::PathBuf>>,
    /// The status last seen (`None` before the first frame), and the one
    /// being shown, since when (`status_toast_ui`).
    seen_status: Option<String>,
    toast: Option<(String, Instant)>,
    /// The Preferences section open, if one is (else the list of them).
    pref_section: Option<PrefTab>,
    /// Editing themes, a level under Preferences > Appearance.
    editing_themes: bool,
    /// The version the Changelog page shows what's new since (after an
    /// update), else all of it.
    changelog_since: Option<crate::changelog::Version>,
    /// When back was pressed with nothing left to go back from: a second
    /// press while its toast shows leaves (`mobile_back`).
    back_to_leave: Option<Instant>,
}

/// How long a toast shows, at least: a failure longer, to be read. A long
/// one stays long enough to read (`READING_SPEED` characters a second).
const TOAST: Duration = Duration::from_secs(4);
const FAILURE_TOAST: Duration = Duration::from_secs(8);
const READING_SPEED: f32 = 15.0;

/// Where a tapped info icon leaves its text for the toast (`info_icon`).
pub(super) const INFO_TOAST: &str = "info_toast";

impl App {
    /// Whether the mobile layout is showing: always on Android, else as
    /// chosen (View > Mobile layout).
    pub(super) fn mobile(&self) -> bool {
        cfg!(target_os = "android") || self.config.mobile_layout
    }

    /// The window size to start the desktop app at, for its layout.
    pub fn start_size(&self) -> egui::Vec2 {
        if self.mobile() { PHONE_SIZE } else { DESKTOP_SIZE }
    }

    /// Switch the desktop between the two layouts, resizing the window to
    /// suit.
    pub(super) fn set_mobile_layout(&mut self, ctx: &egui::Context, on: bool) {
        self.config.mobile_layout = on;
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save preferences: {e}");
        }
        if !self.fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(if on { PHONE_SIZE } else { DESKTOP_SIZE }));
        }
    }

    pub(super) fn mobile_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        let ctx = ui.ctx().clone();
        self.mobile_back(&ctx);
        let view = &shared.view;
        let page = self.mobile_state.page;
        let fill_screen = page == Page::NowPlaying && self.mobile_state.collapsed;
        // The visualizer filling the screen is full screen: no status or
        // navigation bar either (a swipe from the edge shows them a moment).
        #[cfg(target_os = "android")]
        crate::android::set_fullscreen(fill_screen, self.config.fullscreen_over_cutout);

        // Picture-in-picture: a small floating window, just the visualizer.
        #[cfg(target_os = "android")]
        if crate::android::picture_in_picture() {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                self.show_visualizer(ui, &shared.notes, view, DisplayMode::Mini);
            });
            return;
        }

        if !fill_screen {
            egui::Panel::top("mobile_top_bar").show(ui, |ui| self.mobile_top_bar_ui(ui));
        }
        if page == Page::NowPlaying {
            if !fill_screen {
                egui::Panel::bottom("mobile_controls").show(ui, |ui| self.mobile_controls_ui(ui, view));
            }
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                self.script_error_ui(ui, false);
                let mode = if fill_screen { DisplayMode::Dedicated } else { DisplayMode::Window };
                self.show_visualizer(ui, &shared.notes, view, mode);
            });
            if fill_screen {
                self.mobile_expand_handle_ui(&ctx);
            }
        } else {
            egui::Panel::bottom("mobile_now_playing_bar").show(ui, |ui| self.mobile_now_playing_bar_ui(ui, view));
            egui::CentralPanel::default().show(ui, |ui| match page {
                Page::NowPlaying => {}
                Page::Songs => self.songs_ui(ui),
                Page::Soundfonts => self.soundfont_ui(ui),
                Page::Playlists => self.playlist_panel_ui(ui),
                Page::Scripts => self.mobile_scripts_ui(ui),
                Page::Library => self.library_ui(ui),
                Page::Downloads => self.downloaded_songs_ui(ui),
                Page::Preferences => self.mobile_preferences_ui(ui),
                Page::Updates => {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        if self.updates_body_ui(ui, true) {
                            self.updates_message = None;
                            self.mobile_state.page = Page::NowPlaying;
                        }
                    });
                }
                Page::Changelog => {
                    if super::changelog_body_ui(ui, self.mobile_state.changelog_since, true) {
                        self.mobile_state.changelog_since = None;
                    }
                }
                Page::Log => {
                    self.log_body_ui(ui, true);
                }
                Page::Credits => self.credits_body_ui(ui, true),
            });
        }
        if self.mobile_state.drawer_open {
            self.mobile_drawer_ui(&ctx);
        }
    }

    /// What opens a window on the desktop (the launch update check, What's
    /// new after an update, the status bar's link to the log) opens its
    /// page here instead.
    pub(super) fn open_mobile_pages(&mut self, ctx: &egui::Context) {
        let state = &mut self.mobile_state;
        let page = if std::mem::take(&mut self.updates_open) {
            Page::Updates
        } else if let Some(since) = self.changelog.take() {
            state.changelog_since = since;
            Page::Changelog
        } else if std::mem::take(&mut self.log_open) {
            Page::Log
        } else if std::mem::take(&mut self.credits_open) {
            Page::Credits
        } else {
            return;
        };
        state.page = page;
        state.drawer_open = false;
        ctx.request_repaint();
    }

    /// The menu button, the page's name, and notifications and feedback at
    /// the right, as on the desktop.
    fn mobile_top_bar_ui(&mut self, ui: &mut egui::Ui) {
        let library_details = self.mobile_state.page == Page::Library && self.library_details_open();
        ui.horizontal(|ui| {
            // In a Preferences section or a library pick's details: back to
            // the list instead.
            let state = &mut self.mobile_state;
            match state.pref_section.filter(|_| state.page == Page::Preferences) {
                Some(section) => {
                    if ui.add(big_icon_button("arrow-left", 36.0)).on_hover_text("Back").clicked() {
                        if state.editing_themes {
                            state.editing_themes = false;
                        } else {
                            state.pref_section = None;
                        }
                    }
                    ui.heading(if state.editing_themes { "Themes" } else { section.title() });
                }
                None if library_details => {
                    if ui.add(big_icon_button("arrow-left", 36.0)).on_hover_text("Back").clicked() {
                        self.library_close_details();
                    }
                    ui.heading(Page::Library.title());
                }
                None => {
                    if ui.add(big_icon_button("list", 36.0)).on_hover_text("Menu").clicked() {
                        self.mobile_state.drawer_open = true;
                    }
                    // Now playing is the app's home: its name, as the
                    // wordmark.
                    match self.mobile_state.page {
                        Page::NowPlaying => self.mobile_wordmark_ui(ui),
                        Page::Changelog if self.mobile_state.changelog_since.is_some() => {
                            ui.heading("What's new");
                        }
                        page => {
                            ui.heading(page.title());
                        }
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.notifications_button_ui(ui);
                self.feedback_button_ui(ui);
            });
        });
    }

    /// Back (Android's back, Escape on the desktop), a step at a time: a
    /// dialog or popup closes itself (egui's own Escape), then the menu,
    /// a Preferences section, a library pick's details, full screen, any
    /// page but Now playing, and last, on Android, leaving (pressed twice,
    /// as other apps do).
    fn mobile_back(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.key_pressed(egui::Key::Escape))
            || ctx.memory(|m| m.top_modal_layer().is_some())
            || egui::Popup::is_any_open(ctx)
        {
            return;
        }
        let library_details = self.mobile_state.page == Page::Library && self.library_details_open();
        let state = &mut self.mobile_state;
        if state.drawer_open {
            state.drawer_open = false;
        } else if state.page == Page::Preferences && state.editing_themes {
            state.editing_themes = false;
        } else if state.page == Page::Preferences && state.pref_section.is_some() {
            state.pref_section = None;
        } else if library_details {
            self.library_close_details();
        } else if state.page == Page::NowPlaying && state.collapsed {
            state.collapsed = false;
        } else if state.page != Page::NowPlaying {
            state.page = Page::NowPlaying;
        } else if cfg!(target_os = "android") {
            if state.back_to_leave.is_some_and(|at| at.elapsed() < TOAST) {
                state.back_to_leave = None;
                state.toast = None;
                #[cfg(target_os = "android")]
                crate::android::leave();
            } else {
                state.back_to_leave = Some(Instant::now());
                state.toast = Some(("Back again to leave synththing.".into(), Instant::now()));
            }
        }
    }

    /// Preferences > Appearance > Edit themes...: a level under it.
    pub(super) fn open_mobile_themes(&mut self) {
        self.mobile_state.editing_themes = true;
    }

    /// The Preferences page: the sections, each opening in its place.
    fn mobile_preferences_ui(&mut self, ui: &mut egui::Ui) {
        if self.mobile_state.editing_themes && self.mobile_state.pref_section == Some(PrefTab::Appearance) {
            self.mobile_themes_ui(ui);
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.mobile_state.pref_section {
            Some(section) => {
                ui.spacing_mut().item_spacing.y = 8.0;
                if let Some(jump) = self.preference_tab_ui(ui, section) {
                    self.preference_jump(jump);
                }
            }
            None => {
                for section in PrefTab::available() {
                    if section_row(ui, section).clicked() {
                        self.mobile_state.pref_section = Some(section);
                        self.mobile_state.editing_themes = false;
                    }
                }
            }
        });
    }

    /// Ask whether to turn audio files on, for `files`: just imported,
    /// with them off.
    pub(super) fn offer_audio_files(&mut self, files: Vec<std::path::PathBuf>) {
        self.mobile_state.audio_import = Some(files);
    }

    /// Audio files were imported with audio files turned off: turn them on
    /// (and they're in the library), or not (and they go again).
    pub(super) fn audio_import_ui(&mut self, ctx: &egui::Context) {
        let Some(files) = &self.mobile_state.audio_import else { return };
        let names: Vec<String> = files.iter().map(|f| super::nice_name(f)).collect();
        let mut answer = None;
        egui::Modal::new(egui::Id::new("audio_import")).show(ctx, |ui| {
            ui.set_width(super::fit_width(ui, 420.0));
            ui.heading("Play audio files too?");
            ui.add_space(4.0);
            ui.label(format!(
                "{} {} audio, not MIDI. synththing plays audio files once they're turned on (Preferences > \
                 Songs > Show audio files), which lists them with your songs.",
                names.join(", "),
                if names.len() == 1 { "is" } else { "are" },
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Turn them on").clicked() {
                    answer = Some(true);
                }
                if ui.button("No, don't import them").clicked() {
                    answer = Some(false);
                }
            });
        });
        let Some(on) = answer else { return };
        let files = self.mobile_state.audio_import.take().unwrap_or_default();
        if on {
            self.config.show_audio_files = true;
            self.browser.set_extensions(super::listed_song_extensions(&self.config));
            self.status = match self.config.save() {
                Ok(()) => "Audio files turned on.".to_string(),
                Err(e) => format!("Couldn't save preferences: {e}"),
            };
        } else {
            for file in &files {
                if let Err(e) = std::fs::remove_file(file) {
                    log::warn!("couldn't remove {}: {e}", file.display());
                }
            }
            self.browser.rescan();
            self.status = format!("Removed {} audio file{}.", files.len(), if files.len() == 1 { "" } else { "s" });
        }
    }

    /// The mobile layout has no status line: what gets done or fails shows
    /// a moment at the bottom instead (a tap puts it away).
    pub(super) fn status_toast_ui(&mut self, ctx: &egui::Context) {
        let state = &mut self.mobile_state;
        if state.seen_status.as_deref() != Some(self.status.as_str()) {
            // (Not what was there before the first frame.)
            if state.seen_status.is_some() && status_outcome(&self.status).is_some() {
                state.toast = Some((self.status.clone(), Instant::now()));
            }
            state.seen_status = Some(self.status.clone());
        }
        if let Some(info) = ctx.data_mut(|d| d.remove_temp::<String>(egui::Id::new(INFO_TOAST))) {
            state.toast = Some((info, Instant::now()));
        }
        let Some((text, since)) = &state.toast else { return };
        let failed = status_outcome(text) == Some(false);
        let least = if failed { FAILURE_TOAST } else { TOAST };
        if since.elapsed() > least.max(Duration::from_secs_f32(text.len() as f32 / READING_SPEED)) {
            state.toast = None;
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let screen = ctx.content_rect();
        let width = (screen.width() - 32.0).min(480.0);
        let response = egui::Area::new(egui::Id::new("status_toast"))
            .order(egui::Order::Tooltip)
            .sense(egui::Sense::click())
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(width);
                    let mut label = egui::RichText::new(text.as_str());
                    if let Some(color) = status_color(ui, text) {
                        label = label.color(color);
                    }
                    ui.add(egui::Label::new(label).wrap());
                });
            })
            .response;
        if response.clicked() {
            self.mobile_state.toast = None;
        }
    }

    /// Under the visualizer: the seek bar, the song, the transport.
    fn mobile_controls_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        // The seek bar, edge to edge: the line between the visualizer and
        // the controls. In the tempo's color, as on the desktop.
        let accent = self.transport_accent(ui.ctx(), view);
        let mut position = view.position;
        let changed = ui
            .scope(|ui| {
                ui.spacing_mut().slider_width = ui.available_width();
                ui.spacing_mut().interact_size.y = 28.0;
                super::seek_bar_ui(ui, &mut position, view.length, accent).changed()
            })
            .inner;
        if changed {
            self.send(AudioCommand::Seek(position));
        }

        ui.horizontal(|ui| {
            ui.monospace(format!("{} / {}", fmt_time(view.position), fmt_time(view.length)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(big_icon_button("caret-down", 32.0)).on_hover_text("Fold the controls away").clicked() {
                    self.mobile_state.collapsed = true;
                }
            });
        });

        let title = self.current_title().or_else(|| view.track_name.clone());
        marquee(ui, title.as_deref().unwrap_or("Nothing playing"), egui::TextStyle::Heading);
        let mut detail = Vec::new();
        if let Some(np) = &self.now_playing
            && let Some(list) = self.playlists.lists.get(np.list)
        {
            detail.push(format!("{} {}/{}", list.playlist.name, np.entry + 1, list.playlist.entries.len()));
        }
        if view.has_midi {
            detail.push(view.soundfont_name.clone().unwrap_or_else(|| "no soundfont".to_string()));
        }
        ui.weak(if detail.is_empty() { " ".to_string() } else { detail.join("  \u{b7}  ") });
        ui.add_space(6.0);

        // The transport, centered: shuffle and loop either side of
        // previous, play/pause (biggest) and next.
        let sizes = [40.0, 52.0, 68.0, 52.0, 40.0];
        let gap = 12.0;
        let width: f32 = sizes.iter().sum::<f32>() + gap * (sizes.len() - 1) as f32;
        let row = egui::vec2(ui.available_width(), sizes[2]);
        ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            let shuffle = ui.add(big_icon_button("shuffle", sizes[0]).selected(self.shuffle));
            if shuffle.on_hover_text(if self.shuffle { "Shuffle: on" } else { "Shuffle: off" }).clicked() {
                self.set_shuffle(!self.shuffle);
            }
            if ui.add(big_icon_button("skip-back", sizes[1])).on_hover_text("Previous").clicked() {
                self.previous_track(view.position);
            }
            let (glyph, color) = if view.paused { ("play", palette_color(2)) } else { ("pause", palette_color(1)) };
            if ui
                .add(egui::Button::new(egui::RichText::new(icon(glyph)).size(sizes[2] * 0.5).color(color)).min_size(egui::Vec2::splat(sizes[2])))
                .on_hover_text(if view.paused { "Play" } else { "Pause" })
                .clicked()
            {
                self.send(AudioCommand::TogglePause);
            }
            if ui
                .add_enabled(self.now_playing.is_some(), big_icon_button("skip-forward", sizes[3]))
                .on_hover_text("Next")
                .clicked()
            {
                self.next_track();
            }
            let glyph = if self.loop_mode == LoopMode::One { "repeat-once" } else { "repeat" };
            let hover = match self.loop_mode {
                LoopMode::Off => "Loop: off",
                LoopMode::All => "Loop: the playlist",
                LoopMode::One => "Loop: this track",
            };
            if ui.add(big_icon_button(glyph, sizes[4]).selected(self.loop_mode != LoopMode::Off)).on_hover_text(hover).clicked() {
                self.set_loop_mode(self.loop_mode.cycle());
            }
        });
        ui.add_space(6.0);

        // Which visualizer, and the way to pick another.
        let script = self
            .active_script
            .and_then(|i| self.available_scripts.get(i))
            .map(|p| lua_visualizer::display_name(p))
            .unwrap_or_else(|| "(none)".to_string());
        ui.vertical_centered(|ui| {
            if ui.button(format!("{}  {script}", icon("waveform"))).on_hover_text("Pick a visualizer").clicked() {
                self.mobile_state.page = Page::Scripts;
            }
        });
        ui.add_space(6.0);
    }

    /// On the other pages: what's playing, with play/pause; tapping the
    /// name goes to Now playing.
    fn mobile_now_playing_bar_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        let title = self.current_title().or_else(|| view.track_name.clone());
        ui.horizontal(|ui| {
            let (glyph, color) = if view.paused { ("play", palette_color(2)) } else { ("pause", palette_color(1)) };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let play = egui::Button::new(egui::RichText::new(icon(glyph)).size(20.0).color(color)).min_size(egui::Vec2::splat(40.0));
                if ui.add(play).on_hover_text(if view.paused { "Play" } else { "Pause" }).clicked() {
                    self.send(AudioCommand::TogglePause);
                }
                let title = title.as_deref().unwrap_or("Nothing playing");
                let name = egui::Label::new(egui::RichText::new(title).strong()).truncate().sense(egui::Sense::click());
                if ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| ui.add(name)).inner.clicked() {
                    self.mobile_state.page = Page::NowPlaying;
                }
            });
        });
    }

    /// With the controls folded away: the way back, in a corner.
    fn mobile_expand_handle_ui(&mut self, ctx: &egui::Context) {
        egui::Area::new(egui::Id::new("mobile_expand_handle"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
            .show(ctx, |ui| {
                if ui.add(big_icon_button("caret-up", 40.0)).on_hover_text("Show the controls").clicked() {
                    self.mobile_state.collapsed = false;
                }
            });
    }

    /// The Visualizers page: every script, with its preview; picking one
    /// goes back to Now playing with it running.
    fn mobile_scripts_ui(&mut self, ui: &mut egui::Ui) {
        self.script_error_ui(ui, false);
        egui::ScrollArea::vertical().show(ui, |ui| {
            if let Some(super::previews::Picked::Script(idx)) = self.script_list_ui(ui, false) {
                self.load_script(idx);
                self.mobile_state.page = Page::NowPlaying;
            }
        });
    }

    /// The menu: the pages, then what the desktop's menus open, over
    /// everything else (a tap outside it closes it).
    fn mobile_drawer_ui(&mut self, ctx: &egui::Context) {
        let screen = ctx.content_rect();
        let backdrop = egui::Area::new(egui::Id::new("mobile_drawer_backdrop"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter().rect_filled(rect, 0.0, egui::Color32::from_black_alpha(140));
                response
            })
            .inner;
        if backdrop.clicked() {
            self.mobile_state.drawer_open = false;
        }

        let width = (screen.width() * 0.8).min(320.0);
        let drawer = egui::Area::new(egui::Id::new("mobile_drawer"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                egui::Frame::side_top_panel(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    ui.set_min_height(screen.height());
                    // The version along the bottom, the items filling the
                    // rest (scrolling when they don't fit).
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        ui.add_space(8.0);
                        self.mobile_version_ui(ui);
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| self.mobile_drawer_items_ui(ui));
                        });
                    });
                });
            });
        // Over the backdrop, always: a tap on the backdrop (closing the
        // menu) brings it to the front, and it would cover the menu the
        // next time it opened.
        ctx.move_to_top(drawer.response.layer_id);
    }

    /// The wordmark, a heading's size (the name as a heading if it can't be
    /// drawn). One size wherever it's shown, so it's drawn once.
    fn mobile_wordmark_ui(&mut self, ui: &mut egui::Ui) {
        let height = ui.text_style_height(&egui::TextStyle::Heading) * 1.25;
        match self.wordmark.image(ui.ctx(), height, ui.visuals().panel_fill) {
            Some(image) => ui.add(image),
            None => ui.heading("synththing"),
        };
    }

    /// The version, short; a tap shows all of it (the branch and commit
    /// of a test build).
    fn mobile_version_ui(&mut self, ui: &mut egui::Ui) {
        let short = egui::RichText::new(format!("v{}", crate::build_info::VERSION)).weak();
        if ui.add(egui::Label::new(short).sense(egui::Sense::click())).clicked() {
            let full = format!("synththing v{}", crate::build_info::describe());
            self.mobile_state.toast = Some((full, Instant::now()));
        }
    }

    fn mobile_drawer_items_ui(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().button_padding = egui::vec2(10.0, 10.0);
        ui.add_space(8.0);
        self.mobile_wordmark_ui(ui);
        ui.add_space(8.0);
        let mut picked = None;
        for page in Page::ALL {
            if ui.add(drawer_item(ui, self.mobile_state.page == page, page.icon(), page.title())).clicked() {
                picked = Some(page);
            }
        }
        if let Some(page) = picked {
            self.mobile_state.page = page;
            self.mobile_state.drawer_open = false;
        }

        ui.separator();
        let current = self.mobile_state.page;
        let item = |ui: &mut egui::Ui, page: Page| ui.add(drawer_item(ui, current == page, page.icon(), page.title())).clicked();
        let mut close = false;
        if item(ui, Page::Preferences) {
            self.mobile_state.page = Page::Preferences;
            self.mobile_state.pref_section = None;
            self.mobile_state.editing_themes = false;
            close = true;
        }
        if item(ui, Page::Updates) {
            self.launch_update_check = false;
            self.updater.check(self.update_track());
            self.mobile_state.page = Page::Updates;
            close = true;
        }
        if item(ui, Page::Changelog) {
            self.mobile_state.changelog_since = None;
            self.mobile_state.page = Page::Changelog;
            close = true;
        }
        if item(ui, Page::Log) {
            self.mobile_state.page = Page::Log;
            close = true;
        }
        if item(ui, Page::Credits) {
            self.mobile_state.page = Page::Credits;
            close = true;
        }
        if !cfg!(target_os = "android") && ui.add(drawer_item(ui, false, "desktop", "Desktop layout")).clicked() {
            let ctx = ui.ctx().clone();
            self.set_mobile_layout(&ctx, false);
            close = true;
        }
        if close {
            self.mobile_state.drawer_open = false;
        }
    }
}

impl App {
    /// An available update, on Android: unless installing in the app is
    /// on (Preferences > Updates) and allowed, it's only announced,
    /// once. `None` on the desktop, and on Android when it can be
    /// installed (the usual Install buttons follow); `Some(true)` once the
    /// announcement's been seen.
    pub(super) fn android_update_ui(&mut self, ui: &mut egui::Ui) -> Option<bool> {
        if !cfg!(target_os = "android") {
            return None;
        }
        #[cfg(target_os = "android")]
        if self.config.android_install_updates {
            if crate::android::can_install_packages() {
                return None;
            }
            ui.label("Installing updates in the app is on, but Android doesn't allow synththing to install apps yet.");
            if ui.button("Allow it...").on_hover_text("Settings > Install unknown apps, for synththing").clicked() {
                crate::android::open_install_settings();
            }
        }
        ui.label("Download it from the release page and install it.");
        Some(ui.button("OK").clicked())
    }
}

#[cfg(target_os = "android")]
impl App {
    /// The app went into the background: save what closing the app saves
    /// on desktop (`on_exit`), as Android ends an app in the background
    /// without telling it.
    pub fn backgrounded(&mut self) {
        self.save_session();
        if serde_json::to_string(&self.dock).unwrap_or_default() != self.saved_layout_json {
            self.save_layout();
        }
    }

    /// The Android side of `tick`: carry out the media buttons pressed
    /// outside the app, and keep the media session (the notification, the
    /// lock screen) up to date with what's playing.
    pub(super) fn android_tick(&mut self, view: &EngineView) {
        use crate::android::{self, MediaCommand, Playback};

        for command in android::take_media_commands() {
            match command {
                MediaCommand::Play if view.paused => self.send(AudioCommand::TogglePause),
                MediaCommand::Pause if !view.paused => self.send(AudioCommand::TogglePause),
                MediaCommand::Play | MediaCommand::Pause => {}
                MediaCommand::Next if self.now_playing.is_some() => self.next_track(),
                MediaCommand::Next => {}
                MediaCommand::Previous => self.previous_track(view.position),
                MediaCommand::Seek(to) => self.send(AudioCommand::Seek(to)),
            }
        }

        let has_song = view.has_midi || view.has_audio_file;
        let playback = Playback {
            title: self.current_title().or_else(|| view.track_name.clone()).unwrap_or_default(),
            detail: self
                .now_playing
                .as_ref()
                .and_then(|np| self.playlists.lists.get(np.list))
                .map(|list| list.playlist.name.clone())
                .unwrap_or_default(),
            playing: has_song && !view.paused && !view.finished,
            position_ms: (view.position * 1000.0) as i64,
            length_ms: (view.length * 1000.0) as i64,
            has_next: self.now_playing.is_some(),
        };
        // The session moves the position along by itself while playing:
        // only a change, or a jump (a seek), is worth telling it.
        let stale = match &self.mobile_state.told {
            None => true,
            Some((told, at)) => {
                let expected = told.position_ms + if told.playing { at.elapsed().as_millis() as i64 } else { 0 };
                Playback { position_ms: 0, ..told.clone() } != Playback { position_ms: 0, ..playback.clone() }
                    || (playback.position_ms - expected).abs() > 1500
            }
        };
        // Leaving the app goes into the mini player while a song plays.
        let pip = (playback.playing && self.config.mini_player.unwrap_or(true), self.config.mini_player_shape);
        if self.mobile_state.picture_in_picture != Some(pip) {
            android::set_picture_in_picture_allowed(pip.0, pip.1);
            self.mobile_state.picture_in_picture = Some(pip);
        }
        if stale && (has_song || self.mobile_state.told.is_some()) {
            android::set_playback(&playback);
            self.mobile_state.told = Some((playback, std::time::Instant::now()));
        }
    }
}

/// An icon button `size` points square, big enough for a finger.
/// One of the menu's items: its icon and text, the width of `ui`.
fn drawer_item(ui: &egui::Ui, selected: bool, glyph: &str, text: &str) -> egui::Button<'static> {
    egui::Button::selectable(selected, format!("{}   {text}", icon(glyph))).min_size(egui::vec2(ui.available_width(), 0.0))
}

/// A Preferences section in the list: its icon, title and what's in it,
/// the width of `ui`.
fn section_row(ui: &mut egui::Ui, section: PrefTab) -> egui::Response {
    let height = 60.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    let visuals = ui.style().interact(&response);
    if response.hovered() || response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
    }
    let painter = ui.painter();
    let text = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    let body = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let icon_font = egui::FontId::proportional(22.0);
    painter.text(egui::pos2(rect.left() + 22.0, rect.center().y), egui::Align2::CENTER_CENTER, icon(section.icon()), icon_font.clone(), text);
    let left = rect.left() + 48.0;
    painter.text(egui::pos2(left, rect.center().y - 2.0), egui::Align2::LEFT_BOTTOM, section.title(), body, text);
    painter.text(egui::pos2(left, rect.center().y + 2.0), egui::Align2::LEFT_TOP, section.summary(), small, weak);
    painter.text(
        egui::pos2(rect.right() - 16.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        icon("caret-right"),
        egui::FontId::proportional(16.0),
        weak,
    );
    response
}

fn big_icon_button(name: &str, size: f32) -> egui::Button<'static> {
    // (Growing, the icon is centered in the button.)
    egui::Button::new(egui::RichText::new(icon(name)).size(size * 0.5).atom_grow(true)).min_size(egui::Vec2::splat(size))
}

/// `text` on one line the width of `ui`, scrolling sideways (round and
/// round) when it's too long to fit.
fn marquee(ui: &mut egui::Ui, text: &str, style: egui::TextStyle) {
    let font = style.resolve(ui.style());
    let color = ui.visuals().strong_text_color();
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, galley.size().y), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    if galley.size().x <= width {
        painter.galley(egui::pos2(rect.center().x - galley.size().x / 2.0, rect.top()), galley, color);
        return;
    }
    let period = galley.size().x + MARQUEE_GAP;
    let time = ui.input(|i| i.time) as f32;
    let offset = (time * MARQUEE_SPEED) % period;
    painter.galley(egui::pos2(rect.left() - offset, rect.top()), galley.clone(), color);
    painter.galley(egui::pos2(rect.left() - offset + period, rect.top()), galley, color);
    ui.ctx().request_repaint();
}
