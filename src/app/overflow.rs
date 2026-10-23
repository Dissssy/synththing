//! In the mobile layout, notes in the log (Help > Log) anything drawn past
//! the screen's edge, so a report with its log says what didn't fit and
//! where. Looks once a second, at the frame before, and notes each widget
//! once (and only so many in all, so the log isn't flooded).

use std::collections::HashSet;

use eframe::egui;

/// How often to look, in seconds.
const EVERY: f64 = 1.0;
/// How far past the edge counts, in points (rounding aside).
const SLACK: f32 = 1.0;
/// The most widgets noted in a run.
const MOST: usize = 50;

#[derive(Default)]
pub(super) struct Overflow {
    looked: f64,
    noted: HashSet<egui::Id>,
    /// The page the frame before was drawn on: what's looked at.
    drawn: &'static str,
}

impl Overflow {
    /// Look at what was drawn last frame, and log what ran off the screen
    /// (that wasn't noted before). `page` is the one this frame draws: the
    /// widgets looked at are the last frame's, so they're logged under the
    /// page they were on (not the one a tap just went to).
    pub(super) fn check(&mut self, ctx: &egui::Context, page: &'static str) {
        let page = std::mem::replace(&mut self.drawn, page);
        let now = ctx.input(|i| i.time);
        if now - self.looked < EVERY || self.noted.len() >= MOST {
            return;
        }
        self.looked = now;
        let screen = ctx.content_rect();
        let widgets: Vec<egui::WidgetRect> = ctx.viewport(|vp| {
            let widgets = &vp.prev_pass.widgets;
            widgets.layer_ids().flat_map(|layer| widgets.get_layer(layer)).copied().collect()
        });
        let mut found = Vec::new();
        for widget in widgets {
            if widget.layer_id.order == egui::Order::Debug {
                continue;
            }
            let (mut rect, mut shown) = (widget.rect, widget.interact_rect);
            if let Some(transform) = ctx.layer_transform_to_global(widget.layer_id) {
                (rect, shown) = (transform * rect, transform * shown);
            }
            // Past a side, and cut there by the screen itself, not by
            // something it's in (a scroll area's hidden part doesn't
            // count). Only sideways: pages scroll up and down.
            let left = screen.left() - rect.left();
            let right = rect.right() - screen.right();
            let (side, worst) = if left > right { ("left", left) } else { ("right", right) };
            let cut_by_screen = match side {
                "left" => shown.left() <= screen.left() + SLACK,
                _ => shown.right() >= screen.right() - SLACK,
            };
            if worst > SLACK && shown.is_positive() && cut_by_screen {
                found.push((widget, rect, side, worst));
            }
        }
        // A container grows with what's in it: only what's innermost is
        // noted (with the id of the container it's in).
        let innermost: Vec<_> = found
            .iter()
            .filter(|(w, rect, ..)| !found.iter().any(|(o, other, ..)| o.id != w.id && rect.contains_rect(*other) && rect != other))
            .collect();
        for &&(widget, rect, side, worst) in &innermost {
            if self.noted.len() >= MOST || !self.noted.insert(widget.id) {
                continue;
            }
            let kind = if widget.sense.senses_click() || widget.sense.senses_drag() { "a button or other control" } else { "text or a picture" };
            let layer = match widget.layer_id.order {
                egui::Order::Background => "the background",
                egui::Order::Middle => "the page",
                egui::Order::Foreground => "a window or panel over the page",
                egui::Order::Tooltip | egui::Order::Debug => "a popup, menu or toast",
            };
            log::warn!(
                "overflow: on {page}, {kind} on {layer} runs {worst:.0} pt off the {side} of the {:.0} pt wide screen \
                 (at x {:.0}..{:.0}, y {:.0}; widget {:?}, in {:?})",
                screen.width(),
                rect.left(),
                rect.right(),
                rect.top(),
                widget.id,
                widget.parent_id,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame of `draw` on a 200x400 screen.
    fn frame(ctx: &egui::Context, draw: impl FnMut(&mut egui::Ui)) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 400.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, draw);
        output.textures_delta.clear();
    }

    #[test]
    fn notes_what_runs_off_the_screen_once() {
        let ctx = egui::Context::default();
        let mut overflow = Overflow { looked: f64::NEG_INFINITY, ..Overflow::default() };
        let wide = |ui: &mut egui::Ui| {
            ui.horizontal(|ui| {
                ui.label("fits");
                ui.add(egui::Label::new("a line far too long for a screen only two hundred points wide").extend());
            });
        };
        frame(&ctx, wide);
        overflow.check(&ctx, "Songs");
        assert_eq!(overflow.noted.len(), 1, "just the long label");
        overflow.looked = f64::NEG_INFINITY;
        frame(&ctx, wide);
        overflow.check(&ctx, "Songs");
        assert_eq!(overflow.noted.len(), 1, "noted once");
    }

    #[test]
    fn what_a_scroll_area_hides_is_not_noted() {
        let ctx = egui::Context::default();
        let mut overflow = Overflow { looked: f64::NEG_INFINITY, ..Overflow::default() };
        let scrolled = |ui: &mut egui::Ui| {
            ui.set_max_width(150.0);
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.add(egui::Label::new("a line far too long for a screen only two hundred points wide").extend());
            });
        };
        frame(&ctx, scrolled);
        overflow.check(&ctx, "Log");
        assert!(overflow.noted.is_empty());
    }
}
