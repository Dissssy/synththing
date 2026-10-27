//! Menus that stay open while Shift is held: a click in one with Shift
//! down (ticking several columns, say) leaves it open for the next. A
//! click outside it, or Esc, still closes it.

use eframe::egui;
use egui::containers::menu::MenuConfig;

/// How a menu closes: on any click, or with Shift held, only on a click
/// outside it.
pub fn config(ctx: &egui::Context) -> MenuConfig {
    let behavior = if ctx.input(|i| i.modifiers.shift) { egui::PopupCloseBehavior::CloseOnClickOutside } else { egui::PopupCloseBehavior::CloseOnClick };
    MenuConfig::new().close_behavior(behavior)
}

/// `response`'s right-click menu, as [`egui::Response::context_menu`]
/// shows it, staying open on a Shift+click.
pub fn context<R>(response: &egui::Response, contents: impl FnOnce(&mut egui::Ui) -> R) -> Option<egui::InnerResponse<R>> {
    egui::Popup::context_menu(response).close_behavior(config(&response.ctx).close_behavior).show(contents)
}
