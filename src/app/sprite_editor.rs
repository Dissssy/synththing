//! The Sprite Editor tab: draws a sprite of the open script on a pixel
//! grid and writes it back into the code (`sprite_code.rs`).
//!
//! Pick a sprite (the script's `sprite_register` calls), then draw: left
//! button paints with the selected color, right button erases, Alt+click
//! picks a color. Tools: pencil (B), fill (G), picker (I). Ctrl+Z / Ctrl+Y
//! undo and redo. Each stroke is written to the script when the button is
//! let go, and reaches the running script like any other edit.

use eframe::egui;

use super::App;
use crate::layout::Section;
use crate::sprite_code::{self, Color, Sheet, SpriteCall};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tool {
    #[default]
    Pencil,
    Fill,
    Picker,
}

/// The sprite as the editor works on it.
#[derive(Clone, PartialEq)]
struct Pixels {
    rows: Vec<Vec<u32>>,
    colors: Vec<Color>,
    sheet: Option<Sheet>,
}

impl Pixels {
    fn from_call(call: &SpriteCall) -> Option<Self> {
        let mut rows = call.image.as_ref()?.value.clone();
        let colors = call.palette.as_ref()?.value.clone();
        // Rows can be different lengths in the code; the editor works on a
        // rectangle (short rows were transparent past their end anyway).
        let width = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
        for row in &mut rows {
            row.resize(width, 0);
        }
        if rows.is_empty() {
            rows.push(vec![0; width]);
        }
        // A sheet comment that doesn't fit the image is ignored.
        let sheet = call.sheet.filter(|s| s.width * s.count == width && s.height == rows.len());
        Some(Self { rows, colors, sheet })
    }

    fn width(&self) -> usize {
        self.rows.first().map_or(0, Vec::len)
    }

    fn height(&self) -> usize {
        self.rows.len()
    }

    fn frames(&self) -> usize {
        self.sheet.map_or(1, |s| s.count)
    }

    fn frame_width(&self) -> usize {
        self.sheet.map_or(self.width(), |s| s.width)
    }

    /// Resize to `frames` frames of `width` x `height`, keeping each
    /// frame's top-left corner.
    fn resize(&mut self, width: usize, height: usize, frames: usize) {
        let old_frames = self.frames();
        let old_width = self.frame_width();
        let mut rows = vec![vec![0; width * frames]; height];
        for (y, row) in rows.iter_mut().enumerate() {
            let Some(old) = self.rows.get(y) else { break };
            for frame in 0..frames.min(old_frames) {
                for x in 0..width.min(old_width) {
                    row[frame * width + x] = old[frame * old_width + x];
                }
            }
        }
        self.rows = rows;
        self.sheet = (frames > 1).then_some(Sheet { count: frames, width, height });
    }

    fn fill(&mut self, x: usize, y: usize, color: u32) {
        let target = self.rows[y][x];
        if target == color {
            return;
        }
        let mut stack = vec![(x, y)];
        while let Some((x, y)) = stack.pop() {
            if self.rows[y][x] != target {
                continue;
            }
            self.rows[y][x] = color;
            if x > 0 {
                stack.push((x - 1, y));
            }
            if y > 0 {
                stack.push((x, y - 1));
            }
            if x + 1 < self.width() {
                stack.push((x + 1, y));
            }
            if y + 1 < self.height() {
                stack.push((x, y + 1));
            }
        }
    }

    /// Remove palette entry `index` (1-based): its pixels go transparent,
    /// later indices move down.
    fn remove_color(&mut self, index: u32) {
        if index == 0 || index as usize > self.colors.len() {
            return;
        }
        self.colors.remove(index as usize - 1);
        for row in &mut self.rows {
            for cell in row.iter_mut() {
                if *cell == index {
                    *cell = 0;
                } else if *cell > index {
                    *cell -= 1;
                }
            }
        }
    }

    fn color32(&self, index: u32) -> Option<egui::Color32> {
        let color = self.colors.get((index as usize).checked_sub(1)?)?;
        let alpha = color.alpha.unwrap_or(1.0);
        Some(egui::Color32::from_rgba_unmultiplied(color.rgb[0], color.rgb[1], color.rgb[2], (alpha * 255.0).round() as u8))
    }
}

pub struct NewSprite {
    name: String,
    width: usize,
    height: usize,
    frames: usize,
}

pub struct SpriteEditorState {
    /// Which of the script's sprites (index into `find_sprites`).
    pub selected: usize,
    tool: Tool,
    /// The palette index painted with (0 = transparent).
    color: u32,
    /// Size of one sprite pixel on screen; `None` fits the space.
    zoom: Option<f32>,
    grid: bool,
    undo: Vec<Pixels>,
    redo: Vec<Pixels>,
    /// The sprite being drawn on, while a button is held.
    stroke: Option<Pixels>,
    last_cell: Option<(usize, usize)>,
    preview_fps: f32,
    /// The New sprite dialog.
    pub new_sprite: Option<NewSprite>,
}

impl Default for SpriteEditorState {
    fn default() -> Self {
        Self {
            selected: 0,
            tool: Tool::Pencil,
            color: 1,
            zoom: None,
            grid: true,
            undo: Vec::new(),
            redo: Vec::new(),
            stroke: None,
            last_cell: None,
            preview_fps: 8.0,
            new_sprite: None,
        }
    }
}

const MAX_SIZE: usize = 256;
const MAX_FRAMES: usize = 64;

/// Two grays, for showing transparency.
fn checker(x: usize, y: usize) -> egui::Color32 {
    if (x + y).is_multiple_of(2) { egui::Color32::from_gray(70) } else { egui::Color32::from_gray(95) }
}

/// The cells of a line from `a` to `b` (Bresenham), so a fast drag leaves
/// no gaps.
fn line_cells(a: (usize, usize), b: (usize, usize)) -> Vec<(usize, usize)> {
    let (mut x, mut y) = (a.0 as i64, a.1 as i64);
    let (x1, y1) = (b.0 as i64, b.1 as i64);
    let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
    let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
    let mut err = dx + dy;
    let mut cells = vec![(x as usize, y as usize)];
    while (x, y) != (x1, y1) {
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
        cells.push((x as usize, y as usize));
    }
    cells
}

impl App {
    /// Open the Sprite Editor on the sprite whose `sprite_register` is at
    /// or around char `index` of the script (the nearest one before it).
    pub(super) fn edit_sprite_at(&mut self, index: usize) {
        let sprites = sprite_code::find_sprites(&self.editor_text);
        if let Some(n) = sprites.iter().rposition(|s| s.at <= index + "sprite_register".len()) {
            self.select_sprite(n);
        }
        self.reveal_section(Section::Sprites);
    }

    fn select_sprite(&mut self, n: usize) {
        if self.sprite.selected != n {
            self.sprite.selected = n;
            self.sprite.undo.clear();
            self.sprite.redo.clear();
            self.sprite.stroke = None;
        }
    }

    /// Write `pixels` over sprite `call` in the script.
    fn write_sprite(&mut self, call: &SpriteCall, pixels: &Pixels) {
        if sprite_code::write_sprite(&mut self.editor_text, call, &pixels.rows, &pixels.colors, pixels.sheet) {
            self.editor_changed();
        }
    }

    /// An edit to the selected sprite: remembered for undo, then written.
    fn commit_sprite(&mut self, call: &SpriteCall, before: Pixels, after: Pixels) {
        if before == after {
            return;
        }
        self.sprite.undo.push(before);
        if self.sprite.undo.len() > 200 {
            self.sprite.undo.remove(0);
        }
        self.sprite.redo.clear();
        self.write_sprite(call, &after);
    }

    pub(super) fn sprite_editor_ui(&mut self, ui: &mut egui::Ui) {
        let sprites = sprite_code::find_sprites(&self.editor_text);
        ui.horizontal(|ui| {
            if sprites.is_empty() {
                ui.label("This script has no sprites yet.");
            } else {
                let selected = self.sprite.selected.min(sprites.len() - 1);
                let mut pick = selected;
                egui::ComboBox::from_id_salt("sprite_pick").selected_text(sprites[selected].label()).show_ui(ui, |ui| {
                    for (n, sprite) in sprites.iter().enumerate() {
                        ui.selectable_value(&mut pick, n, sprite.label());
                    }
                });
                if pick != self.sprite.selected {
                    self.select_sprite(pick);
                }
            }
            if ui.button("New sprite...").on_hover_text("Add a blank sprite to the script").clicked() {
                self.open_new_sprite_dialog();
            }
        });
        let Some(call) = sprites.get(self.sprite.selected.min(sprites.len().saturating_sub(1))).cloned() else {
            ui.weak("New sprite... adds one, or write a sprite_register call in the Script Editor.");
            return;
        };
        let Some(saved) = call.editable().then(|| Pixels::from_call(&call)).flatten() else {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{} can't be edited here: {}.",
                    call.label(),
                    call.problem.clone().unwrap_or_else(|| "it isn't written out in the code".into())
                ),
            );
            ui.weak("The Sprite Editor edits sprites whose image and palette are tables written in the script.");
            return;
        };
        let mut pixels = self.sprite.stroke.clone().unwrap_or_else(|| saved.clone());

        self.sprite_keys(ui, &call, &saved);
        self.sprite_toolbar_ui(ui, &call, &saved);
        let size_changed = self.sprite_size_ui(ui, &mut pixels);
        let palette_changed = self.sprite_palette_ui(ui, &mut pixels);
        if (size_changed || palette_changed) && self.sprite.stroke.is_none() {
            self.commit_sprite(&call, saved.clone(), pixels.clone());
        }
        ui.separator();

        // The canvas, and the previews beside it.
        let preview_width = 150.0;
        let available = ui.available_size() - egui::vec2(preview_width + 16.0, 0.0);
        let (w, h) = (pixels.width(), pixels.height());
        let fit = (available.x / w as f32).min(available.y / h as f32).floor().clamp(1.0, 64.0);
        let cell = self.sprite.zoom.unwrap_or(fit);
        ui.horizontal_top(|ui| {
            egui::ScrollArea::both().id_salt("sprite_canvas").max_width(available.x).max_height(available.y).show(ui, |ui| {
                self.sprite_canvas_ui(ui, &call, &saved, &mut pixels, cell);
            });
            ui.vertical(|ui| {
                ui.set_width(preview_width);
                sprite_previews_ui(ui, &pixels, &mut self.sprite.preview_fps, preview_width);
            });
        });
    }

    fn sprite_keys(&mut self, ui: &egui::Ui, call: &SpriteCall, saved: &Pixels) {
        if !ui.ui_contains_pointer() || ui.ctx().memory(|m| m.focused().is_some()) {
            return;
        }
        let command = egui::Modifiers::COMMAND;
        let (undo, redo, pencil, fill, picker) = ui.ctx().input_mut(|i| {
            (
                i.consume_key(command, egui::Key::Z),
                i.consume_key(command, egui::Key::Y) || i.consume_key(command | egui::Modifiers::SHIFT, egui::Key::Z),
                i.consume_key(egui::Modifiers::NONE, egui::Key::B),
                i.consume_key(egui::Modifiers::NONE, egui::Key::G),
                i.consume_key(egui::Modifiers::NONE, egui::Key::I),
            )
        });
        if undo {
            self.sprite_undo(call, saved, true);
        }
        if redo {
            self.sprite_undo(call, saved, false);
        }
        if pencil {
            self.sprite.tool = Tool::Pencil;
        }
        if fill {
            self.sprite.tool = Tool::Fill;
        }
        if picker {
            self.sprite.tool = Tool::Picker;
        }
    }

    fn sprite_undo(&mut self, call: &SpriteCall, saved: &Pixels, undo: bool) {
        let (from, to) = if undo { (&mut self.sprite.undo, &mut self.sprite.redo) } else { (&mut self.sprite.redo, &mut self.sprite.undo) };
        if let Some(state) = from.pop() {
            to.push(saved.clone());
            self.write_sprite(call, &state);
        }
    }

    fn sprite_toolbar_ui(&mut self, ui: &mut egui::Ui, call: &SpriteCall, saved: &Pixels) {
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.sprite.tool, Tool::Pencil, "Pencil").on_hover_text("B; right button erases");
            ui.selectable_value(&mut self.sprite.tool, Tool::Fill, "Fill").on_hover_text("G: fill the touching area");
            ui.selectable_value(&mut self.sprite.tool, Tool::Picker, "Pick").on_hover_text("I: pick a pixel's color (or Alt+click)");
            ui.separator();
            if ui.add_enabled(!self.sprite.undo.is_empty(), egui::Button::new("Undo")).on_hover_text("Ctrl+Z").clicked() {
                self.sprite_undo(call, saved, true);
            }
            if ui.add_enabled(!self.sprite.redo.is_empty(), egui::Button::new("Redo")).on_hover_text("Ctrl+Y").clicked() {
                self.sprite_undo(call, saved, false);
            }
            ui.separator();
            ui.checkbox(&mut self.sprite.grid, "Grid");
            let mut fit = self.sprite.zoom.is_none();
            if ui.checkbox(&mut fit, "Fit").on_hover_text("Size the pixels to fit the space").changed() {
                self.sprite.zoom = if fit { None } else { Some(16.0) };
            }
            if let Some(zoom) = &mut self.sprite.zoom {
                ui.add(egui::Slider::new(zoom, 2.0..=64.0).suffix(" px").logarithmic(true));
            }
        });
    }

    /// Frame size and count. Returns whether they changed.
    fn sprite_size_ui(&mut self, ui: &mut egui::Ui, pixels: &mut Pixels) -> bool {
        let (mut width, mut height, mut frames) = (pixels.frame_width(), pixels.height(), pixels.frames());
        ui.horizontal(|ui| {
            ui.label(if frames > 1 { "Frame size" } else { "Size" });
            ui.add(egui::DragValue::new(&mut width).range(1..=MAX_SIZE).speed(0.1));
            ui.label("x");
            ui.add(egui::DragValue::new(&mut height).range(1..=MAX_SIZE).speed(0.1));
            ui.separator();
            ui.label("Frames");
            ui.add(egui::DragValue::new(&mut frames).range(1..=MAX_FRAMES).speed(0.05)).on_hover_text(
                "More than one makes it a sprite sheet: frames side by side, recorded in a comment above \\
                 the sprite_register call. Draw one with sprite(id, x, y, { src = {frame * width, 0, width, height} }).",
            );
        });
        if (width, height, frames) != (pixels.frame_width(), pixels.height(), pixels.frames()) {
            pixels.resize(width, height, frames);
            true
        } else {
            false
        }
    }

    /// The palette: transparent, then each color; click to choose. Returns
    /// whether the palette changed.
    fn sprite_palette_ui(&mut self, ui: &mut egui::Ui, pixels: &mut Pixels) -> bool {
        let mut changed = false;
        self.sprite.color = self.sprite.color.min(pixels.colors.len() as u32);
        ui.horizontal_wrapped(|ui| {
            ui.label("Palette");
            for index in 0..=pixels.colors.len() as u32 {
                let size = egui::vec2(22.0, 22.0);
                let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                let painter = ui.painter();
                match pixels.color32(index) {
                    Some(color) => {
                        if color.a() < 255 {
                            painter.rect_filled(rect, 3.0, checker(0, 1));
                        }
                        painter.rect_filled(rect, 3.0, color);
                    }
                    None => {
                        let half = rect.width() / 2.0;
                        for (n, quarter) in [(0.0, 0.0), (half, 0.0), (0.0, half), (half, half)].into_iter().enumerate() {
                            let r = egui::Rect::from_min_size(rect.min + egui::vec2(quarter.0, quarter.1), egui::vec2(half, half));
                            painter.rect_filled(r, 0.0, checker(n % 2, n / 2));
                        }
                    }
                }
                let selected = index == self.sprite.color;
                let stroke = if selected {
                    egui::Stroke::new(2.0, ui.visuals().selection.stroke.color)
                } else {
                    egui::Stroke::new(1.0, egui::Color32::from_gray(110))
                };
                painter.rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Inside);
                let tip = if index == 0 { "0: transparent (and the right button)".to_string() } else { format!("{index}") };
                if response.on_hover_text(tip).clicked() {
                    self.sprite.color = index;
                }
            }
            if ui.small_button("+").on_hover_text("Add a color").clicked() {
                let copy = pixels.colors.get((self.sprite.color as usize).wrapping_sub(1)).copied();
                pixels.colors.push(copy.unwrap_or(Color { rgb: [255, 255, 255], alpha: None }));
                self.sprite.color = pixels.colors.len() as u32;
                changed = true;
            }
            let index = self.sprite.color;
            if index > 0 && (index as usize) <= pixels.colors.len() {
                let entry = pixels.colors[index as usize - 1];
                let alpha = entry.alpha.unwrap_or(1.0);
                let mut color = egui::Color32::from_rgba_unmultiplied(entry.rgb[0], entry.rgb[1], entry.rgb[2], (alpha * 255.0).round() as u8);
                ui.separator();
                ui.label(format!("Color {index}:"));
                if egui::color_picker::color_edit_button_srgba(ui, &mut color, egui::color_picker::Alpha::OnlyBlend).changed() {
                    let [r, g, b, a] = color.to_srgba_unmultiplied();
                    let alpha = (a < 255 || entry.alpha.is_some()).then_some(f32::from(a) / 255.0);
                    pixels.colors[index as usize - 1] = Color { rgb: [r, g, b], alpha };
                    changed = true;
                }
                if ui.small_button("Remove").on_hover_text("Its pixels become transparent").clicked() {
                    pixels.remove_color(index);
                    self.sprite.color = index - 1;
                    changed = true;
                }
            }
        });
        changed
    }

    fn sprite_canvas_ui(&mut self, ui: &mut egui::Ui, call: &SpriteCall, saved: &Pixels, pixels: &mut Pixels, cell: f32) {
        let (w, h) = (pixels.width(), pixels.height());
        let size = egui::vec2(w as f32 * cell, h as f32 * cell);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        for (y, row) in pixels.rows.iter().enumerate() {
            for (x, &index) in row.iter().enumerate() {
                let r = egui::Rect::from_min_size(rect.min + egui::vec2(x as f32 * cell, y as f32 * cell), egui::vec2(cell, cell));
                match pixels.color32(index) {
                    Some(color) if color.a() == 255 => {
                        painter.rect_filled(r, 0.0, color);
                    }
                    Some(color) => {
                        painter.rect_filled(r, 0.0, checker(x, y));
                        painter.rect_filled(r, 0.0, color);
                    }
                    None => {
                        painter.rect_filled(r, 0.0, checker(x, y));
                    }
                }
            }
        }
        let line = |from: egui::Pos2, to: egui::Pos2, stroke: egui::Stroke| painter.line_segment([from, to], stroke);
        if self.sprite.grid && cell >= 5.0 {
            let stroke = egui::Stroke::new(1.0, egui::Color32::from_black_alpha(70));
            for x in 1..w {
                let px = rect.left() + x as f32 * cell;
                line(egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom()), stroke);
            }
            for y in 1..h {
                let py = rect.top() + y as f32 * cell;
                line(egui::pos2(rect.left(), py), egui::pos2(rect.right(), py), stroke);
            }
        }
        let frame_stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);
        for frame in 1..pixels.frames() {
            let px = rect.left() + (frame * pixels.frame_width()) as f32 * cell;
            line(egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom()), frame_stroke);
        }
        painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, egui::Color32::from_gray(120)), egui::StrokeKind::Outside);

        let cell_at = |pos: egui::Pos2| {
            let local = pos - rect.min;
            let (x, y) = ((local.x / cell).floor(), (local.y / cell).floor());
            (x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h).then_some((x as usize, y as usize))
        };
        if let Some(hover) = response.hover_pos().and_then(cell_at) {
            let r = egui::Rect::from_min_size(rect.min + egui::vec2(hover.0 as f32 * cell, hover.1 as f32 * cell), egui::vec2(cell, cell));
            painter.rect_stroke(r, 0.0, egui::Stroke::new(1.5, egui::Color32::WHITE), egui::StrokeKind::Inside);
            response.clone().on_hover_text_at_pointer(format!("{}, {}: color {}", hover.0, hover.1, pixels.rows[hover.1][hover.0]));
        }

        // Drawing.
        let (primary, secondary, alt) = ui.input(|i| (i.pointer.primary_down(), i.pointer.secondary_down(), i.modifiers.alt));
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let pressed_here = response.is_pointer_button_down_on();
        if pressed_here && let Some(at) = pointer.and_then(cell_at) {
            let tool = if alt { Tool::Picker } else { self.sprite.tool };
            match tool {
                Tool::Picker => {
                    self.sprite.color = pixels.rows[at.1][at.0];
                }
                Tool::Fill => {
                    if self.sprite.stroke.is_none() {
                        let color = if secondary { 0 } else { self.sprite.color };
                        pixels.fill(at.0, at.1, color);
                        self.sprite.stroke = Some(pixels.clone());
                    }
                }
                Tool::Pencil => {
                    let color = if secondary && !primary { 0 } else { self.sprite.color };
                    let from = self.sprite.last_cell.unwrap_or(at);
                    for (x, y) in line_cells(from, at) {
                        pixels.rows[y][x] = color;
                    }
                    self.sprite.last_cell = Some(at);
                    self.sprite.stroke = Some(pixels.clone());
                }
            }
        }
        // Let go: the stroke goes into the script.
        if !pressed_here && let Some(done) = self.sprite.stroke.take() {
            self.sprite.last_cell = None;
            self.commit_sprite(call, saved.clone(), done);
        }
        if !pressed_here {
            self.sprite.last_cell = None;
        }
    }

    /// Ask for a new sprite's name, size and frames (Sprite Editor > New
    /// sprite..., or the code editor's Insert > Sprite...).
    pub(super) fn open_new_sprite_dialog(&mut self) {
        // A name the script doesn't use yet: sprite, sprite2, ...
        let mut name = "sprite".to_string();
        let mut n = 2;
        while self.editor_text.contains(&format!("local {name} ")) {
            name = format!("sprite{n}");
            n += 1;
        }
        self.sprite.new_sprite = Some(NewSprite { name, width: 8, height: 8, frames: 1 });
    }

    /// New sprite...: name, size and frames, then a blank sprite is added
    /// to the script above `function render` and opened here.
    pub(super) fn new_sprite_ui(&mut self, ctx: &egui::Context) {
        let Some(new) = &mut self.sprite.new_sprite else { return };
        let valid_name = new.name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
            && new.name.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !crate::lua_analysis::KEYWORDS.contains(&new.name.as_str());
        let taken = self.editor_text.contains(&format!("local {} ", new.name))
            || self.editor_text.contains(&format!("local {}=", new.name));
        let mut create = false;
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("new_sprite")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading("New sprite");
            egui::Grid::new("new_sprite_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut new.name);
                ui.end_row();
                ui.label(if new.frames > 1 { "Frame size" } else { "Size" });
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut new.width).range(1..=MAX_SIZE).speed(0.1));
                    ui.label("x");
                    ui.add(egui::DragValue::new(&mut new.height).range(1..=MAX_SIZE).speed(0.1));
                });
                ui.end_row();
                ui.label("Frames");
                ui.add(egui::DragValue::new(&mut new.frames).range(1..=MAX_FRAMES).speed(0.05));
                ui.end_row();
            });
            if !valid_name {
                ui.colored_label(ui.visuals().warn_fg_color, "The name has to be a Lua name: letters, digits and _.");
            } else if taken {
                ui.colored_label(ui.visuals().warn_fg_color, "The script already has a local with that name.");
            }
            ui.weak("It's added above function render, at the top level, so it's registered once.");
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.add_enabled(valid_name && !taken, egui::Button::new("Add sprite")).clicked() {
                    create = true;
                }
            });
        });
        if create {
            let code = sprite_code::new_sprite_text(&new.name, new.width, new.height, new.frames);
            let name = new.name.clone();
            // Above `function render`, else at the end.
            let at = self
                .editor_text
                .find("\nfunction render")
                .map(|b| self.editor_text[..b + 1].chars().count())
                .or_else(|| self.editor_text.starts_with("function render").then_some(0))
                .unwrap_or_else(|| self.editor_text.chars().count());
            let code = if at > 0 && !self.editor_text.chars().nth(at - 1).is_some_and(|c| c == '\n') { format!("\n{code}") } else { code };
            crate::code_edit::replace_range(&mut self.editor_text, (at, at), &format!("{code}\n"));
            self.editor_changed();
            let sprites = sprite_code::find_sprites(&self.editor_text);
            if let Some(n) = sprites.iter().position(|s| s.name.as_deref() == Some(name.as_str())) {
                self.select_sprite(n);
            }
            self.sprite.color = 1;
            self.status = format!("Added the sprite `{name}` to the script.");
            self.reveal_section(Section::Sprites);
            close = true;
        }
        if close || response.should_close() {
            self.sprite.new_sprite = None;
        }
    }
}

/// The whole sprite small, and for a sheet, its frames playing.
fn sprite_previews_ui(ui: &mut egui::Ui, pixels: &Pixels, fps: &mut f32, width: f32) {
    let draw = |ui: &mut egui::Ui, frame: Option<usize>, max: f32| {
        let (x0, w) = match frame {
            Some(f) => (f * pixels.frame_width(), pixels.frame_width()),
            None => (0, pixels.width()),
        };
        let h = pixels.height();
        let scale = (max / w.max(h) as f32).floor().clamp(1.0, 8.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w as f32 * scale, h as f32 * scale), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(40));
        for y in 0..h {
            for x in 0..w {
                if let Some(color) = pixels.color32(pixels.rows[y][x0 + x]) {
                    let r = egui::Rect::from_min_size(rect.min + egui::vec2(x as f32 * scale, y as f32 * scale), egui::vec2(scale, scale));
                    painter.rect_filled(r, 0.0, color);
                }
            }
        }
    };
    ui.weak("Preview");
    draw(ui, None, width);
    if pixels.frames() > 1 {
        ui.add_space(8.0);
        ui.weak("Animation");
        let time = ui.input(|i| i.time);
        let frame = (time * f64::from(*fps)) as usize % pixels.frames();
        draw(ui, Some(frame), width);
        ui.add(egui::Slider::new(fps, 1.0..=30.0).suffix(" fps"));
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(rows: Vec<Vec<u32>>) -> Pixels {
        Pixels { rows, colors: vec![Color { rgb: [1, 1, 1], alpha: None }, Color { rgb: [2, 2, 2], alpha: None }], sheet: None }
    }

    #[test]
    fn fill_stops_at_other_colors() {
        let mut p = pixels(vec![vec![0, 0, 1], vec![0, 1, 0], vec![1, 0, 0]]);
        p.fill(0, 0, 2);
        assert_eq!(p.rows, [vec![2, 2, 1], vec![2, 1, 0], vec![1, 0, 0]]);
    }

    #[test]
    fn resizing_keeps_each_frame() {
        let mut p = pixels(vec![vec![1, 1, 2, 2]]);
        p.sheet = Some(Sheet { count: 2, width: 2, height: 1 });
        p.resize(3, 2, 3);
        assert_eq!(p.rows, [vec![1, 1, 0, 2, 2, 0, 0, 0, 0], vec![0; 9]]);
        assert_eq!(p.sheet, Some(Sheet { count: 3, width: 3, height: 2 }));
        p.resize(3, 2, 1);
        assert_eq!(p.sheet, None);
        assert_eq!(p.rows, [vec![1, 1, 0], vec![0, 0, 0]]);
    }

    #[test]
    fn removing_a_color_shifts_the_rest() {
        let mut p = pixels(vec![vec![1, 2, 0]]);
        p.remove_color(1);
        assert_eq!(p.rows, [vec![0, 1, 0]]);
        assert_eq!(p.colors.len(), 1);
    }

    #[test]
    fn lines_have_no_gaps() {
        assert_eq!(line_cells((0, 0), (3, 1)), [(0, 0), (1, 0), (2, 1), (3, 1)]);
    }
}
