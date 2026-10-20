//! Toasts: short notes shown a moment over the window. Every toast is drawn
//! by [`draw`], on one line per line of its text: never wrapped, so a line
//! breaks only where the text has a `\n`.
//!
//! Also the note of what an icon-only button just set ("Loop: all"): the
//! icon alone doesn't say.

use std::time::{Duration, Instant};

use eframe::egui;

/// How long a button's note shows.
const SHOWN: Duration = Duration::from_millis(1500);

/// Draw a toast of `text` (named `id`, at `anchor` moved by `offset`), its
/// lines as they are; whether it was clicked (to put it away).
pub(super) fn draw(ctx: &egui::Context, id: &str, anchor: egui::Align2, offset: egui::Vec2, text: egui::RichText) -> bool {
    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Tooltip)
        .sense(egui::Sense::click())
        .anchor(anchor, offset)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.add(egui::Label::new(text).extend());
            });
        })
        .response
        .clicked()
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
    let left = SHOWN.checked_sub(since.elapsed());
    if left.is_none() || draw(ctx, "button_toast", egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -96.0), text.into()) {
        ctx.data_mut(|d| d.remove::<(String, Instant)>(button_id()));
        return;
    }
    ctx.request_repaint_after(left.unwrap_or_default());
}
