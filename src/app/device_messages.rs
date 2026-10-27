//! Live messages to and from the user's other devices
//! (`device_messages.rs`): where they go, and who hears each topic. They
//! come on the notification stream, or from the hub while this device
//! listens to it (`HubListener`): then only from there.

use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;

use super::App;
use crate::device::Device;
use crate::device_messages::{self, Envelope, HubListener, Outbox, Route};
use crate::library::client::StreamEvent;
use crate::library::identity;
use crate::{panel_share, playback_share};

pub(super) struct LiveState {
    /// This device, once it's been needed.
    me: Option<Device>,
    outbox: Outbox,
    /// Listening to the hub, while this device reaches it.
    hub: Option<HubListener>,
    from_hub: (Sender<StreamEvent>, Receiver<StreamEvent>),
}

impl Default for LiveState {
    fn default() -> Self {
        Self { me: None, outbox: Outbox::default(), hub: None, from_hub: mpsc::channel() }
    }
}

impl App {
    pub(super) fn this_device(&mut self) -> Device {
        self.live.me.get_or_insert_with(Device::load).clone()
    }

    /// Where live messages go now, while this device syncs: the hub, while
    /// it listens to it, else the server.
    fn live_route(&self) -> Option<Route> {
        if !self.sync_on() || !crate::build_info::online(crate::build_info::Online::Scripts) {
            return None;
        }
        let link = self.sync_link();
        let me = self.live.me.as_ref().map_or_else(|| Device::load().id, |me| me.id.clone());
        let hub = self.live.hub.as_ref().filter(|hub| hub.open()).map(|hub| (hub.hub().clone(), me));
        Some(Route { server: link.server, server_key: link.server_key, identity: link.identity?, sync_key: identity::sync_key()?, hub })
    }

    /// Whether live messages come from the hub now (and not the server).
    pub(super) fn live_from_hub(&self) -> bool {
        self.live.hub.as_ref().is_some_and(HubListener::open)
    }

    /// Listen to the hub while this device reaches it, and take what it
    /// passes on.
    pub(super) fn poll_hub_live(&mut self, ctx: &egui::Context) {
        let wanted = self.live_route().and_then(|route| Some((self.hub_reached()?.clone(), route.identity)));
        if wanted.as_ref().map(|(hub, _)| hub) != self.live.hub.as_ref().map(HubListener::hub) {
            let me = self.this_device().id;
            let (tx, ctx) = (self.live.from_hub.0.clone(), ctx.clone());
            self.live.hub = wanted.map(|(hub, identity)| HubListener::start(hub, identity, me, tx, move || ctx.request_repaint()));
        }
        while let Ok(event) = self.live.from_hub.1.try_recv() {
            match event {
                StreamEvent::Opened => self.live_opened(),
                StreamEvent::Live(sealed) => self.live_heard(&sealed),
                StreamEvent::Notification(_) | StreamEvent::Sync(_) => {}
            }
        }
    }

    /// Send `body` on `topic`, to one device (by ID) or every one.
    pub(super) fn send_live(&mut self, to: Option<String>, topic: &str, body: serde_json::Value) {
        let Some(route) = self.live_route() else { return };
        let envelope = Envelope { from: self.this_device(), to, topic: topic.to_string(), body };
        self.live.outbox.send(&route, &envelope);
    }

    /// This device listens again (the notification stream opened).
    pub(super) fn live_opened(&mut self) {
        self.panels_hello();
        self.devices_hello();
    }

    /// A live message came, sealed.
    pub(super) fn live_heard(&mut self, sealed: &str) {
        let Some(sync_key) = identity::sync_key() else { return };
        let me = self.this_device().id;
        let Some(envelope) = device_messages::open(&sync_key, &me, sealed) else { return };
        match envelope.topic.as_str() {
            panel_share::TOPIC => self.panels_heard(envelope.from, envelope.body),
            playback_share::TOPIC => self.devices_heard(envelope.from, envelope.body),
            _ => {}
        }
    }
}
