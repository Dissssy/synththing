//! The page of things to go through before the rest of the app: what the
//! launch has to say or ask (settings that couldn't be read, the welcome,
//! usage data, what's new, an update, installing), and Help's Welcome,
//! Updates and Changelog. Drawn as Preferences is, in place of the panels
//! (the whole screen on a phone), with the rest of the app disabled until
//! it's done: one step per page, pills down the side, Back and Next at the
//! bottom. Answers are kept until Done, which applies them all.

use eframe::egui;

use super::{App, changelog_body_ui, icon, open_in_file_manager, row_icon_button, setting_row, settings_group};
use crate::changelog;

/// One page of the flow. They're always in this order: installing last,
/// since it restarts the app.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    SetAside,
    Welcome,
    UsageData,
    /// What's new since a version, or (`None`) the whole changelog.
    Changelog(Option<changelog::Version>),
    Update,
    #[cfg(windows)]
    Install,
}

impl Step {
    fn rank(self) -> u8 {
        match self {
            Step::SetAside => 0,
            Step::Welcome => 1,
            Step::UsageData => 2,
            Step::Changelog(_) => 3,
            Step::Update => 4,
            #[cfg(windows)]
            Step::Install => 5,
        }
    }

    /// Its pill's name and icon.
    fn name(self) -> &'static str {
        match self {
            Step::SetAside => "Settings files",
            Step::Welcome => "Welcome",
            Step::UsageData => "Usage data",
            Step::Changelog(Some(_)) => "What's new",
            Step::Changelog(None) => "Changelog",
            Step::Update => "Updates",
            #[cfg(windows)]
            Step::Install => "Install",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Step::SetAside => "warning",
            Step::Welcome => "hand-waving",
            Step::UsageData => "chart-bar",
            Step::Changelog(_) => "sparkle",
            Step::Update => "arrow-circle-up",
            #[cfg(windows)]
            Step::Install => "download-simple",
        }
    }

    /// The page's heading.
    fn heading(self) -> &'static str {
        match self {
            Step::SetAside => "Some settings couldn't be read",
            Step::Welcome => "Welcome to synththing",
            Step::UsageData => "Help find problems?",
            Step::Changelog(Some(_)) => "What's new",
            Step::Changelog(None) => "Changelog",
            Step::Update => "Updates",
            #[cfg(windows)]
            Step::Install => "Install synththing?",
        }
    }
}

#[derive(Default)]
pub(super) struct Flow {
    /// What's to go through, in `Step::rank` order. Empty: the flow's
    /// closed.
    steps: Vec<Step>,
    /// The one showing.
    index: usize,
    /// It's been on screen: a step added before the one showing from then
    /// on keeps that one showing (before, the flow starts at the first).
    shown: bool,
}

impl Flow {
    pub(super) fn open(&self) -> bool {
        !self.steps.is_empty()
    }

    pub(super) fn has(&self, step: Step) -> bool {
        self.steps.iter().any(|s| s.rank() == step.rank())
    }

    /// Add a step (in its place; one of the same kind already there is
    /// replaced), keeping the one showing.
    pub(super) fn add(&mut self, step: Step) {
        if let Some(there) = self.steps.iter_mut().find(|s| s.rank() == step.rank()) {
            *there = step;
            return;
        }
        let at = self.steps.iter().position(|s| s.rank() > step.rank()).unwrap_or(self.steps.len());
        if self.shown && at <= self.index {
            self.index += 1;
        }
        self.steps.insert(at, step);
    }

    /// Add a step and go to it (something asked for it).
    pub(super) fn show(&mut self, step: Step) {
        self.add(step);
        self.index = self.steps.iter().position(|s| s.rank() == step.rank()).unwrap_or(0);
    }

    fn current(&self) -> Option<Step> {
        self.steps.get(self.index).copied()
    }
}

impl App {
    /// Every frame, before drawing: steps that have come up since (settings
    /// set aside while reading something later, usage data not asked yet).
    pub(super) fn collect_flow_steps(&mut self) {
        self.set_aside.extend(crate::config::set_aside_notices());
        if !self.set_aside.is_empty() {
            self.flow.add(Step::SetAside);
        }
        if self.config.usage_data.is_none() && crate::usage::available() {
            self.flow.add(Step::UsageData);
        }
    }

    /// The flow's page, filling `ui` (in place of the panels).
    pub(super) fn flow_ui(&mut self, ui: &mut egui::Ui) {
        let Some(step) = self.flow.current() else { return };
        self.flow.shown = true;
        let count = self.flow.steps.len();
        let mut index = self.flow.index;
        let mut advance = false;
        let mut back = false;
        let outer = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 8 });
        egui::CentralPanel::default().frame(outer).show(ui, |ui| {
            let page = egui::Frame::new().fill(ui.visuals().window_fill).corner_radius(6).inner_margin(12);
            page.show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.heading(step.heading());
                ui.add_space(6.0);
                // The buttons at the bottom first, so the step gets the room
                // above them.
                let footer = ui.spacing().interact_size.y + 12.0;
                let height = (ui.available_height() - footer).max(80.0);
                // The pills only where there's room beside them, and
                // something to pick.
                let pills = count > 1 && ui.available_width() >= 560.0;
                if pills {
                    let titles: Vec<String> =
                        self.flow.steps.iter().map(|s| format!("{}  {}", icon(s.icon()), s.name())).collect();
                    super::categories_body(ui, "flow", &titles, &mut index, height, |ui, i| {
                        let step = self.flow.steps[i];
                        advance = self.flow_step_ui(ui, step);
                    });
                } else {
                    let size = egui::vec2(ui.available_width(), height);
                    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_height(height);
                        egui::ScrollArea::vertical().id_salt("flow_body").auto_shrink([false, false]).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            advance = self.flow_step_ui(ui, step);
                        });
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if count > 1 {
                        ui.weak(format!("{} of {count}", index + 1));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let last = index + 1 == count;
                        let label = if count == 1 && !self.flow_asks() {
                            "Close"
                        } else if last && self.flow_restarts() {
                            "Install and restart"
                        } else if last {
                            "Done"
                        } else {
                            "Next"
                        };
                        advance |= ui.add(egui::Button::new(label).selected(true)).clicked();
                        if index > 0 {
                            back = ui.button("Back").clicked();
                        }
                    });
                });
            });
        });
        self.flow.index = index;
        if back {
            self.flow.index = index.saturating_sub(1);
        } else if advance {
            if index + 1 < self.flow.steps.len() {
                self.flow.index = index + 1;
            } else {
                self.finish_flow(ui.ctx());
            }
        }
    }

    /// Whether any step asks something (not only says it).
    fn flow_asks(&self) -> bool {
        self.flow.steps.iter().any(|s| matches!(s, Step::Welcome | Step::UsageData)) || self.flow_restarts()
    }

    /// Whether Done installs, and so restarts the app.
    fn flow_restarts(&self) -> bool {
        #[cfg(windows)]
        if self.flow.has(Step::Install) {
            return self.install.chosen;
        }
        false
    }

    /// One step's page; whether it was left from inside (an update's Not
    /// now, say), as Next would.
    fn flow_step_ui(&mut self, ui: &mut egui::Ui, step: Step) -> bool {
        match step {
            Step::SetAside => {
                self.set_aside_step_ui(ui);
                false
            }
            Step::Welcome => {
                self.welcome_step_ui(ui);
                false
            }
            Step::UsageData => {
                self.usage_step_ui(ui);
                false
            }
            Step::Changelog(since) => {
                changelog_body_ui(ui, since);
                false
            }
            Step::Update => self.updates_body_ui(ui, true),
            #[cfg(windows)]
            Step::Install => {
                self.install_step_ui(ui);
                false
            }
        }
    }

    fn set_aside_step_ui(&mut self, ui: &mut egui::Ui) {
        settings_group("Set aside")
            .icon("warning")
            .hint("Nothing was deleted: the build that saved them can still read the kept copies.")
            .show(ui, |ui| {
                for (note, kept) in &self.set_aside {
                    setting_row(ui, |ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(row_icon_button(ui, "folder-open")).on_hover_text(format!("Show {}", kept.display())).clicked() {
                                open_in_file_manager(kept);
                            }
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.add(egui::Label::new(note).wrap());
                            });
                        });
                    });
                }
            });
    }

    /// Done: apply every step's answers, in order (installing, which
    /// restarts the app, last), and go back to the app.
    #[cfg_attr(not(windows), expect(unused_variables))]
    fn finish_flow(&mut self, ctx: &egui::Context) {
        let steps = std::mem::take(&mut self.flow.steps);
        self.flow.index = 0;
        self.flow.shown = false;
        let mut tour = false;
        for step in steps {
            match step {
                Step::SetAside => self.set_aside.clear(),
                Step::Welcome => tour = self.welcome_done(),
                Step::UsageData => self.usage_done(),
                Step::Changelog(_) => {}
                Step::Update => self.updates_message = None,
                #[cfg(windows)]
                Step::Install => {
                    if self.install_done(ctx) {
                        return;
                    }
                }
            }
        }
        if tour {
            self.start_tour();
        }
    }
}
