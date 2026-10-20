//! A moment's note of what an icon-only button just set ("Loop: all"),
//! low over the window: the icon alone doesn't say.

use std::time::{Duration, Instant};

use eframe::egui;

/// How long a note shows.
const SHOWN: Duration = Duration::from_millis(1500);

fn id() -> egui::Id {
    egui::Id::new("button_toast")
}

/// Show `text`, in place of any note still showing.
pub(super) fn show(ctx: &egui::Context, text: impl Into<String>) {
    ctx.data_mut(|d| d.insert_temp(id(), (text.into(), Instant::now())));
}

/// Draw the note showing, if any; once a frame.
pub(super) fn ui(ctx: &egui::Context) {
    let Some((text, since)) = ctx.data(|d| d.get_temp::<(String, Instant)>(id())) else { return };
    let Some(left) = SHOWN.checked_sub(since.elapsed()) else {
        ctx.data_mut(|d| d.remove::<(String, Instant)>(id()));
        return;
    };
    ctx.request_repaint_after(left);
    egui::Area::new(id())
        .order(egui::Order::Tooltip)
        .interactable(false)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -96.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(text);
            });
        });
}
