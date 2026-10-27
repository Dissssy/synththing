//! What the user's other devices are playing (`playback_share.rs`): this
//! one's told to them as it changes, and theirs shown under the Devices
//! button by the transport.

use std::time::Instant;

use eframe::egui;

use super::{App, fmt_time, icon, icon_button};
use crate::device::Device;
use crate::engine::EngineView;
use crate::fields::Field;
use crate::playback_share::{self, Mark, Message, Players, Sharer, Song, State};

#[derive(Default)]
pub(super) struct DevicesState {
    sharer: Sharer,
    players: Players,
}

impl App {
    /// Every frame (`api_tick`): this device's playback sent when due, and
    /// the devices gone quiet forgotten.
    pub(super) fn devices_tick(&mut self, ctx: &egui::Context, view: &EngineView) {
        if !self.sync_on() {
            self.devices.players.clear();
            return;
        }
        let now = Instant::now();
        let mark = Mark { loaded: self.current_song.is_some(), paused: view.paused, speed: view.speed, jumps: view.generation };
        if self.devices.sharer.tick(now, &mark) {
            let state = self.playback_state(view);
            self.send_playback(&Message::State(state));
        }
        self.devices.players.expire(now);
        let due = self.devices.sharer.due(now, &mark);
        ctx.request_repaint_after(self.devices.players.due(now).map_or(due, |gone| gone.min(due)));
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

    fn send_playback(&mut self, message: &Message) {
        match serde_json::to_value(message) {
            Ok(body) => self.send_live(None, playback_share::TOPIC, body),
            Err(e) => log::warn!("a playback message: {e}"),
        }
    }

    /// This device listens again: ask the others what they play, and tell
    /// them what this one does.
    pub(super) fn devices_hello(&mut self) {
        self.send_playback(&Message::Hello);
        self.devices.sharer.again();
    }

    /// Another device said something about its playback (a hello is
    /// answered at the next tick, to every device).
    pub(super) fn devices_heard(&mut self, from: Device, body: serde_json::Value) {
        match serde_json::from_value::<Message>(body) {
            Ok(Message::Hello) => self.devices.sharer.again(),
            Ok(Message::State(state)) => self.devices.players.hear(from, state, Instant::now()),
            Err(e) => log::info!("a playback message from {} doesn't read: {e}", from.name),
        }
    }

    /// The Devices button, while another device is online: what each one
    /// plays, in a popup.
    pub(super) fn devices_button_ui(&mut self, ui: &mut egui::Ui) {
        let players = self.devices.players.all();
        if players.is_empty() {
            return;
        }
        let hover = match players.len() {
            1 => "Your other device: what it's playing".to_string(),
            n => format!("Your {n} other devices: what they're playing"),
        };
        let button = ui.add(icon_button("devices")).on_hover_text(hover);
        egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(260.0);
            ui.set_max_width(360.0);
            let now = Instant::now();
            for (i, player) in players.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                player_ui(ui, player, now);
            }
            if players.iter().any(|p| p.state.song.is_some() && !p.state.paused) {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        });
    }
}

/// One device: its name, and its song with where it is, or that it's idle.
fn player_ui(ui: &mut egui::Ui, player: &playback_share::Player, now: Instant) {
    let state = &player.state;
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
        return;
    };
    ui.label(match &song.artist {
        Some(artist) => format!("{} \u{2014} {artist}", song.title),
        None => song.title.clone(),
    });
    let position = player.position(now);
    ui.horizontal(|ui| {
        let fraction = if song.length > 0.0 { (position / song.length) as f32 } else { 0.0 };
        ui.add(egui::ProgressBar::new(fraction).desired_width(160.0).desired_height(4.0));
        ui.monospace(format!("{} / {}", fmt_time(position), fmt_time(song.length)));
        if state.speed != 1.0 {
            ui.weak(format!("{:.2}x", state.speed));
        }
    });
}
