//! The Songs search box (`search.rs` says what a query means): filters
//! that don't parse underlined (why, on hover), how many songs it found at
//! its right end, and completion of the filter being typed, its keys and
//! values (Tab or a click puts one in, the arrows pick, Alt switches how
//! the value matches: fuzzy, close or exact). Ctrl+F or / goes to it, Esc
//! empties it.

use eframe::egui;

use crate::fields::Field;
use crate::matching::Mode;
use crate::search::{self, Completion, Problem};

#[derive(Default)]
pub struct SearchBox {
    pub query: String,
    /// The option picked in the completion.
    picked: usize,
    /// How a completed value matches.
    mode: Option<Mode>,
    /// Whether Alt was down last frame (it switches `mode` when pressed).
    alt: bool,
    /// A field's values to complete with, and the library revision they're
    /// from.
    values: Option<(Field, u64, Vec<String>)>,
}

/// What [`SearchBox::ui`] reports.
pub struct Output {
    /// The query changed.
    pub changed: bool,
    /// Save as a smart playlist was clicked.
    pub save: bool,
}

impl SearchBox {
    /// Draw it (filling the rest of the row), `problems` and `found` (how
    /// many songs, when there's a query) from the query as last parsed,
    /// `values` giving a field's values, as of library `revision`.
    pub fn ui(&mut self, ui: &mut egui::Ui, problems: &[Problem], found: Option<usize>, revision: u64, values: &mut dyn FnMut(Field) -> Vec<String>) -> Output {
        let id = ui.id().with("search");
        let ctx = ui.ctx().clone();
        let focused = ctx.memory(|m| m.has_focus(id));
        let mut changed = false;

        // Ctrl+F or / (when nothing else is typed in) goes to it.
        let nothing_focused = ctx.memory(|m| m.focused().is_none());
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F) || (nothing_focused && i.consume_key(egui::Modifiers::NONE, egui::Key::Slash))) {
            ctx.memory_mut(|m| m.request_focus(id));
        }

        let mode = self.mode.unwrap_or(Mode::Fuzzy);
        let cursor = cursor(&ctx, id, &self.query);
        let completion = focused.then(|| self.complete(cursor, mode, revision, values)).flatten();
        let alt = ui.input(|i| i.modifiers.alt);
        if focused && alt && !self.alt {
            self.mode = Some(match mode {
                Mode::Fuzzy => Mode::Close,
                Mode::Close => Mode::Exact,
                Mode::Exact => Mode::Fuzzy,
            });
        }
        self.alt = alt;
        let mut accept = None;
        if let Some(completion) = &completion {
            let count = completion.options.len();
            self.picked = self.picked.min(count - 1);
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.picked = (self.picked + 1) % count;
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.picked = (self.picked + count - 1) % count;
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
                    accept = Some(self.picked);
                }
            });
        }
        if focused && !self.query.is_empty() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.query.clear();
            changed = true;
        }

        let save = !self.query.trim().is_empty()
            && ui
                .add(crate::app::icon_button("engine"))
                .on_hover_text("Save as a smart playlist: the filters become its rules")
                .clicked();

        let warn = ui.visuals().warn_fg_color;
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let text = text.as_str();
            let font = egui::TextStyle::Body.resolve(ui.style());
            let plain = egui::TextFormat { font_id: font.clone(), color: ui.visuals().text_color(), ..Default::default() };
            let bad = egui::TextFormat { underline: egui::Stroke::new(1.5, warn), ..plain.clone() };
            let mut job = egui::text::LayoutJob::default();
            let mut at = 0;
            for p in problems.iter().filter(|p| p.at.end <= text.len() && text.is_char_boundary(p.at.start) && text.is_char_boundary(p.at.end)) {
                if p.at.start < at {
                    continue;
                }
                job.append(&text[at..p.at.start], 0.0, plain.clone());
                job.append(&text[p.at.clone()], 0.0, bad.clone());
                at = p.at.end;
            }
            job.append(&text[at..], 0.0, plain);
            job.wrap.max_width = wrap_width;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let hint = if crate::select_list::touch(&ctx) { "search, or filter: type:mid..." } else { "search, or filter: type:mid... (Ctrl+F)" };
        let output = egui::TextEdit::singleline(&mut self.query).id(id).hint_text(hint).desired_width(f32::INFINITY).layouter(&mut layouter).show(ui);
        let hover = if problems.is_empty() { search::HELP.to_string() } else { problems.iter().map(|p| p.why.as_str()).collect::<Vec<_>>().join("\n") };
        let response = output.response.response.clone().on_hover_text(hover);
        changed |= response.changed();

        if let Some(n) = found {
            let text = if n == 1 { "1 song".to_string() } else { format!("{n} songs") };
            let galley = ui.painter().layout_no_wrap(text, egui::TextStyle::Small.resolve(ui.style()), ui.visuals().weak_text_color());
            let rect = response.rect;
            let width = galley.size().x;
            // (Only while the query leaves it room.)
            if output.galley.size().x + width + 24.0 < rect.width() {
                let pos = egui::pos2(rect.right() - width - 6.0, rect.center().y - galley.size().y / 2.0);
                ui.painter().galley(pos, galley, ui.visuals().weak_text_color());
            }
        }

        if let Some(completion) = &completion {
            let shown = egui::Area::new(id.with("completion"))
                .order(egui::Order::Foreground)
                .fixed_pos(response.rect.left_bottom())
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(response.rect.width().min(320.0));
                        for (i, (label, _)) in completion.options.iter().enumerate() {
                            if ui.selectable_label(i == self.picked, label).clicked() {
                                accept = Some(i);
                            }
                        }
                        let mode = match mode {
                            Mode::Fuzzy => "fuzzy",
                            Mode::Close => "close",
                            Mode::Exact => "exact",
                        };
                        ui.weak(format!("Tab puts it in \u{b7} Alt: {mode}"));
                    })
                });
            // (Clicking an option mustn't take the focus away.)
            if shown.response.contains_pointer() {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        }
        if let Some((i, completion)) = accept.zip(completion) {
            let insert = &completion.options[i].1;
            self.query.replace_range(completion.at.clone(), &format!("{insert}{}", if insert.ends_with(':') { "" } else { " " }));
            let end = self.query[..completion.at.start].chars().count() + insert.chars().count() + usize::from(!insert.ends_with(':'));
            set_cursor(&ctx, id, end);
            ctx.memory_mut(|m| m.request_focus(id));
            self.picked = 0;
            changed = true;
        }
        if changed {
            self.picked = 0;
        }
        Output { changed, save }
    }

    /// The completion at `cursor`, a field's values read once for each
    /// library revision.
    fn complete(&mut self, cursor: usize, mode: Mode, revision: u64, values: &mut dyn FnMut(Field) -> Vec<String>) -> Option<Completion> {
        let cache = &mut self.values;
        search::complete(&self.query, cursor, mode, &mut |field| {
            if cache.as_ref().is_none_or(|(f, r, _)| *f != field || *r != revision) {
                *cache = Some((field, revision, values(field)));
            }
            cache.as_ref().map(|(_, _, v)| v.clone()).unwrap_or_default()
        })
    }
}

/// Where the text cursor is in `text` (a byte index; its end if unknown).
fn cursor(ctx: &egui::Context, id: egui::Id, text: &str) -> usize {
    let at = egui::widgets::text_edit::TextEditState::load(ctx, id).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index.0);
    at.and_then(|chars| text.char_indices().nth(chars).map(|(i, _)| i)).unwrap_or(text.len())
}

fn set_cursor(ctx: &egui::Context, id: egui::Id, chars: usize) {
    let mut state = egui::widgets::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(chars))));
    state.store(ctx, id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame of `search` with `events`, focused on it.
    fn frame(ctx: &egui::Context, search: &mut SearchBox, events: Vec<egui::Event>) -> Output {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0))),
            events,
            ..Default::default()
        };
        let mut out = None;
        let mut output = ctx.run_ui(input, |ui| {
            ui.ctx().memory_mut(|m| m.request_focus(ui.id().with("search")));
            let mut values = |field: Field| if field == Field::Artist { vec!["Daft Punk".to_string()] } else { Vec::new() };
            out = Some(search.ui(ui, &[], Some(3), 0, &mut values));
        });
        output.textures_delta.clear();
        out.expect("drawn")
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn tab_completes_keys_then_values() {
        let ctx = egui::Context::default();
        let mut search = SearchBox::default();
        frame(&ctx, &mut search, Vec::new());
        frame(&ctx, &mut search, vec![egui::Event::Text("art".into())]);
        assert_eq!(search.query, "art");
        frame(&ctx, &mut search, vec![key(egui::Key::Tab)]);
        assert_eq!(search.query, "artist:");
        frame(&ctx, &mut search, vec![egui::Event::Text("dp".into())]);
        frame(&ctx, &mut search, vec![key(egui::Key::Tab)]);
        assert_eq!(search.query, "artist:'Daft Punk' ");
        assert!(frame(&ctx, &mut search, vec![key(egui::Key::Escape)]).changed);
        assert_eq!(search.query, "");
    }
}
