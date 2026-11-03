//! Help > Scripting Reference.

use eframe::egui;

use crate::lua_docs;

use super::{App, inline_code_job};

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
            // (A block out of sight only takes its space, as measured when
            // it was last drawn at this width: laying out the whole
            // reference cost 30-45 ms every frame.)
            let heights_id = ui.scope_id().with("docs_heights");
            let width = ui.available_width();
            let mut heights: (f32, std::collections::HashMap<(usize, usize), f32>) = ui.data(|d| d.get_temp(heights_id)).unwrap_or_default();
            if heights.0 != width {
                heights = (width, Default::default());
            }
            for (s, section) in lua_docs::sections().iter().enumerate() {
                ui.heading(section.title);
                for (b, block) in section.blocks.iter().enumerate() {
                    let top = ui.cursor().min.y;
                    if target_at != Some((s, b))
                        && let Some(&height) = heights.1.get(&(s, b))
                        && !ui.is_rect_visible(egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + height))
                    {
                        ui.add_space(height);
                        ui.add_space(4.0);
                        continue;
                    }
                    let response = ui
                        .vertical(|ui| match block {
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
                        .response;
                    heights.1.insert((s, b), ui.cursor().min.y - top);
                    if target_at == Some((s, b)) {
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
            ui.data_mut(|d| d.insert_temp(heights_id, heights));
        });
        if fade <= 0.0 && target_at.is_some() {
            self.docs_target = None;
        }
    }
}

