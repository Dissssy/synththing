//! The mini player: what clicking the tray icon turns the window into. A
//! small strip, always on top and out of the taskbar, with what's playing,
//! the seek bar and the controls bar's own buttons (`controls_line_ui`, laid
//! out as Preferences > Appearance > Controls bar says). Its drawer opens
//! the Queue, Groupings, Soundfonts and the visualizer (scripts see
//! `display_mode() == "mini"`), the same panels the main window has.
//!
//! It is the main window, made small: the app draws it in place of its
//! layout, so every panel in it is the real one, and the two are never up
//! together. Going back (Show synththing, a right-click on the tray icon,
//! launching again) puts the window back where and as big as it was.
//!
//! It stays until closed and drags around by its song line, opening where
//! it was left; unpinned, it goes away like a menu when it loses focus (or
//! on Escape). Where it was, its size, its page and whether its drawer is
//! open are kept for the session; nothing's saved.

use eframe::egui;

use super::{App, MIN_WINDOW, icon_button};
use crate::audio::PlaybackShared;
use crate::visualizer::DisplayMode;

/// The strip's size, in points: the song line, the seek bar and the
/// controls line, inside its margins.
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

/// The drawer's pages, down its rail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Queue,
    Groupings,
    Soundfonts,
    Visualizer,
}

impl Page {
    const ALL: [Self; 4] = [Self::Queue, Self::Groupings, Self::Soundfonts, Self::Visualizer];

    fn icon(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Groupings => "squares-four",
            Self::Soundfonts => "piano-keys",
            Self::Visualizer => "waveform",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Queue => "Queue",
            Self::Groupings => "Groupings",
            Self::Soundfonts => "Soundfonts",
            Self::Visualizer => "Visualizer",
        }
    }
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
    /// Where it was opened by (the tray icon, in physical screen pixels)
    /// and how many more frames to place it there for (`SETTLE_FRAMES`).
    anchor: Option<([f32; 2], u8)>,
    /// Frames left to keep it out of the taskbar for (`SETTLE_FRAMES`).
    taskbar_frames: u8,
    /// It's had focus since it was opened, so losing it closes it (unpinned).
    focused_once: bool,
    /// It stays until closed, and drags by its song line.
    pinned: bool,
    /// Where it was last (its top-left, in points), to open there again.
    at: Option<egui::Pos2>,
    width: f32,
    drawer: bool,
    drawer_height: f32,
    /// The drawer opens above the strip (there's no room below it).
    drawer_above: bool,
    page: Page,
}

impl Default for MiniPlayer {
    fn default() -> Self {
        Self {
            open: false,
            window: None,
            anchor: None,
            taskbar_frames: 0,
            focused_once: false,
            pinned: true,
            at: None,
            width: STRIP.x,
            drawer: false,
            drawer_height: DRAWER,
            drawer_above: false,
            page: Page::default(),
        }
    }
}

impl MiniPlayer {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    /// The window's size for how it's laid out now.
    fn size(&self) -> egui::Vec2 {
        if self.drawer { egui::vec2(self.width, STRIP.y + self.drawer_height) } else { egui::vec2(self.width, STRIP.y) }
    }

    /// Let it be resized (by its edges) only with the drawer open.
    fn size_limits(&self, ctx: &egui::Context) {
        let (min, max) = if self.drawer {
            (egui::vec2(MIN_WIDTH, STRIP.y + MIN_DRAWER), egui::Vec2::INFINITY)
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
        if self.mini.open {
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
        let at = match mini.at {
            Some(at) => at,
            None => {
                mini.anchor = Some((cursor, SETTLE_FRAMES));
                placement(cursor, pixels_per_point, size)
            }
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        mini.size_limits(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    /// Back from the mini player to the main window as it was (shown or
    /// not, as the caller goes on to make it).
    pub(super) fn leave_mini(&mut self, ctx: &egui::Context) {
        if !std::mem::take(&mut self.mini.open) {
            return;
        }
        self.mini.anchor = None;
        // (A hidden window goes back in by itself once it's shown.)
        if !self.hidden {
            show_in_taskbar(self.api.window, true);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
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

    /// The window as the mini player: the strip, and its drawer above or
    /// below it.
    pub(super) fn mini_player_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        let ctx = ui.ctx().clone();
        if self.mini.taskbar_frames > 0 {
            self.mini.taskbar_frames -= 1;
            show_in_taskbar(self.api.window, false);
        }
        if let Some((cursor, frames)) = self.mini.anchor {
            // Its own scale, now it's on its monitor (see `SETTLE_FRAMES`).
            let size = self.mini.size();
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(placement(cursor, ctx.pixels_per_point(), size)));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            self.mini.anchor = (frames > 1).then_some((cursor, frames - 1));
            ctx.request_repaint();
        } else if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            // Remembered where it's left, and how big (dragged by its edges).
            self.mini.at = Some(rect.min);
            if self.mini.drawer {
                let size = ctx.content_rect().size();
                self.mini.width = size.x.max(MIN_WIDTH);
                self.mini.drawer_height = (size.y - STRIP.y).max(MIN_DRAWER);
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
        let frame = egui::Frame::window(ui.style()).corner_radius(0).inner_margin(egui::Margin::same(MARGIN as i8));
        let strip = |above: bool| {
            let panel = if above { egui::Panel::bottom("mini_strip") } else { egui::Panel::top("mini_strip") };
            panel.exact_size(STRIP.y).frame(frame).show_separator_line(self.mini.drawer)
        };
        strip(self.mini.drawer_above).show(ui, |ui| self.mini_strip_ui(ui, view));
        if self.mini.drawer {
            egui::CentralPanel::default().frame(frame).show(ui, |ui| self.mini_drawer_ui(ui, shared));
        }

        // What the main window shows: it comes back for it.
        if self.preferences_open || self.details_for.is_some() || self.help.is_some() {
            self.show_window(&ctx);
        }
    }

    /// The strip: the song's art and name (what it's dragged by), the
    /// drawer, pin and close buttons; the seek bar; the controls line.
    fn mini_strip_ui(&mut self, ui: &mut egui::Ui, view: &crate::engine::EngineView) {
        let ctx = ui.ctx().clone();
        let art = 40.0;
        let (line, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), art), egui::Sense::hover());
        let square = egui::Rect::from_min_size(line.min, egui::Vec2::splat(art));
        self.now_playing_art_ui(ui, square);

        // The buttons at the right.
        let mut buttons = ui.new_child(egui::UiBuilder::new().max_rect(line).layout(egui::Layout::right_to_left(egui::Align::Center)));
        let close_hover = if self.mini.pinned { "Close the mini player" } else { "Close (Esc)" };
        if buttons.add(icon_button("x")).on_hover_text(close_hover).clicked() {
            self.hide_window(&ctx);
        }
        let (pin, pin_hover) = if self.mini.pinned {
            ("push-pin-slash", "Unpin: it closes when you click elsewhere")
        } else {
            ("push-pin", "Pin: it stays, on top, and drags by the song's name")
        };
        if buttons.add(icon_button(pin)).on_hover_text(pin_hover).clicked() {
            self.mini.pinned = !self.mini.pinned;
            self.mini.focused_once = true;
        }
        let (drawer, drawer_hover) = match (self.mini.drawer, self.mini.drawer_above) {
            (false, false) => ("caret-down", "Open the drawer: Queue, Groupings, Soundfonts and the visualizer"),
            (false, true) => ("caret-up", "Open the drawer: Queue, Groupings, Soundfonts and the visualizer"),
            (true, false) => ("caret-up", "Close the drawer"),
            (true, true) => ("caret-down", "Close the drawer"),
        };
        if buttons.add(icon_button(drawer).selected(self.mini.drawer)).on_hover_text(drawer_hover).clicked() {
            self.set_mini_drawer(&ctx, !self.mini.drawer);
        }
        if buttons.add(icon_button("app-window")).on_hover_text("Show synththing: back to the whole window").clicked() {
            self.show_window(&ctx);
        }
        let buttons_left = buttons.min_rect().left();

        // The song, between the art and the buttons; what it's dragged by.
        let song = egui::Rect::from_min_max(egui::pos2(square.right() + 8.0, line.top()), egui::pos2(buttons_left - 6.0, line.bottom()));
        let title = self.current_title().or_else(|| view.track_name.clone()).unwrap_or_else(|| "Nothing playing".to_string());
        let detail = Some(self.song_detail(view)).filter(|d| !d.is_empty());
        let mut name = ui.new_child(egui::UiBuilder::new().max_rect(song).layout(egui::Layout::left_to_right(egui::Align::Center)));
        name.set_clip_rect(song.intersect(ui.clip_rect()));
        crate::select_list::two_lines(&mut name, std::iter::empty(), egui::RichText::new(title).strong(), detail, true);
        if self.mini.pinned {
            let drag = ui.interact(line.with_max_x(buttons_left - 4.0), egui::Id::unique("mini_drag"), egui::Sense::drag());
            if drag.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
        }

        ui.add_space(2.0);
        self.seek_line_ui(ui, view);
        let width = ui.available_width();
        self.controls_line_ui(ui, view, width);
    }

    /// Open or close the drawer: below the strip, or above it when there's
    /// no room below, the strip staying where it is on screen.
    fn set_mini_drawer(&mut self, ctx: &egui::Context, open: bool) {
        let (outer, monitor) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().monitor_size));
        if open {
            self.mini.drawer_above = match (outer, monitor) {
                (Some(outer), Some(monitor)) => outer.bottom() + self.mini.drawer_height > monitor.y,
                _ => false,
            };
        }
        if let Some(outer) = outer
            && self.mini.drawer_above
        {
            let shift = if open { -self.mini.drawer_height } else { self.mini.drawer_height };
            let at = outer.min + egui::vec2(0.0, shift);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
            self.mini.at = Some(at);
        }
        if !open {
            // (Back to the strip's own width.)
            self.mini.width = STRIP.x;
        }
        self.mini.drawer = open;
        self.mini.size_limits(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(self.mini.size()));
    }

    /// The drawer: its pages down the rail at the left (and the way out at
    /// its foot), the page open beside it.
    fn mini_drawer_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        let ctx = ui.ctx().clone();
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
        rail_ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            if ui.add(icon_button("power")).on_hover_text("Quit: close synththing completely").clicked() {
                self.close_with(&ctx, super::close::CloseAction::Quit);
            }
        });

        let mut page_ui = ui.new_child(egui::UiBuilder::new().id_salt("mini_page").max_rect(page).layout(egui::Layout::top_down(egui::Align::Min)));
        page_ui.set_clip_rect(page.intersect(ui.clip_rect()));
        match self.mini.page {
            Page::Queue => self.queue_ui(&mut page_ui),
            Page::Groupings => self.groupings_ui(&mut page_ui),
            Page::Soundfonts => self.soundfont_ui(&mut page_ui),
            Page::Visualizer => self.mini_visualizer_ui(&mut page_ui, shared),
        }
    }

    /// The visualizer filling the page, with a button on its corner
    /// opening the script picker (the one the visualizer's bar drops
    /// down).
    fn mini_visualizer_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        let rect = ui.available_rect_before_wrap();
        let mut picture = ui.new_child(egui::UiBuilder::new().id_salt("mini_picture").max_rect(rect));
        self.show_visualizer(&mut picture, &shared.notes, &shared.view, DisplayMode::Mini);
        ui.allocate_rect(rect, egui::Sense::hover());
        // (Under a fixed id: the visualizer adds a different number of
        // widgets from frame to frame.)
        let corner = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 6.0), egui::vec2(super::controls_bar::ICON_BUTTON_WIDTH, 24.0));
        let mut button_ui = ui.new_child(egui::UiBuilder::new().id_salt("mini_script_button").max_rect(corner));
        button_ui.visuals_mut().widgets.inactive.weak_bg_fill = egui::Color32::from_black_alpha(140);
        let name = self.script.path().map(super::nice_name).unwrap_or_else(|| "No script".to_string());
        let button = button_ui.add(icon_button("swap")).on_hover_text(format!("{name}: pick another visualizer"));
        self.script_picker_popup(&button, rect.width() - 12.0);
    }
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
