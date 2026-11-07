//! The dock: which sections are open, the saved layouts, and drawing the
//! tabs (`SectionTabs`).

use std::time::Instant;

use eframe::egui;
use egui_dock::{DockArea, DockState, TabViewer};

use crate::frame_timing;
use crate::audio::PlaybackShared;
use crate::layout::{self, Section};
use crate::lua_docs;

use super::{App, LAYOUT_AUTOSAVE_SECS, drag_handle, panel_modal, placeholder, square_corner, tour};

impl App {
    /// Force the visualizer open or closed, e.g. from a `--visualizer`
    /// startup flag.
    pub fn set_visualizer_open(&mut self, open: bool) {
        self.set_open(Section::Visualizer, open);
    }

    pub(super) fn is_open(&self, section: Section) -> bool {
        self.open_sections.contains(&section)
    }

    /// Show (near its home spot) or hide a section's tab.
    pub(super) fn set_open(&mut self, section: Section, open: bool) {
        if self.dock_in_use {
            self.pending_layout.push((section, open));
            return;
        }
        if open {
            layout::show(&mut self.dock, section);
            if section == Section::Editor && !self.editor_opened_this_run {
                self.editor_opened_this_run = true;
                layout::tab_alongside(&mut self.dock, Section::Reference, Section::Editor, false);
            }
        } else {
            layout::hide(&mut self.dock, section);
        }
        self.refresh_open_sections();
        self.save_layout();
    }

    /// Open `section` if it's closed, and make it the shown tab of its
    /// group; in a locked layout that hasn't got it, show it in the panel
    /// dialog instead (`panel_modal.rs`), leaving the layout as it is.
    pub(super) fn reveal_section(&mut self, section: Section) {
        if !self.mobile() && self.layout_locked() && !self.is_open(section) {
            self.open_panel_modal(panel_modal::PanelView::Section(section));
            return;
        }
        if self.dock_in_use {
            self.pending_layout.push((section, true));
            self.pending_focus = Some(section);
            return;
        }
        self.set_open(section, true);
        if let Some(path) = self.dock.find_tab(&section) {
            let _ = self.dock.set_active_tab(path);
            self.dock.set_focused_node_and_surface(path.node_path());
        }
    }

    /// The open section `step` places from the one in front (the focused
    /// tab), in `Section::ALL`'s order, round and round.
    pub(super) fn step_section(&mut self, step: isize) {
        let open: Vec<Section> = Section::ALL.into_iter().filter(|&s| self.is_open(s)).collect();
        let current = self.dock.find_active_focused().map(|(_, tab)| *tab);
        let Some(at) = current.and_then(|c| open.iter().position(|&s| s == c)).or((!open.is_empty()).then_some(0)) else {
            return;
        };
        let next = (at as isize + step).rem_euclid(open.len() as isize) as usize;
        self.reveal_section(open[next]);
    }

    /// Show where `name` is documented in the Scripting Reference.
    pub(super) fn open_reference(&mut self, name: &str) -> bool {
        let Some(target) = lua_docs::find(name) else { return false };
        self.docs_target = Some((target, Instant::now(), true));
        self.reveal_section(Section::Reference);
        true
    }

    pub(super) fn refresh_open_sections(&mut self) {
        self.open_sections =
            Section::ALL.into_iter().filter(|&s| layout::is_open(&self.dock, s)).collect();
    }

    /// Persist the layout. Failure only costs remembering it next launch,
    /// so it's reported but not fatal.
    pub(super) fn save_layout(&mut self) {
        self.saved_layout_json = serde_json::to_string(&self.dock).unwrap_or_default();
        self.config.layout = Some(self.dock.clone());
        self.store_in_active_layout();
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save layout: {e}");
        }
    }

    /// The named layout the showing slot is on, if any.
    pub(super) fn active_layout(&self) -> Option<&String> {
        self.config.active_layout.as_ref()
    }

    pub(super) fn set_active_layout(&mut self, name: Option<String>) {
        self.config.active_layout = name;
    }

    /// Changes to the arrangement go into the active named layout.
    pub(super) fn store_in_active_layout(&mut self) {
        let Some(name) = self.active_layout().cloned() else { return };
        if let Some(saved) = self.config.layout_presets.iter_mut().find(|p| p.name == name) {
            saved.dock = self.dock.clone();
        }
    }

    /// Save the layout if it's changed (tabs dragged, dividers moved),
    /// checked about once a second, and never mid-drag.
    pub(super) fn autosave_layout(&mut self, ctx: &egui::Context) {
        let (now, dragging) = ctx.input(|i| (i.time, i.pointer.any_down()));
        if dragging || now - self.last_layout_check < LAYOUT_AUTOSAVE_SECS {
            return;
        }
        self.last_layout_check = now;
        if serde_json::to_string(&self.dock).unwrap_or_default() != self.saved_layout_json {
            self.save_layout();
        }
    }

    /// Show the named layout (as the user last left it), after keeping the
    /// current arrangement in the layout it belongs to.
    pub(super) fn switch_layout(&mut self, name: &str) {
        if self.dock_in_use {
            return;
        }
        self.save_layout();
        let Some(saved) = self.config.layout_presets.iter().find(|p| p.name == name) else { return };
        let dock = layout::sanitize(saved.dock.clone());
        self.set_active_layout(Some(name.to_string()));
        self.apply_layout(dock);
        self.status = format!("Layout: {name}.");
    }

    /// Switch the arrangement to `dock` (in whichever slot is showing: the
    /// windowed layout, or fullscreen's own) and remember it.
    pub(super) fn apply_layout(&mut self, dock: egui_dock::DockState<Section>) {
        if self.dock_in_use {
            return;
        }
        self.dock = dock;
        if self.is_open_in_dock(Section::Editor) {
            // It already has the reference where the layout put it.
            self.editor_opened_this_run = true;
        }
        self.refresh_open_sections();
        self.save_layout();
    }

    pub(super) fn is_open_in_dock(&self, section: Section) -> bool {
        layout::is_open(&self.dock, section)
    }

    /// The section tabs, filling everything between the menu bar and the
    /// controls. With every tab closed, a pointer to the View menu instead.
    pub(super) fn dock_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        if self.open_sections.is_empty() {
            egui::CentralPanel::default().show(ui, |ui| {
                placeholder(ui, "Nothing open.\nUse the View menu at the top to show Songs (pick something to play), Playlists, the Visualizer, and more.");
            });
            return;
        }

        // DockArea needs `&mut` to the layout while the tabs it draws need
        // `&mut self`, so the layout is moved out for the duration.
        let mut dock = std::mem::replace(&mut self.dock, DockState::new(Vec::new()));
        self.dock_in_use = true;
        // A section alone in its spot has no tab bar: its top edge (with a
        // handle drawn on it) drags it instead.
        // The open tab joins its body, so where it's the first one the
        // body's corner under it is square.
        // A locked layout shows no handles, and its panels neither move
        // nor resize: what dragging a divider did is put back after.
        let locked = self.layout_locked();
        let (mut alone, mut joined) = (Vec::new(), Vec::new());
        for (_, leaf) in dock.iter_leaves_mut() {
            leaf.tab_bar_hidden = leaf.len() == 1;
            if leaf.tab_bar_hidden {
                alone.extend(leaf.tabs().first().copied().filter(|_| !locked));
            } else if leaf.active.0 == 0 {
                joined.extend(leaf.tabs().first().copied());
            }
        }
        let fractions = |dock: &mut DockState<Section>| -> Vec<f32> {
            dock.iter_all_nodes_mut()
                .filter_map(|(_, node)| match node {
                    egui_dock::Node::Horizontal(split) | egui_dock::Node::Vertical(split) => Some(split.fraction),
                    _ => None,
                })
                .collect()
        };
        let before = if locked { fractions(&mut dock) } else { Vec::new() };
        let top = if locked { f32::from(crate::theme::DOCK_MARGIN) } else { crate::theme::DOCK_HANDLE };
        let mut tabs = SectionTabs { app: self, shared, alone, joined, top };
        // (Nothing over the panels: the title bar has room below its buttons.)
        let frame = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 8 });
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            DockArea::new(&mut dock)
                .id(egui::Id::unique("section_dock"))
                .style({
                    let mut style = crate::theme::dock_style(ui.style());
                    if locked {
                        style.separator.color_hovered = egui::Color32::TRANSPARENT;
                        style.separator.color_dragged = egui::Color32::TRANSPARENT;
                        // (No handles, so no room for them.)
                        style.tab.tab_body.inner_margin.top = crate::theme::DOCK_MARGIN;
                    }
                    style
                })
                .hidable_tab_bars(true)
                .draggable_tabs(!locked)
                .show_add_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut tabs);
        });
        if locked {
            let splits = dock.iter_all_nodes_mut().filter_map(|(_, node)| match node {
                egui_dock::Node::Horizontal(split) | egui_dock::Node::Vertical(split) => Some(split),
                _ => None,
            });
            for (split, fraction) in splits.zip(before) {
                split.fraction = fraction;
            }
        }
        self.dock_in_use = false;
        self.dock = dock;

        self.refresh_open_sections();
        for (section, open) in std::mem::take(&mut self.pending_layout) {
            self.set_open(section, open);
        }
        if let Some(section) = self.pending_focus.take() {
            self.reveal_section(section);
        }
    }

    /// The editor tab: plus the script bar when the visualizer (which
    /// normally hosts it) is closed.
    pub(super) fn editor_section_ui(&mut self, ui: &mut egui::Ui) {
        if !self.is_open(Section::Visualizer) {
            self.script_bar_ui(ui, false);
        }
        self.script_error_ui(ui, false);
        self.visualizer_editor_ui(ui);
    }
}

/// Draws each section's tab for `DockArea`.
pub(super) struct SectionTabs<'a> {
    app: &'a mut App,
    shared: &'a PlaybackShared,
    /// The sections alone in their spots, so without a tab bar.
    alone: Vec<Section>,
    /// The sections open as the first of their spots' tabs.
    joined: Vec<Section>,
    /// How tall the top margin of the sections' bodies is.
    top: f32,
}

impl TabViewer for SectionTabs<'_> {
    type Tab = Section;

    fn id(&mut self, tab: &mut Section) -> egui::Id {
        egui::Id::unique(("section_tab", *tab))
    }

    fn title(&mut self, tab: &mut Section) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Section) {
        let app = &mut *self.app;
        let _timing = frame_timing::scope(tab.title());
        app.tour.mark(tour::Target::Tab(*tab), ui.max_rect());
        if self.alone.contains(tab) {
            drag_handle(ui);
        } else if self.joined.contains(tab) {
            square_corner(ui, self.top);
        }
        app.section_ui(ui, *tab, self.shared);
    }

    /// Sections are shown and hidden from View (and the transport's
    /// toggles), not closed from their tabs.
    fn is_closeable(&self, _tab: &Section) -> bool {
        false
    }

    /// Every section scrolls its own content where it needs to; a second,
    /// outer scroll area from the dock would just fight it.
    fn scroll_bars(&self, _tab: &Section) -> [bool; 2] {
        [false, false]
    }
}

impl App {
    /// A section's contents, filling `ui`: in its tab, or in the panel
    /// dialog (`panel_modal.rs`).
    pub(super) fn section_ui(&mut self, ui: &mut egui::Ui, section: Section, shared: &PlaybackShared) {
        let app = self;
        match section {
            Section::Songs => app.songs_ui(ui),
            Section::Groupings => app.groupings_ui(ui),
            Section::Soundfonts => app.soundfont_ui(ui),
            Section::Queue => app.queue_ui(ui),
            Section::Lyrics => app.lyrics_ui(ui, &shared.view),
            Section::Wishlist => app.wishlist_ui(ui),
            Section::Apps => app.apps_ui(ui),
            Section::Visualizer => {
                app.visualizer_preview_ui(ui, shared.notes.clone(), shared.view.clone());
            }
            Section::Editor => app.editor_section_ui(ui),
            // (Dropped from layouts as they load.)
            Section::Removed => {}
            Section::Reference => app.docs_ui(ui),
            Section::Sprites => app.sprite_editor_ui(ui),
            Section::Library => app.library_ui(ui),
        }
    }
}
