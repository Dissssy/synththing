//! Live messages to and from the user's other devices
//! (`device_messages.rs`): where they go, and who hears each topic.

use super::App;
use crate::device::Device;
use crate::device_messages::{self, Envelope, Outbox, Route};
use crate::library::identity;
use crate::{panel_share, playback_share};

#[derive(Default)]
pub(super) struct LiveState {
    /// This device, once it's been needed.
    me: Option<Device>,
    outbox: Outbox,
}

impl App {
    pub(super) fn this_device(&mut self) -> Device {
        self.live.me.get_or_insert_with(Device::load).clone()
    }

    /// Where live messages go now: the server, while this device syncs.
    fn live_route(&self) -> Option<Route> {
        if !self.sync_on() || !crate::build_info::online(crate::build_info::Online::Scripts) {
            return None;
        }
        let link = self.sync_link();
        Some(Route { server: link.server, server_key: link.server_key, identity: link.identity?, sync_key: identity::sync_key()? })
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
