//! Shared widgets for settings-style pages and toolbars: setting rows and
//! groups, the categories layout, icon and confirm buttons, chips.

use eframe::egui;

use crate::lua_docs;

use super::{ICON_SIZE, mobile};

/// How tall a window's `categories_body` is.
pub(super) const CATEGORIES_HEIGHT: f32 = 340.0;

/// The body of a page laid out like Preferences: `categories` down the
/// side (`selected` the one picked), and `body` drawing the picked one's
/// contents beside them, `height` tall. Each side scrolls by itself (the
/// body only when `scrolls` says, for one that has its own scrolling).
pub(super) fn categories_body<S: AsRef<str>>(
    ui: &mut egui::Ui,
    id: &str,
    categories: &[S],
    selected: &mut usize,
    height: f32,
    scrolls: impl Fn(usize) -> bool,
    body: impl FnOnce(&mut egui::Ui, usize),
) {
    // A fixed height: the divider takes the height it's given, so in a
    // window left to size itself, it grows a little every frame.
    *selected = (*selected).min(categories.len().saturating_sub(1));
    let size = egui::vec2(ui.available_width(), height);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.set_height(height);
        ui.vertical(|ui| {
            ui.set_width(140.0);
            egui::ScrollArea::vertical().id_salt((id, "categories")).max_height(height).show(ui, |ui| {
                for (i, title) in categories.iter().enumerate() {
                    let pill = egui::Button::selectable(*selected == i, title.as_ref()).corner_radius(12.0);
                    if ui.add(pill).clicked() {
                        *selected = i;
                    }
                }
            });
        });
        ui.separator();
        ui.vertical(|ui| {
            if !scrolls(*selected) {
                ui.set_height(height);
                body(ui, *selected);
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt((id, "body"))
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    body(ui, *selected);
                });
        });
    });
}

/// A page in the panels' place, like Preferences: `title` with a close
/// button, then `categories` down the side (`selected` the one picked)
/// and `body` beside them (scrolling as `scrolls` says, as
/// [`categories_body`]); with no categories, the body alone. Whether it's to close: the button, or Esc (when
/// `esc` lets it, and no dropdown or dialog takes it first).
pub(super) fn page_ui<S: AsRef<str>>(
    ui: &mut egui::Ui,
    title: &str,
    categories: &[S],
    selected: &mut usize,
    esc: bool,
    scrolls: impl Fn(usize) -> bool,
    body: impl FnOnce(&mut egui::Ui, usize),
) -> bool {
    let mut close = false;
    let outer = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 8 });
    egui::CentralPanel::default().frame(outer).show(ui, |ui| {
        let page = egui::Frame::new().fill(ui.visuals().window_fill).corner_radius(6).inner_margin(12);
        page.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                ui.heading(title);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.add(icon_button("x")).on_hover_text("Close (Esc)").clicked();
                });
            });
            ui.add_space(6.0);
            let height = ui.available_height();
            if !categories.is_empty() {
                categories_body(ui, title, categories, selected, height, scrolls, body);
            } else if scrolls(0) {
                egui::ScrollArea::vertical().id_salt((title, "body")).auto_shrink([false, false]).show(ui, |ui| body(ui, 0));
            } else {
                body(ui, 0);
            }
        });
    });
    let ctx = ui.ctx().clone();
    if esc
        && !egui::Popup::is_any_open(&ctx)
        && ctx.memory(|m| m.top_modal_layer()).is_none()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        close = true;
    }
    close
}

/// `width`, or less on a screen too narrow for it (a phone's): what windows
/// and dialogs set their width to.
pub(super) fn fit_width(ui: &egui::Ui, width: f32) -> f32 {
    width.min(ui.ctx().content_rect().width() - 32.0).max(120.0)
}

/// Where a window may be: the screen, less the reach of its resize handles
/// past its edges, so none of them is cut off by the screen's side.
pub(super) fn window_room(ctx: &egui::Context) -> egui::Rect {
    ctx.content_rect().shrink(ctx.global_style().interaction.resize_grab_radius_corner)
}

/// What an empty panel shows: `text` in weak text, centered in the room
/// left in `ui`, wrapped to a readable width (a line break in it starts a
/// new line). Only painted, so a list drawn after it (a drop target, say)
/// still gets the room.
pub fn placeholder(ui: &egui::Ui, text: &str) {
    let rect = ui.available_rect_before_wrap();
    let mut job = egui::text::LayoutJob::simple(text.to_string(), egui::TextStyle::Body.resolve(ui.style()), ui.visuals().weak_text_color(), (rect.width() - 32.0).clamp(80.0, 420.0));
    job.halign = egui::Align::Center;
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(rect.center() - egui::vec2(0.0, galley.size().y / 2.0), galley, ui.visuals().weak_text_color());
}

/// A Phosphor icon by its name (`shield-check`, `SCROLL`), for text: the
/// scroll if there's no such icon.
pub fn icon(name: &str) -> &'static str {
    let wanted = name.trim().replace('-', "_").to_uppercase();
    egui_phosphor::regular::ICONS
        .iter()
        .find(|(n, _)| *n == wanted)
        .map_or(egui_phosphor::regular::SCROLL, |(_, glyph)| glyph)
}

/// A setting on one line (`add`), with an (i) after it: hovering either
/// shows `info`, the longer explanation.
/// The (i) sits at the row's right end, so they line up down a page.
/// The (i) goes in first, so the setting gets only the room left of it.
pub(super) fn with_info(ui: &mut egui::Ui, info: &str, add: impl FnOnce(&mut egui::Ui) -> egui::Response) {
    setting_row(ui, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            info_icon(ui, info);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| add(ui).on_hover_text(info));
        });
    });
}

/// One line of settings, across the whole width (text too long for it
/// wraps). In a `settings_group`, every other one is on the striped
/// background, as a list's rows are.
pub(super) fn setting_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let id = egui::Id::unique(SETTINGS_GROUP);
    let striped = ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Option<usize>>(id).as_mut().map(|rows| {
            *rows += 1;
            *rows % 2 == 0
        })
    });
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Frame::new().inner_margin(egui::Margin::symmetric(0, GROUP_ROW_MARGIN)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            add(ui)
        })
        .inner
    });
    if striped == Some(true) {
        let rect = inner.response.rect.expand2(egui::vec2(GROUP_MARGIN as f32, 0.0));
        ui.painter().set(background, egui::Shape::rect_filled(rect, 0.0, ui.visuals().faint_bg_color));
    }
    inner.inner
}

/// Where `setting_row` counts the rows of the `settings_group` being drawn.
pub(super) const SETTINGS_GROUP: &str = "settings_group";

/// The room around a settings group's contents, and above and below each
/// of its rows.
pub(super) const GROUP_MARGIN: i8 = 8;

pub(super) const GROUP_ROW_MARGIN: i8 = 3;

/// A box of settings under a heading, drawn as a list is: outlined, the
/// heading (an icon, the title, a line of hint under it) on the header's
/// color, the rows (`setting_row`) striped. Built up, then drawn by `show`.
pub(super) struct SettingsGroup<'a> {
    title: &'a str,
    icon: Option<&'a str>,
    hint: Option<&'a str>,
    /// Opens and closes by its heading (closed at first, unless
    /// `open_at_first`), kept by this
    /// whatever the title says.
    closable: Option<&'a str>,
    /// Starts a `closable` group open.
    open_at_first: bool,
    /// Draws the heading's buttons at its right end (in a right-to-left
    /// layout): what acts on the whole group, as adding to its list.
    buttons: Option<&'a mut dyn FnMut(&mut egui::Ui)>,
}

pub(super) fn settings_group(title: &str) -> SettingsGroup<'_> {
    SettingsGroup { title, icon: None, hint: None, closable: None, open_at_first: false, buttons: None }
}

impl<'a> SettingsGroup<'a> {
    pub(super) fn icon(self, icon: &'a str) -> Self {
        Self { icon: Some(icon), ..self }
    }

    pub(super) fn hint(self, hint: &'a str) -> Self {
        Self { hint: Some(hint), ..self }
    }

    pub(super) fn closable(self, id_salt: &'a str) -> Self {
        Self { closable: Some(id_salt), ..self }
    }

    pub(super) fn open_at_first(self) -> Self {
        Self { open_at_first: true, ..self }
    }

    pub(super) fn buttons(self, buttons: &'a mut dyn FnMut(&mut egui::Ui)) -> Self {
        Self { buttons: Some(buttons), ..self }
    }

    /// Draw it, `add` drawing its contents (unless it's closed: `None`).
    pub(super) fn show<R>(mut self, ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
        let id = egui::Id::unique(SETTINGS_GROUP);
        let visuals = ui.visuals().clone();
        let radius = 6u8;
        let toggle = self.closable.map(|salt| ui.make_persistent_id(("settings_group_open", salt)));
        let open = toggle.is_none_or(|toggle| ui.ctx().data_mut(|d| *d.get_persisted_mut_or_insert_with(toggle, || self.open_at_first)));
        // (No room of its own around it: the item spacing between groups
        // is about the room at its sides.)
        let group = ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let bottom = if open { 0 } else { radius };
            let header = egui::Frame::new()
                .fill(visuals.extreme_bg_color)
                .corner_radius(egui::CornerRadius { nw: radius, ne: radius, sw: bottom, se: bottom })
                .inner_margin(egui::Margin::symmetric(GROUP_MARGIN, 5))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // (The caret and buttons go in first, at the right end,
                    // so the title and hint wrap in the room left of them.
                    // A row's height to start with, as `horizontal` does:
                    // `with_layout` would take all the height left, and with
                    // no caret or buttons to size the row the title centres
                    // in it.)
                    let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if toggle.is_some() {
                            ui.label(icon(if open { "caret-up" } else { "caret-down" }));
                        }
                        if let Some(buttons) = self.buttons.as_mut() {
                            buttons(ui);
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if let Some(name) = self.icon {
                                ui.label(egui::RichText::new(icon(name)).size(16.0).color(visuals.weak_text_color()));
                            }
                            ui.vertical(|ui| {
                                ui.strong(self.title);
                                if let Some(hint) = self.hint {
                                    ui.add(egui::Label::new(egui::RichText::new(hint).weak()).wrap());
                                }
                            });
                        });
                    });
                });
            if let Some(toggle) = toggle
                && ui
                    .interact(header.response.rect, toggle, egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
            {
                ui.ctx().data_mut(|d| d.insert_persisted(toggle, !open));
            }
            if !open {
                return None;
            }
            let outer = ui.ctx().data_mut(|d| d.get_temp::<Option<usize>>(id)).flatten();
            ui.ctx().data_mut(|d| d.insert_temp(id, Some(0usize)));
            let inner = egui::Frame::new().inner_margin(egui::Margin::symmetric(GROUP_MARGIN, 2)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            });
            ui.ctx().data_mut(|d| d.insert_temp(id, outer));
            Some(inner.inner)
        });
        let rect = group.response.rect;
        ui.painter().rect_stroke(rect, radius, visuals.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
        group.inner
    }
}

/// How wide a setting's label is (`setting_label`), so the controls after
/// the labels line up.
pub(super) const LABEL_WIDTH: f32 = 160.0;

/// How wide a setting's dropdown or slider is, where there's room.
pub(super) const CONTROL_WIDTH: f32 = 180.0;

/// The room a slider's value box (`setting_slider`) takes after it: the
/// widest of them ("1000 ms"), and the gap before it.
pub(super) const SLIDER_VALUE_WIDTH: f32 = 64.0;

/// How wide a setting's control is, after its `setting_label`: its usual
/// width, less on a narrow screen, so it and the `beside` points after it
/// fit in the row.
pub(super) fn control_width(ui: &egui::Ui, beside: f32) -> f32 {
    CONTROL_WIDTH.min(ui.available_width() - beside).max(0.0)
}

/// A setting's dropdown, as wide as `control_width` makes it: a choice too
/// long for it is cut off (not wrapped, as the rest of a `setting_row` is).
pub(super) fn setting_combo<'a>(ui: &egui::Ui, id_salt: &str, beside: f32) -> egui::ComboBox<'a> {
    egui::ComboBox::from_id_salt(id_salt).width(control_width(ui, beside)).truncate()
}

/// A dropdown showing `selected`, for a wrapping row (`horizontal_wrapped`).
/// egui lays a ComboBox out in a frame of its own before it knows how wide
/// it is, so a wrapping row can't move one to the next line: it runs past
/// the edge (and widens the page under it). This starts the next line first
/// when it wouldn't fit on this one.
pub(crate) fn wrapping_combo<'a>(ui: &mut egui::Ui, id_salt: &str, selected: &str) -> egui::ComboBox<'a> {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text = ui.painter().layout_no_wrap(selected.to_string(), font, egui::Color32::PLACEHOLDER).size().x;
    let spacing = ui.spacing();
    let width = (text + spacing.icon_spacing + spacing.icon_width + 2.0 * spacing.button_padding.x).max(spacing.combo_width);
    // (A wrapping row's `available_width` is the whole row's: what's left
    // on this line is from the cursor.)
    if ui.max_rect().right() - ui.cursor().min.x < width {
        ui.end_row();
    }
    egui::ComboBox::from_id_salt(id_salt).selected_text(selected)
}

/// A setting's slider, as wide as `control_width` makes it with its value
/// box after it (and `beside` points more).
pub(super) fn setting_slider(ui: &mut egui::Ui, slider: egui::Slider, beside: f32) -> egui::Response {
    ui.spacing_mut().slider_width = control_width(ui, SLIDER_VALUE_WIDTH + beside);
    ui.add(slider)
}

/// A setting's name, in a column of its own width (less on a narrow
/// screen): the control goes after it.
pub(super) fn setting_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let width = LABEL_WIDTH.min(ui.available_width() * 0.4);
    let height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(egui::vec2(width, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_width(width);
        ui.add(egui::Label::new(text).truncate())
    })
    .inner
}

/// A small circled "i": `info` on hover, and in the mobile layout's toast
/// on a tap (a touch screen has no hover; `mobile::INFO_TOAST`).
pub(super) fn info_icon(ui: &mut egui::Ui, info: impl Into<String>) -> egui::Response {
    let info = info.into();
    let size = ui.text_style_height(&egui::TextStyle::Body).min(16.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    if response.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique(mobile::INFO_TOAST), info.clone()));
    }
    let response = response.on_hover_text(info);
    let color = if response.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
    let painter = ui.painter();
    painter.circle_stroke(rect.center(), size * 0.45, egui::Stroke::new(1.2, color));
    painter.text(
        rect.center() + egui::vec2(0.0, 0.5),
        egui::Align2::CENTER_CENTER,
        "i",
        egui::FontId::proportional(size * 0.7),
        color,
    );
    response
}

/// Reference text as one wrapping label: `prefix`, then `text` with its
/// `inline code` spans in the code font and background.
pub(super) fn inline_code_job(ui: &egui::Ui, prefix: &str, text: &str) -> egui::text::LayoutJob {
    let body = egui::TextFormat {
        font_id: egui::TextStyle::Body.resolve(ui.style()),
        color: ui.visuals().text_color(),
        ..Default::default()
    };
    let code = egui::TextFormat {
        font_id: egui::TextStyle::Monospace.resolve(ui.style()),
        color: ui.visuals().text_color(),
        background: ui.visuals().code_bg_color,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(prefix, 0.0, body.clone());
    for (span, is_code) in lua_docs::inline_spans(text) {
        job.append(span, 0.0, if is_code { code.clone() } else { body.clone() });
    }
    job
}

/// A toggle as a chip (a channel's, `ChannelToggles::Chips`), like the
/// library's: filled with its color while it's on, washed out to grey
/// while it's off, outlined in the theme's hover color while hovered.
/// Clicking it toggles.
pub(crate) fn toggle_chip(ui: &mut egui::Ui, on: bool, text: &str, color: egui::Color32) -> egui::Response {
    let fill = if on { color } else { washed_out(color, ui.visuals().dark_mode) };
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, on_color(fill));
    let padding = egui::vec2(8.0, 2.0);
    let (rect, response) = ui.allocate_exact_size(galley.size() + padding * 2.0, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let radius = rect.height() / 2.0;
        ui.painter().rect_filled(rect, radius, fill);
        if response.hovered() {
            let hover = ui.visuals().widgets.hovered.bg_stroke;
            let stroke = egui::Stroke::new(hover.width.max(1.5), hover.color);
            ui.painter().rect_stroke(rect, radius, stroke, egui::StrokeKind::Outside);
        }
        ui.painter().galley(rect.min + padding, galley, on_color(fill));
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, text));
    response
}

/// A chip: `text` on a pill of `color`, black or white (`on_color`).
pub(crate) fn chip(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, on_color(color));
    let padding = egui::vec2(7.0, 1.0);
    let (rect, response) = ui.allocate_exact_size(galley.size() + padding * 2.0, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, rect.height() / 2.0, color);
        ui.painter().galley(rect.min + padding, galley, on_color(color));
    }
    response
}

/// Black or white, whichever reads better on `color` (by its relative
/// luminance): a check mark on a channel's color, a chip's text.
pub(crate) fn on_color(color: egui::Color32) -> egui::Color32 {
    let [r, g, b, _] = color.to_array();
    let light = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b) > 140.0;
    if light { egui::Color32::BLACK } else { egui::Color32::WHITE }
}

/// `color` with most of its color taken out and pushed toward the theme's
/// background: how a channel chip looks while its channel is off, still a
/// hint of which channel it is.
pub(super) fn washed_out(color: egui::Color32, dark: bool) -> egui::Color32 {
    let grey = (0.2126 * f32::from(color.r()) + 0.7152 * f32::from(color.g()) + 0.0722 * f32::from(color.b())) as u8;
    let toward = |c: u8, to: u8, t: f32| (f32::from(c) + (f32::from(to) - f32::from(c)) * t).round() as u8;
    let [r, g, b] = [color.r(), color.g(), color.b()].map(|c| toward(c, grey, 0.65));
    let (to, t) = if dark { (20, 0.45) } else { (250, 0.4) };
    egui::Color32::from_rgb(toward(r, to, t), toward(g, to, t), toward(b, to, t))
}

/// A toolbar button: just its icon (Phosphor), a little larger than text.
/// Give it a hover text saying what it does.
pub(crate) fn icon_button(name: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(icon(name)).size(ICON_SIZE)).min_size(egui::vec2(0.0, 24.0))
}

/// The toolbar toggle for the song list `key` (its `select_list` key)
/// following playback: keeping the playing song in view.
pub(super) fn follow_button_ui(ui: &mut egui::Ui, key: &'static str) {
    let on = crate::select_list::following(ui.ctx(), key);
    let hover = "Follow playback: keep the playing song in view here. Scrolling the list turns it off";
    if ui.add(icon_button("crosshair-simple").selected(on)).on_hover_text(hover).clicked() {
        crate::select_list::set_following(ui.ctx(), key, !on);
        super::toast::show(ui.ctx(), if on { "Follow playback: off" } else { "Follow playback: on" });
    }
}

/// A button for something that can't be taken back (`button`, with
/// `hover`): its first click turns it into a red `confirm` ("Really
/// delete?"), and only a click on that says yes. A click anywhere else,
/// Esc, or the button going out of sight backs out. `id` tells apart such
/// buttons shown at once (one per row, say).
pub(crate) fn confirm_button(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, button: egui::Button<'_>, hover: &str, confirm: &str) -> bool {
    let id = ui.scope_id().with(("confirm_button", id));
    let pass = ui.ctx().cumulative_pass_nr();
    // Armed while it was shown armed the pass before.
    let armed = ui.data(|d| d.get_temp::<u64>(id)).is_some_and(|shown| shown + 1 >= pass);
    let clicked = if armed {
        ui.add(egui::Button::new(egui::RichText::new(confirm).color(ui.visuals().error_fg_color))).clicked()
    } else {
        ui.add(button).on_hover_text(hover).clicked()
    };
    let backed_out = !clicked && ui.input(|i| i.pointer.any_click() || i.key_pressed(egui::Key::Escape));
    let stays_armed = if armed { !clicked && !backed_out } else { clicked };
    if stays_armed {
        ui.data_mut(|d| d.insert_temp(id, pass));
    } else {
        ui.data_mut(|d| d.remove::<u64>(id));
    }
    armed && clicked
}

/// An icon button as tall as the other controls in a row of settings (or
/// any row of text-sized widgets), so everything in the row lines up on
/// its middle. Give it a hover text saying what it does.
pub(crate) fn row_icon_button(ui: &egui::Ui, name: &str) -> egui::Button<'static> {
    let height = ui.spacing().interact_size.y;
    egui::Button::new(egui::RichText::new(icon(name)).size(height - 4.0)).min_size(egui::vec2(height + 6.0, height))
}

/// The room a `row_icon_button` takes in a row, with the gap before it:
/// what to leave for one after a setting's control (`control_width`).
pub(crate) fn row_icon_button_room(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y + 6.0 + ui.spacing().item_spacing.x
}

/// As `icon_button`, its icon in `color`.
pub(super) fn colored_icon_button(name: &str, color: egui::Color32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(icon(name)).size(16.0).color(color)).min_size(egui::vec2(28.0, 24.0))
}

/// Long content drawn block by block, where a block out of sight only
/// takes its space, as measured when it was last drawn at this width:
/// laying all of it out every frame made Credits and the Scripting
/// Reference cost 8-45 ms a frame. `begin` it, draw each `block`, `end` it.
pub(super) struct Culled {
    id: egui::Id,
    width: f32,
    heights: std::collections::HashMap<egui::Id, f32>,
}

impl Culled {
    pub(super) fn begin(ui: &egui::Ui, salt: impl egui::AsIdSalt) -> Self {
        let id = ui.scope_id().with(salt);
        let width = ui.available_width();
        let (seen, heights): (f32, _) = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        Self { id, width, heights: if seen == width { heights } else { Default::default() } }
    }

    /// Draws block `key` with `draw`, unless it's out of sight and not
    /// `shown` (to be scrolled to, say): then it only takes its space.
    pub(super) fn block<R>(&mut self, ui: &mut egui::Ui, key: impl egui::AsIdSalt, shown: bool, draw: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
        let key = self.id.with(key);
        let top = ui.cursor().min.y;
        if !shown
            && let Some(&height) = self.heights.get(&key)
            && !ui.is_rect_visible(egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + height))
        {
            ui.add_space(height);
            return None;
        }
        let drawn = draw(ui);
        self.heights.insert(key, ui.cursor().min.y - top);
        Some(drawn)
    }

    pub(super) fn end(self, ui: &egui::Ui) {
        ui.data_mut(|d| d.insert_temp(self.id, (self.width, self.heights)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocks out of sight are only drawn once, to measure them, and take
    /// the same room after.
    #[test]
    fn culled_blocks_out_of_sight_take_their_room() {
        let ctx = egui::Context::default();
        let frame = || {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 300.0))),
                ..Default::default()
            };
            let (mut drawn, mut bottom) = (0, 0.0);
            let mut output = ctx.run_ui(input, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut culled = Culled::begin(ui, "test");
                    for i in 0..100 {
                        culled.block(ui, i, false, |ui| {
                            drawn += 1;
                            ui.label(format!("Block {i}"));
                        });
                    }
                    bottom = ui.cursor().min.y;
                    culled.end(ui);
                });
            });
            output.textures_delta.clear();
            (drawn, bottom)
        };
        let (first, height) = frame();
        let (second, again) = frame();
        assert_eq!(first, 100, "all measured the first time");
        assert!(second < 30, "only those in sight drawn after: {second}");
        assert_eq!(height, again, "the same room either way");
    }

    /// A dropdown that doesn't fit on a wrapping row's line goes on the
    /// next one, instead of past the edge.
    #[test]
    fn wrapping_combo_wraps() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 400.0))),
            ..Default::default()
        };
        let mut combo = egui::Rect::NOTHING;
        let mut right = 0.0;
        let mut output = ctx.run_ui(input, |ui| {
            right = ui.max_rect().right();
            ui.horizontal_wrapped(|ui| {
                ui.add_sized([150.0, 20.0], egui::Label::new("wide"));
                combo = wrapping_combo(ui, "test_combo", "Newest").show_ui(ui, |_| {}).response.rect;
            });
        });
        output.textures_delta.clear();
        assert!(combo.right() <= right, "{combo:?} runs past {right}");
        assert!(combo.left() < 50.0, "on a line of its own");
    }

    /// A group's heading is as tall as its title and hint, with or without
    /// buttons at its end, even when it's the first thing in a tall window.
    #[test]
    fn group_heading_fits_its_title() {
        let ctx = egui::Context::default();
        for with_buttons in [false, true] {
            let header = std::cell::Cell::new(0.0);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 2000.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                let top = ui.cursor().min.y;
                let mut buttons = |ui: &mut egui::Ui| {
                    let _ = ui.button("New");
                };
                let mut group = settings_group("v1.0.0").icon("tag").hint("2026-11-02");
                if with_buttons {
                    group = group.buttons(&mut buttons);
                }
                group.show(ui, |ui| header.set(ui.min_rect().min.y - top));
            });
            output.textures_delta.clear(); // (no renderer here to hand them to)
            assert!(header.get() < 80.0, "buttons {with_buttons}: heading {} tall", header.get());
        }
    }
}

/// A card: a box a little apart from the settings groups, for the one
/// thing a page is about (the identity on Account, an update on Updates).
pub(super) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let visuals = ui.visuals().clone();
    egui::Frame::new()
        .fill(visuals.faint_bg_color)
        .stroke(visuals.widgets.noninteractive.bg_stroke)
        .corner_radius(10)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
}

/// A card's badge: a circle of `fill` with an icon in it.
pub(super) fn card_badge(ui: &mut egui::Ui, fill: egui::Color32, name: &str, color: egui::Color32) {
    let size = 56.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), size / 2.0, fill);
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, icon(name), egui::FontId::proportional(30.0), color);
}

/// A small rounded label saying how something stands, in `color`.
pub(super) fn status_pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(8, 1))
        .show(ui, |ui| ui.label(egui::RichText::new(text).color(color)));
}
