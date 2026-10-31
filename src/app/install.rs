//! Installing synththing from the app (Windows, `crate::install`): the
//! offer, made once on a launch from anywhere else (a download), and the
//! Install group in Preferences > App, where it's installed, set to start
//! with Windows, or uninstalled.

use eframe::egui;

use super::{App, setting_combo, setting_label, setting_row, settings_group, with_info};
use crate::config::StartupOpens;
use crate::install;

pub(super) struct Install {
    /// The offer's window is up.
    offer: bool,
    /// Delete the exe that was run, once the installed one has taken over.
    delete_old: bool,
    /// Asking whether to uninstall.
    confirm_uninstall: bool,
    /// Whether it starts with Windows (as the registry said at launch, and
    /// since as set here).
    starts_with_windows: bool,
    /// Why installing didn't work, for the offer's window.
    error: Option<String>,
}

impl Install {
    pub(super) fn new(config: &crate::config::Config) -> Self {
        Self {
            offer: install::can_install() && !config.install_asked,
            delete_old: true,
            confirm_uninstall: false,
            starts_with_windows: install::is_installed() && install::starts_with_windows(),
            error: None,
        }
    }
}

impl App {
    /// The offer to install, once nothing else is asking.
    pub(super) fn install_offer_ui(&mut self, ctx: &egui::Context) {
        if !self.install.offer || self.welcome.open || self.tour.running() || self.asking_usage() {
            return;
        }
        let here = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
        let to = install::dir().map(|p| p.display().to_string()).unwrap_or_default();
        let mut answer = None;
        let response = egui::Modal::new(egui::Id::new("install_offer")).show(ctx, |ui| {
            ui.set_width(super::fit_width(ui, 470.0));
            ui.heading("Install synththing?");
            ui.add_space(4.0);
            ui.label(format!(
                "synththing moves itself to {to}, adds itself to the Start menu and to Settings > Apps \
                 (to uninstall it from), and can then start with Windows. It keeps updating itself as it does now."
            ));
            ui.add_space(6.0);
            ui.checkbox(&mut self.install.delete_old, "Delete this copy once it's installed");
            ui.indent("install_old", |ui| ui.weak(&here));
            if let Some(error) = &self.install.error {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("Couldn't install: {error}"));
            }
            ui.add_space(6.0);
            ui.weak("Preferences > App installs it any time.");
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Install").clicked() {
                    answer = Some(true);
                }
                if ui.button("Keep running from here").clicked() {
                    answer = Some(false);
                }
            });
        });
        if answer.is_none() && response.should_close() {
            answer = Some(false);
        }
        let Some(answer) = answer else { return };
        if !self.config.install_asked {
            self.config.install_asked = true;
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
        if !answer {
            self.install.offer = false;
            return;
        }
        match install::install().and_then(|exe| {
            crate::instance::hand_over_to(&exe)?;
            Ok(())
        }) {
            Ok(()) => {
                // (Gone once this app has closed.)
                if self.install.delete_old
                    && let Err(e) = self_replace::self_delete()
                {
                    log::warn!("couldn't delete {here}: {e}");
                }
                self.install.offer = false;
                self.quit(ctx);
            }
            Err(e) => {
                log::warn!("couldn't install: {e:#}");
                self.install.error = Some(format!("{e:#}"));
            }
        }
    }

    /// Preferences > App: install, or (installed) start with Windows and
    /// uninstall.
    pub(super) fn install_preferences_ui(&mut self, ui: &mut egui::Ui) {
        let installed = install::is_installed();
        if !installed && !install::can_install() {
            return;
        }
        settings_group("Install").icon("download-simple").show(ui, |ui| {
            if !installed {
                with_info(
                    ui,
                    "Moves synththing to a folder of its own, adds it to the Start menu and to Settings > Apps \
                     (to uninstall it from), and lets it start with Windows. Updates carry on as before.",
                    |ui| {
                        let response = ui.button("Install...");
                        if response.clicked() {
                            self.install.offer = true;
                            self.install.error = None;
                        }
                        response
                    },
                );
                return;
            }
            setting_row(ui, |ui| {
                ui.weak(format!("Installed in {}", install::dir().map(|p| p.display().to_string()).unwrap_or_default()));
            });
            let mut starts = self.install.starts_with_windows;
            with_info(
                ui,
                "Starts synththing when you sign in to Windows, opening as chosen below.",
                |ui| ui.checkbox(&mut starts, "Start with Windows"),
            );
            if starts != self.install.starts_with_windows {
                match install::set_starts_with_windows(starts) {
                    Ok(()) => self.install.starts_with_windows = starts,
                    Err(e) => self.status = format!("Couldn't change starting with Windows: {e:#}"),
                }
            }
            if self.install.starts_with_windows {
                let mut opens = self.config.startup_opens;
                setting_row(ui, |ui| {
                    setting_label(ui, "Opens");
                    setting_combo(ui, "startup_opens", 0.0).selected_text(opens.name()).show_ui(ui, |ui| {
                        for choice in StartupOpens::ALL {
                            ui.selectable_value(&mut opens, choice, choice.name());
                        }
                    });
                });
                if opens != self.config.startup_opens {
                    self.config.startup_opens = opens;
                    if let Err(e) = self.config.save() {
                        self.status = format!("Couldn't save preferences: {e}");
                    }
                }
            }
            setting_row(ui, |ui| {
                if ui.button("Uninstall...").clicked() {
                    self.install.confirm_uninstall = true;
                }
            });
        });
    }

    /// Asking whether to uninstall, then doing it and quitting.
    pub(super) fn uninstall_ui(&mut self, ctx: &egui::Context) {
        if !self.install.confirm_uninstall {
            return;
        }
        let data = crate::config::config_dir().map(|d| d.display().to_string()).unwrap_or_default();
        let mut answer = None;
        let response = egui::Modal::new(egui::Id::new("uninstall")).show(ctx, |ui| {
            ui.set_width(super::fit_width(ui, 420.0));
            ui.heading("Uninstall synththing?");
            ui.add_space(4.0);
            ui.label(format!(
                "synththing closes and is removed from this computer, the Start menu and Settings > Apps. \
                 Your songs, settings and soundfonts stay in {data}."
            ));
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Uninstall").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
        if answer.is_none() && response.should_close() {
            answer = Some(false);
        }
        let Some(answer) = answer else { return };
        self.install.confirm_uninstall = false;
        if answer {
            match install::uninstall() {
                Ok(()) => self.quit(ctx),
                Err(e) => self.status = format!("Couldn't uninstall: {e:#}"),
            }
        }
    }
}
