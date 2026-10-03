//! The guided tour (offered by the welcome window, and Help > Tour): a
//! card at a time pointing at the real thing on screen, everything else
//! dimmed, moving on by itself once you've done what it asks (or with
//! Next). Not a modal, on purpose: the whole point is clicking the app's
//! own buttons, so nothing is blocked, only dimmed.
//!
//! The parts of the app a step can point at mark where they are each frame
//! (`Tour::mark`); the overlay is drawn last, over that frame's marks.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui;

use super::App;
use crate::audio::PlaybackShared;
use crate::layout::{self, Section};

/// Something on screen a step points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Target {
    /// A whole tab's contents.
    Tab(Section),
    /// The playback controls at the bottom.
    Controls,
    /// The visualizer's script picker.
    ScriptPicker,
    /// The visualizer's channel toggles.
    Channels,
    /// The Fullscreen visualizer button.
    Fullscreen,
}

/// What finishes a step (it moves on by itself shortly after).
#[derive(Clone, Copy)]
enum DoneWhen {
    /// Only the Next button.
    Next,
    SongLoaded,
    SoundfontChanged,
    ScriptChanged,
    ChannelMuted,
    PlaylistGrew,
    SoundfontOnSong,
    Fullscreen,
    /// The running script's file is called this (without .lua).
    Script(&'static str),
}

struct Step {
    title: &'static str,
    text: &'static str,
    target: Option<Target>,
    /// Tabs to open (and bring to the front) when the step starts.
    tabs: &'static [Section],
    done: DoneWhen,
    /// Animate a drag from this to the target.
    drag_from: Option<Target>,
}

const STEPS: &[Step] = &[
    Step {
        title: "Play a song",
        text: "These are the songs in a folder (the starter songs, if you added them). Click one to play it.",
        target: Some(Target::Tab(Section::Songs)),
        tabs: &[Section::Songs],
        done: DoneWhen::SongLoaded,
        drag_from: None,
    },
    Step {
        title: "The controls",
        text: "Pause and play, click or drag along the bar to jump around, and change the speed and the volume. \
               They're always here at the bottom.",
        target: Some(Target::Controls),
        tabs: &[],
        done: DoneWhen::Next,
        drag_from: None,
    },
    Step {
        title: "Soundfonts",
        text: "A soundfont is the set of instruments a MIDI file is played with. Click another one in the list \
               to hear the same song played differently.",
        target: Some(Target::Tab(Section::Soundfonts)),
        tabs: &[Section::Soundfonts],
        done: DoneWhen::SoundfontChanged,
        drag_from: None,
    },
    Step {
        title: "Visualizers",
        text: "The Visualizer tab draws what's playing. Pick another visualizer from this list (keyboard and \
               fft are good ones to start with).",
        target: Some(Target::ScriptPicker),
        tabs: &[Section::Visualizer],
        done: DoneWhen::ScriptChanged,
        drag_from: None,
    },
    Step {
        title: "Channels",
        text: "Each instrument is on its own channel. Click one to mute it, right-click one to hear it alone, \
               and All brings everything back.",
        target: Some(Target::Channels),
        tabs: &[Section::Visualizer],
        done: DoneWhen::ChannelMuted,
        drag_from: None,
    },
    Step {
        title: "Playlists",
        text: "Drag songs from the Songs tab onto this one to make a playlist (a whole folder works too; New \
               makes an empty one). Double-click a song in it to play it; Loop and Shuffle are in the controls.",
        target: Some(Target::Tab(Section::Playlists)),
        tabs: &[Section::Songs, Section::Playlists],
        done: DoneWhen::PlaylistGrew,
        drag_from: Some(Target::Tab(Section::Songs)),
    },
    Step {
        title: "A soundfont for one song",
        text: "Drag a soundfont onto a song in a playlist, and that song always plays with it, whatever's \
               picked in the Soundfonts tab.",
        target: Some(Target::Tab(Section::Playlists)),
        tabs: &[Section::Soundfonts, Section::Playlists],
        done: DoneWhen::SoundfontOnSong,
        drag_from: Some(Target::Tab(Section::Soundfonts)),
    },
    Step {
        title: "Fullscreen",
        text: "This fills the screen with just the visualizer (F11 does too). Esc comes back here.",
        target: Some(Target::Fullscreen),
        tabs: &[Section::Visualizer],
        done: DoneWhen::Fullscreen,
        drag_from: None,
    },
    Step {
        title: "Games",
        text: "Some visualizers are games. Pick highway from the list, click into the visualizer, and play \
               along with the song on the keys it shows.",
        target: Some(Target::ScriptPicker),
        tabs: &[Section::Visualizer],
        done: DoneWhen::Script("highway"),
        drag_from: None,
    },
    Step {
        title: "That's the tour",
        text: "The View menu brings back any tab you close, and View > Layout has ready-made arrangements. \
               Help > Tour runs this again, and Help > Welcome... has the starter pack. Have fun!",
        target: None,
        tabs: &[],
        done: DoneWhen::Next,
        drag_from: None,
    },
];

/// How long "Done!" shows before the next step.
const ADVANCE_AFTER: Duration = Duration::from_millis(900);

#[derive(Default)]
pub(super) struct Tour {
    /// The step showing, while the tour runs.
    step: Option<usize>,
    /// Where each target is this frame.
    marks: HashMap<Target, egui::Rect>,
    /// How things were when the step started.
    baseline: Baseline,
    /// When the step's action was done.
    done_at: Option<Instant>,
    /// Tabs still to open for the step (opened once the dock isn't busy).
    open_tabs: bool,
}

#[derive(Default)]
struct Baseline {
    loads: u64,
    soundfont: Option<usize>,
    script: Option<usize>,
    entries: usize,
    soundfonts_on_songs: usize,
}

impl Tour {
    /// Start of a frame: forget last frame's marks.
    pub(super) fn begin_frame(&mut self) {
        self.marks.clear();
    }

    /// `target` is at `rect` this frame.
    pub(super) fn mark(&mut self, target: Target, rect: egui::Rect) {
        if self.step.is_some() {
            self.marks.insert(target, rect);
        }
    }
}

impl App {
    pub(super) fn start_tour(&mut self) {
        self.go_to_step(0);
    }

    fn go_to_step(&mut self, step: usize) {
        self.tour.step = Some(step);
        self.tour.done_at = None;
        self.tour.open_tabs = true;
        let shared = self.shared.lock().map(|s| s.view.loads).unwrap_or_default();
        self.tour.baseline = Baseline {
            loads: shared,
            soundfont: self.active_sf,
            script: self.active_script,
            entries: self.playlist_entry_count(),
            soundfonts_on_songs: self.soundfonts_on_songs(),
        };
    }

    fn playlist_entry_count(&self) -> usize {
        self.playlists.lists.iter().map(|l| l.playlist.entries.len()).sum()
    }

    fn soundfonts_on_songs(&self) -> usize {
        self.playlists.lists.iter().flat_map(|l| &l.playlist.entries).filter(|e| e.soundfont.is_some()).count()
    }

    fn step_done(&self, done: DoneWhen, shared: &PlaybackShared) -> bool {
        let base = &self.tour.baseline;
        match done {
            DoneWhen::Next => false,
            DoneWhen::SongLoaded => shared.view.loads > base.loads,
            DoneWhen::SoundfontChanged => self.active_sf.is_some() && self.active_sf != base.soundfont,
            DoneWhen::ScriptChanged => self.active_script != base.script,
            DoneWhen::ChannelMuted => {
                shared.notes.detected_channels.iter().any(|&c| !shared.notes.enabled_channels[c as usize])
            }
            DoneWhen::PlaylistGrew => self.playlist_entry_count() > base.entries,
            DoneWhen::SoundfontOnSong => self.soundfonts_on_songs() > base.soundfonts_on_songs,
            DoneWhen::Fullscreen => self.dedicated.is_some(),
            DoneWhen::Script(name) => {
                self.visualizer.script().path().and_then(|p| p.file_stem()).is_some_and(|stem| stem == name)
            }
        }
    }

    /// Bring the step's tabs out: opened if closed, to the front of their
    /// group.
    fn open_step_tabs(&mut self, tabs: &[Section]) {
        for &section in tabs {
            // Playlists go in the visualizer's spot (as a tab beside it)
            // rather than squeezing in another column.
            let beside_visualizer = section == Section::Playlists
                && self.is_open(Section::Visualizer)
                && layout::tab_alongside(&mut self.dock, Section::Playlists, Section::Visualizer, true);
            if !beside_visualizer && !self.is_open(section) {
                self.set_open(section, true);
            }
            layout::focus(&mut self.dock, section);
        }
    }

    /// The tour's overlay and card, drawn over everything else.
    pub(super) fn tour_ui(&mut self, ctx: &egui::Context, shared: &PlaybackShared) {
        let Some(index) = self.tour.step else { return };
        let Some(step) = STEPS.get(index) else {
            self.tour.step = None;
            return;
        };
        if std::mem::take(&mut self.tour.open_tabs) {
            self.open_step_tabs(step.tabs);
        }
        // Done: say so, then move on.
        if self.tour.done_at.is_none() && self.step_done(step.done, shared) {
            self.tour.done_at = Some(Instant::now());
        }
        if self.tour.done_at.is_some_and(|at| at.elapsed() >= ADVANCE_AFTER) && self.dedicated.is_none() {
            self.go_to_step(index + 1);
            ctx.request_repaint();
            return;
        }
        // A dialog is up: it has the floor.
        if self.any_modal_open() {
            return;
        }
        // In the dedicated fullscreen: just a reminder of the way back.
        if self.dedicated.is_some() {
            egui::Area::new(egui::Id::new("tour_fullscreen_hint"))
                .order(egui::Order::Tooltip)
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label("Tour: press Esc to come back.");
                    });
                });
            return;
        }

        let screen = ctx.content_rect();
        let target = step.target.and_then(|t| self.tour.marks.get(&t).copied());
        let source = step.drag_from.and_then(|t| self.tour.marks.get(&t).copied());
        let time = ctx.input(|i| i.time);
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("tour_dim")));
        let dim = egui::Color32::from_black_alpha(150);
        // Everything dimmed but the target (and a drag's source).
        let holes: Vec<egui::Rect> =
            [source, target].into_iter().flatten().map(|r| r.expand(6.0).intersect(screen)).collect();
        for part in dimmed_parts(screen, &holes) {
            painter.rect_filled(part, 0.0, dim);
        }
        let pulse = 0.6 + 0.4 * (time * 3.0).sin().abs() as f32;
        let color = ctx.global_style().visuals.selection.stroke.color.gamma_multiply(pulse);
        for hole in &holes {
            painter.rect_stroke(*hole, 6.0, egui::Stroke::new(3.0, color), egui::StrokeKind::Outside);
        }
        // A drag to make: a ghost sliding from where to where.
        if let (Some(from), Some(to)) = (source, target) {
            let (from, to) = (from.center(), to.center());
            let t = ((time % 3.0) / 2.4).min(1.0) as f32;
            let at = from + (to - from) * t;
            let color = egui::Color32::from_rgb(240, 200, 90);
            painter.arrow(from, (to - from) * 0.95, egui::Stroke::new(2.0, color.gamma_multiply(0.5)));
            painter.rect_filled(egui::Rect::from_center_size(at, egui::vec2(46.0, 18.0)), 4.0, color);
            painter.circle_filled(at + egui::vec2(16.0, 12.0), 5.0, egui::Color32::WHITE);
        }

        // The card, beside the target where there's room.
        const CARD_WIDTH: f32 = 330.0;
        let (pos, pivot) = match target {
            Some(rect) if screen.max.x - rect.max.x > CARD_WIDTH + 30.0 => {
                (egui::pos2(rect.max.x + 18.0, rect.min.y.max(screen.min.y + 30.0)), egui::Align2::LEFT_TOP)
            }
            Some(rect) if rect.min.x - screen.min.x > CARD_WIDTH + 30.0 => {
                (egui::pos2(rect.min.x - 18.0, rect.min.y.max(screen.min.y + 30.0)), egui::Align2::RIGHT_TOP)
            }
            Some(rect) if rect.min.y - screen.min.y > 200.0 => {
                (egui::pos2(rect.center().x, rect.min.y - 18.0), egui::Align2::CENTER_BOTTOM)
            }
            Some(rect) if screen.max.y - rect.max.y > 200.0 => {
                (egui::pos2(rect.center().x, rect.max.y + 18.0), egui::Align2::CENTER_TOP)
            }
            _ => (screen.center(), egui::Align2::CENTER_CENTER),
        };
        let mut next = false;
        let mut back = false;
        let mut stop = false;
        let done = self.tour.done_at.is_some();
        let waiting_for_soundfont = index == 0 && self.active_sf.is_none();
        let download = self.welcome.downloader.state();
        egui::Area::new(egui::Id::new("tour_card"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos)
            .pivot(pivot)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(12.0).show(ui, |ui| {
                    ui.set_width(CARD_WIDTH);
                    ui.horizontal(|ui| {
                        ui.strong(step.title);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.weak(format!("{} of {}", index + 1, STEPS.len()));
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(step.text);
                    if waiting_for_soundfont && download.running {
                        let percent = download.done * 100 / crate::starter::download_size().max(1);
                        ui.weak(format!("(The starter soundfonts are still downloading, {percent}%: songs play once one is ready.)"));
                    }
                    if step.target.is_some() && target.is_none() {
                        ui.weak("(It's not on screen right now: the View menu shows its tab.)");
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if done {
                            ui.colored_label(egui::Color32::from_rgb(120, 200, 120), "Done!");
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let last = index + 1 == STEPS.len();
                            next = ui.button(if last { "Finish" } else { "Next" }).clicked();
                            if index > 0 {
                                back = ui.button("Back").clicked();
                            }
                            if !last {
                                stop = ui.button("End tour").clicked();
                            }
                        });
                    });
                });
            });
        if next {
            if index + 1 == STEPS.len() {
                self.tour.step = None;
            } else {
                self.go_to_step(index + 1);
            }
        } else if back {
            self.go_to_step(index - 1);
        } else if stop {
            self.tour.step = None;
            self.status = "Tour ended. Help > Tour runs it again.".to_string();
        }
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

/// `screen` minus `holes`, as rectangles: the grid every hole's edges
/// make, less the cells inside a hole.
fn dimmed_parts(screen: egui::Rect, holes: &[egui::Rect]) -> Vec<egui::Rect> {
    let mut xs = vec![screen.min.x, screen.max.x];
    let mut ys = vec![screen.min.y, screen.max.y];
    for hole in holes {
        xs.extend([hole.min.x, hole.max.x]);
        ys.extend([hole.min.y, hole.max.y]);
    }
    for edges in [&mut xs, &mut ys] {
        edges.sort_by(f32::total_cmp);
        edges.dedup();
    }
    let mut parts = Vec::new();
    for row in ys.windows(2) {
        for column in xs.windows(2) {
            let cell = egui::Rect::from_min_max(egui::pos2(column[0], row[0]), egui::pos2(column[1], row[1]));
            if !holes.iter().any(|hole| hole.contains(cell.center())) {
                parts.push(cell);
            }
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steps_make_sense() {
        assert!(STEPS.len() >= 8);
        for step in STEPS {
            assert!(!step.title.is_empty() && !step.text.contains('\u{2014}'), "{}", step.title);
            // A step asking for a script asks for one that's bundled.
            if let DoneWhen::Script(name) = step.done {
                assert!(crate::lua_visualizer::bundled_default(&format!("{name}.lua")).is_some(), "{name}");
            }
            // A drag needs both ends on screen.
            if let Some(Target::Tab(from)) = step.drag_from {
                assert!(step.tabs.contains(&from), "{}", step.title);
            }
        }
        assert!(STEPS.last().unwrap().target.is_none(), "the last step is the wrap-up");
    }

    #[test]
    fn dimming_leaves_the_holes_and_covers_the_rest() {
        let screen = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0));
        let holes = [
            egui::Rect::from_min_max(egui::pos2(10.0, 10.0), egui::pos2(30.0, 40.0)),
            egui::Rect::from_min_max(egui::pos2(60.0, 20.0), egui::pos2(90.0, 90.0)),
        ];
        let parts = dimmed_parts(screen, &holes);
        let area: f32 = parts.iter().map(|r| r.area()).sum();
        assert!((area - (10_000.0 - 600.0 - 2_100.0)).abs() < 0.01, "{area}");
        for part in &parts {
            for hole in &holes {
                assert!(part.intersect(*hole).area() <= 0.0, "{part:?} covers {hole:?}");
            }
        }
        assert_eq!(dimmed_parts(screen, &[]), [screen]);
    }
}
