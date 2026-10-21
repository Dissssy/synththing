//! A song's details (Song info, from a song's menu or the Songs toolbar):
//! its editable fields (`fields::editable`), whose edits are kept in the
//! synced library over what its file says (`song_store::Song::edits`),
//! and every detail read from its file. With several songs it's a bulk
//! editor: a field they differ in shows "Mixed", and only the fields
//! touched are written. The last save can be undone from the Songs
//! toolbar, all of its songs at once.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui;

use super::{fit_width, App};
use crate::fields::{self, Facts, Field};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Details,
    File,
}

/// What's to be done to a field on saving.
#[derive(Clone, PartialEq)]
enum Change {
    Set(String),
    /// Back to what each song's file says.
    Revert,
}

/// The details open: of which songs, and what's been changed.
pub(super) struct SongDetails {
    paths: Vec<PathBuf>,
    /// The library's songs among them, by key: only they can be edited.
    keys: Vec<String>,
    tab: Tab,
    /// Each editable field's text as shown, and whether the songs differ
    /// in it.
    texts: BTreeMap<Field, (String, bool)>,
    changes: BTreeMap<Field, Change>,
}

impl App {
    /// Open the details of the songs at `paths`.
    fn open_details(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let keys: Vec<String> = paths.iter().filter_map(|p| self.songs.key(p)).filter(|k| self.songs.get_by_key(k).is_some()).collect();
        let texts = fields::editable()
            .map(|def| {
                let mut values = keys.iter().filter_map(|k| self.songs.get_by_key(k)).map(|song| field_text(&Facts::of(song), def.field));
                let first = values.next().unwrap_or_default();
                let mixed = values.any(|v| v != first);
                (def.field, if mixed { (String::new(), true) } else { (first, false) })
            })
            .collect();
        let tab = if keys.is_empty() { Tab::File } else { Tab::Details };
        self.song_details = Some(SongDetails { paths, keys, tab, texts, changes: BTreeMap::new() });
    }

    /// The details modal, while it's open.
    pub(super) fn song_details_ui(&mut self, ctx: &egui::Context) {
        if let Some(paths) = self.details_for.take() {
            self.open_details(paths);
        }
        let Some(mut details) = self.song_details.take() else { return };
        let mut close = false;
        let mut save = false;
        let one = details.paths.len() == 1;
        let response = egui::Modal::new(egui::Id::new("song_details")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 480.0));
            if one {
                ui.heading(self.song_title(&details.paths[0]));
                // (A library song by the file it came from; one played
                // from elsewhere by where it is.)
                match self.songs.get(&details.paths[0]) {
                    Some(song) => ui.weak(&song.name),
                    None => ui.weak(details.paths[0].display().to_string()),
                };
            } else {
                ui.heading(format!("{} songs", details.paths.len()));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!details.keys.is_empty(), |ui| ui.selectable_value(&mut details.tab, Tab::Details, "Details"))
                    .inner
                    .on_disabled_hover_text("Only songs in your library can be edited");
                if one {
                    ui.selectable_value(&mut details.tab, Tab::File, "File");
                }
            });
            ui.separator();
            match details.tab {
                Tab::Details => self.details_tab_ui(ui, &mut details),
                Tab::File => self.file_tab_ui(ui, &details.paths[0]),
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if details.changes.is_empty() {
                    close |= ui.button("Close").clicked();
                } else {
                    save |= ui.button("Save").clicked();
                    close |= ui.button("Cancel").clicked();
                }
            });
        });
        if save {
            self.save_details(&details);
        } else if !(close || response.should_close()) {
            self.song_details = Some(details);
        }
    }

    /// The editable fields, each with a button back to what the file says
    /// when an edit says otherwise.
    fn details_tab_ui(&self, ui: &mut egui::Ui, details: &mut SongDetails) {
        let SongDetails { keys, texts, changes, .. } = details;
        let songs: Vec<_> = keys.iter().filter_map(|k| self.songs.get_by_key(k)).collect();
        if keys.len() > 1 {
            ui.weak("Only the fields you change are written, to every one of them.");
        }
        egui::Grid::new("song_details_grid").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
            for def in fields::editable() {
                let field = def.field;
                let (text, mixed) = texts.get_mut(&field).expect("every editable field has its text");
                ui.label(def.label);
                let hint = if *mixed && !changes.contains_key(&field) { "Mixed" } else { "" };
                let response = ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(260.0));
                if response.changed() {
                    // (Typed into and emptied again, "Mixed" stays as it is.)
                    if *mixed && text.is_empty() {
                        changes.remove(&field);
                    } else {
                        changes.insert(field, Change::Set(text.clone()));
                    }
                }
                let edited = changes.get(&field) != Some(&Change::Revert) && songs.iter().any(|s| s.edits.contains_key(&field));
                if edited {
                    let unedited: Vec<String> = songs.iter().map(|s| field_text(&Facts::of(s).unedited(), field)).collect();
                    let hover = match unedited.as_slice() {
                        [one] if one.is_empty() => "Back to what the file says (nothing)".to_string(),
                        [one] => format!("Back to what the file says: {one}"),
                        _ => "Back to what each file says".to_string(),
                    };
                    if ui.add(super::icon_button("arrow-counter-clockwise")).on_hover_text(hover).clicked() {
                        changes.insert(field, Change::Revert);
                        let same = unedited.iter().all(|u| *u == unedited[0]);
                        *text = if same { unedited[0].clone() } else { String::new() };
                        *mixed = !same;
                    }
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
    }

    /// Every detail read from the file at `path`.
    fn file_tab_ui(&mut self, ui: &mut egui::Ui, path: &std::path::Path) {
        match self.song_info.get(path) {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Reading...");
                });
            }
            Some(info) if info.details.is_empty() => {
                ui.weak("Nothing to show for this file.");
            }
            Some(info) => {
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    egui::Grid::new("song_info_grid").num_columns(2).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                        for (key, value) in &info.details {
                            ui.strong(key);
                            ui.add(egui::Label::new(value).wrap());
                            ui.end_row();
                        }
                    });
                });
            }
        }
    }

    /// Write the details' changes to their songs (an edit the same as
    /// what the file says is dropped instead), keeping how to undo them.
    fn save_details(&mut self, details: &SongDetails) {
        let mut changes = Vec::new();
        for key in &details.keys {
            let Some(song) = self.songs.get_by_key(key) else { continue };
            let unedited = Facts::of(song).unedited();
            for (&field, change) in &details.changes {
                let value = match change {
                    Change::Set(text) if text.trim() != field_text(&unedited, field) => Some(text.trim().to_string()),
                    _ => None,
                };
                changes.push((key.clone(), field, value));
            }
        }
        let before = self.songs.edit(changes);
        if before.is_empty() {
            return;
        }
        let songs = before.iter().map(|(key, ..)| key).collect::<std::collections::BTreeSet<_>>().len();
        self.last_edit = Some(before);
        self.songs_changed();
        self.status = format!("Edited {songs} song{}.", if songs == 1 { "" } else { "s" });
    }

    /// Undo the last save of songs' details.
    pub(super) fn undo_edit(&mut self) {
        let Some(before) = self.last_edit.take() else { return };
        self.songs.edit(before);
        self.songs_changed();
        self.status = "Undid the last edit.".to_string();
    }
}

/// A field's text as the editor shows it: numbers as they're shown.
fn field_text(song: &Facts, field: Field) -> String {
    song.value(field).map(|v| v.text).unwrap_or_default()
}
