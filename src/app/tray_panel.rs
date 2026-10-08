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

    /// Open it by the tray icon, which is at `icon` (x, y, width, height,
    /// in physical screen pixels): above it, right-aligned with it, or
    /// below it when the taskbar's at the top. `pixels_per_point`: the
    /// screen's scale.
    pub(super) fn open(&self, ctx: &egui::Context, icon: [f32; 4], pixels_per_point: f32) {
        let [x, y, w, h] = icon.map(|v| v / pixels_per_point);
        let left = (x + w - SIZE.x).max(0.0);
        let top = if y - SIZE.y - 8.0 >= 0.0 { y - SIZE.y - 8.0 } else { y + h + 8.0 };
        if let Ok(mut state) = self.state.lock() {
            state.open = true;
            state.focused_once = false;
        }
        let id = viewport_id();
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::OuterPosition(egui::pos2(left, top)));
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
            ui.strong("synththing");
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
