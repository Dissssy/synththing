//! The Apps tab (and page, in the mobile layout): the panels programs put
//! there through the local API (`panel.rs`), and those shared from the
//! user's other devices (`panel_share.rs`), one at a time under a chip
//! per program, with what the user does in them sent back to the program.

use std::collections::HashMap;
use std::time::Instant;

use eframe::egui;

use super::App;
use crate::album_art::{AlbumArt, Source};
use crate::api::ShownPanel;
use crate::device::Device;
use crate::panel::{Element, Event, EventKind, Given, Item, Kind, Link, Panel};
use crate::panel_share::{self, Host, Message, Out, Viewer};
use crate::select_list::{self, Selection};

/// A list's rows with pictures (any item has a link): two lines' height.
const LINKED_ROW: f32 = 40.0;
/// The least height a list gets, in rows.
const LEAST_ROWS: f32 = 3.0;

/// Where a panel is: on this device (none) or another (its ID), and whose.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Place {
    device: Option<String>,
    app: String,
}

/// A panel as the tab shows it.
struct View {
    place: Place,
    /// The other device it's on, by name.
    device: Option<String>,
    name: String,
    panel: Panel,
    /// Whether its program's listening (from the host, for another
    /// device's; this device's are looked at as they're drawn).
    connected: bool,
}

#[derive(Default)]
pub(super) struct PanelsState {
    /// The local API's panels revision these are from, and the viewer's.
    revision: u64,
    remote_revision: u64,
    local: Vec<ShownPanel>,
    host: Host,
    viewer: Viewer,
    shown: Vec<View>,
    picked: Option<Place>,
    /// The newest value given to a textbox that's been taken, by panel.
    taken: HashMap<Place, u64>,
    /// What's typed in each textbox, by panel and element ID.
    texts: HashMap<(Place, String), String>,
    /// Each list's selected rows, by panel and element ID.
    selections: HashMap<(Place, String), Selection>,
    art: AlbumArt,
    /// The panel last drawn, and in which pass.
    drawn: Option<(Place, u64)>,
    /// The panel its program was last told is in view, and whether the
    /// program was listening then.
    visible: Option<(Place, bool)>,
}

/// A textbox's widget ID, to tell whether it's being typed in.
fn textbox_id(place: &Place, element: &str) -> egui::Id {
    egui::Id::new(("api panel textbox", place, element))
}

/// What a click on a linked item (or its menu) asks of the app.
enum Act {
    Play(String),
    Queue(String, bool),
    ShowWishlist,
}

impl PanelsState {
    /// Whether any program has a panel (the phone menu lists Apps then).
    pub(super) fn any(&self) -> bool {
        !self.shown.is_empty()
    }

    /// Show the panels as they are now, this device's then the others':
    /// each textbox given a value since takes it, unless it's being typed in.
    fn rebuild(&mut self, ctx: &egui::Context) {
        let mut shown = Vec::new();
        let mut given: Vec<(Place, &[Given])> = Vec::new();
        for s in &self.local {
            let place = Place { device: None, app: s.app.clone() };
            given.push((place.clone(), &s.given));
            shown.push(View { place, device: None, name: s.name.clone(), panel: s.panel.clone(), connected: false });
        }
        for host in self.viewer.hosts() {
            for p in &host.panels {
                let place = Place { device: Some(host.device.id.clone()), app: p.shared.app.clone() };
                given.push((place.clone(), &p.shared.given));
                let (device, name) = (Some(host.device.name.clone()), p.shared.name.clone());
                shown.push(View { place, device, name, panel: p.panel.clone(), connected: p.shared.connected });
            }
        }
        let gone = |place: &Place| !shown.iter().any(|v| &v.place == place);
        self.texts.retain(|(place, _), _| !gone(place));
        self.selections.retain(|(place, _), _| !gone(place));
        self.taken.retain(|place, _| !gone(place));
        for (place, given) in given {
            let taken = self.taken.entry(place.clone()).or_default();
            for g in given.iter().filter(|g| g.revision > *taken) {
                if !ctx.memory(|m| m.has_focus(textbox_id(&place, &g.id))) {
                    self.texts.insert((place.clone(), g.id.clone()), g.value.clone());
                }
            }
            *taken = given.iter().map(|g| g.revision).max().unwrap_or(0).max(*taken);
        }
        if self.picked.as_ref().is_none_or(|picked| gone(picked)) {
            self.picked = shown.first().map(|v| v.place.clone());
        }
        self.shown = shown;
    }
}

impl App {
    /// Every frame (`api_tick`): the panels as the API and the other
    /// devices have them, what the others are due to hear of this one's,
    /// and the programs told when theirs comes into view or leaves it.
    pub(super) fn panels_tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let server = self.api.server();
        let mut changed = false;
        match &server {
            Some(server) => {
                if let Some(local) = server.panels(&mut self.panels.revision) {
                    self.panels.host.take(&local);
                    self.panels.local = local;
                    changed = true;
                }
            }
            None if !self.panels.local.is_empty() => {
                self.panels.host.take(&[]);
                self.panels.local.clear();
                changed = true;
            }
            None => {}
        }
        if !self.sync_on() {
            self.panels.viewer.clear();
        }
        self.panels.viewer.expire(now);
        if self.panels.viewer.revision() != self.panels.remote_revision {
            self.panels.remote_revision = self.panels.viewer.revision();
            changed = true;
        }
        if changed {
            self.panels.rebuild(ctx);
        }
        let connected = |app: &str| server.as_ref().is_some_and(|s| s.connected(app));
        let out = self.panels.host.tick(now, connected);
        self.send_panels(out);
        let due = [self.panels.host.due(now), self.panels.viewer.due(now)].into_iter().flatten().min();
        if let Some(due) = due {
            ctx.request_repaint_after(due);
        }

        // (Drawn in this pass or the last: it's in view.)
        let pass = ctx.cumulative_pass_nr();
        let state = &self.panels;
        let now_visible = state.drawn.as_ref().filter(|(_, at)| at + 1 >= pass).map(|(place, _)| (place.clone(), self.panel_connected(place)));
        if now_visible == state.visible {
            return;
        }
        if let Some((was, true)) = state.visible.clone()
            && now_visible.as_ref().is_none_or(|(place, _)| place != &was)
        {
            self.panel_event(&was, Event { kind: EventKind::Hidden, ..Event::default() });
        }
        if let Some((place, true)) = &now_visible {
            self.panel_event(place, Event { kind: EventKind::Shown, ..Event::default() });
        }
        self.panels.visible = now_visible;
    }

    /// Whether a panel's program is listening.
    fn panel_connected(&self, place: &Place) -> bool {
        match &place.device {
            None => self.api.server().is_some_and(|s| s.connected(&place.app)),
            Some(_) => self.panels.shown.iter().any(|v| &v.place == place && v.connected),
        }
    }

    /// Tell a panel's program what the user did: straight, or through the
    /// device it's on.
    fn panel_event(&mut self, place: &Place, event: Event) {
        match &place.device {
            None => {
                if let Some(server) = self.api.server() {
                    server.panel_event(&place.app, &event);
                }
            }
            Some(device) => {
                let message = Message::Event { app: place.app.clone(), event };
                self.send_panels(vec![Out { to: Some(device.clone()), message }]);
            }
        }
    }

    fn send_panels(&mut self, out: Vec<Out>) {
        for Out { to, message } in out {
            match serde_json::to_value(&message) {
                Ok(body) => self.send_live(to, panel_share::TOPIC, body),
                Err(e) => log::warn!("a panels message: {e}"),
            }
        }
    }

    /// This device listens again: ask the others for their panels.
    pub(super) fn panels_hello(&mut self) {
        self.send_panels(vec![Out { to: None, message: Message::Hello }]);
    }

    /// Another device said something about panels.
    pub(super) fn panels_heard(&mut self, from: Device, body: serde_json::Value) {
        let message = match serde_json::from_value::<Message>(body) {
            Ok(message) => message,
            Err(e) => return log::info!("a panels message from {} doesn't read: {e}", from.name),
        };
        match message {
            Message::Hello => {
                let server = self.api.server();
                let out = self.panels.host.hello(&from.id, |app| server.as_ref().is_some_and(|s| s.connected(app)));
                self.send_panels(out);
            }
            Message::Event { app, event } => {
                if let (Some(server), Some(event)) = (self.api.server(), self.panels.host.event(from, &app, event)) {
                    server.panel_event(&app, &event);
                }
            }
            message => {
                let answer = self.panels.viewer.hear(from, message, Instant::now());
                self.send_panels(answer.into_iter().collect());
            }
        }
    }

    /// The Apps tab: a chip for each program with a panel, and the picked
    /// one's panel, greyed while its program isn't listening.
    pub(super) fn apps_ui(&mut self, ui: &mut egui::Ui) {
        if self.api.server().is_none() && self.panels.shown.is_empty() {
            super::placeholder(ui, "Programs can show panels here through the local API.\nTurn it on in Preferences > Integrations.");
            return;
        }
        if self.panels.shown.is_empty() {
            super::placeholder(ui, "No program has a panel here yet.\nA program allowed to Show a panel (Preferences > Integrations) puts one here with PUT /v1/panel.");
            return;
        }
        let state = &mut self.panels;
        state.art.frame(ui.ctx(), self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup));
        ui.horizontal_wrapped(|ui| {
            for view in &state.shown {
                let picked = state.picked.as_ref() == Some(&view.place);
                let label = match &view.device {
                    Some(device) => format!("{} · {device}", view.name),
                    None => view.name.clone(),
                };
                if ui.selectable_label(picked, label).clicked() {
                    state.picked = Some(view.place.clone());
                }
            }
        });
        let Some(at) = state.shown.iter().position(|v| state.picked.as_ref() == Some(&v.place)) else { return };
        let place = state.shown[at].place.clone();
        state.drawn = Some((place.clone(), ui.ctx().cumulative_pass_nr()));
        let connected = self.panel_connected(&place);
        let state = &mut self.panels;
        let view = &state.shown[at];
        let mut close = false;
        ui.horizontal(|ui| {
            let (dot, hover) = if connected {
                (egui::Color32::from_rgb(80, 200, 120), "Listening")
            } else {
                (ui.visuals().weak_text_color(), "Not running")
            };
            ui.label(egui::RichText::new("●").color(dot)).on_hover_text(hover);
            ui.strong(if view.panel.title.is_empty() { &view.name } else { &view.panel.title });
            match &view.device {
                Some(device) => ui.weak(format!("from {} on {device}", view.name)),
                None => ui.weak(format!("from {}", view.name)),
            };
            if !connected && view.device.is_none() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.add(super::icon_button("x")).on_hover_text("Close this panel").clicked();
                });
            }
        });
        if !connected {
            match &view.device {
                Some(device) => ui.weak(format!("{} isn't running on {device}. Its panel comes back when it does.", view.name)),
                None => ui.weak(format!("{} isn't running. Its panel comes back when it does.", view.name)),
            };
        }
        ui.separator();
        let mut drawing = Drawing {
            place: &view.place,
            texts: &mut state.texts,
            selections: &mut state.selections,
            art: &mut state.art,
            songs: &self.songs,
            wishes: &self.wishlist.list.wishes,
            events: Vec::new(),
            acts: Vec::new(),
        };
        let elements = &view.panel.elements;
        let lists = elements.iter().filter(|e| matches!(e.kind, Kind::List { .. })).count();
        ui.add_enabled_ui(connected, |ui| {
            // (Lists share the height left under what's above them, and
            // scroll themselves; a panel without any scrolls as a whole.)
            if lists > 0 {
                drawing.elements(ui, elements, false, lists);
            } else {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| drawing.elements(ui, elements, false, 0));
            }
        });
        let (events, acts) = (drawing.events, drawing.acts);
        for event in events {
            self.panel_event(&place, event);
        }
        if close && let Some(server) = self.api.server() {
            server.close_panel(&place.app);
        }
        for act in acts {
            match act {
                Act::Play(key) => self.play_path(self.songs.path(&key)),
                Act::Queue(key, next) => self.queue_songs(ui.ctx(), vec![self.songs.path(&key)], next),
                Act::ShowWishlist => self.go_to_wishlist(),
            }
        }
    }
}

/// What drawing a panel needs, and what the user did in it.
struct Drawing<'a> {
    place: &'a Place,
    texts: &'a mut HashMap<(Place, String), String>,
    selections: &'a mut HashMap<(Place, String), Selection>,
    art: &'a mut AlbumArt,
    songs: &'a crate::song_store::SongStore,
    wishes: &'a std::collections::BTreeMap<String, crate::wishlist::Wish>,
    events: Vec<Event>,
    acts: Vec<Act>,
}

impl Drawing<'_> {
    /// A panel's elements, stacked (or side by side in a row); `lists`:
    /// how many lists are among them, to share the height left.
    fn elements(&mut self, ui: &mut egui::Ui, elements: &[Element], in_row: bool, mut lists: usize) {
        for element in elements {
            let id = &element.id;
            match &element.kind {
                Kind::Label { text, weak } => {
                    if *weak {
                        ui.weak(text);
                    } else {
                        ui.label(text);
                    }
                }
                Kind::Textbox { hint, multiline, .. } => {
                    let text = self.texts.entry((self.place.clone(), id.clone())).or_default();
                    let edit = if *multiline { egui::TextEdit::multiline(text) } else { egui::TextEdit::singleline(text) };
                    let width = if in_row { 200.0 } else { ui.available_width() };
                    let response = ui.add(edit.id(textbox_id(self.place, id)).hint_text(hint.as_str()).desired_width(width));
                    if response.changed() {
                        self.events.push(Event { value: Some(text.clone()), ..Event::of(id, EventKind::Changed) });
                    }
                    if !*multiline && response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.events.push(Event { value: Some(text.clone()), ..Event::of(id, EventKind::Submitted) });
                    }
                }
                Kind::Button { text, enabled } => {
                    if ui.add_enabled(*enabled, egui::Button::new(text)).clicked() {
                        self.events.push(Event::of(id, EventKind::Clicked));
                    }
                }
                Kind::Row { elements } => {
                    ui.horizontal_wrapped(|ui| self.elements(ui, elements, true, 0));
                }
                Kind::List { items } => {
                    let least = LEAST_ROWS * ui.spacing().interact_size.y;
                    let height = (ui.available_height() / lists.max(1) as f32).max(least);
                    lists = lists.saturating_sub(1);
                    ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| self.list(ui, id, items));
                }
            }
        }
    }

    /// A list: a row per item (with its picture, if it's linked), its
    /// actions as buttons at the end; a click picks it (and goes where it
    /// links), a right-click on a linked one offers what fits it.
    fn list(&mut self, ui: &mut egui::Ui, id: &str, items: &[Item]) {
        let selection = self.selections.entry((self.place.clone(), id.to_string())).or_default();
        let linked = items.iter().any(|item| item.link.is_some());
        let (art, songs, wishes) = (&mut *self.art, self.songs, self.wishes);
        let (mut picked, mut acts) = (None, Vec::new());
        let list_id = egui::Id::new(("api panel list", self.place, id));
        let output = select_list::show_tall(ui, list_id, items, selection, None, if linked { LINKED_ROW } else { 0.0 }, |ui, row, item| {
            if let Some(link) = &item.link {
                crate::menu::context(&row.response, |ui| match link {
                    Link::Song(key) if songs.has(key) => {
                        if ui.button("Play").clicked() {
                            acts.push(Act::Play(key.clone()));
                        }
                        if ui.button("Play next").clicked() {
                            acts.push(Act::Queue(key.clone(), true));
                        }
                        if ui.button("Add to queue").clicked() {
                            acts.push(Act::Queue(key.clone(), false));
                        }
                    }
                    Link::Song(_) => {
                        ui.weak("Not in your library");
                    }
                    Link::Wish(_) => {
                        if ui.button("Show in the Wishlist").clicked() {
                            acts.push(Act::ShowWishlist);
                        }
                    }
                });
                let source = match link {
                    Link::Song(key) => Source::of_song(songs.has(key).then(|| songs.path(key)), songs.get_by_key(key)),
                    Link::Wish(wish) => wishes.get(wish).map(super::wishlist::source).unwrap_or_default(),
                };
                let side = ui.available_height();
                let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                art.tile(ui, rect.shrink(1.0), &source, &item.text);
            }
            let scroll = select_list::scrolls(row, false);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for action in item.actions.iter().rev() {
                    if ui.small_button(action).clicked() {
                        picked = Some((row.idx, Some(action.clone())));
                    }
                }
                let detail = (!item.subtitle.is_empty()).then(|| item.subtitle.clone());
                select_list::two_lines(ui, std::iter::empty(), item.text.as_str(), detail, scroll);
            });
        });
        if let Some(i) = output.activated {
            picked = picked.or(Some((i, None)));
            match &items[i].link {
                Some(Link::Song(key)) if picked.as_ref().is_some_and(|(_, action)| action.is_none()) && songs.has(key) => acts.push(Act::Play(key.clone())),
                Some(Link::Wish(_)) if picked.as_ref().is_some_and(|(_, action)| action.is_none()) => acts.push(Act::ShowWishlist),
                _ => {}
            }
        }
        if let Some((i, action)) = picked {
            self.events.push(Event { item: Some(items[i].id.clone()), action, ..Event::of(id, EventKind::Picked) });
        }
        self.acts.extend(acts);
    }
}
