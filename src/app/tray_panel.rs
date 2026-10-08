//! The tray panel: what right-clicking the tray icon opens. A small window
//! by the tray with what's playing and its controls, and little versions
//! of the Soundfonts and Playlists tabs, so synththing can be run from
//! there without opening the window. It goes away like a menu when it
//! loses focus (or on Escape), but only hides: its tab and scroll position
//! are where they were when it's opened again (for the session; nothing's
//! saved).
//!
//! Pinned, it's the mini player: it stays (always on top) until closed, and
//! drags around by its header. Left-clicking the tray icon shows and hides
//! it pinned, where it was last left. Expanded, it grows to the left with
//! the visualizer, small (scripts see `display_mode() == "mini"`).
//!
//! It's a window of its own that draws itself (an egui deferred viewport),
//! so it keeps working while the main window is hidden, when the app's
//! `ui` isn't called. It draws from a snapshot the app refreshes
//! (`App::logic`) and hands back what's clicked as [`Action`]s for the app
//! to carry out; the visualizer it draws itself, through the panel it
//! shares with the main window.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui;

use super::{icon, palette_color};
use crate::audio::PlaybackShared;
use crate::lua_visualizer::Transport;
use crate::script_host::Effects;
use crate::visualizer::{DisplayMode, SampleTap, VisualizerPanel};

/// The panel's window.
pub(super) fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("tray_panel")
}

/// The panel's size, in points.
const SIZE: egui::Vec2 = egui::vec2(300.0, 420.0);

/// The visualizer's size in the expanded mini player, in points: small,
/// and 16:9 like a video.
const PICTURE: egui::Vec2 = egui::vec2(256.0, 144.0);

/// Between the visualizer and the controls.
const GAP: f32 = 10.0;

/// The window's size, expanded or not.
fn size(expanded: bool) -> egui::Vec2 {
    if expanded { egui::vec2(SIZE.x + PICTURE.x + GAP, SIZE.y) } else { SIZE }
}

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
    /// The running script's name.
    pub script: Option<String>,
    /// What the script sees of the playlist and modes.
    pub transport: Transport,
    /// The visualizer is paused (Preferences is open).
    pub frozen: bool,
}

/// What was clicked, for the app to do.
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
    /// What the script asked for while the mini player drew it.
    Effects(Effects),
}

/// What the expanded mini player draws the visualizer with: the panel it
/// shares with the main window, and the audio and notes to hand it.
pub(super) struct Picture {
    pub panel: Arc<Mutex<VisualizerPanel>>,
    pub tap: SampleTap,
    pub playback: Arc<Mutex<PlaybackShared>>,
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
    /// The mini player: stays until closed, drags by its header.
    pinned: bool,
    /// Where the mini player was last (its top-left, in points), to open
    /// there again.
    pinned_at: Option<egui::Pos2>,
    /// With the visualizer, to the left.
    expanded: bool,
    tab: Tab,
    /// The playlist shown in the Playlists tab.
    playlist: Option<usize>,
    snapshot: Snapshot,
}

pub(super) struct TrayPanel {
    state: Arc<Mutex<State>>,
    picture: Arc<Picture>,
    send: Sender<Action>,
    actions: Receiver<Action>,
}

impl TrayPanel {
    pub(super) fn new(picture: Picture) -> Self {
        let (send, actions) = channel();
        Self { state: Arc::default(), picture: Arc::new(picture), send, actions }
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
        let (state, picture, send) = (Arc::clone(&self.state), Arc::clone(&self.picture), self.send.clone());
        ctx.show_viewport_deferred(viewport_id(), builder, move |ui, _| panel_ui(ui, &state, &picture, &send));
    }

    pub(super) fn is_open(&self) -> bool {
        self.state.lock().is_ok_and(|s| s.open)
    }

    /// Open it with its bottom-right corner at `cursor` (physical screen
    /// pixels: where the tray icon was right-clicked). `pixels_per_point`:
    /// a first guess at the scale there (the main window's), for getting it
    /// onto the right monitor; it places itself exactly once it's there.
    pub(super) fn open(&self, ctx: &egui::Context, cursor: [f32; 2], pixels_per_point: f32) {
        self.show(ctx, cursor, pixels_per_point, false);
    }

    /// Left-clicking the tray icon: the mini player (the panel pinned),
    /// where it was last, or by `cursor` the first time; or, if it's up,
    /// away.
    pub(super) fn toggle_mini(&self, ctx: &egui::Context, cursor: [f32; 2], pixels_per_point: f32) {
        let up = self.state.lock().is_ok_and(|s| s.open && s.pinned);
        if up {
            if let Ok(mut state) = self.state.lock() {
                state.open = false;
            }
            ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::Visible(false));
        } else {
            self.show(ctx, cursor, pixels_per_point, true);
        }
    }

    fn show(&self, ctx: &egui::Context, cursor: [f32; 2], pixels_per_point: f32, pinned: bool) {
        let Ok(mut state) = self.state.lock() else { return };
        state.open = true;
        state.focused_once = false;
        state.pinned = pinned;
        if !pinned {
            state.expanded = false;
        }
        let size = size(state.expanded);
        let at = match state.pinned_at.filter(|_| pinned) {
            Some(last) => {
                state.anchor = None;
                last
            }
            None => {
                state.anchor = Some((cursor, PLACE_FRAMES));
                placement(cursor, pixels_per_point, size)
            }
        };
        drop(state);
        let id = viewport_id();
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::OuterPosition(at));
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::InnerSize(size));
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

/// Where its top-left goes (in points, at `pixels_per_point`) for a window
/// `size` big to have its bottom-right at `cursor` (physical pixels); below
/// the cursor instead when there's no room above (a taskbar at the top).
fn placement(cursor: [f32; 2], pixels_per_point: f32, size: egui::Vec2) -> egui::Pos2 {
    let [x, y] = cursor.map(|v| v / pixels_per_point);
    let top = if y - size.y >= 0.0 { y - size.y } else { y };
    egui::pos2(x - size.x, top)
}

/// Close the panel (it only hides).
fn close(ctx: &egui::Context, state: &mut State) {
    state.open = false;
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
}

/// Expand (grow to the left with the visualizer) or contract, keeping the
/// controls where they are on screen.
fn set_expanded(ctx: &egui::Context, state: &mut State, expanded: bool) {
    let shift = if expanded { -(PICTURE.x + GAP) } else { PICTURE.x + GAP };
    state.expanded = expanded;
    if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
        let at = rect.min + egui::vec2(shift, 0.0);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
        state.pinned_at = Some(at);
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size(expanded)));
}

fn panel_ui(ui: &mut egui::Ui, state: &Mutex<State>, picture: &Picture, send: &Sender<Action>) {
    let Ok(mut state) = state.lock() else { return };
    if !state.open {
        return;
    }
    let ctx = ui.ctx().clone();
    if let Some((cursor, frames)) = state.anchor {
        // Its own scale, now it's on its monitor (see `State::anchor`).
        let size = size(state.expanded);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(placement(cursor, ctx.pixels_per_point(), size)));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        state.anchor = (frames > 1).then_some((cursor, frames - 1));
        ctx.request_repaint();
    }
    if state.pinned {
        // The mini player stays; it's remembered where it's left.
        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            state.pinned_at = Some(rect.min);
        }
    } else {
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
    }
    let act = |action: Action| {
        let _ = send.send(action);
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    };

    let frame = egui::Frame::window(ui.style()).inner_margin(egui::Margin::same(10));
    frame.show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        if state.expanded {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(PICTURE.x);
                    picture_ui(ui, picture, &state.snapshot, &act);
                });
                ui.add_space(GAP - ui.spacing().item_spacing.x);
                ui.vertical(|ui| controls_ui(ui, &ctx, &mut state, &act));
            });
        } else {
            controls_ui(ui, &ctx, &mut state, &act);
        }
    });
}

/// The visualizer, small, and what's showing in it.
fn picture_ui(ui: &mut egui::Ui, picture: &Picture, snapshot: &Snapshot, act: &impl Fn(Action)) {
    let (notes, view) = match picture.playback.lock() {
        Ok(shared) => (shared.notes.clone(), shared.view.clone()),
        Err(_) => return,
    };
    // (Never held already: the main window and this one draw in turn.)
    if let Ok(mut panel) = picture.panel.try_lock() {
        // While the main window's showing the visualizer too, it's the one
        // running the script; this just shows the latest picture.
        let frozen = snapshot.frozen || panel.shown_by_main_recently();
        ui.allocate_ui(PICTURE, |ui| {
            ui.set_min_size(PICTURE);
            ui.set_max_size(PICTURE);
            panel.show(ui, &picture.tap, &notes, &view, snapshot.transport.clone(), frozen, DisplayMode::Mini, None, false, None);
        });
        let effects = panel.take_effects();
        if !effects.is_empty() {
            act(Action::Effects(effects));
        }
    }
    ui.add_space(4.0);
    ui.add(egui::Label::new(egui::RichText::new(snapshot.script.as_deref().unwrap_or("No script")).strong()).truncate());
    ui.ctx().request_repaint();
}

/// The panel itself: header, what's playing, transport and volume, the
/// little Playlists and Soundfonts tabs, and the way out.
fn controls_ui(ui: &mut egui::Ui, ctx: &egui::Context, state: &mut State, act: &impl Fn(Action)) {
    let snapshot = state.snapshot.clone();

    // The header: the wordmark, expand, pin and close; pinned, it's what
    // the mini player is dragged by.
    let mut buttons_left = f32::INFINITY;
    let header = ui.horizontal(|ui| {
        let height = ui.text_style_height(&egui::TextStyle::Button) * 1.25;
        match state.wordmark.image(ctx, height, ui.visuals().window_fill) {
            Some(image) => ui.add(image),
            None => ui.strong("synththing"),
        };
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let close_hover = if state.pinned { "Close the mini player" } else { "Close (Esc)" };
            if ui.small_button(icon("x")).on_hover_text(close_hover).clicked() {
                close(ctx, state);
            }
            let (pin, pin_hover) = if state.pinned {
                ("push-pin-slash", "Unpin: back to a panel that closes when you click elsewhere")
            } else {
                ("push-pin", "Pin as the mini player: it stays, on top, and drags by this bar")
            };
            let pin = ui.small_button(icon(pin)).on_hover_text(pin_hover);
            if pin.clicked() {
                state.pinned = !state.pinned;
                state.focused_once = true;
            }
            let (expand, expand_hover) = if state.expanded {
                ("caret-double-right", "Hide the visualizer")
            } else {
                ("caret-double-left", "Show the visualizer, to the left")
            };
            let expand = ui.small_button(icon(expand)).on_hover_text(expand_hover);
            buttons_left = expand.rect.left();
            if expand.clicked() {
                let expanded = !state.expanded;
                // (The visualizer comes with pinning: a panel that closes
                // on a click elsewhere isn't for watching.)
                state.pinned |= expanded;
                set_expanded(ctx, state, expanded);
            }
        });
    });
    if state.pinned {
        // (Up to the buttons, so they still take their own clicks.)
        let mut grip = header.response.rect;
        grip.max.x = grip.max.x.min(buttons_left - 4.0);
        let drag = ui.interact(grip, egui::Id::new("tray_panel_drag"), egui::Sense::drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
    }
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
                close(ctx, state);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(format!("{} Quit", icon("power"))).on_hover_text("Close synththing completely").clicked() {
                    act(Action::Quit);
                    close(ctx, state);
                }
            });
        });
        ui.separator();
    });
}
