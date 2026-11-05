//! The tray icon's side of the app (Windows only, like `crate::tray`):
//! putting the icon up, starting hidden in it, and carrying out what the
//! icon is clicked for (the mini player is `mini_player`).

use eframe::egui;

use super::{App, config};

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

    /// Clicks on the tray icon since last frame: a left-click turns the
    /// window into the mini player, or puts the mini player away; a
    /// right-click brings the window back.
    pub(super) fn handle_tray(&mut self, ctx: &egui::Context) {
        let Some(tray) = &self.tray else { return };
        let (clicks, corner) = (tray.clicks(), tray.corner());
        let native = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0) * ctx.zoom_factor();
        if std::mem::take(&mut self.open_mini_at_start) {
            match corner {
                Some(corner) => self.open_mini(ctx, corner, native),
                None => self.show_window(ctx),
            }
        }
        for click in clicks {
            match click {
                crate::tray::TrayClick::Primary { .. } if self.mini_open() => self.hide_window(ctx),
                crate::tray::TrayClick::Primary { cursor } => self.open_mini(ctx, cursor, native),
                crate::tray::TrayClick::Secondary => self.show_window(ctx),
            }
        }
    }
}
