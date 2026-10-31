//! Asking about anonymous usage data (`crate::usage`), once, on the first
//! launch that has it; and its switch in Preferences > App, with what the
//! next report holds so far.

use eframe::egui;

use super::{settings_group, with_info, App};

/// What's sent, in a sentence or two, for the question and Preferences.
const WHAT: &str = "Once a day: how often each kind of warning and error came up, which version and system it \
                    was, and crashes, as soon as synththing runs again. Only where in the code something happened, \
                    never what it was about: no song names, files, account or anything that says who you are.";

impl App {
    /// Whether the question is up: not asked yet, and nothing else first
    /// (the welcome window, the tour).
    pub(super) fn asking_usage(&self) -> bool {
        self.config.usage_data.is_none() && crate::usage::available() && !self.welcome.open && !self.tour.running()
    }

    pub(super) fn usage_prompt_ui(&mut self, ctx: &egui::Context) {
        if !self.asking_usage() {
            return;
        }
        let mut answer = None;
        let response = egui::Modal::new(egui::Id::new("usage_prompt")).show(ctx, |ui| {
            ui.set_width(super::fit_width(ui, 470.0));
            ui.heading("Help find problems?");
            ui.add_space(4.0);
            ui.label(format!("synththing can send anonymous usage data to its developers. {WHAT}"));
            ui.add_space(4.0);
            ui.weak("You can change this any time in Preferences > App.");
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Send usage data").clicked() {
                    answer = Some(true);
                }
                if ui.button("Don't send").clicked() {
                    answer = Some(false);
                }
            });
        });
        // (Closing it is a no: it isn't asked again.)
        if answer.is_none() && response.should_close() {
            answer = Some(false);
        }
        if let Some(on) = answer {
            self.set_usage_data(on);
        }
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
