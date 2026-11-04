//! Help > Scripting Reference.

use eframe::egui;

use crate::lua_docs;

use super::{App, inline_code_job};
use super::widgets::Culled;

impl App {
    /// The scripting-API reference: one embedded, structured page (see
    /// `lua_docs.rs`) rather than a hosted wiki, the whole API is a few
    /// dozen functions, not a sprawling product.
    pub(super) fn docs_ui(&mut self, ui: &mut egui::Ui) {
        // A spot asked for from the editor: scrolled to once, highlighted
        // for a moment.
        const HIGHLIGHT_SECS: f32 = 2.5;
        let target = self.docs_target.as_mut();
        let (target_at, fade, mut scroll) = match target {
            Some((at, since, scroll)) => {
                let left = 1.0 - since.elapsed().as_secs_f32() / HIGHLIGHT_SECS;
                (Some(*at), left, std::mem::take(scroll))
            }
            None => (None, 0.0, false),
        };
        egui::ScrollArea::vertical().id_salt("docs_scroll").auto_shrink([false, false]).show(ui, |ui| {
            let mut culled = Culled::begin(ui, "docs_blocks");
            for (s, section) in lua_docs::sections().iter().enumerate() {
                ui.heading(section.title);
                for (b, block) in section.blocks.iter().enumerate() {
                    let target = target_at == Some((s, b));
                    let drawn = culled.block(ui, (s, b), target, |ui| {
                        ui.vertical(|ui| match block {
                            lua_docs::Block::P(text) => {
                                ui.label(inline_code_job(ui, "", text));
                            }
                            lua_docs::Block::Code(code) => {
                                ui.code(*code);
                            }
                            lua_docs::Block::Bullets(items) => {
                                for item in *items {
                                    ui.label(inline_code_job(ui, "- ", item));
                                }
                            }
                        })
                        .response
                    });
                    if target && let Some(response) = drawn {
                        if std::mem::take(&mut scroll) {
                            response.scroll_to_me(Some(egui::Align::TOP));
                        }
                        if fade > 0.0 {
                            let color = ui.visuals().selection.stroke.color.gamma_multiply(fade);
                            ui.painter().rect_stroke(response.rect.expand(3.0), 3.0, egui::Stroke::new(2.0, color), egui::StrokeKind::Outside);
                            ui.ctx().request_repaint();
                        }
                    }
                    ui.add_space(4.0);
                }
                ui.separator();
            }
            culled.end(ui);
        });
        if fade <= 0.0 && target_at.is_some() {
            self.docs_target = None;
        }
    }
}

