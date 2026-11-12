//! A hub made for the user on the server (docs/SERVER.md, Hosted hubs):
//! the `hub_granted` notification says where it is and the code it's
//! claimed with, and this window says what they've been given, with one
//! button that signs the hub in.
//!
//! Signing a hub in hands it the identity, so it's never done unasked:
//! the button makes the same handoff as Sign in another device and sends
//! its link straight to the hub (`client::hub_claim`), not through the
//! server. The window comes up by itself once for each grant, on whichever
//! device sees it first, and again from the notification until it's done.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;

use super::App;
use crate::library::pairing::{self, Link};
use crate::library::{client, Notification};

pub(super) const KIND: &str = "hub_granted";

/// The grant in a `hub_granted` notification.
struct Grant {
    url: String,
    code: String,
    size_gb: u64,
}

impl Grant {
    fn of(notification: &Notification) -> Option<Self> {
        let text = |field: &str| notification.data.get(field)?.as_str().map(str::to_string);
        Some(Self { url: text("url")?, code: text("code")?, size_gb: notification.data.get("size_gb")?.as_u64()? })
    }
}

/// The window, for one notification.
struct Window {
    notification: i64,
    busy: bool,
    error: Option<String>,
    /// The hub's signed in.
    done: bool,
}

/// How signing a hub in went: the grant's notification, and what went
/// wrong.
type Claimed = (i64, Result<(), String>);

pub struct HubGrantState {
    events: (Sender<Claimed>, Receiver<Claimed>),
    window: Option<Window>,
    /// The grants whose window came up by itself already.
    offered: HashSet<i64>,
}

impl Default for HubGrantState {
    fn default() -> Self {
        Self { events: mpsc::channel(), window: None, offered: HashSet::new() }
    }
}

impl HubGrantState {
    pub fn open(&self) -> bool {
        self.window.is_some()
    }
}

impl App {
    /// Open the window for a grant (from its notification).
    pub(super) fn open_hub_grant(&mut self, notification: i64) {
        self.hub_grant.offered.insert(notification);
        self.hub_grant.window = Some(Window { notification, busy: false, error: None, done: false });
    }

    /// Hear how signing in went, and offer a grant not offered yet.
    pub(super) fn poll_hub_grant(&mut self) {
        while let Ok((notification, result)) = self.hub_grant.events.1.try_recv() {
            if result.is_ok() {
                self.notification_handled(notification, "claimed");
            }
            if let Some(window) = self.hub_grant.window.as_mut().filter(|w| w.notification == notification) {
                window.busy = false;
                window.done = result.is_ok();
                window.error = result.err();
            }
        }
        if self.hub_grant.window.is_some() {
            return;
        }
        let offered = &self.hub_grant.offered;
        let waiting = self.notifications_waiting(KIND).find(|n| !offered.contains(&n.id)).map(|n| n.id);
        if let Some(notification) = waiting {
            self.open_hub_grant(notification);
        }
    }

    /// Sign the hub in: the identity sealed and parked on the authority,
    /// and the link to it sent to the hub with its claim code.
    fn claim_hub(&mut self, notification: i64, grant: Grant) {
        let (authority, key) = self.authority();
        let Some(identity) = self.library_identity() else { return };
        let tx = self.hub_grant.events.0.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let secret = Link::secret()?;
                let sealed = pairing::seal(&secret, identity.export("", &crate::library::identity::ensure_sync_key()?)?.as_bytes())?;
                let parked = client::pair_park(&authority, &key, &identity, &sealed)?;
                let link = Link { server: authority, id: parked.id, secret };
                client::hub_claim(&grant.url, &grant.code, &link.url())
            })();
            let _ = tx.send((notification, result));
        });
    }

    pub(super) fn hub_grant_ui(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.hub_grant.window.take() else { return };
        let grant = self.notification(window.notification).and_then(Grant::of);
        let Some(grant) = grant else { return };
        let mut close = false;
        let mut claim = false;
        let response = egui::Modal::new(egui::Id::unique("hub_grant")).show(ctx, |ui| {
            ui.set_width(super::fit_width(ui, 420.0));
            ui.horizontal(|ui| {
                ui.heading(super::icon("hard-drives"));
                ui.heading(if window.done { "Your hub is set up" } else { "You have a hub" });
            });
            ui.add_space(4.0);
            if window.done {
                ui.label(format!(
                    "It's signed in as one of your devices. Your songs start copying to it now, up to {} GB, and your \
                     other devices get them from it whenever they're on.",
                    grant.size_gb
                ));
                ui.weak("Preferences > Library > Storage shows how full it is.");
            } else {
                ui.label(format!(
                    "A hub with room for {} GB of your songs has been set up for you. It's always on, so it keeps a \
                     copy of your library and hands songs to your devices even when the others are off.",
                    grant.size_gb
                ));
                ui.add_space(4.0);
                ui.weak(format!(
                    "Starting it signs the hub in as one of your devices, like Sign in another device does. It's at {}.",
                    grant.url
                ));
            }
            if let Some(error) = &window.error {
                ui.add_space(4.0);
                ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't sign it in: {error}"));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if window.done {
                    close = ui.button("Close").clicked();
                    return;
                }
                if ui.add_enabled(!window.busy, egui::Button::new("Start using it")).clicked() {
                    claim = true;
                }
                if window.busy {
                    ui.spinner();
                } else if ui.button("Later").on_hover_text("It waits in your notifications").clicked() {
                    close = true;
                }
            });
        });
        if claim {
            window.busy = true;
            window.error = None;
            self.claim_hub(window.notification, grant);
        }
        if !(close || (response.should_close() && !window.busy)) {
            self.hub_grant.window = Some(window);
        }
    }
}
