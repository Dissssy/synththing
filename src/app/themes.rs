//! The Themes window (Preferences > Appearance > Edit themes...): its own
//! OS window, kept on top, so the app can be clicked around and looked at
//! while themes are picked and edited. Every change shows at once, everywhere. (The UI
//! scale is in Preferences > Appearance; Ctrl + / Ctrl - / Ctrl 0 work
//! anywhere too.)
//!
//! Not a modal, on purpose (like the tour): the point is seeing the app
//! itself in the theme.

use std::time::Instant;

use eframe::egui;
use eframe::egui::color_picker::{Alpha, color_edit_button_srgba};

use super::App;
use crate::theme::{self, Theme};

#[derive(Default)]
pub(super) struct ThemesState {
    pub open: bool,
    /// The theme and UI scale have been put into effect (first frame).
    applied: bool,
    /// A theme edit not saved yet (saved once nothing's being dragged).
    unsaved: Option<Instant>,
    /// A built-in theme being changed: kept here (and shown) until it's
    /// saved as a theme of your own, or another theme is picked.
    draft: Option<Theme>,
    /// Renaming the theme in use: the name being typed, and what's wrong
    /// with it.
    renaming: Option<(String, Option<String>)>,
    /// The UI scale while its slider is being dragged (applied on release,
    /// or the slider would rescale under the pointer).
    scale_drag: Option<f32>,
}

/// What was clicked in the themes' list or buttons.
enum ThemeAction {
    Pick(String),
    Duplicate,
    Delete,
    /// Keep the draft of a built-in theme as your own.
    SaveCopy,
    /// Drop the draft of a built-in theme.
    Discard,
}

/// UI scale bounds and step.
const MIN_SCALE: f32 = 0.75;
const MAX_SCALE: f32 = 2.0;

impl App {
    /// The theme in use: the one picked, else the default.
    pub(super) fn current_theme(&self) -> Theme {
        let name = self.config.theme.as_deref().unwrap_or(theme::DEFAULT_THEME);
        theme::builtins()
            .into_iter()
            .chain(self.config.custom_themes.values().cloned())
            .find(|t| t.name == name)
            .unwrap_or_else(theme::default_theme)
    }

    /// Put the theme into effect again next frame (settings sync changed it).
    pub(super) fn reapply_theme(&mut self) {
        self.themes.applied = false;
    }

    pub(super) fn theme_names(&self) -> Vec<String> {
        theme::builtins().into_iter().map(|t| t.name).chain(self.config.custom_themes.keys().cloned()).collect()
    }

    pub(super) fn pick_theme(&mut self, ctx: &egui::Context, name: String) {
        self.themes.draft = None;
        self.themes.renaming = None;
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
            if let Err(e) = self.config.save() {
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
            .with_inner_size([760.0, 640.0])
            .with_min_inner_size([520.0, 420.0])
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

    /// The Themes window: the themes down the side (built in, then
    /// yours), the picked one's name and buttons along the top, its colors
    /// and shapes by section, and a preview pinned along the bottom.
    fn themes_ui(&mut self, ui: &mut egui::Ui) {
        let (current, builtin) = self.edited_theme();
        let mut action = None;
        let mut edited = current.clone();
        egui::Panel::left("themes_sidebar").resizable(false).exact_size(170.0).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("theme_list").auto_shrink([false, false]).show(ui, |ui| {
                action = self.theme_list_ui(ui, &current.name, |ui, selected, name| {
                    ui.add(egui::Button::selectable(selected, name).corner_radius(12.0)).clicked()
                });
            });
        });
        self.theme_body_ui(ui, &current, builtin, &mut edited, &mut action);
        self.finish_theme_edit(ui.ctx(), &current, builtin, action, edited);
    }

    /// The themes in the mobile layout (Preferences > Appearance > Edit
    /// themes...): the same, but the themes are picked from a dropdown
    /// over the name.
    pub(super) fn mobile_themes_ui(&mut self, ui: &mut egui::Ui) {
        let (current, builtin) = self.edited_theme();
        let mut action = None;
        let mut edited = current.clone();
        egui::Panel::top("mobile_themes_pick").show(ui, |ui| {
            ui.add_space(4.0);
            super::setting_combo(ui, "mobile_theme_pick", 0.0).selected_text(&current.name).show_ui(ui, |ui| {
                action = self.theme_list_ui(ui, &current.name, |ui, selected, name| ui.selectable_label(selected, name).clicked());
            });
            ui.add_space(4.0);
        });
        self.theme_body_ui(ui, &current, builtin, &mut edited, &mut action);
        self.finish_theme_edit(ui.ctx(), &current, builtin, action, edited);
    }

    /// The theme shown: the draft of a built-in one being changed, else
    /// the one in use; and whether it's built in.
    fn edited_theme(&mut self) -> (Theme, bool) {
        let current = self.current_theme();
        let builtin = theme::builtins().iter().any(|t| t.name == current.name);
        // (A draft of another theme than the one in use: it was picked
        // elsewhere, in Preferences say.)
        if self.themes.draft.as_ref().is_some_and(|d| d.name != current.name) || !builtin {
            self.themes.draft = None;
        }
        (self.themes.draft.clone().unwrap_or(current), builtin)
    }

    /// The themes, built in then yours, each drawn by `item` (whether it
    /// was clicked).
    fn theme_list_ui(
        &self,
        ui: &mut egui::Ui,
        current: &str,
        mut item: impl FnMut(&mut egui::Ui, bool, &str) -> bool,
    ) -> Option<ThemeAction> {
        let mut action = None;
        ui.weak("Built in");
        for t in theme::builtins() {
            if item(ui, t.name == current, &t.name) {
                action = Some(ThemeAction::Pick(t.name));
            }
        }
        ui.separator();
        ui.weak("Yours");
        if self.config.custom_themes.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new("None yet: change a built-in one to start your own.").weak()).wrap());
        }
        for t in self.config.custom_themes.values() {
            if item(ui, t.name == current, &t.name) {
                action = Some(ThemeAction::Pick(t.name.clone()));
            }
        }
        action
    }

    /// Everything but the list of themes: the name and buttons on top, the
    /// preview pinned at the bottom, the sections scrolling between.
    fn theme_body_ui(&mut self, ui: &mut egui::Ui, current: &Theme, builtin: bool, edited: &mut Theme, action: &mut Option<ThemeAction>) {
        egui::Panel::top("theme_header").frame(egui::Frame::NONE.inner_margin(egui::Margin { left: 8, right: 4, top: 4, bottom: 8 })).show(ui, |ui| {
            if let Some(clicked) = self.theme_header_ui(ui, current, builtin) {
                *action = Some(clicked);
            }
        });
        egui::Panel::bottom("theme_preview").frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 8))).show(ui, |ui| {
            theme_preview_ui(ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 0))).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("theme_editor").auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                theme_editor_ui(ui, edited);
            });
        });
    }

    /// The theme's name (with a pencil to rename your own), what can be
    /// done to it, and a line on saving a built-in one.
    fn theme_header_ui(&mut self, ui: &mut egui::Ui, current: &Theme, builtin: bool) -> Option<ThemeAction> {
        let mut action = None;
        let changed = self.themes.draft.is_some();
        let mut renamed = None;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !builtin
                    && super::confirm_button(ui, ("theme", &current.name), super::icon_button("trash"), "Delete this theme", "Really delete?")
                {
                    action = Some(ThemeAction::Delete);
                }
                if changed {
                    if ui.button("Discard").on_hover_text("Go back to the built-in theme as it was").clicked() {
                        action = Some(ThemeAction::Discard);
                    }
                    let save = egui::Button::new(format!("{}  Save as a copy", super::icon("floppy-disk")))
                        .fill(ui.visuals().selection.bg_fill);
                    if ui.add(save).on_hover_text("Keep these changes as a theme of your own").clicked() {
                        action = Some(ThemeAction::SaveCopy);
                    }
                } else if ui.add(super::icon_button("copy")).on_hover_text(format!("Make your own theme, starting as a copy of {}", current.name)).clicked() {
                    action = Some(ThemeAction::Duplicate);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| match &mut self.themes.renaming {
                    Some((text, _)) => {
                        let response = ui.add(egui::TextEdit::singleline(text).font(egui::TextStyle::Heading).desired_width(220.0));
                        let commit = response.lost_focus() || ui.add(super::icon_button("check")).on_hover_text("Rename").clicked();
                        if commit {
                            // (Escape gives up renaming.)
                            let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
                            renamed = Some(if escaped { String::new() } else { text.trim().to_string() });
                        } else if !response.has_focus() {
                            response.request_focus();
                        }
                    }
                    None => {
                        ui.add(egui::Label::new(egui::RichText::new(&current.name).heading()).truncate());
                        if !builtin && ui.add(super::icon_button("pencil-simple")).on_hover_text("Rename").clicked() {
                            self.themes.renaming = Some((current.name.clone(), None));
                        }
                    }
                });
            });
        });
        if let Some(name) = renamed {
            self.rename_theme(current, name);
        }
        if let Some((_, Some(error))) = &self.themes.renaming {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if builtin {
            ui.weak(if changed {
                "Built in: your changes show everywhere now, and are kept once you save them as a copy."
            } else {
                "Built in: change anything to start your own theme from it."
            });
        }
        action
    }

    /// Rename the theme in use (your own) to `name`; or say why not, still
    /// renaming. No name, or the name as it was, stops renaming.
    fn rename_theme(&mut self, current: &Theme, name: String) {
        if name == current.name || name.is_empty() {
            self.themes.renaming = None;
            return;
        }
        let taken: Vec<String> = self.theme_names().into_iter().filter(|n| *n != current.name).collect();
        if let Err(e) = theme::check_name(&name, &taken) {
            if let Some((_, error)) = &mut self.themes.renaming {
                *error = Some(format!("{e:#}"));
            }
            return;
        }
        self.themes.renaming = None;
        let Some(mut renamed) = self.config.custom_themes.remove(&current.name) else { return };
        renamed.name = name.clone();
        self.config.custom_themes.insert(name.clone(), renamed);
        self.config.theme = Some(name);
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save the theme's name: {e:#}");
        }
    }

    /// The UI scale's slider (applied when it's let go), and a reset to
    /// 100% (Preferences > Appearance).
    pub(super) fn ui_scale_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        super::with_info(
            ui,
            "How big everything is. Ctrl + and Ctrl - change it anywhere in synththing, Ctrl 0 \
             goes back to 100%. The visualizer's picture isn't affected.",
            |ui| {
                let label = super::setting_label(ui, "UI scale");
                let mut scale = self.themes.scale_drag.unwrap_or_else(|| ctx.zoom_factor());
                let slider = super::setting_slider(
                    ui,
                    egui::Slider::new(&mut scale, MIN_SCALE..=MAX_SCALE)
                        .step_by(0.05)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                    super::row_icon_button_room(ui),
                );
                if slider.dragged() {
                    self.themes.scale_drag = Some(scale);
                } else if slider.changed() || slider.drag_stopped() {
                    self.themes.scale_drag = None;
                    ctx.set_zoom_factor(scale);
                }
                if ui.add(super::row_icon_button(ui, "arrow-counter-clockwise")).on_hover_text("Back to 100%").clicked() {
                    ctx.set_zoom_factor(1.0);
                }
                label | slider
            },
        );
    }

    /// Carry out what was asked of the themes, or put an edit to the
    /// theme into effect: your own is saved, a built-in one becomes a draft.
    fn finish_theme_edit(&mut self, ctx: &egui::Context, current: &Theme, builtin: bool, action: Option<ThemeAction>, edited: Theme) {
        match action {
            Some(ThemeAction::Pick(name)) => self.pick_theme(ctx, name),
            Some(ThemeAction::Duplicate) => {
                let taken = self.theme_names();
                let copy = Theme { name: theme::copy_name(&current.name, &taken), ..current.clone() };
                self.add_theme(ctx, copy);
            }
            Some(ThemeAction::SaveCopy) => {
                let taken = self.theme_names();
                self.add_theme(ctx, Theme { name: theme::copy_name(&current.name, &taken), ..current.clone() });
            }
            Some(ThemeAction::Discard) => {
                let name = current.name.clone();
                self.pick_theme(ctx, name);
            }
            Some(ThemeAction::Delete) => {
                self.config.custom_themes.remove(&current.name);
                self.pick_theme(ctx, theme::DEFAULT_THEME.to_string());
                self.status = format!("Deleted the theme {}.", current.name);
            }
            None if edited != *current => {
                edited.apply(ctx);
                if builtin {
                    self.themes.draft = Some(edited);
                } else {
                    if let Some(slot) = self.config.custom_themes.get_mut(&current.name) {
                        *slot = edited;
                    }
                    self.themes.unsaved = Some(Instant::now());
                }
            }
            None => {}
        }
    }

    /// Keep `theme` as one of yours, and use it.
    fn add_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        let name = theme.name.clone();
        self.config.custom_themes.insert(name.clone(), theme);
        // (Saved with the choice.)
        self.pick_theme(ctx, name);
    }
}

/// A theme's color: its name, then its swatch (opening a picker) and hex.
fn color_row(ui: &mut egui::Ui, label: &str, info: &str, color: &mut theme::Hex, alpha: Alpha) {
    super::with_info(ui, info, |ui| {
        let label = super::setting_label(ui, label);
        let swatch = color_edit_button_srgba(ui, &mut color.0, alpha);
        let [r, g, b, a] = color.0.to_srgba_unmultiplied();
        let hex = if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x} {:.0}%", f32::from(a) / 2.55) };
        ui.weak(hex);
        label | swatch
    });
}

/// The theme's colors and shapes, by section, each one editable.
fn theme_editor_ui(ui: &mut egui::Ui, theme: &mut Theme) {
    use super::settings_group;
    let p = &mut theme.palette;
    let opaque = |ui: &mut egui::Ui, label: &str, info: &str, color: &mut theme::Hex| color_row(ui, label, info, color, Alpha::Opaque);
    settings_group("General").icon("palette").hint("The backgrounds everything sits on").show(ui, |ui| {
        super::with_info(ui, "Built on egui's dark look (else its light one): what the rest of the details start from", |ui| {
            let label = super::setting_label(ui, "Dark base");
            label | ui.checkbox(&mut theme.dark, "")
        });
        opaque(ui, "Background", "Behind the tabs and panels", &mut p.background);
        opaque(ui, "Windows", "Windows, panels, popups and menus", &mut p.window);
        opaque(ui, "Faint", "Every other row of lists and settings, and subtle highlights", &mut p.faint);
        opaque(ui, "Deep", "Text fields, section headings and the deepest backgrounds", &mut p.deep);
        opaque(ui, "Borders", "Separators, window and widget borders", &mut p.border);
        color_row(ui, "Shadows", "Under windows and popups: its opacity is how dark", &mut p.shadow, Alpha::OnlyBlend);
    });
    settings_group("Text").icon("text-aa").show(ui, |ui| {
        opaque(ui, "Text", "Labels and other text", &mut p.text);
        opaque(ui, "Widget text", "Text on buttons and in fields", &mut p.widget_text);
        opaque(ui, "Strong text", "Text being hovered or pressed, and headings", &mut p.strong_text);
        opaque(ui, "Weak text", "Secondary text: hints, times, counts", &mut p.weak_text);
        opaque(ui, "Links", "Links", &mut p.link);
    });
    settings_group("Controls").icon("cursor-click").hint("Buttons, fields, sliders and what's picked").show(ui, |ui| {
        opaque(ui, "Widgets", "Buttons, fields and the like", &mut p.widget);
        opaque(ui, "Hovered", "A widget under the pointer", &mut p.widget_hovered);
        opaque(ui, "Pressed", "A widget being pressed", &mut p.widget_pressed);
        opaque(ui, "Outline", "Around a widget being hovered or pressed", &mut p.widget_outline);
        opaque(ui, "Selection", "Selected items, toggled buttons and selected text", &mut p.selection);
        opaque(ui, "Accent", "Focus outlines, the text cursor, checkmarks and slider fills", &mut p.accent);
    });
    settings_group("Status").icon("info").hint("Messages, the log, and marks on things").show(ui, |ui| {
        opaque(ui, "Success", "Something done: \"Saved ...\" in the status line, lines added, \"Listening\"", &mut p.success);
        opaque(ui, "Warnings", "Warnings, in messages, the log and the script editor", &mut p.warning);
        opaque(ui, "Errors", "Something failed, in messages, the log and the script editor; the record button", &mut p.error);
        opaque(ui, "Official", "synththing's own things in the online library", &mut p.official);
    });
    settings_group("Lists").icon("list-bullets").show(ui, |ui| {
        opaque(ui, "Playing", "Outlining and tinting the song playing, and the pause button", &mut p.playing);
        opaque(ui, "Star", "The mark on starred songs, unless Preferences > Library > Tags picks another color", &mut p.star);
    });
    settings_group("Code").icon("code").hint("The script editor, and inline code").show(ui, |ui| {
        opaque(ui, "Code background", "Behind inline code", &mut p.code);
        opaque(ui, "Keywords", "Lua's own words: local, function, if, end...", &mut p.syntax.keyword);
        opaque(ui, "Functions", "synththing's functions for scripts", &mut p.syntax.function);
        opaque(ui, "Strings", "Text in quotes", &mut p.syntax.string);
        opaque(ui, "Numbers", "Numbers", &mut p.syntax.number);
        opaque(ui, "Comments", "Comments: -- ...", &mut p.syntax.comment);
    });
    let s = &mut theme.shape;
    settings_group("Shape").icon("bounding-box").show(ui, |ui| {
        let slider = |ui: &mut egui::Ui, label: &str, info: &str, slider: egui::Slider| {
            super::with_info(ui, info, |ui| {
                let label = super::setting_label(ui, label);
                label | super::setting_slider(ui, slider, 0.0)
            });
        };
        slider(ui, "Widget corners", "Corner radius of buttons, fields and the like", egui::Slider::new(&mut s.widget_radius, 0..=16).suffix(" px"));
        slider(ui, "Window corners", "Corner radius of windows, popups and menus", egui::Slider::new(&mut s.window_radius, 0..=24).suffix(" px"));
        slider(ui, "Borders", "How thick borders are", egui::Slider::new(&mut s.border_width, 0.0..=3.0).step_by(0.5).suffix(" px"));
        slider(ui, "Shadows", "How far window and popup shadows spread; 0 keeps egui's own", egui::Slider::new(&mut s.shadow, 0..=40).suffix(" px"));
    });
}

/// A bit of everything a theme colors, to see it on.
fn theme_preview_ui(ui: &mut egui::Ui) {
    let palette = theme::palette(ui.ctx());
    super::settings_group("Preview").icon("eye").show(ui, |ui| {
        super::setting_row(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let _ = ui.button("Button");
                let _ = ui.selectable_label(true, "Selected");
                let mut on = true;
                ui.checkbox(&mut on, "Checkbox");
                let mut value = 0.6_f32;
                ui.add(egui::Slider::new(&mut value, 0.0..=1.0).show_value(false));
                let mut text = String::from("A text field");
                ui.add(egui::TextEdit::singleline(&mut text).desired_width(110.0));
            });
        });
        super::setting_row(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Text,");
                ui.weak("weak,");
                ui.strong("strong,");
                ui.code("code");
                ui.hyperlink_to("a link", "https://github.com/Dissssy/synththing");
            });
        });
        super::setting_row(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(palette.success.0, "Saved.");
                ui.colored_label(palette.warning.0, "A warning");
                ui.colored_label(palette.error.0, "Couldn't do it");
                ui.colored_label(palette.official.0, "synththing");
                let star = crate::select_list::star_icon(ui.ctx());
                ui.colored_label(crate::select_list::star_color(ui.ctx()), super::icon(star));
                let playing = egui::RichText::new(format!("{}  Playing", super::icon("speaker-high"))).color(palette.playing.0);
                egui::Frame::new()
                    .fill(palette.playing.0.gamma_multiply(0.15))
                    .stroke(egui::Stroke::new(1.0, palette.playing.0))
                    .corner_radius(ui.visuals().widgets.inactive.corner_radius)
                    .inner_margin(egui::Margin::symmetric(6, 1))
                    .show(ui, |ui| ui.label(playing));
            });
        });
        super::setting_row(ui, |ui| {
            let sample = "local t = 0.5 -- fade\nif t > 1 then clear(\"done\") end";
            let galley = crate::lua_highlight::layout(ui, sample, f32::INFINITY, None, 13.0, false);
            ui.label(galley);
        });
    });
}
