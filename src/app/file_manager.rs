//! Handing files and folders to the system file manager (Explorer, or
//! whichever one the desktop runs), through `opener`: "Show in folder"
//! for a file, selected, and opening a folder. Desktop only: Android has
//! no file manager to hand off to.

use eframe::egui;
use std::path::Path;

/// Show `path` in the system file manager, selected in its folder.
pub(super) fn show_in_folder(path: &Path) {
    if let Err(e) = opener::reveal(path) {
        log::warn!("couldn't show {} in the file manager: {e}", path.display());
    }
}

/// Open the folder `dir` in the system file manager, making it first.
pub(super) fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    if let Err(e) = opener::open(dir) {
        log::warn!("couldn't open {} in the file manager: {e}", dir.display());
    }
}

/// A "Show in folder" menu entry for the file at `path`: it shows it,
/// selected, in the system file manager. Disabled while there's no such
/// file (a song on another device, a soundfont moved away).
pub(super) fn show_in_folder_button(ui: &mut egui::Ui, path: &Path) {
    let here = path.exists();
    if ui
        .add_enabled(here, egui::Button::new("Show in folder"))
        .on_hover_text(path.display().to_string())
        .on_disabled_hover_text("The file isn't on this device")
        .clicked()
    {
        show_in_folder(path);
    }
}
