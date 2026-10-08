//! The tray panel: what right-clicking the tray icon opens. A small window
//! by the tray with what's playing and its controls, and little versions
//! of the Soundfonts and Playlists tabs, so synththing can be run from
//! there without opening the window. It goes away like a menu when it
//! loses focus (or on Escape), but only hides: its tab and scroll position
//! are where they were when it's opened again (for the session; nothing's
//! saved).
//!
//! It's a window of its own that draws itself (an egui deferred viewport),
//! so it keeps working while the main window is hidden, when the app's
//! `ui` isn't called. It draws from a snapshot the app refreshes
//! (`App::logic`) and hands back what's clicked as [`Action`]s for the app
//! to carry out.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui;

use super::{icon, palette_color};

/// The panel's window.
pub(super) fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("tray_panel")
}

/// Its size, in points.
const SIZE: egui::Vec2 = egui::vec2(300.0, 420.0);

/// What the panel shows, from the app.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Snapshot {
    /// What's loaded, if anything.
    pub title: Option<String>,
    pub paused: bool,
    pub position: f64,
    pub length: f64,
    pub volume: f32,
    pub soundfonts: Vec<String>,
    pub active_soundfont: Option<usize>,
    /// Each playlist's name and its entries' names.
    pub playlists: Vec<(String, Vec<String>)>,
    /// The playlist and entry playing.
    pub playing: Option<(usize, usize)>,
}

/// What was clicked, for the app to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Action {
    TogglePause,
    Previous,
    Next,
    Volume(f32),
    Soundfont(usize),
    /// Play a playlist's entry.
    Play(usize, usize),
    ShowWindow,
    Quit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Playlists,
    Soundfonts,
}

/// The panel's own state, kept while it's hidden.
#[derive(Default)]
struct State {
    open: bool,
    /// Where it's anchored (its bottom-right corner, in physical screen
    /// pixels: the right-click), and how many more frames to place it
    /// there for: its window only knows its scale (the monitor's, times the
    /// UI scale) once it's on that monitor, so it's placed again as it
    /// settles.
    anchor: Option<([f32; 2], u8)>,
    wordmark: super::branding::Wordmark,
    /// It's had focus since it was opened, so losing it closes it.
    focused_once: bool,
    tab: Tab,
    /// The playlist shown in the Playlists tab.
    playlist: Option<usize>,
    snapshot: Snapshot,
}

pub(super) struct TrayPanel {
    state: Arc<Mutex<State>>,
    send: Sender<Action>,
    actions: Receiver<Action>,
}

impl TrayPanel {
    pub(super) fn new() -> Self {
        let (send, actions) = channel();
        Self { state: Arc::default(), send, actions }
    }

    /// Every frame the main window draws: keep the panel's window there
    /// (hidden until it's opened). It carries on while the main window's
    /// hidden, when this isn't called.
    pub(super) fn register(&self, ctx: &egui::Context) {
        let builder = egui::ViewportBuilder::default()
            .with_title("synththing")
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_inner_size(SIZE)
            .with_visible(false);
        let (state, send) = (Arc::clone(&self.state), self.send.clone());
        ctx.show_viewport_deferred(viewport_id(), builder, move |ui, _| panel_ui(ui, &state, &send));
    }

    pub(super) fn is_open(&self) -> bool {
        self.state.lock().is_ok_and(|s| s.open)
    }

    /// Open it with its bottom-right corner at `cursor` (physical screen
    /// pixels: where the tray icon was right-clicked). `pixels_per_point`:
    /// a first guess at the scale there (the main window's), for getting it
    /// onto the right monitor; it places itself exactly once it's there.
    pub(super) fn open(&self, ctx: &egui::Context, cursor: [f32; 2], pixels_per_point: f32) {
        if let Ok(mut state) = self.state.lock() {
            state.open = true;
            state.focused_once = false;
            state.anchor = Some((cursor, PLACE_FRAMES));
        }
        let id = viewport_id();
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::OuterPosition(placement(cursor, pixels_per_point)));
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::InnerSize(SIZE));
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
        ctx.request_repaint_of(id);
    }

    /// Show `snapshot` (redrawing the panel if it changed).
    pub(super) fn update(&self, ctx: &egui::Context, snapshot: Snapshot) {
        if let Ok(mut state) = self.state.lock()
            && state.snapshot != snapshot
        {
            state.snapshot = snapshot;
            ctx.request_repaint_of(viewport_id());
        }
    }

    /// What was clicked since the last call.
    pub(super) fn actions(&self) -> Vec<Action> {
        self.actions.try_iter().collect()
    }
}

/// Frames it places itself again for after opening (see `State::anchor`).
const PLACE_FRAMES: u8 = 3;

/// Where its top-left goes (in points, at `pixels_per_point`) for its
/// bottom-right to be at `cursor` (physical pixels); below the cursor
/// instead when there's no room above (a taskbar at the top).
fn placement(cursor: [f32; 2], pixels_per_point: f32) -> egui::Pos2 {
    let [x, y] = cursor.map(|v| v / pixels_per_point);
    let top = if y - SIZE.y >= 0.0 { y - SIZE.y } else { y };
    egui::pos2(x - SIZE.x, top)
}

/// Close the panel (it only hides).
fn close(ctx: &egui::Context, state: &mut State) {
    state.open = false;
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
}

fn panel_ui(ui: &mut egui::Ui, state: &Mutex<State>, send: &Sender<Action>) {
    let Ok(mut state) = state.lock() else { return };
    if !state.open {
        return;
    }
    let ctx = ui.ctx().clone();
    if let Some((cursor, frames)) = state.anchor {
        // Its own scale, now it's on its monitor (see `State::anchor`).
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(placement(cursor, ctx.pixels_per_point())));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(SIZE));
        state.anchor = (frames > 1).then_some((cursor, frames - 1));
        ctx.request_repaint();
    }
    // Like a menu: gone once something else is clicked (it's had focus
    // first, so a slow first focus doesn't close it), or on Escape.
    match ctx.input(|i| i.viewport().focused) {
        Some(true) => state.focused_once = true,
        Some(false) if state.focused_once => return close(&ctx, &mut state),
        _ => {}
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return close(&ctx, &mut state);
    }
    let act = |action: Action| {
        let _ = send.send(action);
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    };

    let frame = egui::Frame::window(ui.style()).inner_margin(egui::Margin::same(10));
    frame.show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let snapshot = state.snapshot.clone();

        // What's playing, and the transport.
        ui.horizontal(|ui| {
            let height = ui.text_style_height(&egui::TextStyle::Button) * 1.25;
            match state.wordmark.image(&ctx, height, ui.visuals().window_fill) {
                Some(image) => ui.add(image),
                None => ui.strong("synththing"),
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(icon("x")).on_hover_text("Close (Esc)").clicked() {
                    close(&ctx, &mut state);
                }
            });
        });
        ui.add_space(2.0);
        match &snapshot.title {
            Some(title) => {
                ui.add(egui::Label::new(egui::RichText::new(title).strong()).truncate());
                let fraction = if snapshot.length > 0.0 { (snapshot.position / snapshot.length) as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).desired_height(4.0));
                ui.weak(format!("{} / {}", super::fmt_time(snapshot.position), super::fmt_time(snapshot.length)));
            }
            None => {
                ui.weak("Nothing playing");
            }
        }
        ui.horizontal(|ui| {
            let button = |glyph: &str| egui::Button::new(egui::RichText::new(icon(glyph)).size(16.0)).min_size(egui::vec2(30.0, 26.0));
            if ui.add(button("skip-back")).on_hover_text("Previous").clicked() {
                act(Action::Previous);
            }
            let (glyph, color, hover) =
                if snapshot.paused { ("play", palette_color(2), "Play") } else { ("pause", palette_color(1), "Pause") };
            let play = egui::Button::new(egui::RichText::new(icon(glyph)).size(16.0).color(color)).min_size(egui::vec2(30.0, 26.0));
            if ui.add(play).on_hover_text(hover).clicked() {
                act(Action::TogglePause);
            }
            if ui.add(button("skip-forward")).on_hover_text("Next").clicked() {
                act(Action::Next);
            }
            ui.separator();
            let mut volume = snapshot.volume;
            ui.label(icon(if volume <= 0.0 { "speaker-x" } else { "speaker-high" }));
            if ui.add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false)).changed() {
                act(Action::Volume(volume));
            }
        });
        ui.separator();

        // Little Playlists and Soundfonts tabs.
        ui.horizontal(|ui| {
            ui.selectable_value(&mut state.tab, Tab::Playlists, format!("{} Playlists", icon("playlist")));
            ui.selectable_value(&mut state.tab, Tab::Soundfonts, format!("{} Soundfonts", icon("music-notes")));
        });
        let footer = 34.0;
        let height = (ui.available_height() - footer).max(60.0);
        match state.tab {
            Tab::Playlists => {
                if snapshot.playlists.is_empty() {
                    ui.weak("No playlists yet: make one in the Playlists tab.");
                } else {
                    let shown = state
                        .playlist
                        .or(snapshot.playing.map(|(list, _)| list))
                        .unwrap_or(0)
                        .min(snapshot.playlists.len() - 1);
                    egui::ComboBox::from_id_salt("tray_playlist")
                        .width(ui.available_width())
                        .selected_text(&snapshot.playlists[shown].0)
                        .show_ui(ui, |ui| {
                            for (i, (name, _)) in snapshot.playlists.iter().enumerate() {
                                if ui.selectable_label(i == shown, name).clicked() {
                                    state.playlist = Some(i);
                                }
                            }
                        });
                    egui::ScrollArea::vertical().id_salt("tray_entries").max_height(height - 28.0).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for (i, entry) in snapshot.playlists[shown].1.iter().enumerate() {
                            let playing = snapshot.playing == Some((shown, i));
                            let text = if playing { format!("{} {entry}", icon("speaker-high")) } else { entry.clone() };
                            if ui.selectable_label(playing, text).on_hover_text("Play it").clicked() {
                                act(Action::Play(shown, i));
                            }
                        }
                        if snapshot.playlists[shown].1.is_empty() {
                            ui.weak("This playlist is empty.");
                        }
                    });
                }
            }
            Tab::Soundfonts => {
                egui::ScrollArea::vertical().id_salt("tray_soundfonts").max_height(height).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for (i, name) in snapshot.soundfonts.iter().enumerate() {
                        if ui.selectable_label(snapshot.active_soundfont == Some(i), name).clicked() {
                            act(Action::Soundfont(i));
                        }
                    }
                    if snapshot.soundfonts.is_empty() {
                        ui.weak("No soundfonts yet: add one in the Soundfonts tab.");
                    }
                });
            }
        }

        // Out of the panel.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.horizontal(|ui| {
                if ui.button(format!("{} Show synththing", icon("app-window"))).clicked() {
                    act(Action::ShowWindow);
                    close(&ctx, &mut state);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("{} Quit", icon("power"))).on_hover_text("Close synththing completely").clicked() {
                        act(Action::Quit);
                        close(&ctx, &mut state);
                    }
                });
            });
            ui.separator();
        });
    });
}
