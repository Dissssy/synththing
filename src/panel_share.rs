//! Panels on the user's other devices: a program's panel (`panel.rs`)
//! shows on every device signed in as the same identity when the program
//! may share it (`Access::share`), and what's done with it there goes back
//! to the program, saying which device. It all travels as live messages
//! (`device_messages.rs`) on the `panels` topic, and nothing is kept: a
//! device that comes online says hello, and the devices hosting panels
//! (whose local API the programs use) send them.
//!
//! A host sends a panel whenever it changes (at most every `GAP`), and
//! which it has every `HEARTBEAT`; a viewer forgets a host it hasn't heard
//! from in `SILENCE`. No egui here: the hub hosts panels the same way.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::api::ShownPanel;
use crate::device::Device;
use crate::panel::{Event, Given, Panel};

pub const TOPIC: &str = "panels";
/// The least time between two sends of one panel (so 20 a second at most).
const GAP: Duration = Duration::from_millis(50);
/// How often a host says which panels it has, while it has any.
const HEARTBEAT: Duration = Duration::from_secs(30);
/// A host not heard from for this long is gone, with its panels.
const SILENCE: Duration = Duration::from_secs(75);

/// A `panels` message's body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    /// From a viewer (to every device, or one it's missing panels from):
    /// send me your panels.
    Hello,
    /// From a host: the panels it has now, by program. Any other is gone.
    Here { apps: Vec<String> },
    /// From a host: one panel, as it is now.
    Panel(Shared),
    /// From a viewer, to the panel's host: what the user did with it.
    Event { app: String, event: Event },
}

/// A panel as a host shares it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shared {
    /// The program, as the host knows it (its token's hash).
    pub app: String,
    /// The program's name.
    pub name: String,
    /// The panel as the program sent it.
    pub panel: serde_json::Value,
    pub given: Vec<Given>,
    /// Whether the program is listening.
    pub connected: bool,
}

/// A message to send: to one device (by ID) or, with none, every one.
#[derive(Debug, PartialEq)]
pub struct Out {
    pub to: Option<String>,
    pub message: Message,
}

/// This device's side as a host: its shared panels, and what the others
/// have been told of them.
#[derive(Default)]
pub struct Host {
    panels: Vec<ShownPanel>,
    /// Each panel's program's state as last sent: listening, and when.
    sent: HashMap<String, (bool, Instant)>,
    /// The panels changed since they were last sent.
    changed: HashSet<String>,
    /// When it last said which panels it has; none: it should now.
    here_at: Option<Instant>,
}

impl Host {
    /// The local API's panels changed: those shared are sent.
    pub fn take(&mut self, shown: &[ShownPanel]) {
        let shared: Vec<ShownPanel> = shown.iter().filter(|s| s.share).cloned().collect();
        let apps = |panels: &[ShownPanel]| panels.iter().map(|s| s.app.clone()).collect::<Vec<_>>();
        if apps(&shared) != apps(&self.panels) {
            self.here_at = None;
        }
        for panel in &shared {
            if !self.panels.contains(panel) {
                self.changed.insert(panel.app.clone());
            }
        }
        self.sent.retain(|app, _| shared.iter().any(|s| &s.app == app));
        self.panels = shared;
    }

    /// A viewer (`from`) said hello: everything, to it.
    pub fn hello(&self, from: &str, connected: impl Fn(&str) -> bool) -> Vec<Out> {
        if self.panels.is_empty() {
            return Vec::new();
        }
        let to = Some(from.to_string());
        let mut out: Vec<Out> = self.panels.iter().map(|s| Out { to: to.clone(), message: Message::Panel(share(s, connected(&s.app))) }).collect();
        out.push(Out { to, message: self.here() });
        out
    }

    /// What's due to be sent now (`connected`: whether a program's
    /// listening).
    pub fn tick(&mut self, now: Instant, connected: impl Fn(&str) -> bool) -> Vec<Out> {
        let mut out = Vec::new();
        for panel in &self.panels {
            let listening = connected(&panel.app);
            let sent = self.sent.get(&panel.app);
            if sent.is_none_or(|(was, _)| *was != listening) {
                self.changed.insert(panel.app.clone());
            }
            if self.changed.contains(&panel.app) && sent.is_none_or(|(_, at)| now >= *at + GAP) {
                self.changed.remove(&panel.app);
                self.sent.insert(panel.app.clone(), (listening, now));
                out.push(Out { to: None, message: Message::Panel(share(panel, listening)) });
            }
        }
        // (After the panels, so a viewer hears of none it hasn't got.)
        let beat = !self.panels.is_empty() && self.here_at.is_some_and(|at| now >= at + HEARTBEAT);
        if self.here_at.is_none() || beat {
            out.push(Out { to: None, message: self.here() });
            self.here_at = Some(now);
        }
        out
    }

    /// How soon `tick` has something to send, if ever.
    pub fn due(&self, now: Instant) -> Option<Duration> {
        let gaps = self.changed.iter().map(|app| self.sent.get(app).map_or(Duration::ZERO, |(_, at)| (*at + GAP).saturating_duration_since(now)));
        let beat = (!self.panels.is_empty()).then(|| self.here_at.map_or(Duration::ZERO, |at| (at + HEARTBEAT).saturating_duration_since(now)));
        gaps.chain(beat).min()
    }

    /// A viewer's event on one of the panels, for its program, saying the
    /// device; none if that panel isn't shared from here.
    pub fn event(&self, from: Device, app: &str, event: Event) -> Option<Event> {
        self.panels.iter().any(|s| s.app == app).then_some(Event { device: Some(from), ..event })
    }

    fn here(&self) -> Message {
        Message::Here { apps: self.panels.iter().map(|s| s.app.clone()).collect() }
    }
}

fn share(panel: &ShownPanel, connected: bool) -> Shared {
    Shared { app: panel.app.clone(), name: panel.name.clone(), panel: panel.source.clone(), given: panel.given.clone(), connected }
}

/// Another device's shared panel, as this one shows it.
#[derive(Clone, Debug)]
pub struct RemotePanel {
    pub shared: Shared,
    pub panel: Panel,
}

/// A device hosting panels, as this one last heard.
#[derive(Clone, Debug)]
pub struct RemoteHost {
    pub device: Device,
    heard: Instant,
    pub panels: Vec<RemotePanel>,
}

/// This device's side as a viewer: the other devices' panels.
#[derive(Default)]
pub struct Viewer {
    hosts: Vec<RemoteHost>,
    /// Goes up with every change.
    revision: u64,
}

impl Viewer {
    pub fn hosts(&self) -> &[RemoteHost] {
        &self.hosts
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// A host's message: anything to answer it with (a hello, when it has
    /// panels this device missed).
    pub fn hear(&mut self, from: Device, message: Message, now: Instant) -> Option<Out> {
        let at = match self.hosts.iter().position(|h| h.device.id == from.id) {
            Some(at) => at,
            None if matches!(message, Message::Here { ref apps } if apps.is_empty()) => return None,
            None => {
                self.hosts.push(RemoteHost { device: from.clone(), heard: now, panels: Vec::new() });
                self.hosts.len() - 1
            }
        };
        let host = &mut self.hosts[at];
        host.device.name.clone_from(&from.name);
        host.heard = now;
        let answer = match message {
            Message::Here { apps } => {
                let before = host.panels.len();
                host.panels.retain(|p| apps.contains(&p.shared.app));
                if host.panels.len() != before {
                    self.revision += 1;
                }
                let missing = apps.iter().any(|app| !host.panels.iter().any(|p| &p.shared.app == app));
                missing.then(|| Out { to: Some(from.id), message: Message::Hello })
            }
            Message::Panel(shared) => {
                let panel = match Panel::parse(&shared.panel) {
                    Ok(panel) => panel,
                    Err(why) => {
                        log::info!("a panel shared from {} doesn't read: {why}", from.name);
                        return None;
                    }
                };
                let remote = RemotePanel { shared, panel };
                match host.panels.iter_mut().find(|p| p.shared.app == remote.shared.app) {
                    Some(old) => *old = remote,
                    None => host.panels.push(remote),
                }
                self.revision += 1;
                None
            }
            Message::Hello | Message::Event { .. } => None,
        };
        // (One with no panels, none coming, is no host.)
        if self.hosts[at].panels.is_empty() && answer.is_none() {
            self.hosts.remove(at);
        }
        answer
    }

    /// Forget hosts gone silent.
    pub fn expire(&mut self, now: Instant) {
        let before = self.hosts.len();
        self.hosts.retain(|h| now < h.heard + SILENCE);
        if self.hosts.len() != before {
            self.revision += 1;
        }
    }

    /// How soon a host might go silent, if any's known.
    pub fn due(&self, now: Instant) -> Option<Duration> {
        self.hosts.iter().map(|h| (h.heard + SILENCE).saturating_duration_since(now)).min()
    }

    /// Forget every host (this device stopped hearing them).
    pub fn clear(&mut self) {
        if !self.hosts.is_empty() {
            self.hosts.clear();
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(app: &str, share: bool, title: &str) -> ShownPanel {
        let source = serde_json::json!({ "title": title, "elements": [{ "id": "q", "type": "textbox", "value": "x" }] });
        let panel = Panel::parse(&source).unwrap();
        let given = vec![Given { id: "q".into(), value: "x".into(), revision: 1 }];
        ShownPanel { app: app.into(), name: format!("{app} program"), panel, given, source, share }
    }

    fn device(id: &str) -> Device {
        Device { id: id.into(), name: format!("{id}'s") }
    }

    #[test]
    fn a_host_sends_its_shared_panels_and_a_viewer_shows_them() {
        let start = Instant::now();
        let mut host = Host::default();
        host.take(&[shown("a", true, "A"), shown("b", false, "B")]);
        let out = host.tick(start, |_| true);
        assert_eq!(out.len(), 2, "the shared one, then which it has: {out:?}");
        assert_eq!(out[1].message, Message::Here { apps: vec!["a".into()] });
        assert!(host.tick(start, |_| true).is_empty(), "nothing new");

        let mut viewer = Viewer::default();
        for o in out {
            assert_eq!(viewer.hear(device("host"), o.message, start), None);
        }
        let panels = &viewer.hosts()[0].panels;
        assert_eq!((panels.len(), panels[0].panel.title.as_str(), panels[0].shared.connected), (1, "A", true));

        // Changed twice quickly: sent once now, once after the gap.
        host.take(&[shown("a", true, "A2")]);
        assert_eq!(host.tick(start + GAP, |_| true).len(), 1);
        host.take(&[shown("a", true, "A3")]);
        assert!(host.tick(start + GAP + Duration::from_millis(10), |_| true).is_empty());
        assert_eq!(host.due(start + GAP + Duration::from_millis(10)), Some(Duration::from_millis(40)));
        assert_eq!(host.tick(start + GAP * 2, |_| true).len(), 1);
        // The program stops listening: sent again.
        assert_eq!(host.tick(start + GAP * 3, |_| false).len(), 1);

        // A viewer missing a panel asks for it, and the host answers it alone.
        let mut late = Viewer::default();
        let hello = late.hear(device("host"), Message::Here { apps: vec!["a".into()] }, start).unwrap();
        assert_eq!(hello, Out { to: Some("host".into()), message: Message::Hello });
        let answer = host.hello("viewer", |_| true);
        assert!(answer.iter().all(|o| o.to.as_deref() == Some("viewer")) && answer.len() == 2);

        // A panel gone (unshared): said at once, and the viewer drops it.
        host.take(&[shown("a", false, "A3")]);
        let out = host.tick(start + GAP * 4, |_| true);
        assert_eq!(out, vec![Out { to: None, message: Message::Here { apps: Vec::new() } }]);
        viewer.hear(device("host"), out.into_iter().next().unwrap().message, start);
        assert!(viewer.hosts().is_empty());
        assert_eq!(host.due(start), None, "nothing to say while it shares nothing");
    }

    #[test]
    fn events_reach_only_shared_panels_saying_the_device() {
        let mut host = Host::default();
        host.take(&[shown("a", true, "A"), shown("b", false, "B")]);
        let event = Event::of("q", crate::panel::EventKind::Submitted);
        assert_eq!(host.event(device("phone"), "a", event.clone()).unwrap().device, Some(device("phone")));
        assert_eq!(host.event(device("phone"), "b", event), None);
    }

    #[test]
    fn silent_hosts_are_forgotten() {
        let start = Instant::now();
        let mut viewer = Viewer::default();
        let mut host = Host::default();
        host.take(&[shown("a", true, "A")]);
        for o in host.tick(start, |_| true) {
            viewer.hear(device("host"), o.message, start);
        }
        viewer.expire(start + SILENCE - Duration::from_secs(1));
        assert_eq!(viewer.hosts().len(), 1);
        viewer.expire(start + SILENCE);
        assert!(viewer.hosts().is_empty());
    }
}

