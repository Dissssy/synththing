//! A panel's contents in a dialog over the layout (`App::open_panel_modal`),
//! for showing something that lives in a panel without changing the
//! layout: a locked one stays as it is, and an unlocked one doesn't gain a
//! tab nobody asked for. Every panel can be shown whole (what its tab
//! shows); some have narrower views, of one thing in them.

use eframe::egui;

use super::{fit_width, App};
use crate::audio::PlaybackShared;
use crate::layout::Section;

/// What the dialog shows.
#[derive(Clone, Copy)]
pub(super) enum PanelView {
    /// A panel, whole.
    Section(Section),
    /// One Online Library script's details: its preview, what it is, and
    /// what can be done with it.
    LibraryScript,
}

/// What a view inside the dialog asks of it.
pub(super) enum PanelChange {
    /// Show this instead.
    Show(PanelView),
    Close,
}

impl PanelView {
    fn title(&self) -> &'static str {
        match self {
            Self::Section(section) => section.title(),
            Self::LibraryScript => Section::Library.title(),
        }
    }
}

/// How big the dialog's body is, at most: panels fill the room they get,
/// so it's given a fixed size, less on a small screen.
const WIDTH: f32 = 720.0;
const HEIGHT: f32 = 600.0;

impl App {
    /// Show `view` in the dialog (over whatever it showed).
    pub(super) fn open_panel_modal(&mut self, view: PanelView) {
        self.panel_modal = Some(view);
    }

    /// Show one Online Library script's details in the dialog.
    pub(super) fn show_library_script(&mut self, id: String) {
        self.library_open_script(id);
        self.open_panel_modal(PanelView::LibraryScript);
    }

    /// The dialog, while it's open.
    pub(super) fn panel_modal_ui(&mut self, ctx: &egui::Context, shared: &PlaybackShared) {
        let Some(mut view) = self.panel_modal.take() else { return };
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("panel_modal")).show(ctx, |ui| {
            let width = fit_width(ui, WIDTH);
            let height = HEIGHT.min(ctx.content_rect().height() - 160.0).max(160.0);
            ui.set_width(width);
            ui.heading(view.title());
            ui.separator();
            ui.allocate_ui_with_layout(egui::vec2(width, height), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_min_size(egui::vec2(width, height));
                match view {
                    PanelView::Section(section) => self.section_ui(ui, section, shared),
                    PanelView::LibraryScript => {
                        let next = egui::ScrollArea::vertical()
                            .id_salt("panel_modal_script")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.library_script_ui(ui))
                            .inner;
                        match next {
                            Some(PanelChange::Show(next)) => view = next,
                            Some(PanelChange::Close) => close = true,
                            None => {}
                        }
                    }
                }
            });
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                close |= ui.button("Close").clicked();
            });
        });
        // (Unless what it showed opened something else in it.)
        if !(close || response.should_close()) && self.panel_modal.is_none() {
            self.panel_modal = Some(view);
        }
    }
}
