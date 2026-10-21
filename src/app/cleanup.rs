//! Clean up (from the Songs toolbar): the library's clean-up suggestions
//! (`crate::cleanup`), each to apply as it is, written otherwise, or
//! dismiss. Applying makes songs' edits, undone like the details
//! editor's from the Songs toolbar.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui;

use super::{fit_width, icon_button, App};
use crate::cleanup::{self, Suggestion, What};

/// The Clean up window, while it's open.
#[derive(Default)]
pub(super) struct CleanUp {
    /// What the suggestions were made from (`App::cleanup_revision`).
    made_from: Option<u64>,
    suggestions: Vec<Suggestion>,
    /// What suggestions are to be written as instead, by ID.
    to: BTreeMap<String, String>,
}

impl App {
    /// What the clean-up suggestions are made from: the songs, and how
    /// their names group.
    fn cleanup_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.songs.revision(), &self.config.group_ignore).hash(&mut hasher);
        hasher.finish()
    }

    /// The Clean up modal, while it's open.
    pub(super) fn cleanup_ui(&mut self, ctx: &egui::Context) {
        let Some(mut cleanup) = self.cleanup.take() else { return };
        let revision = self.cleanup_revision();
        if cleanup.made_from != Some(revision) {
            cleanup.suggestions = cleanup::suggest(&self.songs, &self.config.group_ignore);
            cleanup.made_from = Some(revision);
        }
        let ignore = self.config.group_ignore.clone();
        let dismissed = &self.config.cleanup_dismissed;
        let shown: Vec<(usize, String)> =
            cleanup.suggestions.iter().enumerate().map(|(i, s)| (i, s.id(&ignore))).filter(|(_, id)| !dismissed.contains(id)).collect();
        let hidden = cleanup.suggestions.len() - shown.len();
        let mut apply: Vec<usize> = Vec::new();
        let mut dismiss: Vec<String> = Vec::new();
        let mut show: Option<Vec<PathBuf>> = None;
        let mut bring_back = false;
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("cleanup")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 640.0));
            ui.heading("Clean up");
            ui.weak("Suggestions from what your songs' details say. Nothing changes until you apply one, and the files themselves are never written.");
            ui.add_space(4.0);
            if shown.is_empty() {
                ui.label("Nothing to clean up.");
            }
            let row_height = ui.spacing().interact_size.y;
            let from_width = (ui.available_width() * 0.4).max(120.0);
            egui::ScrollArea::vertical().max_height(400.0).auto_shrink([false, true]).show_rows(ui, row_height, shown.len(), |ui, rows| {
                for (i, id) in &shown[rows] {
                    let suggestion = &cleanup.suggestions[*i];
                    let to = cleanup.to.entry(id.clone()).or_insert_with(|| suggestion.to.clone());
                    ui.horizontal(|ui| {
                        let (from, hover) = describe(suggestion);
                        ui.allocate_ui_with_layout(egui::vec2(from_width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_width(from_width);
                            ui.weak(suggestion.kind.label());
                            ui.add(egui::Label::new(from).truncate()).on_hover_text(hover);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let songs = suggestion.keys.len();
                            if ui.add(icon_button("info")).on_hover_text(format!("See its {songs} song{}", if songs == 1 { "" } else { "s" })).clicked() {
                                show = Some(suggestion.keys.iter().map(|k| self.songs.path(k)).collect());
                            }
                            if ui.add(icon_button("x")).on_hover_text("Dismiss: leave it as it is").clicked() {
                                dismiss.push(id.clone());
                            }
                            if ui.add_enabled(!to.trim().is_empty(), icon_button("check")).on_hover_text("Apply").clicked() {
                                apply.push(*i);
                            }
                            ui.add(egui::TextEdit::singleline(to).desired_width(f32::INFINITY))
                                .on_hover_text("What it'll be written as. Separate artists with semicolons.");
                        });
                    });
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if hidden > 0 && ui.button(format!("Bring back {hidden} dismissed")).clicked() {
                    bring_back = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close |= ui.button("Close").clicked();
                    if !shown.is_empty() && ui.button(format!("Apply all {}", shown.len())).clicked() {
                        apply = shown.iter().map(|(i, _)| *i).collect();
                    }
                });
            });
        });
        if !apply.is_empty() {
            let chosen: Vec<(&Suggestion, &str)> = apply
                .iter()
                .map(|&i| {
                    let suggestion = &cleanup.suggestions[i];
                    let to = cleanup.to.get(&suggestion.id(&ignore)).map_or(suggestion.to.as_str(), String::as_str);
                    (suggestion, to)
                })
                .filter(|(_, to)| !to.trim().is_empty())
                .collect();
            let edits = cleanup::edits(&self.songs, &chosen, &ignore);
            let before = self.songs.edit(edits);
            if !before.is_empty() {
                let songs = before.iter().map(|(key, ..)| key).collect::<std::collections::BTreeSet<_>>().len();
                self.last_edit = Some(before);
                self.songs_changed();
                self.status = format!("Cleaned up {songs} song{}.", if songs == 1 { "" } else { "s" });
            }
        }
        if !dismiss.is_empty() || bring_back {
            if bring_back {
                self.config.cleanup_dismissed.clear();
            }
            self.config.cleanup_dismissed.extend(dismiss);
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
        if let Some(paths) = show {
            // (The details open over it; Clean up's there when they close.)
            self.details_for = Some(paths);
        }
        if !(close || response.should_close()) {
            self.cleanup = Some(cleanup);
        }
    }
}

/// What a suggestion's of, short and in full.
fn describe(suggestion: &Suggestion) -> (String, String) {
    match &suggestion.what {
        What::Text { field, text } => (text.clone(), format!("{}: {text}", field.label())),
        What::Name { spellings } => (spellings.join(" / "), format!("One name spelled {} ways:\n{}", spellings.len(), spellings.join("\n"))),
    }
}
