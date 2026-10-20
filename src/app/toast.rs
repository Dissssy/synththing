//! Toasts: short notes shown a moment over the window. Every toast is drawn
//! by [`draw`], on one line per line of its text: never wrapped, so a line
//! breaks only where the text has a `\n`. A line too long for the window
//! scrolls sideways (see [`crate::marquee`]), and the toast stays up until
//! it has scrolled through.
//!
//! Also the note of what an icon-only button just set ("Loop: all"): the
//! icon alone doesn't say.

use std::time::{Duration, Instant};

use eframe::egui;

/// How long a button's note shows.
const SHOWN: Duration = Duration::from_millis(1500);

/// The room left between a toast and the window's sides.
const MARGIN: f32 = 16.0;

/// Draw a toast of `text` (named `id`, at `anchor` moved by `offset`, in
/// `color` or the usual text color), shown `since`, for `least` or as long
/// as its longest line takes to scroll through; whether to put it away
/// (that time is up, or it was clicked).
#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    ctx: &egui::Context,
    id: &str,
    anchor: egui::Align2,
    offset: egui::Vec2,
    text: &str,
    color: Option<egui::Color32>,
    since: Instant,
    least: Duration,
) -> bool {
    let mut shown_for = least;
    let response = egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Tooltip)
        .sense(egui::Sense::click())
        .anchor(anchor, offset)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let frame = ui.spacing().window_margin.sum().x;
                let room = ctx.content_rect().width() - 2.0 * MARGIN - frame;
                ui.set_max_width(room);
                ui.spacing_mut().item_spacing.y = 0.0;
                for line in text.lines() {
                    let line = egui::RichText::new(line);
                    let line = match color {
                        Some(color) => line.color(color),
                        None => line,
                    };
                    let width = egui::WidgetText::from(line.clone())
                        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body)
                        .size()
                        .x;
                    if width > room {
                        shown_for = shown_for.max(crate::marquee::read_time(width - room));
                    }
                    crate::marquee::show(ui, line, true);
                }
            });
        })
        .response;
    let Some(left) = shown_for.checked_sub(since.elapsed()) else { return true };
    ctx.request_repaint_after(left);
    response.clicked()
}

fn button_id() -> egui::Id {
    egui::Id::new("button_toast")
}

/// Show `text` as a button's note, in place of any still showing.
pub(super) fn show(ctx: &egui::Context, text: impl Into<String>) {
    ctx.data_mut(|d| d.insert_temp(button_id(), (text.into(), Instant::now())));
}

/// Draw the button's note showing, if any; once a frame.
pub(super) fn ui(ctx: &egui::Context) {
    let Some((text, since)) = ctx.data(|d| d.get_temp::<(String, Instant)>(button_id())) else { return };
    if draw(ctx, "button_toast", egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -96.0), &text, None, since, SHOWN) {
        ctx.data_mut(|d| d.remove::<(String, Instant)>(button_id()));
    }
}
