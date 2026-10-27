//! Notifications from the server (docs/SERVER.md): an envelope at the
//! right of the header, red with a count when something's unread; a list
//! of them, newest first; a window for each, with what can be done about
//! it.
//!
//! While the Notifications preference is on and there's an identity, the
//! app keeps a listener running: it fetches what's there, then holds the
//! server's stream open for new ones (and starts over a little while
//! after it drops). A notification is a kind and its fields: the
//! app words it from its own templates, or, for a kind it doesn't know,
//! from the server's template; a field neither has is left as `{field}`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use super::App;
use crate::library::identity::Identity;
use crate::library::{self, client, Notification};

enum Event {
    /// What the server has (replacing what was known).
    List(Vec<Notification>),
    New(Notification),
    /// Synced data changed on the server (`sync.rs`).
    Synced(library::sync::Changed),
    /// The stream opened: live messages (`device_messages.rs`) come now.
    Opened,
    /// A live message, sealed.
    Live(String),
}

pub struct NotificationsState {
    events: (Sender<Event>, Receiver<Event>),
    items: Vec<Notification>,
    /// The listener running: the server key it was started with (one that
    /// changes, trusted again, starts over), its stop flag; and whose it is.
    listening: Option<(String, Arc<AtomicBool>)>,
    listening_as: Option<String>,
    /// The one open in its window, by ID.
    detail: Option<i64>,
}

impl Default for NotificationsState {
    fn default() -> Self {
        Self { events: mpsc::channel(), items: Vec::new(), listening: None, listening_as: None, detail: None }
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
        "new_feedback" => "New feedback ({channel}): {preview}",
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
        "new_feedback" => "chat-circle-dots",
        "copyright_notice" | "takedown_stood" | "new_notice" => "copyright",
        "notice_disputed" | "new_dispute" | "notice_rejected" => "scales",
        "dispute_upheld" | "notice_withdrawn" => "check-circle",
        _ => "bell",
    })
}

/// How much a kind of notification asks of you, 1 to 4: good news (1),
/// something resolved (2), a heads-up (3), something to act on (4); 0 for a
/// kind this app doesn't know.
fn tier_of(kind: &str) -> u8 {
    match kind {
        "encore_milestone" | "download_milestone" | "remixed" => 1,
        "reinstated" | "dispute_upheld" | "notice_withdrawn" | "report_handled" | "notice_rejected" => 2,
        "new_report" | "new_feedback" | "new_notice" | "new_dispute" | "notice_disputed" | "hidden" => 3,
        "copyright_notice" | "takedown_stood" | "removed" => 4,
        _ => 0,
    }
}

/// Each kind's color, from the channel palette by its tier (`tier_of`):
/// the most serious in the theme's error red. Grey once it's been dealt
/// with, and for a kind this app doesn't know.
fn color_of(ui: &egui::Ui, n: &Notification) -> egui::Color32 {
    let palette = |i: u8| {
        let [r, g, b] = crate::lua_visualizer::default_channel_color(i);
        egui::Color32::from_rgb(r, g, b)
    };
    if n.needs_action && n.handled {
        return ui.visuals().weak_text_color();
    }
    match n.kind.as_str() {
        "encore_milestone" => palette(6),
        "download_milestone" => palette(2),
        "remixed" => palette(4),
        "reinstated" | "dispute_upheld" | "notice_withdrawn" => palette(5),
        "report_handled" | "notice_rejected" => palette(0),
        "hidden" => palette(1),
        kind if tier_of(kind) == 3 => palette(3),
        kind if tier_of(kind) == 4 => ui.visuals().error_fg_color,
        _ => ui.visuals().weak_text_color(),
    }
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
    /// Keep the listener running (with an identity, and notifications or
    /// settings sync on), or stop it.
    pub(super) fn keep_listening(&mut self, ctx: &egui::Context) {
        let identity = self.library_identity().filter(|_| self.config.library.notifications || self.sync_on());
        let whose = identity.as_ref().map(Identity::public);
        let key = Some(library::server_key()).filter(|_| crate::build_info::online(crate::build_info::Online::Scripts));
        let notifications = &mut self.notifications;
        if whose != notifications.listening_as {
            if let Some((_, stop)) = notifications.listening.take() {
                stop.store(true, Ordering::Relaxed);
            }
            notifications.items.clear();
            notifications.listening_as = whose;
        }
        // (The server's key changed, or the Online Library's off: stopped.)
        if let Some((started_with, stop)) = &notifications.listening
            && key.as_ref() != Some(started_with)
        {
            stop.store(true, Ordering::Relaxed);
            notifications.listening = None;
        }
        let (Some(identity), Some(key)) = (identity, key) else {
            notifications.items.clear();
            return;
        };
        if notifications.listening.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        notifications.listening = Some((key.clone(), Arc::clone(&stop)));
        let (tx, ctx) = (notifications.events.0.clone(), ctx.clone());
        let server = library::official_url();
        std::thread::Builder::new()
            .name("notifications".into())
            .spawn(move || listen(&server, &key, &identity, &stop, &tx, &ctx))
            .ok();
    }

    pub(super) fn poll_notifications(&mut self, ctx: &egui::Context) {
        self.keep_listening(ctx);
        let wanted = self.config.library.notifications;
        while let Ok(event) = self.notifications.events.1.try_recv() {
            match event {
                // (Listened to again: synced data may have changed meanwhile.)
                Event::List(list) => {
                    self.sync_server_seen(None);
                    self.notifications.items = if wanted { list } else { Vec::new() };
                }
                Event::New(notification) => {
                    let items = &mut self.notifications.items;
                    if wanted && !items.iter().any(|n| n.id == notification.id) {
                        items.push(notification);
                    }
                }
                Event::Synced(changed) => self.sync_server_seen(Some(changed)),
                Event::Opened => self.live_opened(),
                Event::Live(sealed) => self.live_heard(&sealed),
            }
            let items = &mut self.notifications.items;
            items.sort_by(|a, b| b.created.cmp(&a.created).then(b.id.cmp(&a.id)));
        }
    }

    /// The notifications about `field` = `value` (a notice's) are handled
    /// here (the server marked them itself): `resolution` says how.
    pub(super) fn notification_resolved(&mut self, field: &str, value: i64, resolution: &str) {
        for n in &mut self.notifications.items {
            if n.needs_action && n.data.get(field).and_then(serde_json::Value::as_i64) == Some(value) {
                n.handled = true;
                n.read = true;
                n.resolution = Some(resolution.to_string());
            }
        }
    }

    /// Mark these read (or handled) on the server, and here.
    fn mark_notifications(&mut self, ids: Vec<i64>, handled: bool) {
        let Some(identity) = self.library_identity() else { return };
        let key = library::server_key();
        for n in self.notifications.items.iter_mut().filter(|n| ids.contains(&n.id)) {
            n.read = true;
            n.handled |= handled;
        }
        let server = library::official_url();
        std::thread::spawn(move || {
            if let Err(e) = client::mark_notifications(&server, &key, &identity, &ids, false, handled) {
                log::warn!("couldn't mark notifications: {e}");
            }
        });
    }

    /// The envelope, at the right of the header, and its list.
    pub(super) fn notifications_button_ui(&mut self, ui: &mut egui::Ui) {
        if self.library_identity().is_none() || !self.config.library.notifications {
            return;
        }
        let unread = self.notifications.items.iter().filter(|n| !n.read).count();
        // (The badge in the color of the most serious one unread.)
        let worst = self
            .notifications
            .items
            .iter()
            .filter(|n| !n.read)
            .max_by_key(|n| tier_of(&n.kind))
            .map(|n| color_of(ui, n));
        let envelope = super::icon("envelope-simple");
        let button = ui
            .add(egui::Button::new(egui::RichText::new(envelope).size(16.0)).frame(false))
            .on_hover_text(if unread == 0 { "Notifications".to_string() } else { format!("Notifications: {unread} unread") });
        if unread > 0 {
            let center = button.rect.right_top() + egui::vec2(-3.0, 4.0);
            let painter = ui.painter();
            let badge = worst.unwrap_or(ui.visuals().error_fg_color);
            painter.circle_filled(center, 7.0, badge);
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                if unread > 9 { "9+".to_string() } else { unread.to_string() },
                egui::FontId::proportional(9.5),
                super::on_color(badge),
            );
        }
        let mut open: Option<i64> = None;
        let mut all_read = false;
        egui::Popup::menu(&button).show(|ui| {
            // (Its lines wrap to that, so it fits a phone's screen.)
            ui.set_width(super::fit_width(ui, 340.0));
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
                for n in &self.notifications.items {
                    let mut line = egui::text::LayoutJob::default();
                    let style = ui.style();
                    egui::RichText::new(format!("{}  ", icon_of(&n.kind))).color(color_of(ui, n)).append_to(
                        &mut line,
                        style,
                        egui::FontSelection::Default,
                        egui::Align::Center,
                    );
                    let words = egui::RichText::new(text(n));
                    let words = if n.read { words } else { words.strong() };
                    words.append_to(&mut line, style, egui::FontSelection::Default, egui::Align::Center);
                    let when = library::ago(n.created);
                    // (Unread: an accent down its side, and a tint.)
                    let accent = ui.visuals().selection.bg_fill;
                    let fill = if n.read { egui::Color32::TRANSPARENT } else { accent.gamma_multiply(0.18) };
                    let frame = egui::Frame::new().fill(fill).corner_radius(4.0).inner_margin(egui::Margin::symmetric(6, 3));
                    let shown = frame.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let clicked = ui.add(egui::Button::selectable(false, line).wrap_mode(egui::TextWrapMode::Wrap)).clicked();
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
                        open = Some(n.id);
                    }
                }
            });
        });
        if all_read {
            let unread: Vec<i64> = self.notifications.items.iter().filter(|n| !n.read).map(|n| n.id).collect();
            self.mark_notifications(unread, false);
        }
        if let Some(id) = open {
            self.mark_notifications(vec![id], false);
            self.notifications.detail = Some(id);
        }
    }

    /// A notification's window: all of it, and what can be done.
    pub(super) fn notification_ui(&mut self, ctx: &egui::Context) {
        let Some(id) = self.notifications.detail else { return };
        let Some(n) = self.notifications.items.iter().find(|n| n.id == id).cloned() else {
            self.notifications.detail = None;
            return;
        };
        let mut close = false;
        let mut show: Option<String> = None;
        let mut moderate = false;
        let mut moderate_feedback = false;
        let mut copyright = false;
        let mut notice: Option<(i64, bool)> = None;
        let response = egui::Modal::new(egui::Id::new("notification")).show(ctx, |ui| {
            ui.set_width(super::fit_width(ui, 440.0));
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new(icon_of(&n.kind)).color(color_of(ui, &n)));
                ui.add(egui::Label::new(egui::RichText::new(text(&n)).size(15.0)).wrap());
            });
            ui.weak(library::ago(n.created));
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
                    "new_feedback" => {
                        if ui.button("Open Feedback").clicked() {
                            moderate_feedback = true;
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
            self.library_open_script(script);
        } else if moderate {
            self.notifications.detail = None;
            self.open_moderation();
        } else if moderate_feedback {
            self.notifications.detail = None;
            self.open_moderation_feedback();
        } else if copyright {
            self.notifications.detail = None;
            self.open_moderation_copyright();
        } else if let Some((id, dispute)) = notice {
            self.notifications.detail = None;
            self.open_notice(id, dispute);
        } else if close || response.should_close() {
            self.notifications.detail = None;
        }
    }
}

/// The listener: what's there, then the stream, over and over until
/// told to stop: straight away when a stream has run its time (the list
/// again first, for anything between), half a minute after one that failed.
fn listen(server: &str, key: &str, identity: &Identity, stop: &AtomicBool, tx: &Sender<Event>, ctx: &egui::Context) {
    while !stop.load(Ordering::Relaxed) {
        match client::notifications(server, key, identity, None) {
            Ok(list) => {
                let _ = tx.send(Event::List(list));
                ctx.request_repaint();
            }
            Err(e) => log::info!("notifications: {e}"),
        }
        let started = std::time::Instant::now();
        let streamed = client::notification_stream(server, key, identity, |event| {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            let _ = tx.send(match event {
                client::StreamEvent::Notification(n) => Event::New(n),
                client::StreamEvent::Sync(changed) => Event::Synced(changed),
                client::StreamEvent::Opened => Event::Opened,
                client::StreamEvent::Live(sealed) => Event::Live(sealed),
            });
            ctx.request_repaint();
            true
        });
        if let Err(e) = &streamed
            && started.elapsed() < client::STREAM_FOR
        {
            log::info!("notifications: {e}");
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

    /// Every kind the app words has a tier (so a color): a new kind added
    /// to `wording` without one would show grey.
    #[test]
    fn every_kind_has_a_tier() {
        let kinds = [
            "encore_milestone",
            "download_milestone",
            "hidden",
            "reinstated",
            "removed",
            "remixed",
            "report_handled",
            "new_report",
            "new_feedback",
            "copyright_notice",
            "notice_disputed",
            "dispute_upheld",
            "notice_rejected",
            "notice_withdrawn",
            "takedown_stood",
            "new_notice",
            "new_dispute",
        ];
        for kind in kinds {
            assert!(wording(kind).is_some(), "{kind} isn't worded");
            assert!(tier_of(kind) > 0, "{kind} has no tier");
        }
        assert_eq!(tier_of("copyright_notice"), 4);
        assert_eq!(tier_of("encore_milestone"), 1);
        assert_eq!(tier_of("something new"), 0);
    }

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
