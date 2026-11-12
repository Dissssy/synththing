//! Adding synththing to the applications menu from the app (Linux,
//! `crate::desktop_entry`): the offer (a step of the flow, `flow.rs`), made
//! once on a launch while it isn't there, and the Applications menu group in
//! Preferences > App, which adds it, points it here again or removes it.

use eframe::egui;

use super::flow::Step;
use super::{App, setting_row, settings_group, with_info};
use crate::desktop_entry;

pub(super) struct DesktopEntry {
    /// The flow's "Add to the applications menu" checkbox.
    chosen: bool,
    /// The entry starts this binary (as found at launch, and since as set
    /// here).
    added: bool,
    /// Why adding it didn't work, for the flow's page.
    error: Option<String>,
}

impl DesktopEntry {
    pub(super) fn new() -> Self {
        Self { chosen: true, added: desktop_entry::is_added(), error: None }
    }

    /// Whether to offer it at launch: it isn't there for this binary, and
    /// wasn't asked about before.
    pub(super) fn offer(config: &crate::config::Config) -> bool {
        desktop_entry::can_add() && !desktop_entry::is_added() && !config.install_asked
    }
}

/// Where this binary is, for the texts.
fn here() -> String {
    crate::exe::display()
}

impl App {
    /// The flow's page.
    pub(super) fn desktop_entry_step_ui(&mut self, ui: &mut egui::Ui) {
        settings_group("Applications menu").icon("app-window").hint("Preferences > App adds it any time.").show(ui, |ui| {
            setting_row(ui, |ui| {
                ui.label(
                    "synththing can add itself to your desktop's applications menu and app search, with its icon, \
                     starting it from where it is now. It keeps updating itself there.",
                )
            });
            with_info(ui, &format!("Starting {}", here()), |ui| {
                ui.checkbox(&mut self.desktop_entry.chosen, "Add to the applications menu")
            });
            if let Some(error) = &self.desktop_entry.error {
                setting_row(ui, |ui| ui.colored_label(ui.visuals().error_fg_color, format!("Couldn't add it: {error}")));
            }
        });
    }

    /// Done: add it if that was chosen. Either way it isn't offered again.
    pub(super) fn desktop_entry_done(&mut self) {
        if !self.config.install_asked {
            self.config.install_asked = true;
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
        if !self.desktop_entry.chosen {
            return;
        }
        match desktop_entry::add() {
            Ok(()) => self.desktop_entry.added = true,
            Err(e) => {
                log::warn!("couldn't add to the applications menu: {e:#}");
                self.desktop_entry.error = Some(format!("{e:#}"));
                // (Back to the page, to say why.)
                self.flow.show(Step::DesktopEntry);
            }
        }
    }

    /// Preferences > App: add (or point here again), or remove.
    pub(super) fn desktop_entry_preferences_ui(&mut self, ui: &mut egui::Ui) {
        if !desktop_entry::can_add() {
            return;
        }
        settings_group("Applications menu").icon("app-window").show(ui, |ui| {
            let result = if self.desktop_entry.added {
                setting_row(ui, |ui| ui.weak(format!("In the applications menu, starting {}", here())));
                let mut result = None;
                setting_row(ui, |ui| {
                    if ui.button("Remove from the menu").clicked() {
                        result = Some(desktop_entry::remove().map(|()| false));
                    }
                });
                result
            } else {
                let mut result = None;
                with_info(
                    ui,
                    &format!(
                        "Adds synththing to your desktop's applications menu and app search, with its icon, \
                         starting it from where it is now ({}). Moved somewhere else, add it again from there.",
                        here()
                    ),
                    |ui| {
                        let response = ui.button("Add to the applications menu");
                        if response.clicked() {
                            result = Some(desktop_entry::add().map(|()| true));
                        }
                        response
                    },
                );
                result
            };
            match result {
                Some(Ok(added)) => self.desktop_entry.added = added,
                Some(Err(e)) => self.status = format!("Couldn't change the applications menu: {e:#}"),
                None => {}
            }
        });
    }
}
