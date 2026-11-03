//! The tray icon's side of the app (Windows only, like `crate::tray`):
//! putting the icon up, starting hidden in it, and carrying out what the
//! icon and the mini player (`tray_panel`) are clicked for.

use eframe::egui;

use super::{App, AudioCommand, EngineView, close, config, naming, nice_name, title_of, tray_panel};

impl App {
    /// Put synththing's icon in the tray. Once the window exists (desktop.rs,
    /// as eframe starts).
    pub fn attach_tray(&mut self, ctx: &egui::Context) {
        self.tray = crate::tray::Tray::new(ctx);
    }

    /// Started with Windows (`--autostart`): open as Preferences says, the
    /// window hidden in the tray, or as the mini player. (Without a tray,
    /// as the window.)
    pub fn started_with_windows(&mut self, ctx: &egui::Context) {
        if self.tray.is_none() {
            return;
        }
        match self.config.startup_opens {
            config::StartupOpens::Window => {}
            config::StartupOpens::Tray => self.hide_window(ctx),
            config::StartupOpens::MiniPlayer => {
                self.hide_window(ctx);
                self.open_mini_at_start = true;
            }
        }
    }

    /// Clicks on the tray icon since last frame, and what was clicked in
    /// the tray panel.
    pub(super) fn handle_tray(&mut self, ctx: &egui::Context, view: &EngineView) {
        if self.tray.is_none() {
            return;
        }
        if std::mem::take(&mut self.open_mini_at_start) {
            let native = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0);
            match self.tray.as_ref().and_then(crate::tray::Tray::corner) {
                Some(corner) => {
                    let snapshot = self.tray_snapshot(view);
                    self.tray_panel.update(ctx, snapshot);
                    self.tray_panel.toggle_mini(ctx, corner, native * ctx.zoom_factor());
                }
                None => self.show_window(ctx),
            }
        }
        let Some(tray) = &self.tray else { return };
        for click in tray.clicks() {
            let native = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0);
            let snapshot = self.tray_snapshot(view);
            self.tray_panel.update(ctx, snapshot);
            match click {
                crate::tray::TrayClick::Primary { cursor } => {
                    // The mini player takes over from the main window.
                    if !self.tray_panel.is_open() {
                        self.hide_window(ctx);
                    }
                    self.tray_panel.toggle_mini(ctx, cursor, native * ctx.zoom_factor());
                }
                crate::tray::TrayClick::Secondary => self.show_window(ctx),
            }
        }
        for action in self.tray_panel.actions() {
            match action {
                tray_panel::Action::TogglePause => self.send(AudioCommand::TogglePause),
                tray_panel::Action::Previous => self.previous_track(view.position),
                tray_panel::Action::Next => {
                    if self.has_next() {
                        self.next_track();
                    }
                }
                tray_panel::Action::Volume(volume) => {
                    self.volume = volume.clamp(0.0, 1.0);
                    self.send_volume();
                }
                tray_panel::Action::Soundfont(index) => self.activate_soundfont(index),
                tray_panel::Action::Play(list, entry) => self.play_entry(list, entry),
                tray_panel::Action::ShowWindow => self.show_window(ctx),
                tray_panel::Action::Quit => self.close_with(ctx, close::CloseAction::Quit),
                tray_panel::Action::Effects(effects) => self.apply_script_effects(effects, view),
                tray_panel::Action::Script(index) => self.load_script(index),
            }
        }
        if self.tray_panel.is_open() {
            let snapshot = self.tray_snapshot(view);
            self.tray_panel.update(ctx, snapshot);
        }
    }

    /// What the tray panel shows.
    fn tray_snapshot(&mut self, view: &EngineView) -> tray_panel::Snapshot {
        tray_panel::Snapshot {
            title: self.current_title().or_else(|| view.track_name.clone()),
            paused: view.paused,
            position: view.position,
            length: view.length,
            volume: self.volume,
            soundfonts: self.config.soundfonts.iter().map(|p| nice_name(p)).collect(),
            active_soundfont: self.active_sf,
            playlists: self
                .playlists
                .lists
                .iter()
                .map(|l| {
                    let naming = naming(&self.config, &self.songs);
                    let names = l.playlist.entries.iter().map(|e| title_of(&mut self.song_info, &naming, &e.path)).collect();
                    (l.playlist.name.clone(), names)
                })
                .collect(),
            playing: self.playing_entry().map(|n| (n.list, n.entry)),
            script: self.script.path().map(nice_name),
            scripts: self.available_scripts.iter().map(|p| nice_name(p)).collect(),
            active_script: self.active_script,
            transport: self.script_transport(),
            main_shows_visualizer: !self.hidden && self.visualizer_was_drawn,
        }
    }
}
