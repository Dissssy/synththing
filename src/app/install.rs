//! Installing synththing from the app (Windows, `crate::install`): the
//! offer (a step of the flow, `flow.rs`), made once on a launch from
//! anywhere else (a download), and the
//! Install group in Preferences > App, where it's installed, set to start
//! with Windows, or uninstalled.

use eframe::egui;

use super::flow::Step;
use super::{App, setting_combo, setting_label, setting_row, settings_group, with_info};
use crate::config::StartupOpens;
use crate::install;

pub(super) struct Install {
    /// The flow's "Install synththing" checkbox.
    pub(super) chosen: bool,
    /// Delete the exe that was run, once the installed one has taken over.
    delete_old: bool,
    /// Asking whether to uninstall.
    confirm_uninstall: bool,
    /// Whether it starts with Windows (as the registry said at launch, and
    /// since as set here).
    starts_with_windows: bool,
    /// Why installing didn't work, for the flow's Install page.
    error: Option<String>,
}

impl Install {
    pub(super) fn new() -> Self {
        Self {
            chosen: true,
            delete_old: true,
            confirm_uninstall: false,
            starts_with_windows: install::is_installed() && install::starts_with_windows(),
            error: None,
        }
    }

    /// Whether to offer it at launch: run from anywhere else, and not asked
    /// before.
    pub(super) fn offer(config: &crate::config::Config) -> bool {
        install::can_install() && !config.install_asked
    }
}

impl App {
    /// The flow's Install page: it's done (and the app restarts) when the
    /// flow is.
    pub(super) fn install_step_ui(&mut self, ui: &mut egui::Ui) {
        let here = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
        let to = install::dir().map(|p| p.display().to_string()).unwrap_or_default();
        settings_group("Install").icon("download-simple").hint("Preferences > App installs it any time.").show(ui, |ui| {
            setting_row(ui, |ui| {
                ui.label(
                    "synththing moves itself to a folder of its own, adds itself to the Start menu and to Settings > \
                     Apps (to uninstall it from), and can then start with Windows. It keeps updating itself as it \
                     does now.",
                )
            });
            with_info(ui, &format!("To {to}"), |ui| ui.checkbox(&mut self.install.chosen, "Install synththing"));
            with_info(ui, &here, |ui| {
                ui.add_enabled(
                    self.install.chosen,
                    egui::Checkbox::new(&mut self.install.delete_old, "Delete this copy once it's installed"),
                )
            });
            if let Some(error) = &self.install.error {
                setting_row(ui, |ui| ui.colored_label(ui.visuals().error_fg_color, format!("Couldn't install: {error}")));
            }
        });
    }

    /// Done: install if that was chosen, handing over to the installed copy
    /// (`true`: this one's quitting). Either way it isn't offered again.
    pub(super) fn install_done(&mut self, ctx: &egui::Context) -> bool {
        if !self.config.install_asked {
            self.config.install_asked = true;
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
        if !self.install.chosen {
            return false;
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
                    log::warn!("couldn't delete {}: {e}", std::env::current_exe().unwrap_or_default().display());
                }
                self.quit(ctx);
                true
            }
            Err(e) => {
                log::warn!("couldn't install: {e:#}");
                self.install.error = Some(format!("{e:#}"));
                // (Back to the page, to say why.)
                self.flow.show(Step::Install);
                false
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
                            self.install.chosen = true;
                            self.install.error = None;
                            self.flow.show(Step::Install);
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
