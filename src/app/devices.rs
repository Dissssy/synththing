//! What the user's other devices are playing (`playback_share.rs`): this
//! one's told to them as it changes, and theirs shown under the Devices
//! button by the transport, each with its own controls. This device can
//! follow another (Join, or the other's Bring here): it pauses, and its
//! transport and hotkeys drive the leader's until it leaves, plays a song
//! of its own, or the leader goes quiet.

use std::time::{Duration, Instant};

use eframe::egui;

use super::{App, fmt_time, icon, icon_button, seek_bar_ui, toast};
use crate::audio::AudioCommand;
use crate::commands::Command;
use crate::device::Device;
use crate::engine::EngineView;
use crate::fields::Field;
use crate::playback_share::{self, Control, Mark, Message, Player, Players, Sharer, Song, State};

/// How often the shown position of a device playing moves on.
const RUN_ON: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(super) struct DevicesState {
    sharer: Sharer,
    players: Players,
    /// What other devices asked of this one (and which asked), done at the
    /// next tick.
    asked: Vec<(String, Control)>,
    /// A device's seek bar being dragged (its ID), and where it's at: sent
    /// once it's let go.
    seeking: Option<(String, f64)>,
    following: Option<Following>,
}

/// The device this one follows.
struct Following {
    leader: String,
    /// Songs loaded here when it started (`EngineView::loads`): one more
    /// and this device is playing its own.
    loads: u64,
    /// Pause here at the next tick.
    pause: bool,
}

/// What's asked of a device in the Devices popup or the following bar.
enum Act {
    Control(Control),
    Join,
    Bring,
    Leave,
}

impl App {
    /// Every frame (`api_tick`): what other devices asked done, whom this
    /// one follows kept right, its playback sent when due, and the devices
    /// gone quiet forgotten.
    pub(super) fn devices_tick(&mut self, ctx: &egui::Context, view: &EngineView) {
        if !self.sync_on() {
            self.devices.players.clear();
            self.devices.following = None;
            return;
        }
        for (from, control) in std::mem::take(&mut self.devices.asked) {
            self.do_control(ctx, &from, control, view);
        }
        let now = Instant::now();
        self.devices.players.expire(now);
        self.keep_following(ctx, view);
        let mark = Mark {
            following: self.devices.following.as_ref().map(|f| f.leader.clone()),
            loaded: self.current_song.is_some(),
            paused: view.paused,
            speed: view.speed,
            jumps: view.generation,
        };
        if self.devices.sharer.tick(now, &mark) {
            let state = self.playback_state(view);
            self.send_playback(None, &Message::State(state));
        }
        let due = self.devices.sharer.due(now, &mark);
        ctx.request_repaint_after(self.devices.players.due(now).map_or(due, |gone| gone.min(due)));
    }

    /// While following: pause here once, follow the leader's own leader,
    /// and stop when a song's played here or the leader's gone.
    fn keep_following(&mut self, ctx: &egui::Context, view: &EngineView) {
        let me = self.this_device().id;
        let Some(following) = &mut self.devices.following else { return };
        let pause = std::mem::take(&mut following.pause);
        let (leader, loads) = (following.leader.clone(), following.loads);
        if pause && !view.paused {
            self.send(AudioCommand::TogglePause);
        }
        let name = self.devices.players.get(&leader).map(|p| p.device.name.clone());
        let left = match (&name, self.devices.players.leader_of(&leader, &me)) {
            (None, _) => "it went quiet",
            (Some(_), None) => "it's following this device now",
            _ if view.loads != loads => "a song's playing here",
            (Some(_), Some(next)) => {
                if let Some(following) = &mut self.devices.following {
                    following.leader = next;
                }
                return;
            }
        };
        self.devices.following = None;
        toast::show(ctx, format!("Stopped following {}: {left}", name.as_deref().unwrap_or("your other device")));
    }

    /// Follow `id` (or whom it follows), pausing here.
    fn follow(&mut self, id: &str, loads: u64) {
        let me = self.this_device().id;
        if let Some(leader) = self.devices.players.leader_of(id, &me) {
            self.devices.following = Some(Following { leader, loads, pause: true });
        }
    }

    /// Do what another device (`from`) asked, as this one's transport would.
    fn do_control(&mut self, ctx: &egui::Context, from: &str, control: Control, view: &EngineView) {
        let command = match control {
            Control::Play => Command::Play,
            Control::Pause => Command::Pause,
            Control::Previous => Command::PreviousTrack,
            Control::Next => Command::NextTrack,
            Control::Seek { position } => return self.send(AudioCommand::Seek(position)),
            Control::FollowMe => return self.follow(from, view.loads),
        };
        if self.command_available(command) {
            self.run_command(ctx, command, view);
        }
    }

    /// A playback command, sent to the leader instead while following:
    /// whether it was.
    pub(super) fn command_to_leader(&mut self, command: Command) -> bool {
        let Some(leader) = self.devices.following.as_ref().and_then(|f| self.devices.players.get(&f.leader)) else { return false };
        let position = leader.position(Instant::now());
        let length = leader.state.song.as_ref().map_or(0.0, |s| s.length);
        let seek = |by: f64| Control::Seek { position: (position + by).clamp(0.0, length) };
        let control = match command {
            Command::TogglePause if leader.state.paused => Control::Play,
            Command::TogglePause => Control::Pause,
            Command::Play => Control::Play,
            Command::Pause => Control::Pause,
            Command::SeekBack => seek(-5.0),
            Command::SeekForward => seek(5.0),
            Command::SeekBackLong => seek(-30.0),
            Command::SeekForwardLong => seek(30.0),
            Command::RestartSong => Control::Seek { position: 0.0 },
            Command::NextTrack => Control::Next,
            Command::PreviousTrack => Control::Previous,
            _ => return false,
        };
        let to = leader.device.id.clone();
        self.send_playback(Some(to), &Message::Control(control));
        true
    }

    /// Whether this device follows another.
    pub(super) fn following(&self) -> bool {
        self.devices.following.is_some()
    }

    /// What this device plays, as the others are told: nothing while it
    /// follows another.
    fn playback_state(&mut self, view: &EngineView) -> State {
        let following = self.devices.following.as_ref().map(|f| f.leader.clone());
        let song = self.current_song.clone().filter(|_| following.is_none()).map(|path| Song {
            key: self.songs.key(&path),
            title: self.song_title(&path),
            artist: self.playing_facts().and_then(|f| f.text(Field::Artist)).map(|t| t.into_owned()),
            length: view.length,
        });
        State { song, paused: view.paused, position: view.position, speed: view.speed, following }
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
            Ok(Message::Control(control)) => self.devices.asked.push((from.id, control)),
            Err(e) => log::info!("a playback message from {} doesn't read: {e}", from.name),
        }
    }

    /// Carry out what was asked of `id` in the popup or the following bar.
    fn act(&mut self, id: String, act: Act, loads: u64) {
        match act {
            Act::Control(control) => self.send_playback(Some(id), &Message::Control(control)),
            Act::Join => self.follow(&id, loads),
            Act::Bring => self.send_playback(Some(id), &Message::Control(Control::FollowMe)),
            Act::Leave => self.devices.following = None,
        }
    }

    /// In place of the transport while following: the leader's song, its
    /// controls, Leave and the Devices button.
    pub(super) fn following_bar_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        let following = self.devices.following.as_ref().map(|f| f.leader.as_str());
        let Some(leader) = following.and_then(|id| self.devices.players.get(id)).cloned() else { return };
        let mut act = None;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(icon("devices"));
            ui.weak("Playing on");
            ui.strong(&leader.device.name);
            if ui.button("Leave").on_hover_text("Stop following it: this device's own transport comes back").clicked() {
                act = Some(Act::Leave);
            }
            self.devices_button_ui(ui, view);
        });
        if let Some(control) = song_ui(ui, &leader, &mut self.devices.seeking, Instant::now()) {
            act = Some(Act::Control(control));
        }
        ui.add_space(4.0);
        if !leader.state.paused {
            ui.ctx().request_repaint_after(RUN_ON);
        }
        if let Some(act) = act {
            self.act(leader.device.id, act, view.loads);
        }
    }

    /// The Devices button, while another device is online: what each one
    /// plays, with its controls, Join and Bring here, in a popup.
    pub(super) fn devices_button_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        let me = self.this_device().id;
        let DevicesState { players, seeking, following, .. } = &mut self.devices;
        let following = following.as_ref().map(|f| f.leader.as_str());
        let all = players.all();
        if all.is_empty() {
            return;
        }
        let hover = match all.len() {
            1 => "Your other device: what it's playing".to_string(),
            n => format!("Your {n} other devices: what they're playing"),
        };
        let button = ui.add(icon_button("devices")).on_hover_text(hover);
        let mut asked = None;
        egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(260.0);
            ui.set_max_width(360.0);
            let now = Instant::now();
            for (i, player) in all.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                if let Some(act) = player_ui(ui, player, players, &me, following, seeking, now) {
                    asked = Some((player.device.id.clone(), act));
                }
            }
            if all.iter().any(|p| p.state.song.is_some() && !p.state.paused) {
                ui.ctx().request_repaint_after(RUN_ON);
            }
        });
        if let Some((id, act)) = asked {
            self.act(id, act, view.loads);
        }
    }
}

/// One device in the popup: its name, its song with its controls (or whom
/// it's with), and Join, Bring here or Leave. What's asked of it, if
/// anything.
fn player_ui(
    ui: &mut egui::Ui,
    player: &Player,
    players: &Players,
    me: &str,
    following: Option<&str>,
    seeking: &mut Option<(String, f64)>,
    now: Instant,
) -> Option<Act> {
    let state = &player.state;
    let id = player.device.id.as_str();
    let mut asked = None;
    ui.horizontal(|ui| {
        let glyph = match &state.song {
            _ if state.following.is_some() => "devices",
            None => "stop",
            Some(_) if state.paused => "pause",
            Some(_) => "play",
        };
        ui.label(icon(glyph));
        ui.strong(&player.device.name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let with_me = state.following.as_deref() == Some(me);
            if following == Some(id) {
                if ui.small_button("Leave").on_hover_text("Stop following it").clicked() {
                    asked = Some(Act::Leave);
                }
            } else if !with_me {
                if following.is_none()
                    && ui.small_button("Bring here").on_hover_text("Have it follow this device: it pauses, and drives this one's playing").clicked()
                {
                    asked = Some(Act::Bring);
                }
                if ui.small_button("Join").on_hover_text("Follow it: this device pauses, and its transport drives that one's playing").clicked() {
                    asked = Some(Act::Join);
                }
            }
        });
    });
    match state.following.as_deref() {
        Some(leader) if leader == me => {
            ui.weak("With this device");
        }
        Some(leader) => {
            let name = players.get(leader).map_or("another device", |p| p.device.name.as_str());
            ui.weak(format!("With {name}"));
        }
        None => {
            if let Some(control) = song_ui(ui, player, seeking, now) {
                asked = Some(Act::Control(control));
            }
        }
    }
    asked
}

/// A device's song with where it is and its controls, or that it's idle.
/// What it's asked to do, if anything.
fn song_ui(ui: &mut egui::Ui, player: &Player, seeking: &mut Option<(String, f64)>, now: Instant) -> Option<Control> {
    let state = &player.state;
    let id = &player.device.id;
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
