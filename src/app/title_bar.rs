//! The title bar along the top of the desktop layout: the wordmark (which
//! just hops when clicked), then at the right the View, Layout,
//! Preferences, Help, Feedback and Notifications buttons (labelled when
//! there's room, else icons, else all in one menu), and, while the window
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
/// How wide the empty middle that moves the window is at least.
const GRIP: f32 = 48.0;

/// The bar's buttons, in their order from the left.
#[derive(Clone, Copy, PartialEq)]
enum Item {
    View,
    Layout,
    Preferences,
    Help,
    Feedback,
    Notifications,
}

impl Item {
    const ALL: [Item; 6] = [Item::View, Item::Layout, Item::Preferences, Item::Help, Item::Feedback, Item::Notifications];

    fn icon(self) -> &'static str {
        match self {
            Item::View => "squares-four",
            Item::Layout => "layout",
            Item::Preferences => "gear",
            Item::Help => "question",
            Item::Feedback => "chat-circle-dots",
            Item::Notifications => "envelope-simple",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Item::View => "View",
            Item::Layout => "Layout",
            Item::Preferences => "Preferences",
            Item::Help => "Help",
            Item::Feedback => "Feedback",
            Item::Notifications => "Notifications",
        }
    }
}

/// How the bar's buttons fit in its room: each with its label, as icons
/// alone, or (not even those) all in one menu.
#[derive(Clone, Copy, PartialEq)]
enum Fit {
    Labels,
    Icons,
    Menu,
}

impl Fit {
    fn of(ui: &egui::Ui, items: &[Item], room: f32) -> Fit {
        let gap = ui.spacing().item_spacing.x;
        let font = egui::TextStyle::Button.resolve(ui.style());
        let padding = 2.0 * ui.spacing().button_padding.x;
        let labelled: f32 = items
            .iter()
            .map(|item| {
                let text = format!("{}  {}", icon(item.icon()), item.label());
                let width = ui.painter().layout_no_wrap(text, font.clone(), egui::Color32::WHITE).size().x + padding;
                width.max(BUTTON.x) + gap
            })
            .sum();
        if labelled <= room {
            Fit::Labels
        } else if items.len() as f32 * (BUTTON.x + gap) <= room {
            Fit::Icons
        } else {
            Fit::Menu
        }
    }
}

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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.wants_native_frame() {
                    window_buttons_ui(ui);
                }
                let items: Vec<Item> = Item::ALL.into_iter().filter(|&item| item != Item::Notifications || self.notifications_shown()).collect();
                match Fit::of(ui, &items, ui.available_width() - GRIP) {
                    Fit::Menu => self.bar_menu_ui(ui, &items),
                    fit => {
                        for &item in items.iter().rev() {
                            self.bar_item_ui(ui, item, fit);
                        }
                    }
                }
                // (What's left between the wordmark and the buttons.)
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

    /// One of the bar's buttons, as `fit` says.
    fn bar_item_ui(&mut self, ui: &mut egui::Ui, item: Item, fit: Fit) {
        let button = match fit {
            Fit::Labels => bar_button(format!("{}  {}", icon(item.icon()), item.label())),
            _ => bar_icon(item.icon()),
        };
        let menu = |button| egui::containers::menu::MenuButton::from_button(button).config(crate::menu::config(ui.ctx()));
        match item {
            Item::View => {
                menu(button).ui(ui, |ui| self.view_menu_ui(ui));
            }
            Item::Layout => {
                menu(button).ui(ui, |ui| self.layout_menu_ui(ui));
            }
            Item::Help => {
                menu(button).ui(ui, |ui| self.help_menu_ui(ui));
            }
            Item::Preferences => {
                if ui.add(button.selected(self.preferences_open)).on_hover_text(item.label()).clicked() {
                    self.toggle_preferences();
                }
            }
            Item::Feedback => {
                if ui.add(button).on_hover_text("Tell us what's working and what isn't").clicked() {
                    self.open_feedback();
                }
            }
            Item::Notifications => self.notifications_button_ui(ui, button),
        }
    }

    /// When not even the icons fit: one button, everything in its menu.
    fn bar_menu_ui(&mut self, ui: &mut egui::Ui, items: &[Item]) {
        let menu = egui::containers::menu::MenuButton::from_button(bar_icon("list")).config(crate::menu::config(ui.ctx()));
        menu.ui(ui, |ui| {
            for &item in items {
                let text = format!("{}  {}", icon(item.icon()), item.label());
                match item {
                    Item::View => {
                        ui.menu_button(text, |ui| self.view_menu_ui(ui));
                    }
                    Item::Layout => {
                        ui.menu_button(text, |ui| self.layout_menu_ui(ui));
                    }
                    Item::Help => {
                        ui.menu_button(text, |ui| self.help_menu_ui(ui));
                    }
                    Item::Notifications => {
                        let text = match self.notifications_unread() {
                            0 => text,
                            n => format!("{text} ({n})"),
                        };
                        ui.menu_button(text, |ui| self.notifications_list_ui(ui));
                    }
                    Item::Preferences => {
                        if ui.add(egui::Button::selectable(self.preferences_open, text)).clicked() {
                            self.toggle_preferences();
                        }
                    }
                    Item::Feedback => {
                        if ui.button(text).clicked() {
                            self.open_feedback();
                        }
                    }
                }
            }
        });
    }

    /// View: a checkbox for each section.
    fn view_menu_ui(&mut self, ui: &mut egui::Ui) {
        let mut toggled = Vec::new();
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
        for (section, open) in toggled {
            self.set_open(section, open);
        }
    }

    fn help_menu_ui(&mut self, ui: &mut egui::Ui) {
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
}

/// A button on the bar, every one alike: `text` without a frame until
/// it's hovered.
pub(super) fn bar_button(text: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(text).frame_when_inactive(false).min_size(BUTTON)
}

/// How big a [`bar_button`] is at least.
const BUTTON: egui::Vec2 = egui::vec2(36.0, 26.0);

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
