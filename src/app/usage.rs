//! Asking about anonymous usage data (`crate::usage`), once, as a step of
//! the flow (`flow.rs`) on the first launch that has it; and its switch in Preferences > App, with what the
//! next report holds so far.

use eframe::egui;

use super::{setting_row, settings_group, with_info, App};

/// What's sent, in a sentence or two, for the question and Preferences.
const WHAT: &str = "Once a day: how often each kind of warning and error came up, which version and system it \
                    was, how long frames took on what kind of graphics chip and with which display settings, and \
                    crashes, as soon as synththing runs again. Only where in the code something happened, never \
                    what it was about: no song names, files, account or anything that says who you are.";

impl App {
    /// The flow's Usage data page: the answer's taken when it's done.
    pub(super) fn usage_step_ui(&mut self, ui: &mut egui::Ui) {
        settings_group("Usage data")
            .icon("chart-bar")
            .hint("You can change this any time in Preferences > App.")
            .show(ui, |ui| {
                setting_row(ui, |ui| ui.label(format!("synththing can send anonymous usage data to its developers. {WHAT}")));
                setting_row(ui, |ui| ui.checkbox(&mut self.send_usage_data, "Send anonymous usage data"));
            });
    }

    /// Done: the answer (unticked is a no; it isn't asked again).
    pub(super) fn usage_done(&mut self) {
        self.set_usage_data(self.send_usage_data);
    }

    fn set_usage_data(&mut self, on: bool) {
        self.config.usage_data = Some(on);
        crate::usage::set_on(on);
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save preferences: {e}");
        }
    }

    /// The Usage data group of Preferences > App.
    pub(super) fn usage_preferences_ui(&mut self, ui: &mut egui::Ui) {
        if !crate::usage::available() {
            return;
        }
        settings_group("Usage data").icon("chart-bar").show(ui, |ui| {
            let was = self.config.usage_data == Some(true);
            let mut on = was;
            with_info(ui, &format!("{WHAT} Off: nothing is sent, and what's counted so far is deleted."), |ui| {
                ui.checkbox(&mut on, "Send anonymous usage data")
            });
            if on != was {
                self.set_usage_data(on);
            }
            if let Some(preview) = crate::usage::preview() {
                egui::CollapsingHeader::new("See what's sent").id_salt("usage_preview").show(ui, |ui| {
                    ui.weak("The next report so far. The server may want fewer of these lines; it gets only those.");
                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(preview).monospace()).wrap());
                    });
                });
            }
        });
    }
}
