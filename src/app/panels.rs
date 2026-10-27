//! The Apps tab: the panels programs put there through the local API
//! (`panel.rs`), one at a time under a chip per program, with what the
//! user does in them sent back to the program.

use std::collections::HashMap;

use eframe::egui;

use super::App;
use crate::api::ShownPanel;
use crate::panel::{Element, Event, EventKind, Kind};

#[derive(Default)]
pub(super) struct PanelsState {
    /// The API's panels revision these are from.
    revision: u64,
    shown: Vec<ShownPanel>,
    /// The program whose panel shows (its token's hash).
    picked: Option<String>,
    /// What's typed in each textbox, by program and element ID.
    texts: HashMap<(String, String), String>,
}

/// A textbox's widget ID, to tell whether it's being typed in.
fn textbox_id(app: &str, element: &str) -> egui::Id {
    egui::Id::new(("api panel textbox", app, element))
}

impl PanelsState {
    /// Take the panels as they are now: each textbox the program gave a
    /// value takes it, unless it's being typed in.
    fn take(&mut self, shown: Vec<ShownPanel>, ctx: &egui::Context) {
        fn values<'a>(elements: &'a [Element], out: &mut Vec<(&'a str, &'a str)>) {
            for element in elements {
                match &element.kind {
                    Kind::Textbox { value: Some(value), .. } => out.push((&element.id, value)),
                    Kind::Row { elements } => values(elements, out),
                    _ => {}
                }
            }
        }
        self.texts.retain(|(app, _), _| shown.iter().any(|s| &s.app == app));
        for panel in &shown {
            let mut given = Vec::new();
            values(&panel.panel.elements, &mut given);
            for (id, value) in given {
                if !ctx.memory(|m| m.has_focus(textbox_id(&panel.app, id))) {
                    self.texts.insert((panel.app.clone(), id.to_string()), value.to_string());
                }
            }
        }
        if self.picked.as_ref().is_none_or(|picked| !shown.iter().any(|s| &s.app == picked)) {
            self.picked = shown.first().map(|s| s.app.clone());
        }
        self.shown = shown;
    }
}

impl App {
    /// The Apps tab: a chip for each program with a panel, and the picked
    /// one's panel, greyed while its program isn't listening.
    pub(super) fn apps_ui(&mut self, ui: &mut egui::Ui) {
        let Some(server) = self.api.server() else {
            ui.weak("Programs can show panels here through the local API. Turn it on in Preferences > Integrations.");
            return;
        };
        if let Some(shown) = server.panels(&mut self.panels.revision) {
            self.panels.take(shown, ui.ctx());
        }
        let state = &mut self.panels;
        if state.shown.is_empty() {
            ui.weak("No program has a panel here yet. A program allowed to Show a panel (Preferences > Integrations) puts one here with PUT /v1/panel.");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            for shown in &state.shown {
                let picked = state.picked.as_ref() == Some(&shown.app);
                if ui.selectable_label(picked, &shown.name).clicked() {
                    state.picked = Some(shown.app.clone());
                }
            }
        });
        let Some(shown) = state.shown.iter().find(|s| state.picked.as_ref() == Some(&s.app)) else { return };
        let connected = server.connected(&shown.app);
        let mut close = false;
        ui.horizontal(|ui| {
            let (dot, hover) = if connected {
                (egui::Color32::from_rgb(80, 200, 120), "Listening")
            } else {
                (ui.visuals().weak_text_color(), "Not running")
            };
            ui.label(egui::RichText::new("●").color(dot)).on_hover_text(hover);
            ui.strong(if shown.panel.title.is_empty() { &shown.name } else { &shown.panel.title });
            ui.weak(format!("from {}", shown.name));
            if !connected {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.add(super::icon_button("x")).on_hover_text("Close this panel").clicked();
                });
            }
        });
        if !connected {
            ui.weak(format!("{} isn't running. Its panel comes back when it does.", shown.name));
        }
        ui.separator();
        let mut events = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_enabled_ui(connected, |ui| elements_ui(ui, &shown.app, &shown.panel.elements, false, &mut state.texts, &mut events));
        });
        for event in &events {
            server.panel_event(&shown.app, event);
        }
        if close {
            server.close_panel(&shown.app);
        }
    }
}

/// A panel's elements, stacked (or side by side in a row), with what the
/// user did with them added to `events`.
fn elements_ui(ui: &mut egui::Ui, app: &str, elements: &[Element], in_row: bool, texts: &mut HashMap<(String, String), String>, events: &mut Vec<Event>) {
    for element in elements {
        let id = &element.id;
        match &element.kind {
            Kind::Label { text, weak } => {
                if *weak {
                    ui.weak(text);
                } else {
                    ui.label(text);
                }
            }
            Kind::Textbox { hint, multiline, .. } => {
                let text = texts.entry((app.to_string(), id.clone())).or_default();
                let edit = if *multiline { egui::TextEdit::multiline(text) } else { egui::TextEdit::singleline(text) };
                let width = if in_row { 200.0 } else { ui.available_width() };
                let response = ui.add(edit.id(textbox_id(app, id)).hint_text(hint.as_str()).desired_width(width));
                if response.changed() {
                    events.push(Event { element: id.clone(), kind: EventKind::Changed, value: Some(text.clone()) });
                }
                if !*multiline && response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    events.push(Event { element: id.clone(), kind: EventKind::Submitted, value: Some(text.clone()) });
                }
            }
            Kind::Button { text, enabled } => {
                if ui.add_enabled(*enabled, egui::Button::new(text)).clicked() {
                    events.push(Event { element: id.clone(), kind: EventKind::Clicked, value: None });
                }
            }
            Kind::Row { elements } => {
                ui.horizontal_wrapped(|ui| elements_ui(ui, app, elements, true, texts, events));
            }
        }
    }
}
