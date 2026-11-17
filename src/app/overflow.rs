//! In the mobile layout, notes in the log (Help > Log) anything drawn past
//! the screen's edge, so a report with its log says what didn't fit and
//! where. Looks once a second, at the frame before, and notes each widget
//! once (and only so many in all, so the log isn't flooded). Only a frame
//! before that the mobile layout drew, on a screen the same size, is looked
//! at: the last desktop frame, or one drawn before the window or phone
//! turned or resized, didn't have to fit this screen.

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
    /// The last frame the mobile layout drew: what's looked at.
    drawn: Option<Drawn>,
}

/// A frame the mobile layout drew.
#[derive(Clone, Copy)]
struct Drawn {
    /// Which pass it was (`Context::cumulative_pass_nr`).
    pass: u64,
    page: &'static str,
    screen: egui::Rect,
}

impl Overflow {
    /// Look at what was drawn last frame, and log what ran off the screen
    /// (that wasn't noted before). `page` is the one this frame draws: the
    /// widgets looked at are the last frame's, so they're logged under the
    /// page they were on (not the one a tap just went to). Call it at the
    /// start of each pass the mobile layout draws.
    pub(super) fn check(&mut self, ctx: &egui::Context, page: &'static str) {
        let screen = ctx.content_rect();
        let pass = ctx.cumulative_pass_nr();
        let Some(before) = self.drawn.replace(Drawn { pass, page, screen }) else { return };
        if before.pass + 1 != pass || before.screen != screen {
            return;
        }
        let page = before.page;
        let now = ctx.input(|i| i.time);
        if now - self.looked < EVERY || self.noted.len() >= MOST {
            return;
        }
        self.looked = now;
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
                page, kind, layer, side;
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

    /// One frame of `draw` on a `width`x400 screen, looked at first as the
    /// mobile layout does.
    fn frame(ctx: &egui::Context, overflow: &mut Overflow, width: f32, mut draw: impl FnMut(&mut egui::Ui)) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 400.0))),
            ..Default::default()
        };
        overflow.looked = f64::NEG_INFINITY;
        let mut output = ctx.run_ui(input, |ui| {
            overflow.check(ui.ctx(), "Songs");
            draw(ui);
        });
        output.textures_delta.clear();
    }

    fn wide(ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("fits");
            ui.add(egui::Label::new("a line far too long for a screen only two hundred points wide").extend());
        });
    }

    #[test]
    fn notes_what_runs_off_the_screen_once() {
        let ctx = egui::Context::default();
        let mut overflow = Overflow::default();
        frame(&ctx, &mut overflow, 200.0, wide);
        assert!(overflow.noted.is_empty(), "nothing drawn before to look at");
        frame(&ctx, &mut overflow, 200.0, wide);
        assert_eq!(overflow.noted.len(), 1, "just the long label");
        frame(&ctx, &mut overflow, 200.0, wide);
        assert_eq!(overflow.noted.len(), 1, "noted once");
    }

    #[test]
    fn a_frame_from_a_wider_screen_is_not_looked_at() {
        let ctx = egui::Context::default();
        let mut overflow = Overflow::default();
        // Drawn on a wide window, which then shrinks to a phone's size (the
        // desktop switching to the mobile layout).
        frame(&ctx, &mut overflow, 1000.0, |ui| {
            ui.add_sized([900.0, 20.0], egui::Button::new("wide"));
        });
        frame(&ctx, &mut overflow, 1000.0, |ui| {
            ui.add_sized([900.0, 20.0], egui::Button::new("wide"));
        });
        frame(&ctx, &mut overflow, 200.0, |_| {});
        assert!(overflow.noted.is_empty());
    }

    #[test]
    fn a_frame_the_mobile_layout_did_not_draw_is_not_looked_at() {
        let ctx = egui::Context::default();
        let mut overflow = Overflow::default();
        frame(&ctx, &mut overflow, 200.0, |_| {});
        // The desktop layout draws a frame (no look), then mobile again.
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 400.0))),
                ..Default::default()
            },
            wide,
        );
        output.textures_delta.clear();
        frame(&ctx, &mut overflow, 200.0, |_| {});
        assert!(overflow.noted.is_empty());
    }

    #[test]
    fn what_a_scroll_area_hides_is_not_noted() {
        let ctx = egui::Context::default();
        let mut overflow = Overflow::default();
        let scrolled = |ui: &mut egui::Ui| {
            ui.set_max_width(150.0);
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.add(egui::Label::new("a line far too long for a screen only two hundred points wide").extend());
            });
        };
        frame(&ctx, &mut overflow, 200.0, scrolled);
        frame(&ctx, &mut overflow, 200.0, scrolled);
        assert!(overflow.noted.is_empty());
    }
}
