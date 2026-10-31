//! What each of the user's devices is playing, told to the others: every
//! device signed in as the same identity with sync on says what it plays
//! (`State`) as live messages (`device_messages.rs`) on the `playback`
//! topic, and nothing is kept. A device that comes online says hello, and
//! the others answer with theirs.
//!
//! A device sends its state whenever it changes (a song, pause, speed or a
//! seek; at most every `GAP`), and every `HEARTBEAT` besides, which keeps
//! the position honest; the others forget one not heard from in `SILENCE`
//! and run the position on from when they heard it. A device can also be
//! told to play, pause, skip or seek (`Control`), as its own transport
//! would; what it does then reaches the others as its state.
//!
//! A device can follow another (its leader): it plays nothing itself, and
//! its transport drives the leader's. There are no sessions to keep: a
//! leader and the devices following it are one, and following a device
//! that follows another follows that one (`Players::leader_of`). No egui
//! here.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::device::Device;

pub const TOPIC: &str = "playback";
/// The least time between two sends.
const GAP: Duration = Duration::from_millis(250);
/// How often a device says what it plays, changed or not.
const HEARTBEAT: Duration = Duration::from_secs(30);
/// A device not heard from for this long is gone.
const SILENCE: Duration = Duration::from_secs(75);

/// A `playback` message's body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    /// To every device: tell me what you play.
    Hello,
    /// What the sender plays now.
    State(State),
    /// To one device: do this.
    Control(Control),
}

/// What another device can have this one do. Play and pause rather than a
/// toggle, so one sent on a state already out of date still lands right.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Control {
    Play,
    Pause,
    Previous,
    Next,
    /// To this many seconds in.
    Seek { position: f64 },
    /// Follow the device that sent this (Bring here, or it continues
    /// here what this one played).
    FollowMe,
}

/// What a device plays.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// None: nothing's loaded.
    pub song: Option<Song>,
    pub paused: bool,
    /// Seconds, as it was sent.
    pub position: f64,
    pub speed: f64,
    /// The device it follows (by ID), if it does: it plays nothing then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub following: Option<String>,
    /// The playlist (by its synced ID) the song plays from, if any: a
    /// device continuing it carries on through the same playlist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist: Option<String>,
}

/// The song a device plays, as the others show it (they may not have it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Song {
    /// Its key in the song store, if it's in it.
    pub key: Option<String>,
    pub title: String,
    pub artist: Option<String>,
    /// Seconds.
    pub length: f64,
}

/// What makes a state worth sending at once: anything but the position
/// running on. `jumps` counts the position's jumps, a new song's included
/// (`EngineView::generation`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mark {
    pub following: Option<String>,
    pub loaded: bool,
    pub paused: bool,
    pub speed: f64,
    pub jumps: u64,
}

/// This device's side: what the others were last told, and when.
#[derive(Default)]
pub struct Sharer {
    sent: Option<(Mark, Instant)>,
}

impl Sharer {
    /// Whether to send the state now: it changed (and the last send was
    /// `GAP` ago), or a heartbeat's due.
    pub fn tick(&mut self, now: Instant, mark: &Mark) -> bool {
        let due = match &self.sent {
            None => true,
            Some((sent, at)) => (sent != mark && now >= *at + GAP) || now >= *at + HEARTBEAT,
        };
        if due {
            self.sent = Some((mark.clone(), now));
        }
        due
    }

    /// How soon `tick` might send (a change waiting out the gap, or the
    /// heartbeat).
    pub fn due(&self, now: Instant, mark: &Mark) -> Duration {
        match &self.sent {
            None => Duration::ZERO,
            Some((sent, at)) if sent != mark => (*at + GAP).saturating_duration_since(now),
            Some((_, at)) => (*at + HEARTBEAT).saturating_duration_since(now),
        }
    }

    /// Send at the next tick (another device came online).
    pub fn again(&mut self) {
        self.sent = None;
    }
}

/// Another device, as this one last heard of it.
#[derive(Clone, Debug)]
pub struct Player {
    pub device: Device,
    pub state: State,
    heard: Instant,
}

impl Player {
    /// When its state was last heard.
    pub fn heard(&self) -> Instant {
        self.heard
    }

    /// Where it is now: run on from when it was heard while playing.
    pub fn position(&self, now: Instant) -> f64 {
        let state = &self.state;
        let ran = if state.paused { 0.0 } else { now.saturating_duration_since(self.heard).as_secs_f64() * state.speed };
        let position = state.position + ran;
        state.song.as_ref().map_or(position, |song| position.min(song.length))
    }
}

/// The other devices online, and what each plays.
#[derive(Default)]
pub struct Players {
    players: Vec<Player>,
}

impl Players {
    pub fn all(&self) -> &[Player] {
        &self.players
    }

    pub fn get(&self, id: &str) -> Option<&Player> {
        self.players.iter().find(|p| p.device.id == id)
    }

    /// Whom to follow to be with `id`: the device it follows, if it
    /// follows one (`me` aside: following one that follows this device
    /// would be a circle), else it.
    pub fn leader_of(&self, id: &str, me: &str) -> Option<String> {
        let player = self.get(id)?;
        match player.state.following.as_deref() {
            Some(leader) if leader == me => None,
            Some(leader) => Some(leader.to_string()),
            None => Some(id.to_string()),
        }
    }

    /// A device's state.
    pub fn hear(&mut self, from: Device, state: State, now: Instant) {
        let player = Player { device: from, state, heard: now };
        match self.players.iter_mut().find(|p| p.device.id == player.device.id) {
            Some(old) => *old = player,
            None => {
                self.players.push(player);
                self.players.sort_by(|a, b| a.device.name.cmp(&b.device.name));
            }
        }
    }

    /// `id` asked this device to follow it, so it follows none itself
    /// (whatever its last state said): it's a leader before its next
    /// state is heard.
    pub fn leads(&mut self, id: &str) {
        if let Some(player) = self.players.iter_mut().find(|p| p.device.id == id) {
            player.state.following = None;
        }
    }

    /// Forget devices gone silent.
    pub fn expire(&mut self, now: Instant) {
        self.players.retain(|p| now < p.heard + SILENCE);
    }

    /// How soon one might go silent, if any's known.
    pub fn due(&self, now: Instant) -> Option<Duration> {
        self.players.iter().map(|p| (p.heard + SILENCE).saturating_duration_since(now)).min()
    }

    /// Forget every device (this one stopped hearing them).
    pub fn clear(&mut self) {
        self.players.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(paused: bool, jumps: u64) -> Mark {
        Mark { following: None, loaded: true, paused, speed: 1.0, jumps }
    }

    fn device(id: &str) -> Device {
        Device { id: id.into(), name: format!("{id}'s") }
    }

    #[test]
    fn changes_are_sent_at_most_every_gap_and_heartbeats_between() {
        let start = Instant::now();
        let mut sharer = Sharer::default();
        assert!(sharer.tick(start, &mark(false, 0)), "the first, at once");
        assert!(!sharer.tick(start + Duration::from_secs(5), &mark(false, 0)), "nothing new");
        assert_eq!(sharer.due(start, &mark(false, 0)), HEARTBEAT);

        let paused = mark(true, 0);
        assert!(!sharer.tick(start + Duration::from_millis(100), &paused), "too soon after the last");
        assert_eq!(sharer.due(start + Duration::from_millis(100), &paused), Duration::from_millis(150));
        assert!(sharer.tick(start + GAP, &paused));
        assert!(sharer.tick(start + GAP * 2, &mark(true, 1)), "a seek");
        assert!(sharer.tick(start + GAP * 2 + HEARTBEAT, &mark(true, 1)), "the heartbeat");

        sharer.again();
        assert!(sharer.tick(start + GAP * 2 + HEARTBEAT, &mark(true, 1)), "asked again");
    }

    #[test]
    fn players_run_on_while_playing_and_are_forgotten_when_silent() {
        let start = Instant::now();
        let mut players = Players::default();
        let song = Song { key: None, title: "a".into(), artist: None, length: 100.0 };
        players.hear(device("phone"), State { song: Some(song.clone()), paused: false, position: 10.0, speed: 2.0, ..State::default() }, start);
        let later = start + Duration::from_secs(5);
        assert_eq!(players.all()[0].position(later), 20.0);
        assert_eq!(players.all()[0].position(start + Duration::from_secs(60)), 100.0, "not past the end");

        players.hear(device("phone"), State { song: Some(song), paused: true, position: 30.0, speed: 2.0, ..State::default() }, later);
        assert_eq!(players.all().len(), 1, "the same device, its state replaced");
        assert_eq!(players.all()[0].position(later + Duration::from_secs(5)), 30.0, "paused");

        players.expire(later + SILENCE - Duration::from_secs(1));
        assert_eq!(players.all().len(), 1);
        players.expire(later + SILENCE);
        assert!(players.all().is_empty());
    }

    #[test]
    fn controls_read_back_as_sent() {
        for control in [Control::Play, Control::Pause, Control::Previous, Control::Next, Control::Seek { position: 12.5 }, Control::FollowMe] {
            let body = serde_json::to_value(Message::Control(control)).unwrap();
            assert_eq!(serde_json::from_value::<Message>(body).unwrap(), Message::Control(control));
        }
        let seek = serde_json::to_value(Message::Control(Control::Seek { position: 3.0 })).unwrap();
        assert_eq!(seek, serde_json::json!({ "kind": "control", "do": "seek", "position": 3.0 }));
    }

    #[test]
    fn following_a_follower_follows_its_leader() {
        let now = Instant::now();
        let mut players = Players::default();
        players.hear(device("pc"), State::default(), now);
        players.hear(device("phone"), State { following: Some("pc".into()), ..State::default() }, now);
        players.hear(device("tv"), State { following: Some("me".into()), ..State::default() }, now);
        assert_eq!(players.leader_of("pc", "me").as_deref(), Some("pc"));
        assert_eq!(players.leader_of("phone", "me").as_deref(), Some("pc"));
        assert_eq!(players.leader_of("tv", "me"), None, "it's with this device already");
        assert_eq!(players.leader_of("gone", "me"), None);

        // (The TV continues here what this device played, and asks it to
        // follow: before its next state comes, it leads.)
        players.leads("tv");
        assert_eq!(players.leader_of("tv", "me").as_deref(), Some("tv"));
    }
}
