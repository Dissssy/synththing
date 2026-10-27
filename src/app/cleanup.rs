//! Clean up (from the Songs toolbar): the library's clean-up suggestions
//! (`crate::cleanup`), each to apply as it is, written otherwise, or
//! dismiss. Applying makes songs' edits, undone like the details
//! editor's from the Songs toolbar, except for songs that aren't songs,
//! which are offered removed from the library. On the device that looks
//! albums up, the ones it wasn't sure of come first (Needs a look): pick
//! the release each is, or none of them. The rows are worked out on a
//! thread of their own (from a copy of the songs), so applying one
//! doesn't hold the window up.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui;

use super::lookup::{self, Reviewed, Unsure};
use super::{App, fit_width, icon_button};
use crate::cleanup::{self, Kind, Suggestion, What};
use crate::duplicates;
use crate::lookup::{Kept, Tried};
use crate::playlist::PlaylistEntry;
use crate::song_store::SongStore;

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
    /// The copy picked to keep of each duplicate song (by the
    /// suggestion's ID), by index; the best's the default.
    keep: BTreeMap<String, usize>,
    /// The rows being worked out, and what from.
    working: Option<(u64, mpsc::Receiver<Rows>)>,
}

/// Clean up's rows.
struct Rows {
    suggestions: Vec<Suggestion>,
    unsure: Vec<Unsure>,
}

impl Rows {
    /// `store`'s rows: its offline suggestions, then those from what the
    /// lookup `kept`; and, on the device that looks albums up (where it's
    /// `tried` them), the albums it wasn't sure of.
    fn of(store: &SongStore, kept: &Kept, tried: Option<&BTreeMap<String, Tried>>, ignore: &str) -> Self {
        let mut suggestions = cleanup::suggest(store, ignore);
        lookup::artist_suggestions(store, kept, &mut suggestions, ignore);
        suggestions.extend(lookup::musicbrainz_suggestions(store, kept, ignore));
        let unsure = tried.map(|tried| lookup::unsure_albums(store, kept, tried, ignore)).unwrap_or_default();
        Self { suggestions, unsure }
    }
}

impl App {
    /// Make the song `keep` stand in for its duplicates `others`, before
    /// they're removed: it gets what they have that it lacks
    /// (`duplicates::merge`), and their places in playlists and in sets
    /// that play together (a playlist with it already loses theirs).
    fn merge_duplicates(&mut self, keep: &str, others: &[String]) {
        let merged = duplicates::merge(&self.songs, keep, others);
        if let Some(tags) = merged.tags {
            self.songs.set_song_tags(keep, tags);
        }
        self.songs.edit(merged.edits);
        if merged.online.is_some() {
            self.songs.set_online(vec![(keep.to_string(), merged.online)]);
        }
        if merged.position.is_some() {
            self.songs.set_position(keep, merged.position);
        }
        let sets: Vec<(String, Vec<String>)> = self
            .songs
            .together()
            .iter()
            .filter(|(_, keys)| keys.iter().any(|k| others.contains(k)))
            .map(|(id, keys)| (id.clone(), keys.iter().map(|k| if others.contains(k) { keep.to_string() } else { k.clone() }).collect()))
            .collect();
        for (id, keys) in sets {
            self.songs.set_together(&id, keys);
        }
        let kept = self.songs.path(keep);
        let gone: Vec<PathBuf> = others.iter().map(|k| self.songs.path(k)).collect();
        for list in 0..self.playlists.lists.len() {
            let playlist = &mut self.playlists.lists[list].playlist;
            if playlist.smart.is_some() || !playlist.entries.iter().any(|e| gone.contains(&e.path)) {
                continue;
            }
            let mut has = playlist.entries.iter().any(|e| e.path == kept);
            let mut drop = Vec::new();
            for (i, entry) in playlist.entries.iter_mut().enumerate().filter(|(_, e)| gone.contains(&e.path)) {
                if has {
                    drop.push(i);
                } else {
                    *entry = PlaylistEntry { soundfont: entry.soundfont.take(), ..PlaylistEntry::new(kept.clone()) };
                    has = true;
                }
            }
            if drop.is_empty() {
                self.save_playlist(list);
            } else {
                self.remove_entries(list, &drop);
            }
        }
        self.songs_changed();
    }

    /// What the clean-up suggestions are made from: the songs, and how
    /// their names group.
    fn cleanup_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.songs.revision(), &self.config.group_ignore, self.lookup.artists_asked).hash(&mut hasher);
        hasher.finish()
    }

    /// The Clean up modal, while it's open.
    pub(super) fn cleanup_ui(&mut self, ctx: &egui::Context) {
        let Some(mut cleanup) = self.cleanup.take() else { return };
        if let Some((made_from, receive)) = &cleanup.working {
            match receive.try_recv() {
                Ok(rows) => {
                    cleanup.suggestions = rows.suggestions;
                    cleanup.unsure = rows.unsure;
                    cleanup.made_from = Some(*made_from);
                    cleanup.working = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => cleanup.working = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let revision = self.cleanup_revision();
        if cleanup.made_from != Some(revision) && cleanup.working.is_none() {
            let (store, kept, tried, ignore) = (self.songs.clone(), self.lookup.kept.clone(), self.lookup_tried(), self.config.group_ignore.clone());
            let (send, receive) = mpsc::channel();
            let repaint = ctx.clone();
            let spawned = std::thread::Builder::new().name("clean up".into()).spawn(move || {
                let _ = send.send(Rows::of(&store, &kept, tried.as_ref(), &ignore));
                repaint.request_repaint();
            });
            match spawned {
                Ok(_) => cleanup.working = Some((revision, receive)),
                Err(e) => log::warn!("couldn't work out Clean up's rows: {e}"),
            }
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
            if cleanup.made_from.is_none() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Looking through your songs...");
                });
            } else if shown.is_empty() && cleanup.unsure.is_empty() {
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
                            let fixed = matches!(suggestion.kind, Kind::Compilation | Kind::Junk | Kind::Duplicate)
                                || matches!(suggestion.what, What::Fields { .. } | What::Online { .. });
                            let look = suggestion.kind == Kind::LengthDiffers;
                            if !look && ui.add_enabled(fixed || !to.trim().is_empty(), icon_button("check")).on_hover_text("Apply").clicked() {
                                apply.push(*i);
                            }
                            if look {
                                ui.add(egui::Label::new(to.as_str()).truncate());
                            } else if suggestion.kind == Kind::Junk {
                                ui.label("Remove from the library");
                            } else if suggestion.kind == Kind::Duplicate {
                                let keep = cleanup.keep.entry(id.clone()).or_default();
                                let copy = |key: &String| self.songs.get_by_key(key).map(duplicates::describe).unwrap_or_default();
                                egui::ComboBox::from_id_salt(("duplicate", id))
                                    .width(ui.available_width())
                                    .truncate()
                                    .selected_text(format!("Keep {}", copy(&suggestion.keys[*keep])))
                                    .show_ui(ui, |ui| {
                                        for (i, key) in suggestion.keys.iter().enumerate() {
                                            ui.selectable_value(keep, i, copy(key));
                                        }
                                    });
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
            // (Gone from the rows now, not once they're worked out again.)
            let album = match &reviewed {
                Reviewed::This(album, _) | Reviewed::NoneOf(album, _) => album.id.clone(),
            };
            cleanup.unsure.retain(|u| u.album.id != album);
            self.reviewed(reviewed);
        }
        // (Junk's removed, asked about first: `App::remove_songs_ui`; so
        // are duplicates, once they're merged onto the copy kept.)
        let (junk, apply): (Vec<usize>, Vec<usize>) = apply.into_iter().partition(|&i| cleanup.suggestions[i].kind == Kind::Junk);
        for i in junk {
            self.remove_songs.extend(cleanup.suggestions[i].keys.iter().map(|k| self.songs.path(k)));
        }
        let (copies, apply): (Vec<usize>, Vec<usize>) = apply.into_iter().partition(|&i| cleanup.suggestions[i].kind == Kind::Duplicate);
        for i in copies {
            let keys = &cleanup.suggestions[i].keys;
            let keep = cleanup.keep.get(&cleanup.suggestions[i].id(&ignore)).copied().unwrap_or_default().min(keys.len() - 1);
            let others: Vec<String> = keys.iter().enumerate().filter(|(j, _)| *j != keep).map(|(_, k)| k.clone()).collect();
            self.merge_duplicates(&keys[keep], &others);
            self.remove_songs.extend(others.iter().map(|k| self.songs.path(k)));
            self.remove_everywhere = true;
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
            let mut i = 0;
            cleanup.suggestions.retain(|_| {
                i += 1;
                !apply.contains(&(i - 1))
            });
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
            egui::ComboBox::from_id_salt(("unsure", &album.id)).width(ui.available_width()).truncate().selected_text(&unsure.lines[*pick]).show_ui(ui, |ui| {
                for (i, line) in unsure.lines.iter().enumerate() {
                    ui.selectable_value(pick, i, line);
                }
            });
        });
    });
    reviewed
}

/// What a suggestion's of, short and in full.
fn describe(suggestion: &Suggestion) -> (String, String) {
    match &suggestion.what {
        What::Text { field, text } => (text.clone(), format!("{}: {text}", field.label())),
        What::Name { spellings, artist: None } => (spellings.join(" / "), format!("One name spelled {} ways:\n{}", spellings.len(), spellings.join("\n"))),
        What::Name { spellings, artist: Some(_) } => {
            (spellings.join(" / "), format!("{} at MusicBrainz, spelled here as:\n{}", suggestion.to, spellings.join("\n")))
        }
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
        What::Duplicates { title } => (
            format!("{title} ({} copies)", suggestion.keys.len()),
            format!(
                "{title} is in your library {} times, as one recording at MusicBrainz. Applying keeps the copy picked, puts what the others have onto it (playlists, tags and stars, edits, its match), then asks to remove the others.",
                suggestion.keys.len()
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
