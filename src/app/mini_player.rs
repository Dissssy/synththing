//! The mini player: what clicking the tray icon turns the window into. A
//! small strip, always on top and out of the taskbar, with what's playing,
//! the seek bar and a line of its own buttons (`mini_controls_ui`,
//! Preferences > Appearance > Mini player buttons). Its drawer opens
//! the Queue, Groupings, Soundfonts and the visualizer (scripts see
//! `display_mode() == "mini"`), the same panels the main window has.
//!
//! It is the main window, made small: the app draws it in place of its
//! layout, so every panel in it is the real one, and the two are never up
//! together. Going back (Show synththing, a right-click on the tray icon,
//! launching again) puts the window back where and as big as it was.
//!
//! It drags around by its song line and opens where it was left. Pinned,
//! it stays until closed; unpinned, it goes away like a menu when it loses focus (or
//! on Escape). Where it was, its size, its page and whether its drawer is
//! open are kept for the session; nothing's saved.

use eframe::egui;

use super::{App, MIN_WINDOW, icon_button};
use crate::audio::PlaybackShared;
use crate::config::BarItem;
use crate::visualizer::DisplayMode;

/// The strip's width, in points, and its height to begin with: the song
/// line, the seek bar and the controls line, inside its margins (measured
/// once it's drawn, `MiniPlayer::strip_height`).
const STRIP: egui::Vec2 = egui::vec2(400.0, 132.0);

/// The drawer's height to begin with, and the least it can be dragged to.
const DRAWER: f32 = 340.0;
const MIN_DRAWER: f32 = 220.0;

/// The narrowest the mini player can be dragged to, with its drawer open.
const MIN_WIDTH: f32 = 340.0;

/// The rail's width, down the drawer's left.
const RAIL: f32 = 36.0;

/// Inside the window's edge.
const MARGIN: f32 = 8.0;

/// Frames it places itself again for after opening: the window only
/// knows its scale (the monitor's, times the UI scale) once it's on that
/// monitor, so it's placed again as it settles. Out of the taskbar too,
/// once it's shown (showing a window puts it back in).
const SETTLE_FRAMES: u8 = 3;

/// Frames the pointer is read from the system's cursor for after the
/// window moves under it (`MiniPlayer::pointer_frames`): the move lands a
/// frame or two later.
const POINTER_FRAMES: u8 = 6;

/// The most frames the drawer button waits for its resize to land.
const RESIZE_FRAMES: u8 = 30;

/// The drawer's pages, down its rail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Queue,
    Lyrics,
    Groupings,
    Soundfonts,
    Visualizer,
}

impl Page {
    const ALL: [Self; 5] = [Self::Queue, Self::Lyrics, Self::Groupings, Self::Soundfonts, Self::Visualizer];

    fn icon(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Lyrics => "microphone-stage",
            Self::Groupings => "squares-four",
            Self::Soundfonts => "piano-keys",
            Self::Visualizer => "waveform",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Queue => "Queue",
            Self::Lyrics => "Lyrics",
            Self::Groupings => "Groupings",
            Self::Soundfonts => "Soundfonts",
            Self::Visualizer => "Visualizer",
        }
    }
}

/// Where the mini player goes as it settles.
#[derive(Clone, Copy, Debug)]
enum Place {
    /// By the tray icon (its bottom-right at the cursor, in physical
    /// screen pixels): its window only knows its scale (the monitor's,
    /// times the UI scale) once it's on that monitor, so it's worked out
    /// again each frame.
    Cursor([f32; 2]),
    /// Its top-left, in points.
    At(egui::Pos2),
}

/// The main window as it was before it became the mini player.
#[derive(Clone, Copy, Debug)]
struct Window {
    /// Its top-left, in points.
    at: egui::Pos2,
    inner: egui::Vec2,
    maximized: bool,
}

/// The mini player's state, kept while it's away.
pub(super) struct MiniPlayer {
    /// The window is the mini player.
    open: bool,
    /// What the window goes back to.
    window: Option<Window>,
    /// Where to put it, and how many more frames to put it there for
    /// (`SETTLE_FRAMES`).
    placing: Option<(Place, u8)>,
    /// Frames left to keep it out of the taskbar for (`SETTLE_FRAMES`).
    taskbar_frames: u8,
    /// It's had focus since it was opened, so losing it closes it (unpinned).
    focused_once: bool,
    /// It stays until closed.
    pinned: bool,
    /// Where it was last (its top-left, in points), to open there again.
    at: Option<egui::Pos2>,
    width: f32,
    drawer: bool,
    drawer_height: f32,
    page: Page,
    /// The strip's height as drawn (its line heights are the theme's).
    strip_height: f32,
    /// The Visualizer page shows the script picker instead.
    picking: bool,
    /// Frames left to tell egui where the pointer is, from the system's
    /// cursor, after the window moved under it (moved up to make room for
    /// the drawer):
    /// no mouse move tells it, so the button under it wouldn't take a
    /// click until the mouse moved.
    pointer_frames: u8,
    /// Which of Preferences, Song info and Help were open (`main_shows`),
    /// so opening one from here brings the main window back.
    main_shows: [bool; 3],
    /// How wide each of the buttons and sliders was drawn
    /// (`mini_controls_ui`), to tell what fits on a row next frame.
    drawn: std::collections::HashMap<BarItem, f32>,
    /// Frames left to wait for the drawer's resize to land
    /// (`RESIZE_FRAMES`); the drawer button waits too.
    resize_frames: u8,
}

impl Default for MiniPlayer {
    fn default() -> Self {
        Self {
            open: false,
            window: None,
            placing: None,
            taskbar_frames: 0,
            focused_once: false,
            pinned: true,
            at: None,
            width: STRIP.x,
            drawer: false,
            drawer_height: DRAWER,
            page: Page::default(),
            picking: false,
            pointer_frames: 0,
            resize_frames: 0,
            drawn: std::collections::HashMap::new(),
            main_shows: [false; 3],
            strip_height: STRIP.y,
        }
    }
}

impl MiniPlayer {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    /// The window's size for how it's laid out now.
    fn size(&self) -> egui::Vec2 {
        if self.drawer { egui::vec2(self.width, self.strip_height + self.drawer_height) } else { egui::vec2(self.width, self.strip_height) }
    }

    /// Let it be resized (by its edges) only with the drawer open.
    fn size_limits(&self, ctx: &egui::Context) {
        let (min, max) = if self.drawer {
            (egui::vec2(MIN_WIDTH, self.strip_height + MIN_DRAWER), egui::Vec2::INFINITY)
        } else {
            (self.size(), self.size())
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min));
        ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(max));
    }
}

/// Where its top-left goes (in points, at `pixels_per_point`) for a window
/// `size` big to have its bottom-right at `cursor` (physical pixels); below
/// the cursor instead when there's no room above (a taskbar at the top).
fn placement(cursor: [f32; 2], pixels_per_point: f32, size: egui::Vec2) -> egui::Pos2 {
    let [x, y] = cursor.map(|v| v / pixels_per_point);
    let top = if y - size.y >= 0.0 { y - size.y } else { y };
    egui::pos2(x - size.x, top)
}

impl App {
    /// The window becomes the mini player: where it was last, or by
    /// `cursor` (physical screen pixels, at `pixels_per_point`) the first
    /// time.
    pub(super) fn open_mini(&mut self, ctx: &egui::Context, cursor: [f32; 2], pixels_per_point: f32) {
        self.mini.main_shows = self.main_shows();
        if self.mini.open {
            // Put away (hidden as the mini player): just shown again.
            self.hidden = false;
            self.mini.focused_once = false;
            self.mini.taskbar_frames = SETTLE_FRAMES;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
            return;
        }
        self.exit_dedicated(ctx);
        let (outer, inner, maximized) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect, i.viewport().maximized.unwrap_or(false)));
        if let (Some(outer), Some(inner)) = (outer, inner) {
            self.mini.window = Some(Window { at: outer.min, inner: inner.size(), maximized });
        }
        self.hidden = false;
        let mini = &mut self.mini;
        mini.open = true;
        mini.focused_once = false;
        mini.taskbar_frames = SETTLE_FRAMES;
        let size = mini.size();
        let (place, at) = match mini.at {
            Some(at) => (Place::At(at), at),
            None => (Place::Cursor(cursor), placement(cursor, pixels_per_point, size)),
        };
        // (Placed again as it settles: a window coming out of being
        // maximized goes back to its old size first.)
        mini.placing = Some((place, SETTLE_FRAMES));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        // (Without a maximize button, a double-click on what it's dragged
        // by doesn't maximize it, as Windows does for a title bar.)
        ctx.send_viewport_cmd(egui::ViewportCommand::EnableButtons { close: true, minimized: false, maximize: false });
        mini.size_limits(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    /// Back from the mini player to the main window as it was, before
    /// it's shown (`show_window`). Closing the mini player only hides it,
    /// as the mini player, so nothing moves or resizes on screen.
    pub(super) fn leave_mini(&mut self, ctx: &egui::Context) {
        if !std::mem::take(&mut self.mini.open) {
            return;
        }
        self.mini.placing = None;
        // (A hidden window goes back in by itself once it's shown.)
        if !self.hidden {
            show_in_taskbar(self.api.window, true);
        }
        self.mini.picking = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
        ctx.send_viewport_cmd(egui::ViewportCommand::EnableButtons { close: true, minimized: true, maximize: true });
        ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(egui::Vec2::INFINITY));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(MIN_WINDOW));
        // (Checked again at the main window's scale: `keep_min_window`.)
        self.min_window_scale = 0.0;
        if let Some(window) = self.mini.window {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(window.inner));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(window.at));
            if window.maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
    }

    /// The window as the mini player: the strip, and its drawer below it.
    pub(super) fn mini_player_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        let ctx = ui.ctx().clone();
        if self.mini.taskbar_frames > 0 {
            self.mini.taskbar_frames -= 1;
            show_in_taskbar(self.api.window, false);
        }
        // Never maximized as the mini player: un-maximizing puts a window
        // back at its old size and place (the main window's), so it's put
        // where it was again after.
        if ctx.input(|i| i.viewport().maximized == Some(true)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            let at = self.mini.at.unwrap_or_default();
            self.mini.placing = Some((Place::At(at), SETTLE_FRAMES));
        }
        if let Some((place, frames)) = self.mini.placing {
            let size = self.mini.size();
            let at = match place {
                Place::Cursor(cursor) => placement(cursor, ctx.pixels_per_point(), size),
                Place::At(at) => at,
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            self.mini.placing = (frames > 1).then_some((place, frames - 1));
            ctx.request_repaint();
        } else if self.mini.resize_frames > 0 {
            // A drawer resize on its way: until it's landed, the window's
            // size is the old one.
            let landed = (ctx.content_rect().size() - self.mini.size()).length() < 1.0;
            self.mini.resize_frames = if landed { 0 } else { self.mini.resize_frames - 1 };
            ctx.request_repaint();
        } else if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            // Remembered where it's left, and how big (dragged by its edges).
            self.mini.at = Some(rect.min);
            if self.mini.drawer {
                let size = ctx.content_rect().size();
                self.mini.width = size.x.max(MIN_WIDTH);
                self.mini.drawer_height = (size.y - self.mini.strip_height).max(MIN_DRAWER);
            }
        }
        if !self.mini.pinned {
            // Like a menu: gone once something else is clicked (it's had
            // focus first, so a slow first focus doesn't close it), or on
            // Escape.
            match ctx.input(|i| i.viewport().focused) {
                Some(true) => self.mini.focused_once = true,
                Some(false) if self.mini.focused_once => return self.hide_window(&ctx),
                _ => {}
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                return self.hide_window(&ctx);
            }
        }

        let view = &shared.view;
        // (No outline: the panels' separator is the only line between them.)
        let frame = egui::Frame::window(ui.style()).corner_radius(0).stroke(egui::Stroke::NONE).inner_margin(egui::Margin::same(MARGIN as i8));
        egui::Panel::top("mini_strip")
            .exact_size(self.mini.strip_height)
            .frame(frame)
            .show_separator_line(self.mini.drawer)
            .show(ui, |ui| self.mini_strip_ui(ui, view));
        if self.mini.drawer {
            egui::CentralPanel::default().frame(frame).show(ui, |ui| self.mini_drawer_ui(ui, shared));
        }

        // What only the main window shows, opened from here (Song info
        // from a row's menu, say): it comes back for it. Left open there
        // before, it waits for the main window.
        let shows = self.main_shows();
        let opened = shows.iter().zip(self.mini.main_shows).any(|(&now, before)| now && !before);
        self.mini.main_shows = shows;
        if opened {
            self.show_window(&ctx);
        }
    }

    /// The strip: the song's art and name (what it's dragged by), the
    /// drawer, pin and close buttons; the seek bar; the controls line.
    fn mini_strip_ui(&mut self, ui: &mut egui::Ui, view: &crate::engine::EngineView) {
        let ctx = ui.ctx().clone();
        let top = ui.cursor().top();
        let art = 40.0;
        let (line, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), art), egui::Sense::hover());
        let square = egui::Rect::from_min_size(line.min, egui::Vec2::splat(art));
        if self.current_song.is_some() {
            self.now_playing_art_ui(ui, square);
        } else {
            let visuals = ui.visuals();
            ui.painter().rect_filled(square, 4.0, visuals.faint_bg_color);
            let font = egui::FontId::proportional(20.0);
            ui.painter().text(square.center(), egui::Align2::CENTER_CENTER, super::icon("music-note"), font, visuals.weak_text_color());
        }

        // The buttons at the right.
        let mut buttons = ui.new_child(egui::UiBuilder::new().max_rect(line).layout(egui::Layout::right_to_left(egui::Align::Center)));
        let close_hover = if self.mini.pinned { "Close the mini player" } else { "Close (Esc)" };
        if buttons.add(icon_button("x")).on_hover_text(close_hover).clicked() {
            self.hide_window(&ctx);
        }
        let (pin, pin_hover) = if self.mini.pinned {
            ("push-pin-slash", "Unpin: it closes when you click elsewhere")
        } else {
            ("push-pin", "Pin: it stays, on top, until it's closed")
        };
        if buttons.add(icon_button(pin)).on_hover_text(pin_hover).clicked() {
            self.mini.pinned = !self.mini.pinned;
            self.mini.focused_once = true;
        }
        let (drawer, drawer_hover) = if self.mini.drawer {
            ("caret-up", "Close the drawer")
        } else {
            ("caret-down", "Open the drawer below: Queue, Groupings, Soundfonts and the visualizer")
        };
        // (Not again until the last resize has landed: resizes piling up
        // can hang the graphics driver.)
        let settled = self.mini.resize_frames == 0;
        if buttons.add_enabled(settled, icon_button(drawer).selected(self.mini.drawer)).on_hover_text(drawer_hover).clicked() {
            self.set_mini_drawer(&ctx, !self.mini.drawer);
        }
        if buttons.add(icon_button("app-window")).on_hover_text("Show synththing: back to the whole window").clicked() {
            self.show_window(&ctx);
        }
        if buttons.add(icon_button("power")).on_hover_text("Quit: close synththing completely").clicked() {
            self.close_with(&ctx, super::close::CloseAction::Quit);
        }
        let buttons_left = buttons.min_rect().left();

        // The song, between the art and the buttons; what it's dragged by.
        let song = egui::Rect::from_min_max(egui::pos2(square.right() + 8.0, line.top()), egui::pos2(buttons_left - 6.0, line.bottom()));
        let title = self.current_title().or_else(|| view.track_name.clone()).unwrap_or_else(|| "Nothing playing".to_string());
        let detail = Some(self.song_detail(view)).filter(|d| !d.is_empty());
        let mut name = ui.new_child(egui::UiBuilder::new().max_rect(song).layout(egui::Layout::left_to_right(egui::Align::Center)));
        name.set_clip_rect(song.intersect(ui.clip_rect()));
        crate::select_list::two_lines(&mut name, std::iter::empty(), egui::RichText::new(title).strong(), detail, true);
        {
            let drag = ui.interact(line.with_max_x(buttons_left - 4.0), egui::Id::unique("mini_drag"), egui::Sense::drag());
            if drag.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
        }

        ui.add_space(2.0);
        self.seek_line_ui(ui, view);
        self.mini_controls_ui(ui, view);

        // As tall as it turned out (once, or when the theme changes).
        // (From where the last row ended: the panel's own rect is the
        // height it was given, so it would never shrink.)
        let height = ui.cursor().top() - ui.spacing().item_spacing.y - top + 2.0 * MARGIN;
        if (height - self.mini.strip_height).abs() > 0.5 {
            self.mini.strip_height = height;
            self.mini.size_limits(&ctx);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(self.mini.size()));
        }
    }

    /// The mini player's buttons and sliders (`Config::mini_controls`),
    /// left to right, on as many rows as they take: all of them always
    /// there.
    fn mini_controls_ui(&mut self, ui: &mut egui::Ui, view: &crate::engine::EngineView) {
        use super::controls_bar::{MIN_SLIDER, PLAY_WIDTH, Shapes, SPEAKER_WIDTH, SPEED_TEXT_WIDTH, VOLUME_TEXT_WIDTH};
        let items: Vec<BarItem> = self.config.mini_controls.0.iter().filter(|item| self.bar_item_shown(item)).cloned().collect();
        let gap = ui.spacing().item_spacing.x;
        let button = super::controls_bar::icon_button_width(ui.spacing());
        // How wide each is: as drawn last frame (buttons come out a little
        // wider than their least size), else as worked out.
        let width = |item: &BarItem| {
            self.mini.drawn.get(item).copied().unwrap_or(match item {
                BarItem::PlayPause => PLAY_WIDTH,
                BarItem::Volume => SPEAKER_WIDTH + MIN_SLIDER + VOLUME_TEXT_WIDTH + 2.0 * gap,
                BarItem::Speed => button + MIN_SLIDER + SPEED_TEXT_WIDTH + 2.0 * gap,
                _ => button,
            })
        };
        // Rows, each as full as it fits (an item wider than a whole row
        // gets one to itself).
        let room = ui.available_width();
        let mut rows: Vec<Vec<BarItem>> = Vec::new();
        let mut used = room;
        for item in items {
            let w = width(&item);
            if used + gap + w > room {
                rows.push(Vec::new());
                used = -gap;
            }
            used += gap + w;
            rows.last_mut().expect("a row was just added").push(item);
        }
        ui.spacing_mut().slider_width = MIN_SLIDER;
        for row in rows {
            ui.horizontal(|ui| {
                for item in row {
                    let before = ui.cursor().left();
                    self.controls_item_ui(ui, view, &item, Shapes::SLIDERS);
                    self.mini.drawn.insert(item, ui.cursor().left() - before - gap);
                }
            });
        }
    }

    /// Whether Preferences, Song info and Help are open: what only the
    /// main window shows.
    fn main_shows(&self) -> [bool; 3] {
        [self.preferences_open, self.details_for.is_some(), self.help.is_some()]
    }

    /// Open or close the drawer, always below the strip (so the button
    /// stays under the mouse); without room below, the window moves up to
    /// make it.
    fn set_mini_drawer(&mut self, ctx: &egui::Context, open: bool) {
        let (outer, monitor) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().monitor_size));
        if open
            && let (Some(outer), Some(monitor)) = (outer, monitor)
        {
            let over = outer.bottom() + self.mini.drawer_height - monitor.y;
            if over > 0.0 {
                let at = outer.min - egui::vec2(0.0, over.min(outer.top()));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
                self.mini.at = Some(at);
                self.mini.pointer_frames = POINTER_FRAMES;
            }
        }
        if !open {
            // (Back to the strip's own width.)
            self.mini.width = STRIP.x;
        }
        self.mini.drawer = open;
        self.mini.size_limits(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(self.mini.size()));
        self.mini.resize_frames = RESIZE_FRAMES;
    }

    /// The drawer: its pages down the rail at the left, the page open
    /// beside it.
    fn mini_drawer_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        let full = ui.available_rect_before_wrap();
        let rail = full.with_max_x(full.left() + RAIL);
        let page = full.with_min_x(rail.right() + MARGIN);
        ui.allocate_rect(full, egui::Sense::hover());

        let mut rail_ui = ui.new_child(egui::UiBuilder::new().max_rect(rail).layout(egui::Layout::top_down(egui::Align::Center)));
        for item in Page::ALL {
            if rail_ui.add(icon_button(item.icon()).selected(self.mini.page == item)).on_hover_text(item.title()).clicked() {
                self.mini.page = item;
            }
        }

        let mut page_ui = ui.new_child(egui::UiBuilder::new().id_salt("mini_page").max_rect(page).layout(egui::Layout::top_down(egui::Align::Min)));
        page_ui.set_clip_rect(page.intersect(ui.clip_rect()));
        match self.mini.page {
            Page::Queue => self.queue_ui(&mut page_ui),
            Page::Lyrics => {
                self.lyrics_ui(&mut page_ui, &shared.view);
            }
            Page::Groupings => self.groupings_ui(&mut page_ui),
            Page::Soundfonts => self.soundfont_ui(&mut page_ui),
            Page::Visualizer => self.mini_visualizer_ui(&mut page_ui, shared),
        }
    }

    /// The visualizer filling the page, with a button on its corner that
    /// swaps it for the script picker (the list the visualizer's bar drops
    /// down): a script picked starts at once, Back goes back to it.
    fn mini_visualizer_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        if self.mini.picking {
            ui.horizontal(|ui| {
                if ui.button(format!("{}  Back", super::icon("arrow-left"))).on_hover_text("Back to the visualizer").clicked() {
                    self.mini.picking = false;
                }
                ui.strong("Pick a visualizer");
            });
            ui.add_space(4.0);
            if let Some(super::previews::Picked::Script(idx)) = self.script_list_ui(ui, false) {
                self.load_script(idx);
            }
            return;
        }
        let rect = ui.available_rect_before_wrap();
        let mut picture = ui.new_child(egui::UiBuilder::new().id_salt("mini_picture").max_rect(rect));
        self.show_visualizer(&mut picture, &shared.notes, &shared.view, DisplayMode::Mini);
        ui.allocate_rect(rect, egui::Sense::hover());
        // (Under a fixed id: the visualizer adds a different number of
        // widgets from frame to frame.)
        let corner = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 6.0), egui::vec2(super::controls_bar::icon_button_width(ui.spacing()), 24.0));
        let mut button_ui = ui.new_child(egui::UiBuilder::new().id_salt("mini_script_button").max_rect(corner));
        button_ui.visuals_mut().widgets.inactive.weak_bg_fill = egui::Color32::from_black_alpha(140);
        let name = self.script.path().map(super::nice_name).unwrap_or_else(|| "No script".to_string());
        if button_ui.add(icon_button("swap")).on_hover_text(format!("{name}: pick another visualizer")).clicked() {
            self.mini.picking = true;
        }
    }

    /// Before a frame's input reaches egui: where the pointer is, from the
    /// system's cursor, for a few frames after the window moved under it
    /// (`MiniPlayer::pointer_frames`), unless the mouse moved itself.
    pub(super) fn mini_raw_input(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.mini.pointer_frames == 0 {
            return;
        }
        self.mini.pointer_frames -= 1;
        ctx.request_repaint();
        let moved = raw_input.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerGone));
        let inner = raw_input.viewports.get(&egui::ViewportId::ROOT).and_then(|v| v.inner_rect);
        if moved {
            return;
        }
        if let (Some(inner), Some(cursor)) = (inner, cursor_position()) {
            let pos = (cursor / ctx.pixels_per_point()) - inner.min.to_vec2();
            if egui::Rect::from_min_size(egui::Pos2::ZERO, inner.size()).contains(pos) {
                raw_input.events.insert(0, egui::Event::PointerMoved(pos));
            }
        }
    }
}

/// Where the mouse cursor is on screen, in physical pixels.
fn cursor_position() -> Option<egui::Pos2> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT::default();
    // SAFETY: writes the cursor's position into `point`.
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some(egui::pos2(point.x as f32, point.y as f32))
}

/// Put the window in the taskbar, or take it out (the mini player isn't
/// in it). As winit's `set_skip_taskbar`, which eframe doesn't reach.
fn show_in_taskbar(hwnd: Option<usize>, shown: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
    let Some(hwnd) = hwnd else { return };
    let hwnd = HWND(hwnd as *mut std::ffi::c_void);
    // SAFETY: the window's own HWND, on the thread that made it (where
    // winit has COM going).
    let result = unsafe {
        CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER).and_then(|list| {
            list.HrInit()?;
            if shown { list.AddTab(hwnd) } else { list.DeleteTab(hwnd) }
        })
    };
    if let Err(e) = result {
        log::warn!("couldn't {} the taskbar: {e}", if shown { "put the window back in" } else { "take the mini player out of" });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_above_the_tray_icon_unless_theres_no_room() {
        let size = egui::vec2(400.0, 132.0);
        assert_eq!(placement([1900.0, 1060.0], 1.0, size), egui::pos2(1500.0, 928.0));
        assert_eq!(placement([3800.0, 2120.0], 2.0, size), egui::pos2(1500.0, 928.0));
        // A taskbar at the top: below the cursor.
        assert_eq!(placement([1900.0, 20.0], 1.0, size), egui::pos2(1500.0, 20.0));
    }

    #[test]
    fn the_drawer_adds_its_height_and_lets_the_window_resize() {
        let mut mini = MiniPlayer::default();
        assert_eq!(mini.size(), STRIP);
        mini.drawer = true;
        assert_eq!(mini.size(), egui::vec2(STRIP.x, STRIP.y + DRAWER));
    }
}
