//! Preferences > Browsing > Song names: the title patterns (`song_title`),
//! tried in order, each with how many songs it names and a menu of fields
//! to put in, and a preview: a made-up song with every tag, and a few of
//! your own songs, a different few every few seconds.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;

use eframe::egui;

use super::{row_icon_button, setting_label, setting_row, settings_group, title_of, App};
use crate::fields::{Facts, FIELDS};
use crate::song_info::SongInfo;
use crate::song_store::Meta;
use crate::song_title::{render, DEFAULT_PATTERNS};

/// How many of your songs the preview shows, and for how long.
const PREVIEW_SONGS: usize = 3;
const PREVIEW_SECS: f64 = 4.0;

impl App {
    pub(super) fn song_names_ui(&mut self, ui: &mut egui::Ui) {
        let hint = "Each song is named by the first pattern it has every field of, or else by its file's name \
                    (as it was before it was imported).";
        settings_group("Song names").icon("text-aa").hint(hint).show(ui, |ui| self.patterns_ui(ui));
    }

    fn patterns_ui(&mut self, ui: &mut egui::Ui) {
        let mut patterns: Vec<String> =
            self.config.title_patterns.clone().unwrap_or_else(|| DEFAULT_PATTERNS.map(String::from).to_vec());
        let counts = self.pattern_counts(ui.ctx(), &patterns);
        let mut changed = false;
        let mut remove = None;
        let mut raise = None;
        let last = patterns.len().saturating_sub(1);
        for (i, pattern) in patterns.iter_mut().enumerate() {
            setting_row(ui, |ui| {
                ui.weak(format!("{}.", i + 1));
                // The buttons at the right end, the pattern filling the rest
                // (narrow on a phone).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(row_icon_button(ui, "x")).on_hover_text("Remove it").clicked() {
                        remove = Some(i);
                    }
                    if ui.add_enabled(i < last, row_icon_button(ui, "arrow-down")).on_hover_text("Try it later").clicked() {
                        raise = Some(i + 1);
                    }
                    if ui.add_enabled(i > 0, row_icon_button(ui, "arrow-up")).on_hover_text("Try it sooner").clicked() {
                        raise = Some(i);
                    }
                    count_label(ui, counts[i]);
                    let edit = ui.make_persistent_id(("title_pattern", i));
                    if let Some(key) = field_menu(ui) {
                        insert_field(ui.ctx(), edit, pattern, key);
                        changed = true;
                    }
                    changed |= ui.add(egui::TextEdit::singleline(pattern).id(edit).desired_width(f32::INFINITY)).changed();
                });
            });
        }
        let mut reset = false;
        setting_row(ui, |ui| {
            ui.weak("Otherwise, the file's name");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                reset = ui
                    .add_enabled(self.config.title_patterns.is_some(), row_icon_button(ui, "arrow-counter-clockwise"))
                    .on_hover_text(format!("Back to {}", DEFAULT_PATTERNS.map(|p| format!("\"{p}\"")).join(", then ")))
                    .clicked();
                if ui.add(row_icon_button(ui, "plus")).on_hover_text("Add a pattern").clicked() {
                    patterns.push("{title}".to_string());
                    changed = true;
                }
                // (Where a pattern's Try it sooner is, so the counts line up.)
                let height = ui.spacing().interact_size.y;
                ui.allocate_space(egui::vec2(height + 6.0, height));
                count_label(ui, counts[counts.len() - 1]);
            });
        });
        if let Some(i) = raise {
            patterns.swap(i - 1, i);
            changed = true;
        }
        if let Some(i) = remove {
            patterns.remove(i);
            changed = true;
        }
        if changed || reset {
            let defaults = patterns.iter().map(String::as_str).eq(DEFAULT_PATTERNS);
            self.config.title_patterns = (!reset && !defaults).then_some(patterns.clone());
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }

        let info = example_info();
        let example = Facts { file_name: "harbour_lights.mid", meta: Some(&info.meta), info: Some(&info), imported: Some(1_760_000_000), tags: &[], edits: None, added_by: None, online: None };
        let named = patterns.iter().find_map(|p| render(p, &example)).unwrap_or_else(|| "harbour_lights".to_string());
        let songs = self.preview_songs(ui);
        let naming = super::naming(&self.config, &self.songs);
        setting_row(ui, |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                setting_label(ui, "Preview");
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(named).on_hover_text("A song with every field");
                    });
                    for song in songs {
                        let name = title_of(&mut self.song_info, &naming, &song);
                        ui.add(egui::Label::new(egui::RichText::new(name).weak()).truncate()).on_hover_text(naming.song(&song, None).file_name);
                    }
                });
            });
        });
    }

    /// How many songs each of `patterns` names (the first one they have
    /// every field of), and last, how many are left to their file names.
    /// Counted again when the patterns or the library change.
    fn pattern_counts(&self, ctx: &egui::Context, patterns: &[String]) -> Vec<usize> {
        let mut hasher = DefaultHasher::new();
        (patterns, self.songs.revision()).hash(&mut hasher);
        let key = hasher.finish();
        let id = egui::Id::new("title_pattern_counts");
        if let Some((at, counts)) = ctx.data(|d| d.get_temp::<(u64, Vec<usize>)>(id))
            && at == key
        {
            return counts;
        }
        let naming = super::naming(&self.config, &self.songs);
        let mut counts = vec![0; patterns.len() + 1];
        for (path, _) in self.songs.songs() {
            let song = naming.song(&path, None);
            counts[patterns.iter().position(|p| render(p, &song).is_some()).unwrap_or(patterns.len())] += 1;
        }
        ctx.data_mut(|d| d.insert_temp(id, (key, counts.clone())));
        counts
    }

    /// A few songs from the library, a different few every `PREVIEW_SECS`.
    fn preview_songs(&self, ui: &egui::Ui) -> Vec<PathBuf> {
        let files: Vec<PathBuf> = self.songs.songs().map(|(path, _)| path).collect();
        if files.len() <= PREVIEW_SONGS {
            return files;
        }
        let now = ui.input(|i| i.time);
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(PREVIEW_SECS - now % PREVIEW_SECS));
        let round = (now / PREVIEW_SECS) as u64;
        let mut picked: Vec<usize> = Vec::new();
        let mut k = 0u64;
        while picked.len() < PREVIEW_SONGS {
            let mut hasher = DefaultHasher::new();
            (round, k).hash(&mut hasher);
            let i = (hasher.finish() % files.len() as u64) as usize;
            if !picked.contains(&i) {
                picked.push(i);
            }
            k += 1;
        }
        picked.into_iter().map(|i| files[i].clone()).collect()
    }
}

fn example_info() -> SongInfo {
    let meta = Meta {
        title: Some("Harbour Lights".into()),
        artist: Some("The Night Owls".into()),
        album: Some("Late Trains".into()),
        album_artist: Some("Various Artists".into()),
        composer: Some("Ada Moreau".into()),
        genre: Some("Jazz".into()),
        track: Some(3),
        disc: Some(1),
        year: Some(2010),
        length: Some(203.0),
        size: 48_213,
        compilation: true,
        ..Meta::default()
    };
    SongInfo { meta, bpm: Some(92.0), notes: Some(4210), ..SongInfo::default() }
}

/// "12 songs", weak, a pattern's share of the library.
fn count_label(ui: &mut egui::Ui, count: usize) {
    let text = format!("{count} song{}", if count == 1 { "" } else { "s" });
    ui.add_sized([64.0, ui.spacing().interact_size.y], egui::Label::new(egui::RichText::new(text).weak()))
        .on_hover_text("The songs it names now");
}

/// The { } button by a pattern: the fields a pattern can use, the one
/// picked to put in it.
fn field_menu(ui: &mut egui::Ui) -> Option<&'static str> {
    let mut picked = None;
    let button = row_icon_button(ui, "brackets-curly");
    let (response, _) = egui::containers::menu::MenuButton::from_button(button).config(crate::menu::config(ui.ctx())).ui(ui, |ui| {
        let fields = || FIELDS.iter().filter(|d| d.field.in_patterns());
        let sections: [(&str, Vec<&crate::fields::Def>); 3] = [
            ("Song info", fields().filter(|d| d.editable && !d.key.starts_with("sort_")).collect()),
            ("Sort names", fields().filter(|d| d.key.starts_with("sort_")).collect()),
            ("The file", fields().filter(|d| !d.editable).collect()),
        ];
        for (title, defs) in sections {
            ui.menu_button(title, |ui| {
                for def in defs {
                    if ui.button(def.label).on_hover_text(format!("{{{}}}", def.key)).clicked() {
                        picked = Some(def.key);
                    }
                }
            });
        }
    });
    response.on_hover_text("Put in a field");
    picked
}

/// Put `{key}` into `pattern` where the cursor of its box (`edit`) last
/// was (the end, if it hasn't had one), and the cursor after it.
fn insert_field(ctx: &egui::Context, edit: egui::Id, pattern: &mut String, key: &str) {
    let field = format!("{{{key}}}");
    let mut state = egui::text_edit::TextEditState::load(ctx, edit).unwrap_or_default();
    let chars = pattern.chars().count();
    let at = state.cursor.char_range().map_or(chars, |range| range.primary.index.0.min(chars));
    let byte = pattern.char_indices().nth(at).map_or(pattern.len(), |(b, _)| b);
    pattern.insert_str(byte, &field);
    let after = egui::text::CCursor::new(at + field.chars().count());
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(after)));
    state.store(ctx, edit);
    ctx.memory_mut(|m| m.request_focus(edit));
}
