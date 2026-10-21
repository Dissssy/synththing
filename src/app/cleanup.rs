//! Clean up (from the Songs toolbar): the library's clean-up suggestions
//! (`crate::cleanup`), each to apply as it is, written otherwise, or
//! dismiss. Applying makes songs' edits, undone like the details
//! editor's from the Songs toolbar, except for songs that aren't songs,
//! which are offered removed from the library. On the device that looks
//! albums up, the ones it wasn't sure of come first (Needs a look): pick
//! the release each is, or none of them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui;

use super::lookup::{Reviewed, Unsure};
use super::{App, fit_width, icon_button};
use crate::cleanup::{self, Kind, Suggestion, What};

/// The Clean up window, while it's open.
#[derive(Default)]
pub(super) struct CleanUp {
    /// What the suggestions were made from (`App::cleanup_revision`).
    made_from: Option<u64>,
    suggestions: Vec<Suggestion>,
    /// What suggestions are to be written as instead, by ID.
    to: BTreeMap<String, String>,
    /// The albums the lookup wasn't sure of.
    unsure: Vec<Unsure>,
    /// The release picked for each (by its album's id), by index.
    picks: BTreeMap<String, usize>,
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
            cleanup.suggestions.extend(self.musicbrainz_suggestions());
            cleanup.unsure = self.unsure_albums();
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
        let mut reviewed: Option<Reviewed> = None;
        let response = egui::Modal::new(egui::Id::new("cleanup")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 640.0));
            ui.heading("Clean up");
            ui.weak("Suggestions from what your songs' details say. Nothing changes until you apply one, and the files themselves are never written.");
            ui.add_space(4.0);
            if shown.is_empty() && cleanup.unsure.is_empty() {
                ui.label("Nothing to clean up.");
            }
            let row_height = ui.spacing().interact_size.y;
            let from_width = (ui.available_width() * 0.4).max(120.0);
            for unsure in &cleanup.unsure {
                let pick = cleanup.picks.entry(unsure.album.id.clone()).or_default();
                if let Some(r) = unsure_row_ui(ui, unsure, pick, from_width, row_height, &mut show, |k| self.songs.path(k)) {
                    reviewed = Some(r);
                }
            }
            if !cleanup.unsure.is_empty() {
                ui.separator();
            }
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
                            let fixed = matches!(suggestion.kind, Kind::Compilation | Kind::Junk)
                                || matches!(suggestion.what, What::Fields { .. } | What::Online { .. });
                            let look = suggestion.kind == Kind::LengthDiffers;
                            if !look && ui.add_enabled(fixed || !to.trim().is_empty(), icon_button("check")).on_hover_text("Apply").clicked() {
                                apply.push(*i);
                            }
                            if look {
                                ui.add(egui::Label::new(to.as_str()).truncate());
                            } else if suggestion.kind == Kind::Junk {
                                ui.label("Remove from the library");
                            } else if suggestion.kind == Kind::Compilation {
                                ui.label(format!("Mark a compilation, by {to}"));
                            } else if fixed {
                                let more = if songs > 1 { format!(", and {} more like it", songs - 1) } else { String::new() };
                                ui.add(egui::Label::new(format!("{to}{more}")).truncate());
                            } else {
                                ui.add(egui::TextEdit::singleline(to).desired_width(f32::INFINITY))
                                    .on_hover_text("What it'll be written as. Separate artists with semicolons.");
                            }
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
                    let applicable: Vec<usize> = shown.iter().map(|(i, _)| *i).filter(|&i| cleanup.suggestions[i].kind != Kind::LengthDiffers).collect();
                    if !applicable.is_empty() && ui.button(format!("Apply all {}", applicable.len())).clicked() {
                        apply = applicable;
                    }
                });
            });
        });
        if let Some(reviewed) = reviewed {
            self.reviewed(reviewed);
        }
        // (Junk's removed, asked about first: `App::remove_songs_ui`.)
        let (junk, apply): (Vec<usize>, Vec<usize>) = apply.into_iter().partition(|&i| cleanup.suggestions[i].kind == Kind::Junk);
        for i in junk {
            self.remove_songs.extend(cleanup.suggestions[i].keys.iter().map(|k| self.songs.path(k)));
        }
        if !apply.is_empty() {
            let chosen: Vec<(&Suggestion, &str)> = apply
                .iter()
                .map(|&i| {
                    let suggestion = &cleanup.suggestions[i];
                    let to = cleanup.to.get(&suggestion.id(&ignore)).map_or(suggestion.to.as_str(), String::as_str);
                    (suggestion, to)
                })
                .filter(|(s, to)| s.kind == Kind::Compilation || !to.trim().is_empty())
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

/// A Needs a look row: the album, the release it might be (picked from
/// those it might), and This one or None of these.
fn unsure_row_ui(
    ui: &mut egui::Ui,
    unsure: &Unsure,
    pick: &mut usize,
    from_width: f32,
    row_height: f32,
    show: &mut Option<Vec<PathBuf>>,
    path: impl Fn(&str) -> PathBuf,
) -> Option<Reviewed> {
    let album = &unsure.album;
    *pick = (*pick).min(unsure.releases.len() - 1);
    let mut reviewed = None;
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(from_width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(from_width);
            ui.weak("Needs a look");
            let by = album.artist.as_deref().map(|a| format!(" by {a}")).unwrap_or_default();
            ui.add(egui::Label::new(&album.name).truncate()).on_hover_text(format!(
                "{}{by}, {} songs: the lookup isn't sure which release it is.",
                album.name,
                album.songs.len()
            ));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let songs = album.songs.len();
            if ui.add(icon_button("info")).on_hover_text(format!("See its {songs} song{}", if songs == 1 { "" } else { "s" })).clicked() {
                *show = Some(album.songs.iter().map(|s| path(&s.key)).collect());
            }
            if ui.add(icon_button("x")).on_hover_text("None of these: leave it unmatched, and never take these for it").clicked() {
                reviewed = Some(Reviewed::NoneOf(album.clone(), unsure.releases.iter().map(|r| r.id.clone()).collect()));
            }
            let release = &unsure.releases[*pick];
            if ui.add(icon_button("check")).on_hover_text("This one: match it to the release picked").clicked() {
                reviewed = Some(Reviewed::This(album.clone(), release.clone()));
            }
            if ui.add(icon_button("arrow-square-out")).on_hover_text("Open the release picked on MusicBrainz").clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(format!("https://musicbrainz.org/release/{}", release.id)));
            }
            egui::ComboBox::from_id_salt(("unsure", &album.id)).width(ui.available_width()).truncate().selected_text(unsure.describe(release)).show_ui(
                ui,
                |ui| {
                    for (i, release) in unsure.releases.iter().enumerate() {
                        ui.selectable_value(pick, i, unsure.describe(release));
                    }
                },
            );
        });
    });
    reviewed
}

/// What a suggestion's of, short and in full.
fn describe(suggestion: &Suggestion) -> (String, String) {
    match &suggestion.what {
        What::Text { field, text } => (text.clone(), format!("{}: {text}", field.label())),
        What::Name { spellings } => (spellings.join(" / "), format!("One name spelled {} ways:\n{}", spellings.len(), spellings.join("\n"))),
        What::Album { name } => {
            (name.clone(), format!("Album: {name}, by several artists with no album artist. Marked a compilation, it's by Various Artists."))
        }
        What::Fields { from, set, .. } => {
            let n = set.len();
            let label = suggestion.kind.label();
            (
                from.clone(),
                format!(
                    "{label}: {from}{}",
                    if n > 1 { format!("\n(and {} more songs like it: see them with the info button)", n - 1) } else { String::new() }
                ),
            )
        }
        What::Online { field, album, changes, .. } => {
            let mut lines: Vec<String> = changes.iter().take(20).map(|(now, to)| format!("“{now}” to “{to}”")).collect();
            if changes.len() > 20 {
                lines.push(format!("and {} more", changes.len() - 20));
            }
            (format!("{album}: {}", field.label()), format!("{} on {album}, as MusicBrainz has it:\n{}", field.label(), lines.join("\n")))
        }
        What::Length { title, .. } => (
            title.clone(),
            format!(
                "{title} is far from its track's length at MusicBrainz: it may be another version, or a bad rip. Nothing to apply; dismiss it once you've had a look."
            ),
        ),
        What::AppleDouble => {
            let n = suggestion.keys.len();
            (
                format!("{n} macOS \"._\" file{}", if n == 1 { "" } else { "s" }),
                "Files whose names start with \"._\" are the notes macOS's Finder leaves beside a file on drives that can't hold them, not songs.".to_string(),
            )
        }
    }
}
