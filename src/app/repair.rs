//! Repair library (Preferences > App): every song on this device checked
//! (`song_repair`). Damaged files are deleted, to be fetched again from
//! the other devices; songs that can't be read at all are listed, to
//! remove and block. Suggested once, the first time a song in the library
//! can't be read.

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;

use super::App;
use crate::song_repair::{Check, Problem};

#[derive(Default)]
pub struct RepairState {
    open: bool,
    /// Opened because a song couldn't be read.
    suggested: bool,
    check: Option<Check>,
    /// What the last check found.
    outcome: Option<Outcome>,
}

struct Outcome {
    checked: usize,
    /// Damaged, deleted: fetched again.
    refetched: usize,
    /// Whole, and can't be read: each one's file and why.
    unreadable: Vec<(PathBuf, String)>,
    /// What couldn't be deleted.
    failed: Vec<String>,
}

impl App {
    pub(super) fn open_repair(&mut self) {
        self.repair.open = true;
        self.repair.suggested = false;
    }

    /// The first song in the library seen to be unreadable brings up
    /// Repair library, once ever.
    pub(super) fn suggest_repair(&mut self) {
        let Some(path) = self.song_info.take_unreadable() else {
            return;
        };
        if self.config.repair_offered || self.songs.key(&path).is_none() || self.repair.open {
            return;
        }
        self.config.repair_offered = true;
        if let Err(e) = self.config.save() {
            log::warn!("couldn't save preferences: {e}");
        }
        self.repair = RepairState {
            open: true,
            suggested: true,
            ..RepairState::default()
        };
    }

    /// A check that's finished: damaged songs deleted, the rest listed.
    pub(super) fn poll_repair(&mut self) {
        let Some(found) = self.repair.check.as_ref().and_then(Check::finished) else {
            return;
        };
        let checked = self.repair.check.take().map_or(0, |c| c.progress().1);
        let mut outcome = Outcome {
            checked,
            refetched: 0,
            unreadable: Vec::new(),
            failed: Vec::new(),
        };
        for (key, problem) in found {
            match problem {
                Problem::Damaged => match self.songs.discard(&key) {
                    Ok(()) => outcome.refetched += 1,
                    Err(e) => outcome.failed.push(format!("{e:#}")),
                },
                Problem::Unreadable(why) => outcome.unreadable.push((self.songs.path(&key), why)),
            }
        }
        if outcome.refetched > 0 {
            self.refresh_missing();
        }
        self.repair.outcome = Some(outcome);
    }

    pub(super) fn repair_ui(&mut self, ctx: &egui::Context) {
        if !self.repair.open {
            return;
        }
        let mut start = false;
        let mut remove = false;
        let mut close = false;
        let unreadable: Vec<PathBuf> = self
            .repair
            .outcome
            .iter()
            .flat_map(|o| o.unreadable.iter().map(|(path, _)| path.clone()))
            .collect();
        let titles: Vec<String> = unreadable
            .iter()
            .map(|path| self.song_title(path))
            .collect();
        let state = &self.repair;
        let modal = egui::Modal::new(egui::Id::new("repair_library")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading("Repair library");
            if let Some(check) = &state.check {
                let (checked, total) = check.progress();
                ui.label(format!("Checking songs: {checked} of {total}..."));
                ui.add(egui::ProgressBar::new(if total == 0 { 1.0 } else { checked as f32 / total as f32 }));
                ui.weak("Closing this leaves it checking.");
                ctx.request_repaint_after(Duration::from_millis(100));
            } else if let Some(outcome) = &state.outcome {
                ui.label(format!("Checked {} song{}.", outcome.checked, plural(outcome.checked)));
                if outcome.refetched > 0 {
                    ui.label(format!(
                        "{} damaged song{} deleted. Your other devices send {} again, if they have {}.",
                        outcome.refetched,
                        if outcome.refetched == 1 { " was" } else { "s were" },
                        if outcome.refetched == 1 { "it" } else { "them" },
                        if outcome.refetched == 1 { "it" } else { "them" },
                    ));
                }
                for e in &outcome.failed {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't delete a damaged song: {e}"));
                }
                if !outcome.unreadable.is_empty() {
                    ui.label(format!(
                        "{} song{} can't be played. {} broken when {} came in, so fetching {} again won't help.",
                        outcome.unreadable.len(),
                        plural(outcome.unreadable.len()),
                        if outcome.unreadable.len() == 1 { "It was" } else { "They were" },
                        if outcome.unreadable.len() == 1 { "it" } else { "they" },
                        if outcome.unreadable.len() == 1 { "it" } else { "them" },
                    ));
                    egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                        for (title, (_, why)) in titles.iter().zip(&outcome.unreadable) {
                            ui.label(title).on_hover_text(why);
                        }
                    });
                } else if outcome.refetched == 0 && outcome.failed.is_empty() {
                    ui.label("Every song is fine.");
                }
            } else {
                if state.suggested {
                    ui.label("A song in your library couldn't be read.");
                }
                ui.label(
                    "Repair library checks every song on this device. A damaged song is deleted and fetched \
                     again from your other devices. A song that can't be played at all is listed, for you to remove.",
                );
            }
            ui.horizontal(|ui| {
                if state.check.is_none() {
                    let label = if state.outcome.is_some() { "Check again" } else { "Check every song" };
                    start = ui.button(label).clicked();
                }
                if state.outcome.as_ref().is_some_and(|o| !o.unreadable.is_empty()) {
                    remove = ui
                        .button("Remove and block")
                        .on_hover_text(
                            "Delete them here, and don't take them from your other devices again \
                             (Fetch again, in Sync, undoes it)",
                        )
                        .clicked();
                }
                close = ui.button(if state.check.is_none() && state.outcome.is_none() { "Not now" } else { "Close" }).clicked();
            });
        });
        if start {
            let songs = self
                .songs
                .songs()
                .filter_map(|(path, _)| Some((self.songs.key(&path)?, path)))
                .collect();
            let ctx = ctx.clone();
            self.repair.outcome = None;
            self.repair.check = Some(Check::start(songs, move || ctx.request_repaint()));
        }
        if remove && let Some(outcome) = &mut self.repair.outcome {
            let unreadable = std::mem::take(&mut outcome.unreadable);
            for (path, _) in &unreadable {
                if let Err(e) = self.songs.remove(path) {
                    outcome.failed.push(format!("{e:#}"));
                }
            }
            self.refresh_missing();
            self.song_list.clear_selection();
            self.status = format!(
                "Removed {} unplayable song{} from the library.",
                unreadable.len(),
                plural(unreadable.len())
            );
        }
        if close || modal.should_close() {
            self.repair.open = false;
            if self.repair.check.is_none() {
                self.repair.outcome = None;
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
