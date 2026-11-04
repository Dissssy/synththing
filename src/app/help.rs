//! Help's page: the log and the credits, as a page in the panels' place
//! like Preferences (a phone shows each as a page of its own).

use eframe::egui;

use super::{App, page_ui};

/// Help's sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HelpTab {
    Log,
    Credits,
}

impl HelpTab {
    const ALL: [HelpTab; 2] = [HelpTab::Log, HelpTab::Credits];

    fn title(self) -> &'static str {
        match self {
            Self::Log => "Log",
            Self::Credits => "Credits & licenses",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Log => "notepad",
            Self::Credits => "info",
        }
    }
}

impl App {
    /// Open Help at `tab` (closing Preferences, which takes the same room).
    pub(super) fn open_help(&mut self, tab: HelpTab) {
        if self.preferences_open {
            self.toggle_preferences();
        }
        self.help = Some(tab);
    }

    /// Whether Preferences or Help is up, in the panels' place (and the
    /// View and Layout menus, which change the panels, are off).
    pub(super) fn page_open(&self) -> bool {
        self.preferences_open || self.help.is_some()
    }

    /// The desktop's Help page, while it's open.
    pub(super) fn help_ui(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.help else { return };
        let titles: Vec<String> = HelpTab::ALL.iter().map(|t| format!("{}  {}", super::icon(t.icon()), t.title())).collect();
        let mut index = HelpTab::ALL.iter().position(|&t| t == tab).unwrap_or(0);
        // (Both scroll by themselves: the log only the lines in sight.)
        let close = page_ui(ui, "Help", &titles, &mut index, true, |_| false, |ui, index| match HelpTab::ALL[index] {
            HelpTab::Log => self.log_body_ui(ui),
            HelpTab::Credits => self.credits_body_ui(ui),
        });
        self.help = (!close).then_some(HelpTab::ALL[index]);
    }
}
