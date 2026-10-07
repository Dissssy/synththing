//! Notifications from library servers (docs/SERVER.md): an envelope at the
//! right of the header, red with a count when something's unread; a list
//! of them, newest first, every server's merged by time; a window for
//! each, with what can be done about it.
//!
//! While the Notifications preference is on and there's an identity, the
//! app keeps a listener per server in use: it fetches what's there, then
//! holds the server's stream open for new ones (and starts over a little
//! while after it drops). A notification is a kind and its fields: the
//! app words it from its own templates, or, for a kind it doesn't know,
//! from the server's template; a field neither has is left as `{field}`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use super::library::host;
use super::App;
use crate::library::identity::Identity;
use crate::library::{self, client, Notification};

enum Event {
    /// What a server has (replacing what was known from it).
    List(String, Vec<Notification>),
    New(String, Notification),
}

pub struct NotificationsState {
    events: (Sender<Event>, Receiver<Event>),
    /// Every server's, as (server, notification).
    items: Vec<(String, Notification)>,
    /// The listeners running, by server: the server key each was started
    /// with (one that changes, trusted again, starts over), its stop flag;
    /// and whose they are.
    listening: HashMap<String, (String, Arc<AtomicBool>)>,
    listening_as: Option<String>,
    /// The one open in its window: (server, id).
    detail: Option<(String, i64)>,
}

impl Default for NotificationsState {
    fn default() -> Self {
        Self { events: mpsc::channel(), items: Vec::new(), listening: HashMap::new(), listening_as: None, detail: None }
    }
}

impl NotificationsState {
    pub fn open(&self) -> bool {
        self.detail.is_some()
    }
}

/// The app's wording for each kind it knows.
fn wording(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "encore_milestone" => "\"{name}\" has {count} encores!",
        "download_milestone" => "\"{name}\" has been installed {count} times!",
        "hidden" => "The moderators hid \"{name}\": {reason}",
        "reinstated" => "\"{name}\" is back up",
        "removed" => "The moderators deleted \"{name}\"",
        "remixed" => "{by} remixed \"{name}\": \"{remix_name}\"",
        "report_handled" => "Your report of \"{name}\" was dealt with: {outcome}",
        "new_report" => "New report of \"{name}\": {reason}",
        "copyright_notice" => "\"{name}\" was taken down by a copyright notice from {claimant}, for {on_behalf_of}",
        "notice_disputed" => "{uploader} disputed your copyright notice on \"{name}\"",
        "dispute_upheld" => "Your dispute was upheld: \"{name}\" is back up",
        "notice_rejected" => "Your copyright notice on \"{name}\" was turned down",
        "notice_withdrawn" => "The copyright notice on \"{name}\" was found bogus: it's back up",
        "takedown_stood" => "The takedown of \"{name}\" stands ({how})",
        "new_notice" => "New copyright notice on \"{name}\"",
        "new_dispute" => "A copyright notice on \"{name}\" was disputed",
        _ => return None,
    })
}

/// Each kind's icon (a Phosphor name).
fn icon_of(kind: &str) -> &'static str {
    super::icon(match kind {
        "encore_milestone" => "heart",
        "download_milestone" => "download-simple",
        "hidden" => "eye-slash",
        "reinstated" => "arrow-counter-clockwise",
        "removed" => "trash",
        "remixed" => "git-fork",
        "report_handled" | "new_report" => "flag",
        "copyright_notice" | "takedown_stood" | "new_notice" => "copyright",
        "notice_disputed" | "new_dispute" | "notice_rejected" => "scales",
        "dispute_upheld" | "notice_withdrawn" => "check-circle",
        _ => "bell",
    })
}

/// `template` with each `{field}` filled in from `data` (one it doesn't
/// have is left as it is).
fn fill(template: &str, data: &serde_json::Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if after[..close].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                let field = &after[..close];
                match data.get(field) {
                    Some(serde_json::Value::String(s)) => out.push_str(s),
                    Some(serde_json::Value::Null) | None => out.push_str(&rest[open..open + close + 2]),
                    Some(other) => out.push_str(&other.to_string()),
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What was done about a notification, as said.
fn resolution_text(resolution: &str) -> String {
    match resolution {
        "disputed" => "You disputed it: the moderators will decide.".into(),
        "let stand" => "You let it stand.".into(),
        "found bogus" => "The moderators found the notice bogus: it's back up.".into(),
        "not disputed in 14 days" => "It wasn't disputed in 14 days, so it stands.".into(),
        "the dispute was rejected" => "The moderators rejected the dispute: it stands.".into(),
        other => format!("Done: {other}."),
    }
}

/// A notification, worded.
pub fn text(notification: &Notification) -> String {
    let template = wording(&notification.kind).unwrap_or(&notification.template);
    let mut data = notification.data.clone();
    if let Some(map) = data.as_object_mut() {
        map.entry("kind").or_insert_with(|| serde_json::Value::String(notification.kind.clone()));
    }
    fill(template, &data)
}

impl App {
    /// Keep a listener running for each server in use (with an identity,
    /// and the preference on); stop the others.
    pub(super) fn keep_listening(&mut self, ctx: &egui::Context) {
        let identity = self.library_identity().filter(|_| self.config.library.notifications);
        let whose = identity.as_ref().map(Identity::public);
        if whose != self.notifications.listening_as {
            for (_, stop) in self.notifications.listening.values() {
                stop.store(true, Ordering::Relaxed);
            }
            self.notifications.listening.clear();
            self.notifications.items.clear();
            self.notifications.listening_as = whose;
        }
        let Some(identity) = identity else { return };
        let wanted: Vec<(String, String)> = self
            .config
            .library
            .enabled_servers()
            .into_iter()
            .filter_map(|s| self.config.library.pinned.get(&s).cloned().map(|key| (s, key)))
            .collect();
        let notifications = &mut self.notifications;
        notifications.listening.retain(|server, (key, stop)| {
            // (Not in use any more, or its key's changed: stopped.)
            let keep = wanted.iter().any(|(s, k)| s == server && k == key);
            if !keep {
                stop.store(true, Ordering::Relaxed);
            }
            keep
        });
        notifications.items.retain(|(server, _)| wanted.iter().any(|(s, _)| s == server));
        for (server, key) in wanted {
            if notifications.listening.contains_key(&server) {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            notifications.listening.insert(server.clone(), (key.clone(), Arc::clone(&stop)));
            let (tx, identity, ctx) = (notifications.events.0.clone(), identity.clone(), ctx.clone());
            std::thread::Builder::new()
                .name("notifications".into())
                .spawn(move || listen(&server, &key, &identity, &stop, &tx, &ctx))
                .ok();
        }
    }

    pub(super) fn poll_notifications(&mut self, ctx: &egui::Context) {
        self.keep_listening(ctx);
        while let Ok(event) = self.notifications.events.1.try_recv() {
            let items = &mut self.notifications.items;
            match event {
                Event::List(server, list) => {
                    items.retain(|(s, _)| *s != server);
                    items.extend(list.into_iter().map(|n| (server.clone(), n)));
                }
                Event::New(server, notification) => {
                    if !items.iter().any(|(s, n)| *s == server && n.id == notification.id) {
                        items.push((server, notification));
                    }
                }
            }
            items.sort_by(|a, b| b.1.created.cmp(&a.1.created).then(b.1.id.cmp(&a.1.id)));
        }
    }

    /// The notifications about `field` = `value` (a notice's) are handled
    /// here (the server marked them itself): `resolution` says how.
    pub(super) fn notification_resolved(&mut self, server: &str, field: &str, value: i64, resolution: &str) {
        for (s, n) in &mut self.notifications.items {
            if s == server && n.needs_action && n.data.get(field).and_then(serde_json::Value::as_i64) == Some(value) {
                n.handled = true;
                n.read = true;
                n.resolution = Some(resolution.to_string());
            }
        }
    }

    /// Mark these read (or handled) on their servers, and here.
    fn mark_notifications(&mut self, which: Vec<(String, i64)>, handled: bool) {
        let Some(identity) = self.library_identity() else { return };
        let mut by_server: HashMap<String, Vec<i64>> = HashMap::new();
        for (server, id) in which {
            if let Some((_, n)) = self.notifications.items.iter_mut().find(|(s, n)| *s == server && n.id == id) {
                n.read = true;
                n.handled |= handled;
            }
            by_server.entry(server).or_default().push(id);
        }
        for (server, ids) in by_server {
            let Some(key) = self.config.library.pinned.get(&server).cloned() else { continue };
            let identity = identity.clone();
            std::thread::spawn(move || {
                if let Err(e) = client::mark_notifications(&server, &key, &identity, &ids, false, handled) {
                    log::warn!("couldn't mark notifications on {server}: {e}");
                }
            });
        }
    }

    /// The envelope, at the right of the header, and its list.
    pub(super) fn notifications_button_ui(&mut self, ui: &mut egui::Ui) {
        if self.library_identity().is_none() || !self.config.library.notifications {
            return;
        }
        let unread = self.notifications.items.iter().filter(|(_, n)| !n.read).count();
        let envelope = super::icon("envelope-simple");
        let button = ui
            .add(egui::Button::new(egui::RichText::new(envelope).size(16.0)).frame(false))
            .on_hover_text(if unread == 0 { "Notifications".to_string() } else { format!("Notifications: {unread} unread") });
        if unread > 0 {
            let center = button.rect.right_top() + egui::vec2(-3.0, 4.0);
            let painter = ui.painter();
            painter.circle_filled(center, 7.0, egui::Color32::from_rgb(220, 50, 50));
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                if unread > 9 { "9+".to_string() } else { unread.to_string() },
                egui::FontId::proportional(9.5),
                egui::Color32::WHITE,
            );
        }
        let many_servers = self.notifications.items.iter().any(|(s, _)| *s != self.notifications.items[0].0);
        let mut open: Option<(String, i64)> = None;
        let mut all_read = false;
        egui::Popup::menu(&button).show(|ui| {
            ui.set_min_width(340.0);
            ui.horizontal(|ui| {
                ui.strong("Notifications");
                if unread > 0 && ui.small_button("Mark all read").clicked() {
                    all_read = true;
                }
            });
            ui.separator();
            if self.notifications.items.is_empty() {
                ui.weak("Nothing yet.");
            }
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                for (server, n) in &self.notifications.items {
                    let mut line = egui::RichText::new(format!("{}  {}", icon_of(&n.kind), text(n)));
                    if !n.read {
                        line = line.strong();
                    }
                    let mut when = library::ago(n.created);
                    if many_servers {
                        when = format!("{when}, {}", host(server));
                    }
                    // (Unread: an accent down its side, and a tint.)
                    let accent = ui.visuals().selection.bg_fill;
                    let fill = if n.read { egui::Color32::TRANSPARENT } else { accent.gamma_multiply(0.18) };
                    let frame = egui::Frame::new().fill(fill).corner_radius(4.0).inner_margin(egui::Margin::symmetric(6, 3));
                    let shown = frame.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let clicked = ui.selectable_label(false, line).clicked();
                        ui.weak(egui::RichText::new(when).small());
                        clicked
                    });
                    if !n.read {
                        let rect = shown.response.rect;
                        ui.painter().rect_filled(
                            egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.left() + 3.0, rect.bottom())),
                            2.0,
                            accent,
                        );
                    }
                    let response = shown.inner;
                    if response {
                        open = Some((server.clone(), n.id));
                    }
                }
            });
        });
        if all_read {
            let unread: Vec<(String, i64)> =
                self.notifications.items.iter().filter(|(_, n)| !n.read).map(|(s, n)| (s.clone(), n.id)).collect();
            self.mark_notifications(unread, false);
        }
        if let Some((server, id)) = open {
            self.mark_notifications(vec![(server.clone(), id)], false);
            self.notifications.detail = Some((server, id));
        }
    }

    /// A notification's window: all of it, and what can be done.
    pub(super) fn notification_ui(&mut self, ctx: &egui::Context) {
        let Some((server, id)) = self.notifications.detail.clone() else { return };
        let Some(n) = self.notifications.items.iter().find(|(s, n)| *s == server && n.id == id).map(|(_, n)| n.clone())
        else {
            self.notifications.detail = None;
            return;
        };
        let mut close = false;
        let mut show: Option<String> = None;
        let mut moderate = false;
        let mut copyright = false;
        let mut notice: Option<(i64, bool)> = None;
        let response = egui::Modal::new(egui::Id::new("notification")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.horizontal(|ui| {
                ui.heading(icon_of(&n.kind));
                ui.label(egui::RichText::new(text(&n)).size(15.0));
            });
            ui.weak(format!("{} on {}", library::ago(n.created), host(&server)));
            // (The counter-notice's contact details, sent on to the claimant.)
            if n.kind == "notice_disputed" {
                let field = |f: &str| n.data.get(f).and_then(|v| v.as_str()).unwrap_or_default().to_string();
                ui.label(format!("Their address: {}", field("address")));
                ui.label(format!("Their email: {}", field("email")));
            }
            // Done with: what was done, in place of what could be.
            if n.needs_action && n.handled {
                ui.add_space(6.0);
                let done = n.resolution.as_deref().map_or_else(|| "Done.".to_string(), resolution_text);
                ui.label(egui::RichText::new(format!("{}  {done}", super::icon("check"))).strong());
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let script = n.data.get("script").and_then(|s| s.as_str()).map(str::to_string);
                let id = n.data.get("notice").and_then(serde_json::Value::as_i64);
                match n.kind.as_str() {
                    "copyright_notice" if !n.handled => {
                        if let Some(id) = id {
                            if ui.button("View notice and dispute...").clicked() {
                                notice = Some((id, true));
                            }
                            if ui.button("Let it stand...").clicked() {
                                notice = Some((id, false));
                            }
                        }
                    }
                    "copyright_notice" => {}
                    "new_notice" | "new_dispute" => {
                        if ui.button("Open Moderation").clicked() {
                            copyright = true;
                        }
                    }
                    "remixed" => {
                        if let Some(remix) = n.data.get("remix").and_then(|s| s.as_str())
                            && ui.button("Show the remix").clicked()
                        {
                            show = Some(remix.to_string());
                        }
                        if let Some(script) = &script
                            && ui.button("Show yours").clicked()
                        {
                            show = Some(script.clone());
                        }
                    }
                    "new_report" => {
                        if ui.button("Open Moderation").clicked() {
                            moderate = true;
                        }
                    }
                    "removed" => {}
                    _ => {
                        if let Some(script) = &script
                            && ui.button("Show in Library").clicked()
                        {
                            show = Some(script.clone());
                        }
                    }
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if let Some(script) = show {
            self.notifications.detail = None;
            self.set_open(crate::layout::Section::Library, true);
            self.library_open_script(server, script);
        } else if moderate {
            self.notifications.detail = None;
            self.open_moderation();
        } else if copyright {
            self.notifications.detail = None;
            self.open_moderation_copyright();
        } else if let Some((id, dispute)) = notice {
            self.notifications.detail = None;
            self.open_notice(server, id, dispute);
        } else if close || response.should_close() {
            self.notifications.detail = None;
        }
    }
}

/// A server's listener: what's there, then the stream, over and over until
/// told to stop: straight away when a stream has run its time (the list
/// again first, for anything between), half a minute after one that failed.
fn listen(server: &str, key: &str, identity: &Identity, stop: &AtomicBool, tx: &Sender<Event>, ctx: &egui::Context) {
    while !stop.load(Ordering::Relaxed) {
        match client::notifications(server, key, identity, None) {
            Ok(list) => {
                let _ = tx.send(Event::List(server.to_string(), list));
                ctx.request_repaint();
            }
            // (A server from before notifications: nothing to listen for.)
            Err(e) if e.contains("no such page") => {
                log::info!("{server} doesn't have notifications (it runs an older version)");
                return;
            }
            Err(e) => log::info!("notifications from {server}: {e}"),
        }
        let started = std::time::Instant::now();
        let streamed = client::notification_stream(server, key, identity, |n| {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            let _ = tx.send(Event::New(server.to_string(), n));
            ctx.request_repaint();
            true
        });
        if let Err(e) = &streamed
            && started.elapsed() < client::STREAM_FOR
        {
            log::info!("notifications from {server}: {e}");
        }
        // (It ran its time, or nearly: open it again now.)
        if started.elapsed() >= Duration::from_secs(60) {
            continue;
        }
        for _ in 0..30 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wording_fills_in() {
        let data = serde_json::json!({ "name": "Disco", "count": 100 });
        assert_eq!(fill("\"{name}\" has {count} encores!", &data), "\"Disco\" has 100 encores!");
        // A field it hasn't got, or not a field at all: left as it is.
        assert_eq!(fill("{name} and {missing} {not a field} {", &data), "Disco and {missing} {not a field} {");
        let unknown = Notification {
            id: 1,
            kind: "brand_new".into(),
            data: serde_json::json!({ "thing": "x" }),
            template: "Something new: {thing} ({kind})".into(),
            created: 0,
            read: false,
            needs_action: false,
            handled: false,
            resolution: None,
        };
        assert_eq!(resolution_text("disputed"), "You disputed it: the moderators will decide.");
        assert_eq!(text(&unknown), "Something new: x (brand_new)");
    }
}
