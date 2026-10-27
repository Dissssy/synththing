//! Live messages between the user's devices (signed in as the same
//! identity, with sync on): what one tells the others while they're
//! online, never stored. Each is an envelope (from which device, to which,
//! or to all; a topic, and its body) sealed with the sync key, so only the
//! user's devices can read it. They go through the server (`POST
//! /api/v1/live`, out on each device's notification stream). A direct link
//! to the hub, once there is one, is meant to carry them first, with the
//! server for when it's down: a route here, the same envelopes.

use std::sync::mpsc::{self, SyncSender, TrySendError};

use serde::{Deserialize, Serialize};

use crate::device::Device;
use crate::library::identity::Identity;
use crate::library::{client, sync};

/// Messages waiting to go out, at most: more are dropped (a device that
/// can't reach the server falls behind on live things, which say where
/// they are often).
const QUEUED: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub from: Device,
    /// The device it's for (by ID): none, every one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// What it's about, which says what the body is (`panel_share::TOPIC`).
    pub topic: String,
    pub body: serde_json::Value,
}

/// The envelope deflated and sealed, as it's sent.
pub fn seal(sync_key: &[u8; 32], envelope: &Envelope) -> Result<String, String> {
    let plain = serde_json::to_vec(envelope).map_err(|e| e.to_string())?;
    sync::seal(sync_key, &sync::pack(&plain))
}

/// A sealed message, if it opens with this sync key and is for this device
/// (`me`, its ID) from another.
pub fn open(sync_key: &[u8; 32], me: &str, sealed: &str) -> Option<Envelope> {
    let plain = sync::unpack(&sync::open(sync_key, sealed).ok()?).ok()?;
    let envelope: Envelope = serde_json::from_slice(&plain).ok()?;
    (envelope.from.id != me && envelope.to.as_deref().is_none_or(|to| to == me)).then_some(envelope)
}

/// Where to send them: the server, as the identity.
#[derive(Clone)]
pub struct Route {
    pub server: String,
    pub server_key: String,
    pub identity: Identity,
    pub sync_key: [u8; 32],
}

/// Sends messages on a thread of its own (made with the first), so whoever
/// sends one never waits on the network.
#[derive(Default)]
pub struct Outbox {
    tx: Option<SyncSender<(Route, String)>>,
}

impl Outbox {
    pub fn send(&mut self, route: &Route, envelope: &Envelope) {
        let sealed = match seal(&route.sync_key, envelope) {
            Ok(sealed) if sealed.len() <= sync::MAX_LIVE => sealed,
            Ok(_) => return log::warn!("a live message about {} is too big to send", envelope.topic),
            Err(e) => return log::warn!("sealing a live message: {e}"),
        };
        let tx = self.tx.get_or_insert_with(|| {
            let (tx, rx) = mpsc::sync_channel::<(Route, String)>(QUEUED);
            let spawned = std::thread::Builder::new().name("live messages".into()).spawn(move || {
                for (route, sealed) in rx {
                    if let Err(e) = client::live_send(&route.server, &route.server_key, &route.identity, sealed) {
                        log::info!("sending a live message: {e}");
                    }
                }
            });
            if let Err(e) = spawned {
                log::warn!("couldn't start sending live messages: {e}");
            }
            tx
        });
        match tx.try_send((route.clone(), sealed)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => log::info!("live messages are backed up: one dropped"),
            Err(TrySendError::Disconnected(_)) => self.tx = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_messages_for_this_device_from_another_open() {
        let key = [7; 32];
        let device = |id: &str| Device { id: id.into(), name: format!("{id}'s") };
        let envelope = |from: &str, to: Option<&str>| Envelope {
            from: device(from),
            to: to.map(str::to_string),
            topic: "panels".into(),
            body: serde_json::json!({ "kind": "hello" }),
        };
        let to_all = envelope("a", None);
        let sealed = seal(&key, &to_all).unwrap();
        assert_eq!(open(&key, "b", &sealed), Some(to_all));
        assert_eq!(open(&key, "a", &sealed), None, "its own");
        assert_eq!(open(&[8; 32], "b", &sealed), None, "another identity's sync key");
        let to_c = seal(&key, &envelope("a", Some("c"))).unwrap();
        assert_eq!(open(&key, "b", &to_c), None, "for another device");
        assert!(open(&key, "c", &to_c).is_some());
    }
}
