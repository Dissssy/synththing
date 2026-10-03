//! The Themes window (synththing > Themes...): its own OS window, kept on
//! top, so the app can be clicked around and looked at while themes are
//! picked and edited. Every change shows at once, everywhere. Also where
//! the UI scale is set (Ctrl + / Ctrl - / Ctrl 0 work anywhere too).
//!
//! Not a modal, on purpose (like the tour): the point is seeing the app
//! itself in the theme.

use std::time::Instant;

use eframe::egui;
use eframe::egui::color_picker::{Alpha, color_edit_button_srgba};

use super::App;
use crate::theme::{self, Theme};

pub(super) struct ThemesState {
    pub open: bool,
    /// Your themes (the built-in ones are `theme::builtins()`).
    custom: Vec<Theme>,
    /// The theme and UI scale have been put into effect (first frame).
    applied: bool,
    /// A theme edit not saved yet (saved once nothing's being dragged).
    unsaved: Option<Instant>,
    /// Renaming the selected theme: the name being typed, and what's wrong
    /// with it.
    renaming: Option<(String, Option<String>)>,
    /// Delete was clicked once; the next click deletes.
    confirm_delete: bool,
    /// The UI scale while its slider is being dragged (applied on release,
    /// or the slider would rescale under the pointer).
    scale_drag: Option<f32>,
}

impl Default for ThemesState {
    fn default() -> Self {
        Self {
            open: false,
            custom: theme::load_custom(),
            applied: false,
            unsaved: None,
            renaming: None,
            confirm_delete: false,
            scale_drag: None,
        }
    }
}

/// UI scale bounds and step.
const MIN_SCALE: f32 = 0.75;
const MAX_SCALE: f32 = 2.0;

impl App {
    /// The theme in use: the one picked, else the default.
    fn current_theme(&self) -> Theme {
        let name = self.config.theme.as_deref().unwrap_or(theme::DEFAULT_THEME);
        theme::builtins()
            .into_iter()
            .chain(self.themes.custom.iter().cloned())
            .find(|t| t.name == name)
            .unwrap_or_else(|| theme::builtins().into_iter().find(|t| t.name == theme::DEFAULT_THEME).unwrap())
    }

    fn theme_names(&self) -> Vec<String> {
        theme::builtins().into_iter().map(|t| t.name).chain(self.themes.custom.iter().map(|t| t.name.clone())).collect()
    }

    fn pick_theme(&mut self, ctx: &egui::Context, name: String) {
        self.themes.renaming = None;
        self.themes.confirm_delete = false;
        self.config.theme = (name != theme::DEFAULT_THEME).then_some(name);
        self.current_theme().apply(ctx);
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save the theme choice: {e:#}");
        }
    }

    /// Every frame: put the theme and scale into effect on the first one,
    /// save theme edits once nothing's being dragged, and remember a UI
    /// scale changed with the keyboard.
    pub(super) fn sync_theme(&mut self, ctx: &egui::Context) {
        if !self.themes.applied {
            self.themes.applied = true;
            self.current_theme().apply(ctx);
            ctx.set_zoom_factor(self.config.ui_scale.unwrap_or(1.0).clamp(MIN_SCALE, MAX_SCALE));
            return;
        }
        let dragging = ctx.input(|i| i.pointer.any_down());
        if self.themes.unsaved.is_some() && !dragging {
            self.themes.unsaved = None;
            if let Err(e) = theme::save_custom(&self.current_theme()) {
                self.status = format!("Couldn't save the theme: {e:#}");
            }
        }
        let zoom = ctx.zoom_factor();
        let saved = self.config.ui_scale.unwrap_or(1.0);
        if (zoom - saved).abs() > 0.001 && !dragging {
            self.config.ui_scale = ((zoom - 1.0).abs() > 0.001).then_some(zoom);
            if let Err(e) = self.config.save() {
                log::warn!("couldn't save the UI scale: {e:#}");
            }
        }
    }

    /// The Themes window, while it's open.
    pub(super) fn themes_window(&mut self, ctx: &egui::Context) {
        if !self.themes.open {
            return;
        }
        let builder = egui::ViewportBuilder::default()
            .with_title("synththing themes")
            .with_inner_size([620.0, 560.0])
            .with_min_inner_size([480.0, 380.0])
            .with_always_on_top();
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("synththing_themes"), builder, |ui, class| {
            if class == egui::ViewportClass::EmbeddedWindow {
                self.themes_ui(ui);
            } else {
                egui::CentralPanel::default().show(ui, |ui| self.themes_ui(ui));
            }
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                self.themes.open = false;
            }
        });
    }

    fn themes_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let current = self.current_theme();
        let builtin = theme::builtins().iter().any(|t| t.name == current.name);
        let mut pick: Option<String> = None;
        let mut duplicate = false;
        let mut delete = false;
        let mut edited = current.clone();

        // The scale, along the bottom.
        egui::Panel::bottom("themes_scale").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("UI scale");
                let mut scale = self.themes.scale_drag.unwrap_or_else(|| ctx.zoom_factor());
                let slider = ui.add(
                    egui::Slider::new(&mut scale, MIN_SCALE..=MAX_SCALE)
                        .step_by(0.05)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
                if slider.dragged() {
                    self.themes.scale_drag = Some(scale);
                } else if slider.changed() || slider.drag_stopped() {
                    self.themes.scale_drag = None;
                    ctx.set_zoom_factor(scale);
                }
                if ui.button("100%").clicked() {
                    ctx.set_zoom_factor(1.0);
                }
                super::info_icon(ui).on_hover_text(
                    "How big everything is. Ctrl + and Ctrl - change it anywhere in synththing, Ctrl 0 \
                     goes back to 100%. The visualizer's picture isn't affected.",
                );
            });
            ui.add_space(2.0);
        });

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                // The themes down the side, with what can be done to them.
                ui.vertical(|ui| {
                    ui.set_width(160.0);
                    egui::ScrollArea::vertical().id_salt("theme_list").max_height(ui.available_height() - 90.0).show(
                        ui,
                        |ui| {
                            ui.weak("Built in");
                            for t in theme::builtins() {
                                if ui.selectable_label(t.name == current.name, &t.name).clicked() {
                                    pick = Some(t.name);
                                }
                            }
                            ui.add_space(6.0);
                            ui.weak("Yours");
                            if self.themes.custom.is_empty() {
                                ui.weak("(none yet: duplicate one to make your own)");
                            }
                            for t in &self.themes.custom {
                                if ui.selectable_label(t.name == current.name, &t.name).clicked() {
                                    pick = Some(t.name.clone());
                                }
                            }
                        },
                    );
                    ui.separator();
                    duplicate = ui
                        .button("Duplicate")
                        .on_hover_text(format!("Make your own theme, starting as a copy of {}", current.name))
                        .clicked();
                    ui.add_enabled_ui(!builtin, |ui| {
                        if ui.button("Rename...").clicked() {
                            self.themes.renaming = Some((current.name.clone(), None));
                        }
                        let label = if self.themes.confirm_delete { "Really delete?" } else { "Delete" };
                        if ui.button(label).clicked() {
                            if self.themes.confirm_delete {
                                delete = true;
                            } else {
                                self.themes.confirm_delete = true;
                            }
                        }
                    });
                });
                ui.separator();

                // The theme: name, then its colors and shapes, then a preview.
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical().id_salt("theme_editor").show(ui, |ui| {
                        self.theme_name_ui(ui, &current);
                        if builtin {
                            ui.weak("Built in, so it can't be changed: Duplicate it to make your own from it.");
                        }
                        ui.add_space(4.0);
                        ui.add_enabled_ui(!builtin, |ui| theme_editor_ui(ui, &mut edited));
                        ui.add_space(8.0);
                        ui.strong("Preview");
                        theme_preview_ui(ui);
                    });
                });
            });
        });

        if let Some(name) = pick {
            self.pick_theme(&ctx, name);
        } else if duplicate {
            let taken = self.theme_names();
            let copy = Theme { name: theme::copy_name(&current.name, &taken), ..current.clone() };
            match theme::save_custom(&copy) {
                Ok(()) => {
                    let name = copy.name.clone();
                    self.themes.custom.push(copy);
                    self.themes.custom.sort_by_key(|t| t.name.to_lowercase());
                    self.pick_theme(&ctx, name);
                }
                Err(e) => self.status = format!("Couldn't save the theme: {e:#}"),
            }
        } else if delete {
            match theme::delete_custom(&current.name) {
                Ok(()) => {
                    self.themes.custom.retain(|t| t.name != current.name);
                    self.pick_theme(&ctx, theme::DEFAULT_THEME.to_string());
                    self.status = format!("Deleted the theme {}.", current.name);
                }
                Err(e) => self.status = format!("Couldn't delete the theme: {e:#}"),
            }
        } else if edited != current && !builtin {
            edited.apply(&ctx);
            if let Some(slot) = self.themes.custom.iter_mut().find(|t| t.name == current.name) {
                *slot = edited;
            }
            self.themes.unsaved = Some(Instant::now());
        }
    }

    /// The selected theme's name, or the box renaming it.
    fn theme_name_ui(&mut self, ui: &mut egui::Ui, current: &Theme) {
        let Some((name, error)) = &mut self.themes.renaming else {
            ui.heading(&current.name);
            return;
        };
        let mut done = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let edit = ui.text_edit_singleline(name);
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                done = true;
            }
            done |= ui.button("Rename").clicked();
            cancel = ui.button("Cancel").clicked();
        });
        if let Some(error) = error {
            ui.colored_label(ui.visuals().error_fg_color, error.as_str());
        }
        if cancel {
            self.themes.renaming = None;
            return;
        }
        if !done {
            return;
        }
        let new_name = name.trim().to_string();
        if new_name == current.name {
            self.themes.renaming = None;
            return;
        }
        let taken: Vec<String> = self.theme_names().into_iter().filter(|n| *n != current.name).collect();
        let renamed = Theme { name: new_name.clone(), ..current.clone() };
        let result = theme::check_name(&new_name, &taken)
            .and_then(|()| theme::save_custom(&renamed))
            .and_then(|()| theme::delete_custom(&current.name));
        match result {
            Ok(()) => {
                if let Some(slot) = self.themes.custom.iter_mut().find(|t| t.name == current.name) {
                    *slot = renamed;
                }
                self.themes.custom.sort_by_key(|t| t.name.to_lowercase());
                self.themes.renaming = None;
                self.config.theme = Some(new_name);
                if let Err(e) = self.config.save() {
                    self.status = format!("Couldn't save the theme choice: {e:#}");
                }
            }
            Err(e) => {
                if let Some((_, error)) = &mut self.themes.renaming {
                    *error = Some(format!("{e:#}"));
                }
            }
        }
    }
}

/// The theme's colors and shapes, each one editable.
fn theme_editor_ui(ui: &mut egui::Ui, theme: &mut Theme) {
    ui.checkbox(&mut theme.dark, "Dark base")
        .on_hover_text("Built on egui's dark look (else its light one): what the rest of the details start from");
    let p = &mut theme.palette;
    let colors: [(&str, &str, &mut theme::Hex); 19] = [
        ("Background", "Behind the tabs and panels", &mut p.background),
        ("Windows", "Windows, popups and menus", &mut p.window),
        ("Faint", "Subtle stripes and highlights", &mut p.faint),
        ("Deep", "Text fields and the deepest backgrounds", &mut p.deep),
        ("Text", "Labels and other text", &mut p.text),
        ("Widget text", "Text on buttons and in fields", &mut p.widget_text),
        ("Strong text", "Text being hovered or pressed, and headings", &mut p.strong_text),
        ("Weak text", "Secondary text", &mut p.weak_text),
        ("Selection", "Selected items and text", &mut p.selection),
        ("Accent", "Focus outlines, selected text, checkmarks and slider fills", &mut p.accent),
        ("Links", "Links", &mut p.link),
        ("Warnings", "Warning text", &mut p.warning),
        ("Errors", "Error text", &mut p.error),
        ("Code", "Behind inline code", &mut p.code),
        ("Widgets", "Buttons, fields and the like", &mut p.widget),
        ("Hovered", "A widget under the pointer", &mut p.widget_hovered),
        ("Pressed", "A widget being pressed", &mut p.widget_pressed),
        ("Outline", "Around a widget being hovered or pressed", &mut p.widget_outline),
        ("Borders", "Separators, window and widget borders", &mut p.border),
    ];
    egui::Grid::new("theme_colors").num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
        for (i, (label, info, color)) in colors.into_iter().enumerate() {
            color_edit_button_srgba(ui, &mut color.0, Alpha::Opaque).on_hover_text(info);
            ui.label(label).on_hover_text(info);
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });
    ui.add_space(4.0);
    let s = &mut theme.shape;
    egui::Grid::new("theme_shape").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label("Widget corners");
        ui.add(egui::Slider::new(&mut s.widget_radius, 0..=16).suffix(" px"));
        ui.end_row();
        ui.label("Window corners");
        ui.add(egui::Slider::new(&mut s.window_radius, 0..=24).suffix(" px"));
        ui.end_row();
        ui.label("Borders");
        ui.add(egui::Slider::new(&mut s.border_width, 0.0..=3.0).step_by(0.5).suffix(" px"));
        ui.end_row();
        ui.label("Shadows");
        ui.add(egui::Slider::new(&mut s.shadow, 0..=40).suffix(" px"))
            .on_hover_text("How far window and popup shadows spread; 0 keeps egui's own");
        ui.end_row();
    });
}

/// A few of each kind of widget, to see the theme on.
fn theme_preview_ui(ui: &mut egui::Ui) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            let _ = ui.button("Button");
            let _ = ui.selectable_label(true, "Selected");
            let _ = ui.selectable_label(false, "Not selected");
            let mut on = true;
            ui.checkbox(&mut on, "Checkbox");
        });
        ui.horizontal_wrapped(|ui| {
            let mut value = 0.6_f32;
            ui.add(egui::Slider::new(&mut value, 0.0..=1.0));
            let mut text = String::from("A text field");
            ui.add(egui::TextEdit::singleline(&mut text).desired_width(120.0));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Text,");
            ui.weak("weak text,");
            ui.strong("strong text,");
            ui.code("code");
            ui.hyperlink_to("a link", "https://github.com/Dissssy/synththing");
        });
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(ui.visuals().warn_fg_color, "A warning");
            ui.colored_label(ui.visuals().error_fg_color, "An error");
            ui.add(egui::ProgressBar::new(0.4).desired_width(140.0).show_percentage());
        });
    });
}
