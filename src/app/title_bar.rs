//! The title bar along the top of the desktop layout: the wordmark (which
//! just hops when clicked), the View and Help menus, the record,
//! Preferences, feedback and notification buttons, and, while the window
//! draws its own frame (`App::wants_native_frame` off), the window's
//! minimize, maximize and close buttons. The bar's empty middle moves the
//! window (double-click: maximize), and the window's edges resize it.

use eframe::egui::{self, ResizeDirection, ViewportCommand};

use super::{icon, profiler, App, Section};

/// How tall the bar is.
pub(super) const HEIGHT: f32 = 30.0;
/// How far in from the window's edges they resize it.
const EDGE: f32 = 5.0;
/// How long the wordmark's hop takes.
const HOP: f32 = 0.35;

impl App {
    /// Whether the window has the system's frame: when asked for in
    /// Preferences, and in the layouts without the title bar.
    pub fn wants_native_frame(&self) -> bool {
        self.config.native_frame || self.mobile() || self.dedicated.is_some()
    }

    /// Give the window the system's frame or take it away, as
    /// [`Self::wants_native_frame`] says.
    pub(super) fn apply_window_frame(&mut self, ctx: &egui::Context) {
        let native = self.wants_native_frame();
        if self.native_frame != Some(native) {
            self.native_frame = Some(native);
            ctx.send_viewport_cmd(ViewportCommand::Decorations(native));
        }
        if !native {
            resize_edges(ctx);
        }
    }

    pub(super) fn title_bar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            self.wordmark_ui(ui);
            let menus = crate::menu::config(ui.ctx());
                let mut toggled = Vec::new();
                let mut switch_to_mobile = false;
                egui::containers::menu::MenuButton::from_button(bar_button("View")).config(menus.clone()).ui(ui, |ui| {
                    let mut section_checkbox = |ui: &mut egui::Ui, section: Section| {
                        let mut open = self.open_sections.contains(&section);
                        if ui.checkbox(&mut open, section.title()).changed() {
                            toggled.push((section, open));
                        }
                    };
                    section_checkbox(ui, Section::Songs);
                    section_checkbox(ui, Section::Groupings);
                    section_checkbox(ui, Section::Soundfonts);
                    section_checkbox(ui, Section::Queue);
                    section_checkbox(ui, Section::Wishlist);
                    section_checkbox(ui, Section::Apps);
                    ui.separator();
                    section_checkbox(ui, Section::Visualizer);
                    section_checkbox(ui, Section::Editor);
                    section_checkbox(ui, Section::Reference);
                    section_checkbox(ui, Section::Sprites);
                    section_checkbox(ui, Section::Library);
                    ui.separator();
                    ui.menu_button("Layout", |ui| self.layout_menu_ui(ui));
                    if ui.button("Mobile layout").on_hover_text("The Android app's layout, one page at a time, in a phone-sized window").clicked() {
                        switch_to_mobile = true;
                    }
                });
                if switch_to_mobile {
                    let ctx = ui.ctx().clone();
                    self.set_mobile_layout(&ctx, true);
                }
                for (section, open) in toggled {
                    self.set_open(section, open);
                }
                egui::containers::menu::MenuButton::from_button(bar_button("Help")).config(menus).ui(ui, |ui| {
                    if ui.button("Welcome...").on_hover_text("The welcome window, and the starter pack").clicked() {
                        self.open_welcome();
                    }
                    if ui.button("Tour").on_hover_text("A walk through the app, one thing at a time").clicked() {
                        self.start_tour();
                    }
                    if ui.button("Check for updates...").clicked() {
                        self.launch_update_check = false;
                        self.updates_open = true;
                        self.updater.check(self.update_track());
                    }
                    if ui.button("Changelog...").clicked() {
                        self.changelog = Some(None);
                    }
                    if ui.button("Send feedback...").on_hover_text("Tell us what's working and what isn't").clicked() {
                        self.open_feedback();
                    }
                    if ui.button("Log...").clicked() {
                        self.log_open = true;
                    }
                    let mut profiler = profiler::is_open();
                    if ui
                        .checkbox(&mut profiler, "Profiler")
                        .on_hover_text("How long each part of every frame takes, live, as a flame graph")
                        .changed()
                    {
                        profiler::set_open(profiler);
                    }
                    if ui.button("Credits & licenses...").clicked() {
                        self.credits_open = true;
                    }
                    ui.separator();
                    ui.weak(format!("synththing v{}", crate::build_info::describe()));
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.wants_native_frame() {
                    window_buttons_ui(ui);
                }
                self.notifications_button_ui(ui);
                self.feedback_button_ui(ui);
                if ui.add(bar_icon("gear")).on_hover_text("Preferences").clicked() {
                    self.preferences_open = true;
                }
                self.record_button_ui(ui);
                // (What's left between the menus and the buttons.)
                let rest = ui.available_rect_before_wrap();
                let grip = ui.interact(rest, ui.id().with("title_drag"), egui::Sense::click_and_drag());
                if !self.wants_native_frame() {
                    if grip.double_clicked() {
                        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
                        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                    } else if grip.drag_started() {
                        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                    }
                }
            });
        });
    }

    /// The wordmark, which does nothing but hop when clicked (or the name,
    /// if it can't be drawn).
    fn wordmark_ui(&mut self, ui: &mut egui::Ui) {
        let height = ui.text_style_height(&egui::TextStyle::Button) * 1.25;
        let Some(image) = self.wordmark.image(ui.ctx(), height, ui.visuals().panel_fill) else {
            ui.strong("synththing");
            return;
        };
        let size = image.calc_size(egui::Vec2::splat(f32::INFINITY), image.size());
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        let id = response.id.with("hop");
        let now = ui.input(|i| i.time);
        if response.clicked() {
            ui.data_mut(|d| d.insert_temp(id, now));
        }
        let since = ui.data(|d| d.get_temp::<f64>(id)).map(|at| (now - at) as f32);
        let lift = match since {
            Some(t) if t < HOP => {
                ui.ctx().request_repaint();
                (t / HOP * std::f32::consts::PI).sin() * 5.0
            }
            _ => 0.0,
        };
        image.paint_at(ui, rect.translate(egui::vec2(0.0, -lift)));
        ui.add_space(4.0);
    }

    /// Record: the recording's menu (start one, or its controls while it
    /// runs), lit while recording.
    fn record_button_ui(&mut self, ui: &mut egui::Ui) {
        let busy = self.recording.recorder.is_some() || self.recording.render.is_some();
        let button = ui.add(bar_icon("record").selected(busy)).on_hover_text("Record the visualizer");
        egui::Popup::menu(&button).show(|ui| self.recording_menu_ui(ui));
    }
}

/// A button on the bar, every one alike: `text` without a frame until
/// it's hovered.
pub(super) fn bar_button(text: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(text).frame_when_inactive(false).min_size(egui::vec2(36.0, 26.0))
}

/// A [`bar_button`] showing icon `name`.
pub(super) fn bar_icon(name: &str) -> egui::Button<'static> {
    bar_button(egui::RichText::new(icon(name)).size(16.0))
}

/// Minimize, maximize (or restore) and close, for a window without the
/// system's frame. Close tints red under the pointer.
fn window_buttons_ui(ui: &mut egui::Ui) {
    let button = bar_icon;
    ui.scope(|ui| {
        let red = egui::Color32::from_rgb(196, 43, 28);
        let w = &mut ui.visuals_mut().widgets;
        (w.hovered.weak_bg_fill, w.active.weak_bg_fill) = (red, red.gamma_multiply(0.8));
        (w.hovered.fg_stroke.color, w.active.fg_stroke.color) = (egui::Color32::WHITE, egui::Color32::WHITE);
        if ui.add(button("x")).on_hover_text("Close").clicked() {
            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
        }
    });
    let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
    let (glyph, hover) = if maximized { ("copy", "Restore") } else { ("square", "Maximize") };
    if ui.add(button(glyph)).on_hover_text(hover).clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    if ui.add(button("minus")).on_hover_text("Minimize").clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

/// Resize the window from within `EDGE` of its sides and corners, as its
/// frame would (not while maximized or fullscreen).
fn resize_edges(ctx: &egui::Context) {
    let (maximized, fullscreen) = ctx.input(|i| (i.viewport().maximized.unwrap_or(false), i.viewport().fullscreen.unwrap_or(false)));
    if maximized || fullscreen {
        return;
    }
    let Some(pointer) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    let window = ctx.viewport_rect();
    let (left, right) = (pointer.x - window.left() < EDGE, window.right() - pointer.x < EDGE);
    let (top, bottom) = (pointer.y - window.top() < EDGE, window.bottom() - pointer.y < EDGE);
    let direction = match (left, right, top, bottom) {
        (true, _, true, _) => ResizeDirection::NorthWest,
        (_, true, true, _) => ResizeDirection::NorthEast,
        (true, _, _, true) => ResizeDirection::SouthWest,
        (_, true, _, true) => ResizeDirection::SouthEast,
        (true, ..) => ResizeDirection::West,
        (_, true, ..) => ResizeDirection::East,
        (_, _, true, _) => ResizeDirection::North,
        (.., true) => ResizeDirection::South,
        _ => return,
    };
    ctx.set_cursor_icon(match direction {
        ResizeDirection::North => egui::CursorIcon::ResizeNorth,
        ResizeDirection::South => egui::CursorIcon::ResizeSouth,
        ResizeDirection::East => egui::CursorIcon::ResizeEast,
        ResizeDirection::West => egui::CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => egui::CursorIcon::ResizeNorthEast,
        ResizeDirection::SouthEast => egui::CursorIcon::ResizeSouthEast,
        ResizeDirection::NorthWest => egui::CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthWest => egui::CursorIcon::ResizeSouthWest,
    });
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}
