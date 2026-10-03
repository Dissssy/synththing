//! The script editor: a syntax-highlighted `TextEdit` with line numbers,
//! smart editing keys (`code_edit.rs`), find and replace, a choice of when
//! edits reach the running script, local history, zoom and wrap.
//!
//! Keys, while the editor has focus: Enter auto-indents, Tab / Shift+Tab
//! indent and outdent (a selection: every line in it), Ctrl+/ toggles
//! comments, brackets and quotes close themselves, `end` lines itself up
//! with its block. Ctrl+S applies (and saves) now, Ctrl+F finds, Ctrl+H
//! replaces, Ctrl+scroll zooms.
//!
//! Understanding the code (`lua_analysis`, run in the background on every
//! edit): problems underlined (and listed), completions as you type
//! (Ctrl+Space to ask), signature help inside a call, an outline to jump
//! between functions, F12 / Ctrl+click to go to a name's definition, hover
//! for your own names, and Shift+Alt+F to format the script.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use serde::{Deserialize, Serialize};

use super::{char_index_of_line, identifier_at, App, SCRIPT_EDITOR_ID};
use crate::code_edit::{self, Selection};
use crate::lua_analysis::{self, Analysis, Item, Severity};
use crate::lua_completion;
use crate::lua_highlight;
use crate::lua_visualizer;

/// When edits reach the running script (and its file).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplyMode {
    /// Every keystroke, as soon as it's typed.
    #[serde(rename = "every_edit")]
    EveryEdit,
    /// Once typing pauses for `PAUSE`.
    #[default]
    #[serde(rename = "pause")]
    Pause,
    /// Only on Ctrl+S.
    #[serde(rename = "save")]
    OnSave,
}

impl ApplyMode {
    const ALL: [ApplyMode; 3] = [ApplyMode::EveryEdit, ApplyMode::Pause, ApplyMode::OnSave];

    fn label(self) -> &'static str {
        match self {
            ApplyMode::EveryEdit => "Every edit",
            ApplyMode::Pause => "When typing pauses",
            ApplyMode::OnSave => "On Ctrl+S",
        }
    }
}

/// How long typing has to pause before `ApplyMode::Pause` applies.
const PAUSE: Duration = Duration::from_millis(700);
/// At most one history snapshot this often while editing.
const SNAPSHOT_EVERY: Duration = Duration::from_secs(60);
/// Snapshots kept per script.
const HISTORY_KEPT: usize = 50;
pub const DEFAULT_FONT_SIZE: f32 = 13.0;
const FONT_SIZES: std::ops::RangeInclusive<f32> = 8.0..=32.0;

#[derive(Default)]
pub struct EditorState {
    /// Edits not applied to the running script yet, since when.
    dirty_since: Option<Instant>,
    find: Option<Find>,
    pub history_open: bool,
    history: Vec<Snapshot>,
    history_selected: usize,
    last_snapshot: Option<Instant>,
    /// Put the text cursor here and scroll to it (a find match, a line).
    scroll_to: Option<usize>,
    /// The completion list, while it's up.
    popup: Option<Popup>,
    /// Ctrl+Space: show completions on the next frame even with nothing
    /// typed.
    force_popup: bool,
    /// The color picker, while it's open for a color table.
    color_edit: Option<ColorEdit>,
}

struct ColorEdit {
    /// Char index of the table's `{`.
    start: usize,
    rgb: [u8; 3],
    alpha: f32,
    has_alpha: bool,
    /// Where the picker shows (under the swatch).
    at: egui::Pos2,
    /// Opened this frame: the click that opened it doesn't close it.
    fresh: bool,
}

struct Popup {
    items: Vec<Item>,
    selected: usize,
    /// The typed part being completed: chars `from..cursor`.
    from: usize,
    cursor: usize,
}

struct Find {
    query: String,
    replacement: String,
    show_replace: bool,
    case_sensitive: bool,
    /// Index into the current matches.
    current: usize,
    focus_query: bool,
}

struct Snapshot {
    path: PathBuf,
    /// Milliseconds since the Unix epoch, from the file name.
    millis: u128,
    text: String,
}

/// `<config dir>/script_history/<script file name>/`.
fn history_dir(script: &Path) -> Option<PathBuf> {
    let name = script.file_name()?;
    Some(crate::config::config_dir().ok()?.join("script_history").join(name))
}

fn now_millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or_default()
}

fn load_history(script: &Path) -> Vec<Snapshot> {
    let Some(dir) = history_dir(script) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut snapshots: Vec<Snapshot> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let millis = path.file_stem()?.to_str()?.parse().ok()?;
            let text = std::fs::read_to_string(&path).ok()?;
            Some(Snapshot { path, millis, text })
        })
        .collect();
    snapshots.sort_by_key(|s| std::cmp::Reverse(s.millis));
    snapshots
}

/// Save `text` as a new snapshot of `script` (unless it's the same as the
/// newest), keeping the newest `HISTORY_KEPT`.
fn snapshot(script: &Path, text: &str) {
    let Some(dir) = history_dir(script) else { return };
    let existing = load_history(script);
    if existing.first().is_some_and(|s| s.text == text) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(dir.join(format!("{}.lua", now_millis())), text))
    {
        log::warn!("couldn't save script history in {}: {e}", dir.display());
        return;
    }
    for old in existing.iter().skip(HISTORY_KEPT - 1) {
        let _ = std::fs::remove_file(&old.path);
    }
}

/// Moves a script's history along when it's renamed.
pub fn rename_history(old: &Path, new: &Path) {
    if let (Some(from), Some(to)) = (history_dir(old), history_dir(new))
        && from.is_dir()
        && let Err(e) = std::fs::rename(&from, &to)
    {
        log::warn!("couldn't move script history to {}: {e}", to.display());
    }
}

fn ago(millis: u128) -> String {
    let secs = (now_millis().saturating_sub(millis) / 1000) as u64;
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h {} min ago", secs / 3600, secs / 60 % 60),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// Lines in `a` but not `b`, and in `b` but not `a` (as multisets): a rough
/// "+added / -removed" against the current text.
fn line_changes(old: &str, new: &str) -> (usize, usize) {
    let mut counts = std::collections::HashMap::<&str, i64>::new();
    for line in old.lines() {
        *counts.entry(line).or_default() -= 1;
    }
    for line in new.lines() {
        *counts.entry(line).or_default() += 1;
    }
    let added = counts.values().filter(|&&n| n > 0).map(|&n| n as usize).sum();
    let removed = counts.values().filter(|&&n| n < 0).map(|&n| (-n) as usize).sum();
    (added, removed)
}

fn load_selection(ctx: &egui::Context, id: egui::Id, text: &str) -> Selection {
    let state = egui::widgets::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    let end = text.chars().count();
    match state.cursor.char_range() {
        Some(range) => (range.secondary.index.0.min(end), range.primary.index.0.min(end)),
        None => (end, end),
    }
}

fn store_selection(ctx: &egui::Context, id: egui::Id, (anchor, cursor): Selection) {
    let mut state = egui::widgets::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
        egui::text::CCursor::new(anchor),
        egui::text::CCursor::new(cursor),
    )));
    state.store(ctx, id);
}

/// The message part of a Lua error (`[string "visualizer"]:12: oops` ->
/// `oops`), shortened.
fn short_message(error: &str, line: usize) -> String {
    let marker = format!(":{line}:");
    let message = error.find(&marker).map_or(error, |i| &error[i + marker.len()..]).trim();
    let message = message.lines().next().unwrap_or(message);
    if message.chars().count() > 120 { format!("{}...", message.chars().take(117).collect::<String>()) } else { message.to_string() }
}

impl App {
    fn editor_font_size(&self) -> f32 {
        self.config.editor_font_size.unwrap_or(DEFAULT_FONT_SIZE).clamp(*FONT_SIZES.start(), *FONT_SIZES.end())
    }

    /// The editor's text changed (typed, or an edit made for it).
    pub(super) fn editor_changed(&mut self) {
        self.completion.request(self.editor_text.clone());
        match self.config.editor_apply {
            ApplyMode::EveryEdit => self.apply_editor(),
            _ => self.editor.dirty_since = Some(Instant::now()),
        }
    }

    /// Send the editor's text to the running script and its file now.
    pub(super) fn apply_editor(&mut self) {
        self.editor.dirty_since = None;
        self.editor_saved = self.editor_text.clone();
        let save_error =
            self.visualizer.script().path().and_then(|path| lua_visualizer::save_script(path, &self.editor_text));
        self.visualizer.script_mut().set_source(self.editor_text.clone(), save_error);
        let due = self.editor.last_snapshot.is_none_or(|t| t.elapsed() >= SNAPSHOT_EVERY);
        if due && let Some(path) = self.visualizer.script().path().map(Path::to_path_buf) {
            snapshot(&path, &self.editor_text);
            self.editor.last_snapshot = Some(Instant::now());
        }
    }

    /// Apply any edits still waiting (before switching scripts, renaming,
    /// closing).
    pub(super) fn flush_editor(&mut self) {
        if self.editor.dirty_since.is_some() {
            self.apply_editor();
        }
    }

    /// A script was opened in the editor: keep a snapshot of it as it was.
    pub(super) fn editor_opened(&mut self, path: &Path) {
        snapshot(path, &self.editor_text);
        self.editor.dirty_since = None;
        self.editor.last_snapshot = Some(Instant::now());
        self.editor_saved = self.editor_text.clone();
    }

    /// Per frame: apply once typing pauses.
    pub(super) fn editor_tick(&mut self, ctx: &egui::Context) {
        let Some(since) = self.editor.dirty_since else { return };
        if self.config.editor_apply == ApplyMode::Pause {
            let waited = since.elapsed();
            if waited >= PAUSE {
                self.apply_editor();
            } else {
                ctx.request_repaint_after(PAUSE - waited);
            }
        }
    }

    /// Whether there are edits the script hasn't got yet.
    pub(super) fn editor_dirty(&self) -> bool {
        self.editor.dirty_since.is_some()
    }

    /// Ctrl+S / F / H, and the editing keys while the editor has focus:
    /// handled before the `TextEdit` sees them. Returns whether the text
    /// changed.
    fn editor_keys(&mut self, ctx: &egui::Context, id: egui::Id) -> bool {
        let command = egui::Modifiers::COMMAND;
        let (save, find, replace) = ctx.input_mut(|i| {
            (
                i.consume_key(command, egui::Key::S),
                i.consume_key(command, egui::Key::F),
                i.consume_key(command, egui::Key::H),
            )
        });
        if save {
            self.apply_editor();
            self.status = "Script applied and saved.".to_string();
        }
        if find || replace {
            let find = self.editor.find.get_or_insert_with(|| Find {
                query: String::new(),
                replacement: String::new(),
                show_replace: false,
                case_sensitive: false,
                current: 0,
                focus_query: true,
            });
            find.focus_query = true;
            find.show_replace |= replace;
            // Start from the selected text, if there's a one-line selection.
            let (a, b) = load_selection(ctx, id, &self.editor_text);
            let (a, b) = (a.min(b), a.max(b));
            let selected: String = self.editor_text.chars().skip(a).take(b - a).collect();
            if !selected.is_empty() && !selected.contains('\n') {
                find.query = selected;
            }
        }

        if !ctx.memory(|m| m.has_focus(id)) {
            self.editor.popup = None;
            return false;
        }
        let mut sel = load_selection(ctx, id, &self.editor_text);
        let mut changed = false;

        // The completion list takes the arrows, Enter, Tab and Esc.
        if let Some(popup) = &mut self.editor.popup {
            let (down, up, accept, close) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                )
            });
            let count = popup.items.len().max(1);
            if down {
                popup.selected = (popup.selected + 1) % count;
            }
            if up {
                popup.selected = (popup.selected + count - 1) % count;
            }
            if close {
                self.editor.popup = None;
            } else if accept {
                let popup = self.editor.popup.take().expect("checked above");
                if let Some(item) = popup.items.get(popup.selected) {
                    let end = code_edit::replace_range(&mut self.editor_text, (popup.from, sel.1.max(popup.from)), &item.label);
                    store_selection(ctx, id, (end, end));
                    return true;
                }
            }
        }
        let (force, definition, format) = ctx.input_mut(|i| {
            (
                i.consume_key(command, egui::Key::Space),
                i.consume_key(egui::Modifiers::NONE, egui::Key::F12),
                i.consume_key(egui::Modifiers::SHIFT | egui::Modifiers::ALT, egui::Key::F),
            )
        });
        if force {
            self.editor.force_popup = true;
        }
        if definition {
            self.go_to_definition(ctx, id, sel.1);
        }
        if format {
            return self.format_script(ctx, id);
        }

        let text = &mut self.editor_text;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                sel = code_edit::newline(text, sel);
                changed = true;
            }
            if i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab) {
                sel = code_edit::outdent(text, sel);
                changed = true;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
                sel = code_edit::tab(text, sel);
                changed = true;
            }
            if i.consume_key(command, egui::Key::Slash) {
                sel = code_edit::toggle_comment(text, sel);
                changed = true;
            }
            if i.key_pressed(egui::Key::Backspace)
                && i.modifiers.is_none()
                && let Some(new) = code_edit::backspace_pair(text, sel)
            {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace);
                sel = new;
                changed = true;
            }
            // Brackets and quotes, as typed.
            let mut handled = Vec::new();
            for (n, event) in i.events.iter().enumerate() {
                if let egui::Event::Text(typed) = event {
                    let mut chars = typed.chars();
                    if let (Some(c), None) = (chars.next(), chars.next())
                        && let Some(new) = code_edit::type_char(text, sel, c)
                    {
                        sel = new;
                        changed = true;
                        handled.push(n);
                    }
                }
            }
            for n in handled.into_iter().rev() {
                i.events.remove(n);
            }
        });
        if changed {
            store_selection(ctx, id, sel);
        }
        changed
    }

    /// The script source editor: toolbar, find bar, and the text itself
    /// with its line numbers.
    pub(super) fn visualizer_editor_ui(&mut self, ui: &mut egui::Ui) {
        let editor_id = egui::Id::new(SCRIPT_EDITOR_ID);
        let ctx = ui.ctx().clone();

        if let Some((word, detail)) = self.editor_hover_info.clone() {
            ui.horizontal(|ui| {
                ui.strong(&word);
                ui.label(&detail);
                if crate::lua_docs::find(&word).is_some()
                    && ui.link("Reference").on_hover_text("Show it in the Scripting Reference").clicked()
                {
                    self.open_reference(&word);
                }
                if word == "sprite_register"
                    && ui.link("Sprite Editor").on_hover_text("Edit this sprite (or Ctrl+click it)").clicked()
                {
                    let (_, cursor) = load_selection(ui.ctx(), editor_id, &self.editor_text);
                    self.edit_sprite_at(cursor);
                }
            });
        } else {
            ui.weak("Hover a function, or press F1 at the text cursor, for info about it.");
        }
        self.editor_toolbar_ui(ui, editor_id);
        self.find_bar_ui(ui, editor_id);

        if self.editor_keys(&ctx, editor_id) {
            self.editor_changed();
        }

        // "Go to line" from the error banner.
        if let Some(line) = self.editor_goto_line.take() {
            self.editor.scroll_to = Some(char_index_of_line(&self.editor_text, line));
            let index = self.editor.scroll_to.unwrap_or_default();
            store_selection(&ctx, editor_id, (index, index));
            ctx.memory_mut(|m| m.request_focus(editor_id));
        }

        let wrap = self.config.editor_wrap.unwrap_or(true);
        let font_size = self.editor_font_size();
        let font = egui::FontId::monospace(font_size);
        let error = self.visualizer.script().error();
        let error_line = error.as_deref().and_then(lua_visualizer::error_line);
        let matches = match &self.editor.find {
            Some(find) => code_edit::find_all(&self.editor_text, &find.query, find.case_sensitive),
            None => Vec::new(),
        };
        let current_match = self.editor.find.as_ref().map(|f| f.current);
        let analysis = self.completion.analysis();
        // Lines with problems get a mark by their number.
        let mut marks = std::collections::HashMap::new();
        for d in &analysis.diagnostics {
            let color = severity_color(d.severity);
            let entry = marks.entry(d.line).or_insert(color);
            if d.severity == Severity::Error {
                *entry = color;
            }
        }

        let scroll = if wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
        scroll.id_salt("visualizer_editor_scroll").auto_shrink([false, false]).show(ui, |ui| {
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                let width = if wrap { wrap_width } else { f32::INFINITY };
                lua_highlight::layout(ui, buf.as_str(), width, error_line, font_size, true)
            };
            let digits = self.editor_text.lines().count().max(1).to_string().len().max(2);
            let char_width = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
            let gutter_width = char_width * digits as f32 + 14.0;
            let (output, gutter, behind) = ui
                .horizontal_top(|ui| {
                    let (gutter, _) = ui.allocate_exact_size(egui::vec2(gutter_width, 0.0), egui::Sense::hover());
                    // A spot for highlights under the text, filled in once
                    // its layout is known.
                    let behind = ui.painter().add(egui::Shape::Noop);
                    let output = egui::TextEdit::multiline(&mut self.editor_text)
                        .id(editor_id)
                        .code_editor()
                        .desired_width(if wrap { f32::INFINITY } else { 0.0 })
                        .layouter(&mut layouter)
                        .show(ui);
                    (output, gutter, behind)
                })
                .inner;
            paint_line_numbers(ui, &output, gutter, &font, error_line, &marks);

            // Highlights behind the text: find matches, and the bracket pair
            // at the cursor.
            let mut shapes = Vec::new();
            let row_rect = |from: usize, to: usize| {
                let a = output.galley.pos_from_cursor(egui::text::CCursor::new(from));
                let b = output.galley.pos_from_cursor(egui::text::CCursor::new(to));
                let right = if (a.min.y - b.min.y).abs() < 1.0 { b.min.x } else { a.min.x + char_width * (to - from) as f32 };
                egui::Rect::from_min_max(a.min, egui::pos2(right.max(a.min.x + 2.0), a.max.y))
                    .translate(output.galley_pos.to_vec2())
            };
            let visible = ui.clip_rect();
            let find_color = egui::Color32::from_rgba_unmultiplied(230, 190, 60, 60);
            let current_color = egui::Color32::from_rgba_unmultiplied(240, 160, 40, 150);
            for (n, &(from, to)) in matches.iter().enumerate() {
                let rect = row_rect(from, to);
                if rect.intersects(visible) {
                    let color = if Some(n) == current_match { current_color } else { find_color };
                    shapes.push(egui::Shape::rect_filled(rect, 2.0, color));
                }
            }
            if output.response.has_focus()
                && let Some(range) = output.cursor_range
                && range.primary.index.0 == range.secondary.index.0
                && let Some((a, b)) = code_edit::matching_bracket(&self.editor_text, range.primary.index.0)
            {
                let stroke = egui::Stroke::new(1.0, ui.visuals().weak_text_color());
                for at in [a, b] {
                    let rect = row_rect(at, at + 1);
                    shapes.push(egui::Shape::rect_filled(rect, 1.0, ui.visuals().selection.bg_fill.gamma_multiply(0.35)));
                    shapes.push(egui::Shape::rect_stroke(rect, 1.0, stroke, egui::StrokeKind::Inside));
                }
            }
            // Problems, underlined with a wavy line.
            let length = self.editor_text.chars().count();
            for d in &analysis.diagnostics {
                let from = analysis.char_index(d.start).min(length);
                let to = analysis.char_index(d.end).clamp(from, length).max(from + 1).min(length.max(from + 1));
                let rect = row_rect(from, to.min(length));
                if rect.intersects(visible) {
                    shapes.push(squiggle(rect, severity_color(d.severity)));
                }
            }
            ui.painter().set(behind, shapes);

            // Color tables: a swatch in the room the layout left before each
            // `{`; clicking one opens a picker.
            let room = lua_highlight::swatch_width(font_size);
            let pointer = ui.input(|i| i.pointer.interact_pos());
            let clicked = ui.input(|i| i.pointer.primary_clicked());
            for literal in code_edit::color_literals(&self.editor_text) {
                let glyph = output
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(literal.start))
                    .translate(output.galley_pos.to_vec2());
                let swatch = egui::Rect::from_min_max(
                    egui::pos2(glyph.min.x - room + 3.0, glyph.min.y + 2.0),
                    egui::pos2(glyph.min.x - 3.0, glyph.max.y - 2.0),
                );
                if !swatch.intersects(visible) {
                    continue;
                }
                let (rgb, alpha) = literal.rgba();
                paint_swatch(ui.painter(), swatch, rgb, alpha);
                if pointer.is_some_and(|p| swatch.contains(p)) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    if clicked {
                        self.editor.color_edit = Some(ColorEdit {
                            start: literal.start,
                            rgb,
                            alpha,
                            has_alpha: literal.has_alpha(),
                            at: swatch.left_bottom() + egui::vec2(0.0, 4.0),
                            fresh: true,
                        });
                    }
                }
            }
            self.color_picker_ui(ui);

            // The error's message, at the end of its line.
            if let (Some(error), Some(line)) = (&error, error_line)
                && let Some(row) = line_last_row(&output.galley, line)
            {
                let at = output.galley_pos + egui::vec2(row.rect().max.x + char_width * 3.0, row.pos.y);
                let message = short_message(error, line);
                let small = egui::FontId::proportional((font_size - 1.0).max(8.0));
                ui.painter().text(at, egui::Align2::LEFT_TOP, message, small, egui::Color32::from_rgb(220, 90, 90));
            }

            if let Some(index) = self.editor.scroll_to.take() {
                let rect = output.galley.pos_from_cursor(egui::text::CCursor::new(index));
                ui.scroll_to_rect(rect.translate(output.galley_pos.to_vec2()), Some(egui::Align::Center));
            }

            if output.response.changed() {
                // `end` and friends line up with their block as they're typed.
                let typed = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_))));
                if typed
                    && let Some(range) = output.cursor_range
                    && range.primary.index.0 == range.secondary.index.0
                    && let Some(cursor) = code_edit::dedent_closer(&mut self.editor_text, range.primary.index.0)
                {
                    store_selection(&ctx, editor_id, (cursor, cursor));
                }
                self.editor_changed();
            }

            // Ctrl+scroll zooms.
            if output.response.hovered() {
                let zoom = ui.input(|i| i.zoom_delta());
                if zoom != 1.0 {
                    let size = (font_size * zoom).clamp(*FONT_SIZES.start(), *FONT_SIZES.end());
                    self.config.editor_font_size = Some(size);
                    self.editor_settings_changed();
                }
            }

            // Completions: as a name is typed (or after `.`/`:`), or when
            // asked for; gone when the cursor moves off.
            let cursor = output.cursor_range.filter(|r| r.primary.index.0 == r.secondary.index.0).map(|r| r.primary.index.0);
            let typed = ui.input(|i| {
                i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == ':')))
            });
            let forced = std::mem::take(&mut self.editor.force_popup);
            match cursor {
                Some(cursor) if output.response.has_focus() => {
                    let keep = self.editor.popup.as_ref().is_some_and(|p| p.cursor == cursor);
                    let refresh = forced || typed || (keep && output.response.changed());
                    if refresh {
                        self.editor.popup = lua_analysis::complete(&analysis, &self.editor_text, cursor, forced).map(|c| {
                            let selected = self.editor.popup.as_ref().map_or(0, |p| p.selected).min(c.items.len() - 1);
                            Popup { items: c.items, selected: if typed { 0 } else { selected }, from: c.from, cursor }
                        });
                    } else if !keep {
                        self.editor.popup = None;
                    }
                }
                _ => self.editor.popup = None,
            }
            if let Some(cursor) = cursor
                && output.response.has_focus()
            {
                let at = output.galley.pos_from_cursor(egui::text::CCursor::new(cursor)).translate(output.galley_pos.to_vec2());
                if let Some(popup) = &mut self.editor.popup {
                    if let Some(accepted) = completion_popup(ui, popup, at, font_size) {
                        let end = code_edit::replace_range(&mut self.editor_text, (popup.from, cursor), &accepted);
                        store_selection(&ctx, editor_id, (end, end));
                        self.editor.popup = None;
                        self.editor_changed();
                    }
                } else if let Some(signature) = lua_analysis::signature_at(&analysis, &self.editor_text, cursor) {
                    signature_help(ui, &signature, at, font_size);
                }
            }

            // Ctrl+click: go to the definition.
            if output.response.clicked()
                && ui.input(|i| i.modifiers.command)
                && let Some(pointer) = ui.input(|i| i.pointer.interact_pos())
            {
                let index = output.galley.cursor_from_pos(pointer - output.galley_pos).index.0;
                self.go_to_definition(&ctx, editor_id, index);
            }

            // Hovering: a problem's message, else what a name is.
            if output.response.hovered()
                && let Some(pointer) = ui.input(|i| i.pointer.hover_pos())
            {
                let char_index = output.galley.cursor_from_pos(pointer - output.galley_pos).index.0;
                let byte = analysis.byte_index(char_index);
                if let Some(d) = analysis.diagnostics.iter().find(|d| d.start <= byte && byte < d.end.max(d.start + 1)) {
                    let label = if d.severity == Severity::Error { "Error" } else { "Warning" };
                    self.editor_hover_info = Some((label.to_string(), d.message.clone()));
                } else if let Some(word) = identifier_at(&self.editor_text, char_index) {
                    if let Some(info) = describe_name(&analysis, byte, &word) {
                        self.editor_hover_info = Some((word, info));
                    } else if let Some(detail) = lua_completion::lookup(&word) {
                        self.editor_hover_info = Some((word, detail.to_string()));
                    }
                }
            }

            // F1 at the text cursor.
            if output.response.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::F1))
                && let Some(range) = output.cursor_range
            {
                self.editor_hover_info = identifier_at(&self.editor_text, range.primary.index.0)
                    .and_then(|word| lua_completion::lookup(&word).map(|d| (word, d.to_string())));
            }
        });
    }

    /// The color picker for a color table: changing it rewrites the
    /// table's numbers. Esc, Done, or a click elsewhere closes it.
    fn color_picker_ui(&mut self, ui: &egui::Ui) {
        let Some(edit) = &mut self.editor.color_edit else { return };
        let ctx = ui.ctx().clone();
        let mut color = egui::Color32::from_rgba_unmultiplied(edit.rgb[0], edit.rgb[1], edit.rgb[2], (edit.alpha * 255.0).round() as u8);
        let mut changed = false;
        let mut done = false;
        let alpha_mode = if edit.has_alpha { egui::color_picker::Alpha::OnlyBlend } else { egui::color_picker::Alpha::Opaque };
        let area = egui::Area::new(egui::Id::new("editor_color_picker"))
            .order(egui::Order::Foreground)
            .fixed_pos(edit.at)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    changed = egui::color_picker::color_picker_color32(ui, &mut color, alpha_mode);
                    ui.horizontal(|ui| {
                        let [r, g, b, a] = color.to_srgba_unmultiplied();
                        if edit.has_alpha {
                            ui.weak(format!("r = {r}, g = {g}, b = {b}, a = {:.2}", f32::from(a) / 255.0));
                        } else {
                            ui.weak(format!("r = {r}, g = {g}, b = {b}"));
                        }
                        if ui.button("Done").clicked() {
                            done = true;
                        }
                    });
                });
            });
        let clicked_outside = ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p))
        });
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if std::mem::take(&mut edit.fresh) {
            // The click that opened it.
        } else if done || escape || clicked_outside {
            self.editor.color_edit = None;
            return;
        }
        if changed {
            let [r, g, b, a] = color.to_srgba_unmultiplied();
            edit.rgb = [r, g, b];
            edit.alpha = f32::from(a) / 255.0;
            let (start, rgb, alpha) = (edit.start, edit.rgb, edit.alpha);
            if code_edit::set_color(&mut self.editor_text, start, rgb, alpha) {
                self.editor_changed();
            } else {
                self.editor.color_edit = None;
            }
        }
    }

    /// Put the text cursor at char `index` and scroll there.
    fn jump_to(&mut self, ctx: &egui::Context, id: egui::Id, index: usize) {
        store_selection(ctx, id, (index, index));
        self.editor.scroll_to = Some(index);
        ctx.memory_mut(|m| m.request_focus(id));
    }

    /// F12 / Ctrl+click: jump to where the name at char `index` is defined
    /// (a local, or one of the script's own functions).
    fn go_to_definition(&mut self, ctx: &egui::Context, id: egui::Id, index: usize) {
        let analysis = self.completion.analysis();
        let Some(word) = identifier_at(&self.editor_text, index) else { return };
        if word == "sprite_register" {
            self.edit_sprite_at(index);
            return;
        }
        let byte = analysis.byte_index(index);
        let target = analysis
            .ref_at(byte)
            .and_then(|r| r.local)
            .and_then(|i| analysis.locals.get(i))
            .map(|l| l.at)
            .or_else(|| analysis.local_named(&word, byte).map(|l| l.at))
            .or_else(|| analysis.functions.iter().find(|f| f.name == word).map(|f| f.at));
        match target {
            Some(at) => {
                let index = analysis.char_index(at);
                self.jump_to(ctx, id, index);
            }
            None => {
                if self.open_reference(&word) {
                    if let Some(detail) = lua_completion::lookup(&word) {
                        self.editor_hover_info = Some((word, detail.to_string()));
                    }
                } else if let Some(detail) = lua_completion::lookup(&word) {
                    self.editor_hover_info = Some((word, detail.to_string()));
                } else {
                    self.status = format!("Couldn't find where `{word}` is defined.");
                }
            }
        }
    }

    /// Shift+Alt+F: lay the script out with StyLua (4-space indents, lines
    /// up to 120 wide). The text cursor stays on the same line. Returns
    /// whether the text changed.
    fn format_script(&mut self, ctx: &egui::Context, id: egui::Id) -> bool {
        let mut config = stylua_lib::Config::new();
        config.syntax = stylua_lib::LuaVersion::Lua54;
        config.indent_type = stylua_lib::IndentType::Spaces;
        config.indent_width = 4;
        config.column_width = 120;
        config.quote_style = stylua_lib::QuoteStyle::AutoPreferDouble;
        match stylua_lib::format_code(&self.editor_text, config, None, stylua_lib::OutputVerification::None) {
            Ok(formatted) if formatted != self.editor_text => {
                let (_, cursor) = load_selection(ctx, id, &self.editor_text);
                let line = self.editor_text.chars().take(cursor).filter(|&c| c == '\n').count() + 1;
                self.editor_text = formatted;
                let index = char_index_of_line(&self.editor_text, line);
                store_selection(ctx, id, (index, index));
                self.status = "Formatted the script.".to_string();
                true
            }
            Ok(_) => false,
            Err(e) => {
                self.status = format!("Can't format the script until it parses: {e}");
                false
            }
        }
    }

    fn editor_settings_changed(&mut self) {
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save editor settings: {e}");
        }
    }

    /// Functions, Find, History, when edits apply, wrap and text size.
    fn editor_toolbar_ui(&mut self, ui: &mut egui::Ui, editor_id: egui::Id) {
        ui.horizontal_wrapped(|ui| {
            let mut insert = None;
            egui::ComboBox::from_id_salt("visualizer_editor_functions").selected_text("Functions").show_ui(ui, |ui| {
                for suggestion in self.completion.suggestions() {
                    if ui.selectable_label(false, &suggestion.label).on_hover_text(&suggestion.detail).clicked() {
                        insert = Some(suggestion.label);
                    }
                }
            });
            if let Some(name) = insert {
                let ctx = ui.ctx().clone();
                let sel = load_selection(&ctx, editor_id, &self.editor_text);
                let end = code_edit::replace_range(&mut self.editor_text, sel, &name);
                store_selection(&ctx, editor_id, (end, end));
                self.editor_changed();
            }
            let mut snippet = None;
            let mut new_sprite = false;
            ui.menu_button("Insert", |ui| {
                for s in crate::snippets::SNIPPETS {
                    let where_ = match s.place {
                        crate::snippets::Place::Cursor => "at the text cursor",
                        crate::snippets::Place::TopLevel => "above function render",
                    };
                    if ui.button(s.name).on_hover_text(format!("{} ({where_})", s.description)).clicked() {
                        snippet = Some(s);
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Sprite...").on_hover_text("A blank sprite of the size you choose, opened in the Sprite Editor").clicked() {
                    new_sprite = true;
                    ui.close();
                }
            });
            if let Some(s) = snippet {
                let ctx = ui.ctx().clone();
                let (_, cursor) = load_selection(&ctx, editor_id, &self.editor_text);
                let at = crate::snippets::insert(&mut self.editor_text, cursor, s);
                store_selection(&ctx, editor_id, (at, at));
                self.editor.scroll_to = Some(at);
                ctx.memory_mut(|m| m.request_focus(editor_id));
                self.editor_changed();
            }
            if new_sprite {
                self.open_new_sprite_dialog();
            }
            let analysis = self.completion.analysis();
            ui.menu_button("Go to", |ui| {
                if analysis.functions.is_empty() {
                    ui.weak("No functions yet");
                }
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for function in &analysis.functions {
                        let label = format!("{}({})", function.name, function.params.join(", "));
                        if ui.button(label).on_hover_text(format!("line {}", function.line)).clicked() {
                            let index = analysis.char_index(function.at);
                            self.jump_to(ui.ctx(), editor_id, index);
                            ui.close();
                        }
                    }
                });
            })
            .response
            .on_hover_text("The script's functions; F12 or Ctrl+click on a name goes to where it's defined");
            let problems = analysis.diagnostics.len();
            if problems > 0 {
                let errors = analysis.diagnostics.iter().any(|d| d.severity == Severity::Error);
                let color = severity_color(if errors { Severity::Error } else { Severity::Warning });
                let label = egui::RichText::new(format!("{problems} problem{}", if problems == 1 { "" } else { "s" }))
                    .color(color);
                ui.menu_button(label, |ui| {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for d in &analysis.diagnostics {
                            let text = egui::RichText::new(format!("{}: {}", d.line, d.message)).color(severity_color(d.severity));
                            if ui.button(text).clicked() {
                                let index = analysis.char_index(d.start);
                                self.jump_to(ui.ctx(), editor_id, index);
                                ui.close();
                            }
                        }
                    });
                });
            }
            if ui.button("Format").on_hover_text("Tidy the script's layout (Shift+Alt+F)").clicked() {
                let ctx = ui.ctx().clone();
                if self.format_script(&ctx, editor_id) {
                    self.editor_changed();
                }
            }
            if ui.button("Find").on_hover_text("Ctrl+F; Ctrl+H to replace").clicked() {
                self.editor.find = Some(Find {
                    query: String::new(),
                    replacement: String::new(),
                    show_replace: false,
                    case_sensitive: false,
                    current: 0,
                    focus_query: true,
                });
            }
            let script = self.visualizer.script().path().map(Path::to_path_buf);
            if ui
                .add_enabled(script.is_some(), egui::Button::new("History..."))
                .on_hover_text("Earlier versions of this script, kept automatically")
                .clicked()
                && let Some(script) = script
            {
                self.flush_editor();
                self.editor.history = load_history(&script);
                self.editor.history_selected = 0;
                self.editor.history_open = true;
            }

            ui.separator();
            let mut mode = self.config.editor_apply;
            ui.label("Apply:");
            egui::ComboBox::from_id_salt("editor_apply_mode").selected_text(mode.label()).show_ui(ui, |ui| {
                for option in ApplyMode::ALL {
                    ui.selectable_value(&mut mode, option, option.label());
                }
            })
            .response
            .on_hover_text(
                "When your edits reach the running script (and its file). Applying restarts the script, \\
                 so a game loses its state: \"when typing pauses\" or \"on Ctrl+S\" keep that from happening \\
                 on every keystroke. Ctrl+S always applies right away.",
            );
            if mode != self.config.editor_apply {
                self.config.editor_apply = mode;
                if mode == ApplyMode::EveryEdit {
                    self.flush_editor();
                }
                self.editor_settings_changed();
            }
            let mut wrap = self.config.editor_wrap.unwrap_or(true);
            if ui.checkbox(&mut wrap, "Wrap").changed() {
                self.config.editor_wrap = (!wrap).then_some(false);
                self.editor_settings_changed();
            }
            let mut size = self.editor_font_size();
            if ui
                .add(egui::DragValue::new(&mut size).range(FONT_SIZES).speed(0.2).suffix(" pt"))
                .on_hover_text("Text size (Ctrl+scroll over the text)")
                .changed()
            {
                self.config.editor_font_size = Some(size.round());
                self.editor_settings_changed();
            }
            if self.editor_dirty() {
                ui.separator();
                let label = egui::RichText::new("Not applied yet").color(ui.visuals().warn_fg_color);
                ui.label(label);
                if ui.small_button("Apply (Ctrl+S)").clicked() {
                    self.apply_editor();
                }
            }
        });
    }

    /// Find (and replace), above the text.
    fn find_bar_ui(&mut self, ui: &mut egui::Ui, editor_id: egui::Id) {
        let Some(find) = &mut self.editor.find else { return };
        let ctx = ui.ctx().clone();
        let matches = code_edit::find_all(&self.editor_text, &find.query, find.case_sensitive);
        let mut close = false;
        let mut go: Option<isize> = None;
        let mut replace_one = false;
        let mut replace_all = false;
        ui.horizontal(|ui| {
            let query = ui.add(egui::TextEdit::singleline(&mut find.query).hint_text("Find").desired_width(180.0));
            if std::mem::take(&mut find.focus_query) {
                query.request_focus();
            }
            if query.changed() {
                // Jump to the first match at or after the cursor.
                let (cursor, _) = load_selection(&ctx, editor_id, &self.editor_text);
                let fresh = code_edit::find_all(&self.editor_text, &find.query, find.case_sensitive);
                find.current = fresh.iter().position(|&(from, _)| from >= cursor).unwrap_or(0);
                go = Some(0);
            }
            if query.has_focus() || query.lost_focus() {
                let (enter, shift, escape) =
                    ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift, i.key_pressed(egui::Key::Escape)));
                if enter {
                    go = Some(if shift { -1 } else { 1 });
                    query.request_focus();
                }
                if escape {
                    close = true;
                }
            }
            let count = if find.query.is_empty() {
                String::new()
            } else if matches.is_empty() {
                "no matches".to_string()
            } else {
                format!("{} of {}", find.current.min(matches.len() - 1) + 1, matches.len())
            };
            ui.weak(count);
            if ui.small_button("Previous").on_hover_text("Shift+Enter").clicked() {
                go = Some(-1);
            }
            if ui.small_button("Next").on_hover_text("Enter").clicked() {
                go = Some(1);
            }
            ui.toggle_value(&mut find.case_sensitive, "Aa").on_hover_text("Match case");
            ui.toggle_value(&mut find.show_replace, "Replace");
            if ui.small_button("x").on_hover_text("Close (Esc)").clicked() {
                close = true;
            }
        });
        if find.show_replace {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut find.replacement).hint_text("Replace with").desired_width(180.0));
                replace_one = ui.add_enabled(!matches.is_empty(), egui::Button::new("Replace")).clicked();
                replace_all = ui.add_enabled(!matches.is_empty(), egui::Button::new("Replace all")).clicked();
            });
        }

        if replace_all {
            let replacement = find.replacement.clone();
            for &range in matches.iter().rev() {
                code_edit::replace_range(&mut self.editor_text, range, &replacement);
            }
            self.status = format!("Replaced {} matches.", matches.len());
            self.editor_changed();
            return;
        }
        if replace_one && let Some(&range) = matches.get(find.current.min(matches.len().saturating_sub(1))) {
            let replacement = find.replacement.clone();
            code_edit::replace_range(&mut self.editor_text, range, &replacement);
            go = Some(0);
            self.editor_changed();
        }
        let Some(find) = &mut self.editor.find else { return };
        if close {
            self.editor.find = None;
            ctx.memory_mut(|m| m.request_focus(editor_id));
            return;
        }
        if let Some(step) = go {
            let matches = code_edit::find_all(&self.editor_text, &find.query, find.case_sensitive);
            if !matches.is_empty() {
                let len = matches.len() as isize;
                find.current = (find.current.min(matches.len() - 1) as isize + step).rem_euclid(len) as usize;
                let (from, to) = matches[find.current];
                store_selection(&ctx, editor_id, (from, to));
                self.editor.scroll_to = Some(from);
            }
        }
    }

    /// History...: earlier versions of the script, to look at and restore.
    pub(super) fn history_ui(&mut self, ctx: &egui::Context) {
        if !self.editor.history_open {
            return;
        }
        let mut close = false;
        let mut restore = None;
        let response = egui::Modal::new(egui::Id::new("script_history")).show(ctx, |ui| {
            ui.set_width(760.0);
            ui.heading("Script history");
            ui.weak(
                "synththing keeps a copy of the script when you open it and about once a minute while \\
                 you edit (the last 50). Restoring one keeps the current version in the list too.",
            );
            ui.add_space(6.0);
            if self.editor.history.is_empty() {
                ui.label("No earlier versions yet.");
            }
            ui.horizontal_top(|ui| {
                egui::ScrollArea::vertical().id_salt("history_list").max_height(380.0).max_width(250.0).show(ui, |ui| {
                    for (n, snap) in self.editor.history.iter().enumerate() {
                        let (added, removed) = line_changes(&self.editor_text, &snap.text);
                        let label = format!(
                            "{}\n{} lines, +{added} -{removed} vs now",
                            ago(snap.millis),
                            snap.text.lines().count()
                        );
                        if ui.selectable_label(self.editor.history_selected == n, label).clicked() {
                            self.editor.history_selected = n;
                        }
                    }
                });
                if let Some(snap) = self.editor.history.get(self.editor.history_selected) {
                    ui.vertical(|ui| {
                        egui::ScrollArea::both().id_salt("history_preview").max_height(380.0).show(ui, |ui| {
                            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _: f32| {
                                lua_highlight::layout(ui, buf.as_str(), f32::INFINITY, None, DEFAULT_FONT_SIZE, false)
                            };
                            let mut text = snap.text.as_str();
                            ui.add(egui::TextEdit::multiline(&mut text).code_editor().layouter(&mut layouter));
                        });
                    });
                }
            });
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
                if ui
                    .add_enabled(!self.editor.history.is_empty(), egui::Button::new("Restore this version"))
                    .clicked()
                {
                    restore = self.editor.history.get(self.editor.history_selected).map(|s| s.text.clone());
                }
            });
        });
        if let Some(text) = restore {
            if let Some(path) = self.visualizer.script().path().map(Path::to_path_buf) {
                snapshot(&path, &self.editor_text);
            }
            self.editor_text = text;
            self.completion.request(self.editor_text.clone());
            self.apply_editor();
            self.status = "Restored an earlier version of the script.".to_string();
            close = true;
        }
        if close || response.should_close() {
            self.editor.history_open = false;
            self.editor.history.clear();
        }
    }
}

/// The last row (of a wrapped line, possibly several) of 1-based `line`.
fn line_last_row(galley: &egui::Galley, line: usize) -> Option<&egui::epaint::text::PlacedRow> {
    let mut current = 1;
    let last = galley.rows.len().checked_sub(1)?;
    for (n, row) in galley.rows.iter().enumerate() {
        if current == line && (row.ends_with_newline || n == last) {
            return Some(row);
        }
        if row.ends_with_newline {
            current += 1;
        }
    }
    None
}

/// Number each line of the editor's text in the gutter left of it, at the
/// same height as the line itself (wrapped continuation rows get none).
/// The error line's number is red, with a marker beside it.
fn paint_line_numbers(
    ui: &egui::Ui,
    output: &egui::text_edit::TextEditOutput,
    gutter: egui::Rect,
    font: &egui::FontId,
    error_line: Option<usize>,
    marks: &std::collections::HashMap<usize, egui::Color32>,
) {
    let painter = ui.painter();
    let normal = ui.visuals().weak_text_color();
    let error = egui::Color32::from_rgb(220, 90, 90);
    let right = gutter.right() - 8.0;
    let visible = ui.clip_rect();
    let mut line = 1;
    let mut starts_line = true;
    for row in &output.galley.rows {
        if starts_line {
            let top = output.galley_pos.y + row.pos.y;
            if top <= visible.bottom() && top + row.row.size.y >= visible.top() {
                let is_error = error_line == Some(line);
                let color = if is_error { error } else { normal };
                painter.text(egui::pos2(right, top), egui::Align2::RIGHT_TOP, line.to_string(), font.clone(), color);
                let middle = top + row.row.size.y / 2.0;
                if is_error {
                    painter.circle_filled(egui::pos2(gutter.right() - 3.0, middle), 2.5, error);
                } else if let Some(&color) = marks.get(&line) {
                    painter.circle_filled(egui::pos2(gutter.right() - 3.0, middle), 2.0, color);
                }
            }
            line += 1;
        }
        starts_line = row.ends_with_newline;
    }
}

fn severity_color(severity: Severity) -> egui::Color32 {
    match severity {
        Severity::Error => egui::Color32::from_rgb(230, 80, 80),
        Severity::Warning => egui::Color32::from_rgb(220, 180, 60),
    }
}

/// A wavy line along the bottom of `rect`.
fn squiggle(rect: egui::Rect, color: egui::Color32) -> egui::Shape {
    let y = rect.bottom() - 1.5;
    let step = 3.0;
    let mut points = Vec::new();
    let mut x = rect.left();
    let mut up = true;
    while x <= rect.right() {
        points.push(egui::pos2(x, if up { y - 1.5 } else { y + 0.5 }));
        x += step;
        up = !up;
    }
    if points.len() < 2 {
        points.push(egui::pos2(rect.right(), y));
    }
    egui::Shape::line(points, egui::Stroke::new(1.0, color))
}

/// What a name of the script's own is, for hovering it.
fn describe_name(analysis: &Analysis, byte: usize, word: &str) -> Option<String> {
    let local = analysis
        .ref_at(byte)
        .and_then(|r| r.local)
        .and_then(|i| analysis.locals.get(i))
        .or_else(|| analysis.locals.iter().find(|l| l.at <= byte && byte <= l.at + l.name.len()))?;
    if local.name != word {
        return None;
    }
    Some(match (local.kind, &local.params) {
        (_, Some(params)) => format!("local function {}({}), line {}", local.name, params.join(", "), local.line),
        (lua_analysis::LocalKind::Parameter, _) => "parameter".to_string(),
        (lua_analysis::LocalKind::LoopVariable, _) => format!("loop variable, line {}", local.line),
        _ => format!("local, line {}", local.line),
    })
}

/// The completion list under the text cursor (`at` is the cursor's rect).
/// Returns an item's label when one is clicked.
fn completion_popup(ui: &egui::Ui, popup: &mut Popup, at: egui::Rect, font_size: f32) -> Option<String> {
    let mut accepted = None;
    let font = egui::FontId::monospace(font_size);
    egui::Area::new(egui::Id::new("editor_completions"))
        .order(egui::Order::Tooltip)
        .fixed_pos(at.left_bottom() + egui::vec2(0.0, 2.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(520.0);
                let shown = popup.items.len().min(10);
                // Keep the selection in the window of shown items.
                let first = popup.selected.saturating_sub(shown.saturating_sub(1));
                for (n, item) in popup.items.iter().enumerate().skip(first).take(shown) {
                    let selected = n == popup.selected;
                    let text = egui::RichText::new(format!("{} {}", item.kind.tag(), item.label)).font(font.clone());
                    let response = ui.add(egui::Button::selectable(selected, text).frame_when_inactive(false));
                    if response.clicked() {
                        accepted = Some(item.label.clone());
                    }
                }
                if popup.items.len() > shown {
                    ui.weak(format!("{} more", popup.items.len() - shown));
                }
                if let Some(item) = popup.items.get(popup.selected)
                    && !item.detail.is_empty()
                {
                    ui.separator();
                    ui.weak(&item.detail);
                }
                ui.weak("Enter / Tab to use, Esc to close");
            });
        });
    accepted
}

/// The current call's parameters above the text cursor, the one being
/// typed in bold.
fn signature_help(ui: &egui::Ui, signature: &lua_analysis::Signature, at: egui::Rect, font_size: f32) {
    let font = egui::FontId::monospace(font_size);
    let visuals = ui.visuals();
    let mut job = egui::text::LayoutJob::default();
    let plain = egui::TextFormat { font_id: font.clone(), color: visuals.text_color(), ..Default::default() };
    let active =
        egui::TextFormat { font_id: font.clone(), color: visuals.strong_text_color(), underline: egui::Stroke::new(1.0, visuals.strong_text_color()), ..Default::default() };
    job.append(&format!("{}(", signature.name), 0.0, plain.clone());
    for (n, param) in signature.params.iter().enumerate() {
        if n > 0 {
            job.append(", ", 0.0, plain.clone());
        }
        job.append(param, 0.0, if n == signature.active { active.clone() } else { plain.clone() });
    }
    job.append(")", 0.0, plain.clone());
    if !signature.rest.is_empty() {
        let weak = egui::TextFormat { font_id: font, color: visuals.weak_text_color(), ..Default::default() };
        job.append(&format!(" {}", signature.rest), 0.0, weak);
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let height = galley.size().y + 12.0;
    egui::Area::new(egui::Id::new("editor_signature"))
        .order(egui::Order::Tooltip)
        .fixed_pos(at.left_top() - egui::vec2(0.0, height + 2.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(galley);
            });
        });
}

/// A color swatch: the color over a checkerboard (so transparency shows),
/// with a thin border.
fn paint_swatch(painter: &egui::Painter, rect: egui::Rect, rgb: [u8; 3], alpha: f32) {
    if alpha < 1.0 {
        let half = rect.width() / 2.0;
        let (light, dark) = (egui::Color32::from_gray(200), egui::Color32::from_gray(120));
        let top = egui::Rect::from_min_size(rect.min, egui::vec2(half, rect.height() / 2.0));
        painter.rect_filled(rect, 2.0, light);
        painter.rect_filled(top, 0.0, dark);
        painter.rect_filled(top.translate(egui::vec2(half, rect.height() / 2.0)), 0.0, dark);
    }
    let color = egui::Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], (alpha * 255.0).round() as u8);
    painter.rect_filled(rect, 2.0, color);
    painter.rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::from_gray(140)), egui::StrokeKind::Inside);
}
