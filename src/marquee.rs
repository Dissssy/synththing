//! Text that scrolls sideways when it's too long for its room.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use eframe::egui;

/// The speed, in points a second, and the gap between the end of the text
/// and its next start.
const SPEED: f32 = 40.0;
const GAP: f32 = 48.0;
/// How long the text rests at its start, in seconds, before each pass.
const PAUSE: f64 = 1.5;
/// How often a scrolling text is redrawn.
const FRAME: Duration = Duration::from_millis(33);

/// `text` on one line, in the width left in `ui`. Text that fits is drawn
/// as a label is; text that doesn't scrolls sideways while `scroll` is on
/// (from its start, after a rest, each time it's turned on or the text
/// changes) and is cut
/// short with "..." otherwise. A scrolling text asks for a redraw only
/// when it next moves, not every frame.
pub fn show(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, scroll: bool) -> egui::Response {
    if !scroll {
        return still(ui, egui::Label::new(text).truncate());
    }
    let galley = text
        .into()
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
    let width = ui.available_width();
    if galley.size().x <= width {
        return still(ui, egui::Label::new(galley));
    }

    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, galley.size().y), egui::Sense::hover());
    let now = ui.input(|i| i.time);
    let text = {
        let mut hasher = DefaultHasher::new();
        galley.text().hash(&mut hasher);
        hasher.finish()
    };
    let start = ui.data_mut(|d| {
        let (shown, start) = d.get_temp_mut_or_insert_with(response.id, || (text, now));
        if *shown != text {
            (*shown, *start) = (text, now);
        }
        *start
    });
    let period = galley.size().x + GAP;
    let cycle = PAUSE + f64::from(period / SPEED);
    let into_cycle = (now - start) % cycle;
    let offset = ((into_cycle - PAUSE).max(0.0) as f32) * SPEED;
    let color = ui.visuals().text_color();
    let painter = ui.painter_at(rect);
    painter.galley(rect.left_top() - egui::vec2(offset, 0.0), galley.clone(), color);
    painter.galley(rect.left_top() + egui::vec2(period - offset, 0.0), galley, color);
    let next = if into_cycle < PAUSE { Duration::from_secs_f64(PAUSE - into_cycle) } else { FRAME };
    ui.ctx().request_repaint_after(next);
    response
}

/// How long a text `overflow` points wider than its room takes, scrolling,
/// to show its end (and rest there a moment).
pub fn read_time(overflow: f32) -> Duration {
    Duration::from_secs_f64(2.0 * PAUSE) + Duration::from_secs_f32(overflow.max(0.0) / SPEED)
}

/// A label that isn't scrolling, which starts the next scroll over.
fn still(ui: &mut egui::Ui, label: egui::Label) -> egui::Response {
    let response = ui.add(label.selectable(false));
    ui.data_mut(|d| d.remove::<(u64, f64)>(response.id));
    response
}
