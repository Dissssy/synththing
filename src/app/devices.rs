//! What the user's other devices are playing (`playback_share.rs`): this
//! one's told to them as it changes, and theirs shown under the Devices
//! button by the transport, each with its own controls.

use std::time::Instant;

use eframe::egui;

use super::{App, fmt_time, icon, icon_button, seek_bar_ui};
use crate::audio::AudioCommand;
use crate::commands::Command;
use crate::device::Device;
use crate::engine::EngineView;
use crate::fields::Field;
use crate::playback_share::{self, Control, Mark, Message, Player, Players, Sharer, Song, State};

#[derive(Default)]
pub(super) struct DevicesState {
    sharer: Sharer,
    players: Players,
    /// What other devices asked of this one, done at the next tick.
    asked: Vec<Control>,
    /// A device's seek bar being dragged (its ID), and where it's at: sent
    /// once it's let go.
    seeking: Option<(String, f64)>,
}

impl App {
    /// Every frame (`api_tick`): this device's playback sent when due, and
    /// the devices gone quiet forgotten.
    pub(super) fn devices_tick(&mut self, ctx: &egui::Context, view: &EngineView) {
        if !self.sync_on() {
            self.devices.players.clear();
            return;
        }
        for control in std::mem::take(&mut self.devices.asked) {
            self.do_control(ctx, control, view);
        }
        let now = Instant::now();
        let mark = Mark { loaded: self.current_song.is_some(), paused: view.paused, speed: view.speed, jumps: view.generation };
        if self.devices.sharer.tick(now, &mark) {
            let state = self.playback_state(view);
            self.send_playback(None, &Message::State(state));
        }
        self.devices.players.expire(now);
        let due = self.devices.sharer.due(now, &mark);
        ctx.request_repaint_after(self.devices.players.due(now).map_or(due, |gone| gone.min(due)));
    }

    /// Do what another device asked, as this one's transport would.
    fn do_control(&mut self, ctx: &egui::Context, control: Control, view: &EngineView) {
        let command = match control {
            Control::Play => Command::Play,
            Control::Pause => Command::Pause,
            Control::Previous => Command::PreviousTrack,
            Control::Next => Command::NextTrack,
            Control::Seek { position } => return self.send(AudioCommand::Seek(position)),
        };
        if self.command_available(command) {
            self.run_command(ctx, command, view);
        }
    }

    /// What this device plays, as the others are told.
    fn playback_state(&mut self, view: &EngineView) -> State {
        let song = self.current_song.clone().map(|path| Song {
            key: self.songs.key(&path),
            title: self.song_title(&path),
            artist: self.playing_facts().and_then(|f| f.text(Field::Artist)).map(|t| t.into_owned()),
            length: view.length,
        });
        State { song, paused: view.paused, position: view.position, speed: view.speed }
    }

    /// Send to one device (by ID) or every one.
    fn send_playback(&mut self, to: Option<String>, message: &Message) {
        match serde_json::to_value(message) {
            Ok(body) => self.send_live(to, playback_share::TOPIC, body),
            Err(e) => log::warn!("a playback message: {e}"),
        }
    }

    /// This device listens again: ask the others what they play, and tell
    /// them what this one does.
    pub(super) fn devices_hello(&mut self) {
        self.send_playback(None, &Message::Hello);
        self.devices.sharer.again();
    }

    /// Another device said something about its playback (a hello is
    /// answered at the next tick, to every device).
    pub(super) fn devices_heard(&mut self, from: Device, body: serde_json::Value) {
        match serde_json::from_value::<Message>(body) {
            Ok(Message::Hello) => self.devices.sharer.again(),
            Ok(Message::State(state)) => self.devices.players.hear(from, state, Instant::now()),
            Ok(Message::Control(control)) => self.devices.asked.push(control),
            Err(e) => log::info!("a playback message from {} doesn't read: {e}", from.name),
        }
    }

    /// The Devices button, while another device is online: what each one
    /// plays, with its controls, in a popup.
    pub(super) fn devices_button_ui(&mut self, ui: &mut egui::Ui) {
        let DevicesState { players, seeking, .. } = &mut self.devices;
        let players = players.all();
        if players.is_empty() {
            return;
        }
        let hover = match players.len() {
            1 => "Your other device: what it's playing".to_string(),
            n => format!("Your {n} other devices: what they're playing"),
        };
        let button = ui.add(icon_button("devices")).on_hover_text(hover);
        let mut asked = None;
        egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(260.0);
            ui.set_max_width(360.0);
            let now = Instant::now();
            for (i, player) in players.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                if let Some(control) = player_ui(ui, player, seeking, now) {
                    asked = Some((player.device.id.clone(), control));
                }
            }
            if players.iter().any(|p| p.state.song.is_some() && !p.state.paused) {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        });
        if let Some((device, control)) = asked {
            self.send_playback(Some(device), &Message::Control(control));
        }
    }
}

/// One device: its name, and its song with where it is and its controls,
/// or that it's idle. What it's asked to do, if anything.
fn player_ui(ui: &mut egui::Ui, player: &Player, seeking: &mut Option<(String, f64)>, now: Instant) -> Option<Control> {
    let state = &player.state;
    let id = &player.device.id;
    ui.horizontal(|ui| {
        let glyph = match &state.song {
            None => "stop",
            Some(_) if state.paused => "pause",
            Some(_) => "play",
        };
        ui.label(icon(glyph));
        ui.strong(&player.device.name);
    });
    let Some(song) = &state.song else {
        ui.weak("Nothing playing");
        return None;
    };
    ui.label(match &song.artist {
        Some(artist) => format!("{} \u{2014} {artist}", song.title),
        None => song.title.clone(),
    });
    let mut asked = None;
    ui.horizontal(|ui| {
        let held = seeking.as_ref().filter(|(device, _)| device == id).map(|(_, at)| *at);
        let mut position = held.unwrap_or_else(|| player.position(now));
        ui.spacing_mut().slider_width = 160.0;
        let bar = seek_bar_ui(ui, &mut position, song.length, None);
        if bar.dragged() {
            *seeking = Some((id.clone(), position));
        }
        if bar.drag_stopped() || (bar.changed() && !bar.dragged()) {
            *seeking = None;
            asked = Some(Control::Seek { position });
        }
        ui.monospace(format!("{} / {}", fmt_time(position), fmt_time(song.length)));
        if state.speed != 1.0 {
            ui.weak(format!("{:.2}x", state.speed));
        }
    });
    ui.horizontal(|ui| {
        if ui.add(icon_button("skip-back")).on_hover_text("Previous").clicked() {
            asked = Some(Control::Previous);
        }
        let (glyph, hover, control) = if state.paused { ("play", "Play", Control::Play) } else { ("pause", "Pause", Control::Pause) };
        if ui.add(icon_button(glyph)).on_hover_text(hover).clicked() {
            asked = Some(control);
        }
        if ui.add(icon_button("skip-forward")).on_hover_text("Next").clicked() {
            asked = Some(Control::Next);
        }
    });
    asked
}
