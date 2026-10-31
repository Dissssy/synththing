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
use crate::song_info::format_length;
use crate::fields::{self, Facts, Field, Kind, Section};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Details,
    /// What the names sort as (`Field::sorted_as`).
    Sorting,
    /// How it plays.
    Options,
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
        let mut preview = None;
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
                for (tab, label) in [(Tab::Details, "Details"), (Tab::Sorting, "Sorting"), (Tab::Options, "Options")] {
                    ui.add_enabled_ui(!details.keys.is_empty(), |ui| ui.selectable_value(&mut details.tab, tab, label))
                        .inner
                        .on_disabled_hover_text("Only songs in your library can be edited");
                }
                if one {
                    ui.selectable_value(&mut details.tab, Tab::File, "File");
                }
            });
            ui.separator();
            match details.tab {
                Tab::Details => self.details_tab_ui(ui, &mut details, Section::Details),
                Tab::Sorting => {
                    ui.weak("What each name sorts as, if not itself: \"Beatles\" for \"The Beatles\". Empty sorts it as it is (past a leading The, A or An, as Preferences > Browsing says).");
                    self.details_tab_ui(ui, &mut details, Section::Sorting);
                }
                Tab::Options => {
                    ui.weak("How it plays: times as 1:23, the volume from -100 to 100%. Remember position starts it again where you left it.");
                    preview = self.options_range_ui(ui, &mut details);
                    self.details_tab_ui(ui, &mut details, Section::Options);
                    ui.separator();
                    self.together_ui(ui, &details);
                    ui.separator();
                    self.plays_ui(ui, &details);
                }
                Tab::File => {
                    self.file_tab_ui(ui, &details.paths[0]);
                    self.online_match_ui(ui, &details.paths[0]);
                }
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
        if let Some(at) = preview {
            self.preview_at(&details.paths[0], at);
        }
        if save {
            self.save_details(&details);
        } else if !(close || response.should_close()) {
            self.song_details = Some(details);
        }
    }

    /// The editable fields Song info shows in `section`, each with a
    /// button back to what the file says when an edit says otherwise.
    fn details_tab_ui(&self, ui: &mut egui::Ui, details: &mut SongDetails, section: Section) {
        let shown: Vec<&fields::Def> = fields::editable().filter(|d| d.field.section() == section).collect();
        let SongDetails { keys, texts, changes, .. } = details;
        let songs: Vec<_> = keys.iter().filter_map(|k| self.songs.get_by_key(k)).collect();
        if keys.len() > 1 {
            ui.weak("Only the fields you change are written, to every one of them.");
        }
        // (Rows rather than a grid: a grid squeezes its text boxes to
        // nothing in a modal. Each box fills what's left of its row,
        // beside a label and the revert button's room.)
        let font = egui::TextStyle::Body.resolve(ui.style());
        let label_width = shown.iter().map(|d| ui.painter().layout_no_wrap(d.label.to_string(), font.clone(), egui::Color32::WHITE).size().x).fold(0.0, f32::max);
        let row_height = ui.spacing().interact_size.y;
        for def in shown {
            let field = def.field;
            let (text, mixed) = texts.get_mut(&field).expect("every editable field has its text");
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(label_width, row_height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_width(label_width);
                    ui.label(def.label);
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let edited = changes.get(&field) != Some(&Change::Revert) && songs.iter().any(|s| s.edits.contains_key(&field));
                    let revert = ui.add_visible(edited, super::icon_button("arrow-counter-clockwise"));
                    if edited {
                        let unedited: Vec<String> = songs.iter().map(|s| field_text(&Facts::of(s).unedited(), field)).collect();
                        let hover = match unedited.as_slice() {
                            [one] if def.kind == Kind::Flag => format!("Back to what the file says: {}", if one.is_empty() { "no" } else { "yes" }),
                            [one] if one.is_empty() => "Back to what the file says (nothing)".to_string(),
                            [one] => format!("Back to what the file says: {one}"),
                            _ => "Back to what each file says".to_string(),
                        };
                        if revert.on_hover_text(hover).clicked() {
                            changes.insert(field, Change::Revert);
                            let same = unedited.iter().all(|u| *u == unedited[0]);
                            *text = if same { unedited[0].clone() } else { String::new() };
                            *mixed = !same;
                        }
                    }
                    let hint = if *mixed && !changes.contains_key(&field) { "Mixed" } else { "" };
                    if field == Field::VolumeAdjust {
                        // (A slider; at 0 while the songs differ, till it's moved.)
                        let mut percent = fields::Facts { edits: Some(&[(field, text.clone())].into()), ..Facts::default() }.number(field).unwrap_or(0.0);
                        let slider = egui::Slider::new(&mut percent, -100.0..=100.0).suffix("%").step_by(1.0).custom_formatter(|n, _| format!("{n:+.0}"));
                        let response = ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| ui.add(slider)).inner;
                        if response.changed() {
                            *text = if percent == 0.0 { String::new() } else { format!("{percent:+.0}") };
                            changes.insert(field, Change::Set(text.clone()));
                        }
                        if !hint.is_empty() {
                            response.on_hover_text("The songs differ: moving it sets them all");
                        }
                        return;
                    }
                    if def.kind == Kind::Flag {
                        // (Its box is a dash while the songs differ.)
                        let mut on = text == "1";
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if ui.add(egui::Checkbox::without_text(&mut on).indeterminate(!hint.is_empty())).changed() {
                                *text = if on { "1" } else { "" }.to_string();
                                changes.insert(field, Change::Set(text.clone()));
                            }
                        });
                        return;
                    }
                    let response = ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(f32::INFINITY));
                    if response.changed() {
                        // (Typed into and emptied again, "Mixed" stays as it is.)
                        if *mixed && text.is_empty() {
                            changes.remove(&field);
                        } else {
                            changes.insert(field, Change::Set(text.clone()));
                        }
                    }
                });
            });
        }
    }

    /// One song's start and stop times as a range over its length, each
    /// end dragged, with buttons to hear each; the times' boxes below say
    /// them exactly. What to play from, when a button's clicked.
    fn options_range_ui(&self, ui: &mut egui::Ui, details: &mut SongDetails) -> Option<f64> {
        let [key] = details.keys.as_slice() else { return None };
        let length = self.songs.get_by_key(key).and_then(|s| Facts::of(s).number(Field::Length)).filter(|&l| l > 1.0)?;
        let at = |text: &str| fields::seconds(text).filter(|&s| s > 0.0);
        let mut start = at(&details.texts[&Field::StartTime].0).unwrap_or(0.0).min(length);
        let mut stop = at(&details.texts[&Field::StopTime].0).unwrap_or(length).min(length);
        let mut play = None;
        ui.horizontal(|ui| {
            if ui.add(super::icon_button("play")).on_hover_text("Hear it from where it starts").clicked() {
                play = Some(start);
            }
            if ui.add(super::icon_button("skip-forward")).on_hover_text("Hear the last seconds before it stops").clicked() {
                play = Some((stop - 5.0).max(start));
            }
            let label = format!("{} to {}", format_length(start), format_length(stop));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.monospace(label);
                if time_range(ui, &mut start, &mut stop, length) {
                    // (Its very start or end is no time at all.)
                    let text = |t: f64, none: bool| if none { String::new() } else { format_length(t) };
                    for (field, t, none) in [(Field::StartTime, start, start < 0.5), (Field::StopTime, stop, stop > length - 0.5)] {
                        let text = text(t, none);
                        if details.texts[&field].0 != text {
                            details.texts.insert(field, (text.clone(), false));
                            details.changes.insert(field, Change::Set(text));
                        }
                    }
                }
            });
        });
        play
    }

    /// The songs one song plays together with (`SongStore::together`), in
    /// order, each to move or take out: changed at once, not on saving.
    fn together_ui(&mut self, ui: &mut egui::Ui, details: &SongDetails) {
        let [key] = details.keys.as_slice() else { return };
        let Some(id) = self.songs.together_of(&self.songs.path(key)).map(|(id, _)| id.to_string()) else {
            ui.weak("To play songs together, in order and as one when shuffling, select them in Songs and pick Play together from their menu.");
            return;
        };
        let mut keys = self.songs.together()[&id].clone();
        ui.label("Plays together with, in order (as one when shuffling):");
        let mut remove = None;
        let mut raise = None;
        let last = keys.len() - 1;
        for (i, k) in keys.iter().enumerate() {
            let path = self.songs.path(k);
            let title = egui::RichText::new(self.song_title(&path));
            ui.horizontal(|ui| {
                ui.weak(format!("{}.", i + 1));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(super::icon("x")).on_hover_text("Take it out").clicked() {
                        remove = Some(i);
                    }
                    if ui.add_enabled(i < last, egui::Button::new(super::icon("arrow-down"))).on_hover_text("Play it later").clicked() {
                        raise = Some(i + 1);
                    }
                    if ui.add_enabled(i > 0, egui::Button::new(super::icon("arrow-up"))).on_hover_text("Play it sooner").clicked() {
                        raise = Some(i);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(if k == key { title.strong() } else { title }).truncate());
                    });
                });
            });
        }
        if raise.is_none() && remove.is_none() {
            return;
        }
        if let Some(i) = raise {
            keys.swap(i - 1, i);
        }
        if let Some(i) = remove {
            keys.remove(i);
        }
        self.songs.set_together(&id, keys);
        self.songs_changed();
    }

    /// How often the songs have been played and skipped, every device's,
    /// with a button to count them from nothing again (at once, not on
    /// saving).
    fn plays_ui(&mut self, ui: &mut egui::Ui, details: &SongDetails) {
        let totals: Vec<_> = details.keys.iter().map(|k| self.songs.totals(k)).collect();
        let plays: u32 = totals.iter().map(|t| t.plays).sum();
        let skips: u32 = totals.iter().map(|t| t.skips).sum();
        let last = |time: Option<i64>| time.map_or_else(String::new, |t| format!(", last on {}", fields::date(t)));
        let times = |n: u32| if n == 1 { "once".to_string() } else { format!("{n} times") };
        ui.horizontal(|ui| {
            ui.label(format!(
                "Played {}{}. Skipped {}{}.",
                times(plays),
                last(totals.iter().filter_map(|t| t.last_played).max()),
                times(skips),
                last(totals.iter().filter_map(|t| t.last_skipped).max()),
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let reset = ui
                    .add_enabled(plays + skips > 0, egui::Button::new("Reset"))
                    .on_hover_text("Count plays and skips from nothing again, on every device");
                if reset.clicked() && self.songs.reset_stats(&details.keys) {
                    self.stats_changed();
                    self.status = "Play counts reset.".to_string();
                }
            });
        });
    }

    /// Play the song at `path` from `at` seconds, to hear its options.
    fn preview_at(&mut self, path: &std::path::Path, at: f64) {
        if self.current_song.as_deref() == Some(path) {
            self.send(crate::audio::AudioCommand::Seek(at));
        } else {
            self.seek_on_load = Some((path.to_path_buf(), at));
            self.request_play(path, None, None, false);
        }
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
                        // Its MusicBrainz IDs (in the rows above too), as links.
                        for (kind, id) in [("release", &info.meta.musicbrainz_release), ("recording", &info.meta.musicbrainz_recording)] {
                            if let Some(id) = id {
                                ui.strong(format!("MusicBrainz {kind}"));
                                ui.hyperlink_to("Open on MusicBrainz", format!("https://musicbrainz.org/{kind}/{id}"));
                                ui.end_row();
                            }
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
        // (The playing song's volume adjust may have changed.)
        self.send_volume();
        self.status = format!("Edited {songs} song{}.", if songs == 1 { "" } else { "s" });
    }

    /// Undo the last save of songs' details.
    pub(super) fn undo_edit(&mut self) {
        let Some(before) = self.last_edit.take() else { return };
        self.songs.edit(before);
        self.songs_changed();
        self.send_volume();
        self.status = "Undid the last edit.".to_string();
    }
}

/// Two handles over `0..length` seconds, `start` and `stop`, each dragged
/// by a press right on it, at least a second apart: whether either moved.
fn time_range(ui: &mut egui::Ui, start: &mut f64, stop: &mut f64, length: f64) -> bool {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let track = rect.shrink2(egui::vec2(8.0, 0.0));
    let x_of = |t: f64| track.left() + (t / length) as f32 * track.width();
    let t_of = |x: f32| f64::from(((x - track.left()) / track.width()).clamp(0.0, 1.0)) * length;
    const GRAB: f32 = 8.0;
    // The handle under `x`, if one is (`true`: the stop's), the nearer of
    // the two where they meet.
    let (start_x, stop_x) = (x_of(*start), x_of(*stop));
    let under = move |x: f32| {
        let (to_start, to_stop) = ((x - start_x).abs(), (x - stop_x).abs());
        (to_start.min(to_stop) <= GRAB).then_some(to_stop < to_start)
    };
    let id = response.id;
    let mut changed = false;
    if let Some(pointer) = response.interact_pointer_pos() {
        // (The handle pressed is kept for the drag, so they can't swap
        // halfway; a press beside them grabs nothing.)
        if ui.input(|i| i.pointer.any_pressed()) {
            ui.data_mut(|d| d.insert_temp(id, under(pointer.x)));
        }
        let t = t_of(pointer.x);
        match ui.data(|d| d.get_temp::<Option<bool>>(id)).flatten() {
            Some(true) => *stop = t.max(*start + 1.0).min(length),
            Some(false) => *start = t.min(*stop - 1.0).max(0.0),
            None => {}
        }
        changed = ui.data(|d| d.get_temp::<Option<bool>>(id)).flatten().is_some();
    }
    let over = response.hover_pos().and_then(|p| under(p.x)).is_some();
    if over || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    let visuals = ui.visuals();
    let y = rect.center().y;
    let painter = ui.painter();
    painter.line_segment([egui::pos2(track.left(), y), egui::pos2(track.right(), y)], egui::Stroke::new(4.0, visuals.widgets.inactive.bg_fill));
    painter.line_segment([egui::pos2(x_of(*start), y), egui::pos2(x_of(*stop), y)], egui::Stroke::new(4.0, visuals.selection.bg_fill));
    let handle = if over || response.dragged() { visuals.widgets.hovered.fg_stroke.color } else { visuals.widgets.inactive.fg_stroke.color };
    for t in [*start, *stop] {
        painter.circle_filled(egui::pos2(x_of(t), y), 6.0, handle);
    }
    response.on_hover_text("Drag either end: where it starts and stops");
    changed
}

/// A field's text as the editor shows it: numbers as they're shown, a
/// flag "1" when it's set.
fn field_text(song: &Facts, field: Field) -> String {
    match field.kind() {
        Kind::Flag => if song.flag(field) { "1" } else { "" }.to_string(),
        _ => song.value(field).map(|v| v.text).unwrap_or_default(),
    }
}
