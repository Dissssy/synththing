//! The script editor: a syntax-highlighted `TextEdit` with line numbers,
//! smart editing keys (`code_edit.rs`), find and replace, a choice of when
//! edits reach the running script, local history, zoom and wrap.
//!
//! Keys, while the editor has focus: Enter auto-indents, Tab / Shift+Tab
//! indent and outdent (a selection: every line in it), Ctrl+/ toggles
//! comments, brackets and quotes close themselves, `end` lines itself up
//! with its block. Ctrl+S applies (and saves) now, Ctrl+F finds, Ctrl+H
//! replaces, Ctrl+scroll zooms.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use serde::{Deserialize, Serialize};

use super::{char_index_of_line, identifier_at, App, SCRIPT_EDITOR_ID};
use crate::code_edit::{self, Selection};
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
    fn editor_changed(&mut self) {
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
        self.visualizer.visualizer_mut().set_source_and_save(self.editor_text.clone());
        let due = self.editor.last_snapshot.is_none_or(|t| t.elapsed() >= SNAPSHOT_EVERY);
        if due && let Some(path) = self.visualizer.visualizer().path().map(Path::to_path_buf) {
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
            return false;
        }
        let mut sel = load_selection(ctx, id, &self.editor_text);
        let mut changed = false;
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

        if let Some((word, detail)) = &self.editor_hover_info {
            ui.horizontal(|ui| {
                ui.strong(word);
                ui.label(detail);
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
        let error = self.visualizer.visualizer().error().map(str::to_string);
        let error_line = error.as_deref().and_then(lua_visualizer::error_line);
        let matches = match &self.editor.find {
            Some(find) => code_edit::find_all(&self.editor_text, &find.query, find.case_sensitive),
            None => Vec::new(),
        };
        let current_match = self.editor.find.as_ref().map(|f| f.current);

        let scroll = if wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
        scroll.id_salt("visualizer_editor_scroll").auto_shrink([false, false]).show(ui, |ui| {
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                let width = if wrap { wrap_width } else { f32::INFINITY };
                lua_highlight::layout(ui, buf.as_str(), width, error_line, font_size)
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
            paint_line_numbers(ui, &output, gutter, &font, error_line);

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
            ui.painter().set(behind, shapes);

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

            // Mouse hovering a known identifier.
            if output.response.hovered()
                && let Some(pointer) = ui.input(|i| i.pointer.hover_pos())
            {
                let local = pointer - output.galley_pos;
                let char_index = output.galley.cursor_from_pos(local).index.0;
                if let Some(word) = identifier_at(&self.editor_text, char_index)
                    && let Some(detail) = lua_completion::lookup(&word)
                {
                    self.editor_hover_info = Some((word, detail.to_string()));
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
            let script = self.visualizer.visualizer().path().map(Path::to_path_buf);
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
                                lua_highlight::layout(ui, buf.as_str(), f32::INFINITY, None, DEFAULT_FONT_SIZE)
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
            if let Some(path) = self.visualizer.visualizer().path().map(Path::to_path_buf) {
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
                if is_error {
                    let middle = top + row.row.size.y / 2.0;
                    painter.circle_filled(egui::pos2(gutter.right() - 3.0, middle), 2.5, error);
                }
            }
            line += 1;
        }
        starts_line = row.ends_with_newline;
    }
}
