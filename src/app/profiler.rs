//! The profiler overlay (Help > Profiler, or Profiler in the phone's
//! menu): puffin's view of the last frames, timed in the same sections as
//! the slow-frame log (`frame_timing`). Profiling runs only while it's
//! open.

use eframe::egui;

/// Opens or closes the overlay.
pub(super) fn set_open(open: bool) {
    puffin::set_scopes_on(open);
}

pub(super) fn is_open() -> bool {
    puffin::are_scopes_on()
}

/// The overlay, if it's open: a window the screen's width at most, so it
/// fits a phone too.
pub(super) fn window(ctx: &egui::Context) {
    if !is_open() {
        return;
    }
    let screen = ctx.content_rect();
    let room = super::window_room(ctx);
    let mut open = true;
    egui::Window::new("Profiler")
        .open(&mut open)
        .default_size([(screen.width() - 32.0).min(1024.0), (screen.height() * 0.6).min(600.0)])
        .max_size(room.size())
        .constrain_to(room)
        // (Scrolling sideways inside it: its toolbar is wider than a phone.)
        .show(ctx, |ui| {
            egui::ScrollArea::horizontal().show(ui, puffin_egui::profiler_ui);
        });
    if !open {
        set_open(false);
    }
    // Live: the app otherwise only draws when something changes.
    ctx.request_repaint();
}
