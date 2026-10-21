//! The command palette (Ctrl+Shift+P): every command that would do
//! something now, by name, with its key beside it. Typing filters them
//! (the Songs search's fuzzy match), Up/Down move the highlight, Enter or
//! a click runs one, Escape closes.

use eframe::egui;

use super::{fit_width, App};
use crate::commands::{self, Command};
use crate::engine::EngineView;
use crate::song_list::fuzzy_score;

#[derive(Default)]
pub(super) struct Palette {
    query: String,
    highlighted: usize,
}

/// `commands` that match `query`, best first; all of them, in order, for
/// an empty one.
fn ranked(commands: impl IntoIterator<Item = Command>, query: &str) -> Vec<Command> {
    let mut scored: Vec<(i32, Command)> =
        commands.into_iter().filter_map(|c| Some((fuzzy_score(c.name(), query)?, c))).collect();
    if !query.trim().is_empty() {
        // (Stable: ties keep the palette's order.)
        scored.sort_by_key(|(score, _)| -score);
    }
    scored.into_iter().map(|(_, c)| c).collect()
}

/// How tall the list gets before it scrolls.
const LIST_HEIGHT: f32 = 320.0;

/// The matching commands, each with its key on the right; the one
/// clicked, if any. `scroll` brings the highlighted one into view.
fn list_ui(ui: &mut egui::Ui, matches: &[Command], highlighted: usize, scroll: bool) -> Option<Command> {
    let mut clicked = None;
    // The modal is laid out within last frame's size, so without the
    // minimum a list that grows back (after a backspace) would stay as
    // short as it was.
    egui::ScrollArea::vertical().max_height(LIST_HEIGHT).min_scrolled_height(LIST_HEIGHT).show(ui, |ui| {
        for (n, &command) in matches.iter().enumerate() {
            let row = ui.horizontal(|ui| {
                let label = ui.selectable_label(n == highlighted, command.name());
                if n == highlighted && scroll {
                    label.scroll_to_me(None);
                }
                if let Some(key) = commands::shortcut_text(ui.ctx(), command) {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.weak(key));
                }
                label
            });
            if row.inner.clicked() {
                clicked = Some(command);
            }
        }
    });
    clicked
}

impl App {
    pub(super) fn palette_ui(&mut self, ctx: &egui::Context, view: &EngineView) {
        let Some(palette) = &self.palette else { return };
        let available = Command::ALL
            .into_iter()
            .filter(|&c| c != Command::CommandPalette && self.command_available(c));
        let matches = ranked(available, &palette.query);
        let Some(palette) = &mut self.palette else { return };

        // Taken before the search box sees them (it would move its cursor).
        let (up, down, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            )
        });
        if up {
            palette.highlighted = palette.highlighted.saturating_sub(1);
        }
        if down {
            palette.highlighted += 1;
        }
        palette.highlighted = palette.highlighted.min(matches.len().saturating_sub(1));
        let mut chosen = enter.then(|| matches.get(palette.highlighted).copied()).flatten();

        let response = egui::Modal::new(egui::Id::new("command_palette")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 420.0));
            let field = ui.add(
                egui::TextEdit::singleline(&mut palette.query)
                    .hint_text("Type a command...")
                    .desired_width(f32::INFINITY),
            );
            field.request_focus();
            if field.changed() {
                palette.highlighted = 0;
            }
            ui.add_space(4.0);
            if matches.is_empty() {
                ui.weak("No command matches.");
            }
            if let Some(command) = list_ui(ui, &matches, palette.highlighted, up || down) {
                chosen = Some(command);
            }
        });
        if let Some(command) = chosen {
            self.palette = None;
            self.run_command(ctx, command, view);
        } else if response.should_close() {
            self.palette = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_by_match() {
        assert_eq!(ranked(Command::ALL, "").len(), Command::ALL.len());
        assert_eq!(ranked(Command::ALL, "quit"), [Command::Quit]);
        assert_eq!(ranked(Command::ALL, "next")[0], Command::NextTrack);
        assert!(ranked(Command::ALL, "zzz").is_empty());
    }

    /// The modal's content height with `matches` listed, after a few
    /// frames to settle.
    fn listed_height(ctx: &egui::Context, matches: &[Command]) -> f32 {
        let mut height = 0.0;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                egui::Modal::new(egui::Id::new("test_palette")).show(ui.ctx(), |ui| {
                    list_ui(ui, matches, 0, false);
                    height = ui.min_rect().height();
                });
            });
            output.textures_delta.clear();
        }
        height
    }

    #[test]
    fn list_grows_back() {
        let ctx = egui::Context::default();
        let all = listed_height(&ctx, &Command::ALL);
        let few = listed_height(&ctx, &Command::ALL[..3]);
        assert!(few < all / 2.0, "{few} {all}");
        assert_eq!(listed_height(&ctx, &Command::ALL), all);
    }
}
