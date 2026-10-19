//! Preferences > Appearance > Song names: the title patterns (`song_title`),
//! tried in order, with a preview: a made-up song with every tag, and a few
//! of your own songs, a different few every few seconds.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use eframe::egui;

use super::{title_of, App};
use crate::song_info::{SongInfo, Tags};
use crate::song_title::{render, Song, DEFAULT_PATTERNS, FIELDS};

/// How many of your songs the preview shows, and for how long.
const PREVIEW_SONGS: usize = 3;
const PREVIEW_SECS: f64 = 4.0;

impl App {
    pub(super) fn song_names_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("Song names");
        ui.weak(format!(
            "Each song is named by the first of these it has every field of, or else by its file's name. \
             {{file_path}} is the folder it's in, inside the Songs list's folder. Fields: {}.",
            FIELDS.map(|f| format!("{{{f}}}")).join(", ")
        ));
        let mut patterns: Vec<String> =
            self.config.title_patterns.clone().unwrap_or_else(|| DEFAULT_PATTERNS.map(String::from).to_vec());
        let mut changed = false;
        let mut remove = None;
        let mut raise = None;
        let last = patterns.len().saturating_sub(1);
        for (i, pattern) in patterns.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.weak(format!("{}.", i + 1));
                // The buttons at the right end, the pattern filling the rest
                // (narrow on a phone).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(super::icon("x")).on_hover_text("Remove it").clicked() {
                        remove = Some(i);
                    }
                    if ui.add_enabled(i < last, egui::Button::new(super::icon("arrow-down"))).on_hover_text("Try it later").clicked() {
                        raise = Some(i + 1);
                    }
                    if ui.add_enabled(i > 0, egui::Button::new(super::icon("arrow-up"))).on_hover_text("Try it sooner").clicked() {
                        raise = Some(i);
                    }
                    changed |= ui.add(egui::TextEdit::singleline(pattern).desired_width(f32::INFINITY)).changed();
                });
            });
        }
        if let Some(i) = raise {
            patterns.swap(i - 1, i);
            changed = true;
        }
        if let Some(i) = remove {
            patterns.remove(i);
            changed = true;
        }
        let mut reset = false;
        ui.horizontal(|ui| {
            if ui.button("Add").clicked() {
                patterns.push("{title}".to_string());
                changed = true;
            }
            reset = ui
                .add_enabled(self.config.title_patterns.is_some(), egui::Button::new("Reset"))
                .on_hover_text(format!("Back to {}", DEFAULT_PATTERNS.map(|p| format!("\"{p}\"")).join(", then ")))
                .clicked();
        });
        if changed || reset {
            let defaults = patterns.iter().map(String::as_str).eq(DEFAULT_PATTERNS);
            self.config.title_patterns = (!reset && !defaults).then_some(patterns.clone());
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }

        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            let info = example_info();
            let example = Song {
                path: Path::new("/music/games/ds3/harbour_lights.mid"),
                info: Some(&info),
                folder: Path::new("/music"),
            };
            let named = patterns.iter().find_map(|p| render(p, &example)).unwrap_or_else(|| "harbour_lights".to_string());
            ui.horizontal_wrapped(|ui| {
                ui.weak("A song with every field:");
                ui.label(named);
            });
            let songs = self.preview_songs(ui);
            if !songs.is_empty() {
                ui.weak("Yours:");
            }
            let naming = super::naming(&self.config, &self.browser);
            for song in songs {
                let name = title_of(&mut self.song_info, &naming, &song);
                ui.label(name).on_hover_text(song.display().to_string());
            }
        });
    }

    /// A few songs from the Songs list's folder, a different few every
    /// `PREVIEW_SECS`.
    fn preview_songs(&self, ui: &egui::Ui) -> Vec<PathBuf> {
        let files: Vec<PathBuf> = self.browser.files().map(|p| p.to_path_buf()).collect();
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
    let tags = Tags {
        title: Some("Harbour Lights".into()),
        artist: Some("The Night Owls".into()),
        album: Some("Late Trains".into()),
        album_artist: Some("Various Artists".into()),
        composer: Some("Ada Moreau".into()),
        genre: Some("Jazz".into()),
        track: Some("3".into()),
        year: Some("2010".into()),
    };
    SongInfo { length: Some(203.0), bpm: Some(92.0), notes: Some(4210), tags, ..SongInfo::default() }
}
