//! Running commands (hotkeys, the menus, the local API) and keeping the
//! keymap in step.

use std::sync::Arc;

use eframe::egui;

use crate::commands::{Command, Keymap};
use crate::audio::AudioCommand;
use crate::engine::EngineView;
use crate::select_list;
use crate::layout::Section;

use super::{App, close, palette, toast};

impl App {
    /// Carries out a command, whichever source it came from.
    pub(super) fn run_command(&mut self, ctx: &egui::Context, command: Command, view: &EngineView) {
        if self.command_to_leader(command) {
            return;
        }
        match command {
            Command::TogglePause => self.send(AudioCommand::TogglePause),
            Command::Play | Command::Pause => {
                if view.paused == (command == Command::Play) {
                    self.send(AudioCommand::TogglePause);
                }
            }
            Command::SeekBack => self.send(AudioCommand::SeekRelative(-5.0)),
            Command::SeekForward => self.send(AudioCommand::SeekRelative(5.0)),
            Command::SeekBackLong => self.send(AudioCommand::SeekRelative(-30.0)),
            Command::SeekForwardLong => self.send(AudioCommand::SeekRelative(30.0)),
            Command::RestartSong => self.send(AudioCommand::Seek(0.0)),
            Command::NextTrack => {
                if self.has_next() {
                    self.next_track();
                }
            }
            Command::PreviousTrack => self.previous_track(view.position),
            Command::ToggleVisualizer if self.layout_locked() => toast::show(ctx, "The layout is locked (Layout > Lock layout)"),
            Command::ToggleVisualizer => self.set_open(Section::Visualizer, !self.is_open(Section::Visualizer)),
            Command::ExitFullscreen => self.exit_dedicated(ctx),
            Command::DedicatedVisualizer => {
                if self.dedicated.is_some() {
                    self.exit_dedicated(ctx);
                } else {
                    self.enter_dedicated(ctx);
                }
            }
            Command::RestartScript => self.restart_script(),
            Command::ToggleRecording => self.toggle_recording(view),
            Command::PauseRecording => self.toggle_recording_pause(),
            Command::Quit => self.close_with(ctx, close::CloseAction::Quit),
            Command::CommandPalette => self.palette = Some(palette::Palette::default()),
            Command::GoTo(_) | Command::NextSection | Command::PreviousSection => {
                match command {
                    Command::GoTo(section) => self.reveal_section(section),
                    Command::NextSection => self.step_section(1),
                    _ => self.step_section(-1),
                }
                // Off whatever has keyboard focus (a button Tab moved to),
                // so the keys go to the tab's list.
                ctx.memory_mut(|m| m.stop_text_input());
            }
            Command::Cursor(steer) => select_list::steer(ctx, steer, ctx.input(|i| i.modifiers.shift)),
        }
    }

    /// The list in the tab (or page) in front, for the keys to steer.
    pub(super) fn find_keyboard_list(&mut self) -> Option<&'static str> {
        if self.dedicated.is_some() {
            return None;
        }
        let section = if self.mobile() {
            self.mobile_list_section()?
        } else {
            *self.dock.find_active_focused()?.1
        };
        match section {
            Section::Songs => Some(crate::song_list::LIST_KEY),
            Section::Groupings => Some(super::playlist_panel::PLAYLIST_ROWS),
            Section::Soundfonts => Some("soundfonts"),
            Section::Queue => Some("queue"),
            Section::Wishlist => Some("wishlist"),
            _ => None,
        }
    }

    /// Rebuild the keymap when the config's keymaps changed (in
    /// Preferences, or by a sync), and put it in use.
    pub(super) fn sync_keymap(&mut self, ctx: &egui::Context) {
        let keymap = Keymap::new(&self.config.keymap, &self.config.local_keymap);
        if *self.keymap != keymap {
            self.keymap = Arc::new(keymap);
            self.keymap.install(ctx);
        }
    }

    /// Whether a command would do anything now. One that wouldn't leaves
    /// its key alone and stays out of the palette.
    pub(super) fn command_available(&self, command: Command) -> bool {
        match command {
            Command::ExitFullscreen => self.dedicated.is_some(),
            Command::NextTrack => self.has_next() || self.following(),
            // (The mobile layout has pages, not tabs.)
            Command::GoTo(section) => !self.mobile() && self.dedicated.is_none() && section != Section::Removed,
            Command::NextSection | Command::PreviousSection => !self.mobile() && self.dedicated.is_none(),
            Command::Cursor(_) => self.keyboard_list.is_some(),
            _ => true,
        }
    }
}

