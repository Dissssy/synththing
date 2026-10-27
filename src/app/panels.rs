//! The Apps tab (and page, in the mobile layout): the panels programs put
//! there through the local API (`panel.rs`), one at a time under a chip
//! per program, with what the user does in them sent back to the program.

use std::collections::HashMap;

use eframe::egui;

use super::App;
use crate::album_art::{AlbumArt, Source};
use crate::api::ShownPanel;
use crate::panel::{Element, Event, EventKind, Item, Kind, Link};
use crate::select_list::{self, Selection};

/// A list's rows with pictures (any item has a link): two lines' height.
const LINKED_ROW: f32 = 40.0;
/// The least height a list gets, in rows.
const LEAST_ROWS: f32 = 3.0;

#[derive(Default)]
pub(super) struct PanelsState {
    /// The API's panels revision these are from.
    revision: u64,
    shown: Vec<ShownPanel>,
    /// The program whose panel shows (its token's hash).
    picked: Option<String>,
    /// What's typed in each textbox, by program and element ID.
    texts: HashMap<(String, String), String>,
    /// Each list's selected rows, by program and element ID.
    selections: HashMap<(String, String), Selection>,
    art: AlbumArt,
    /// The panel last drawn, and in which pass.
    drawn: Option<(String, u64)>,
    /// The panel its program was last told is in view, and whether the
    /// program was listening then.
    visible: Option<(String, bool)>,
}

/// A textbox's widget ID, to tell whether it's being typed in.
fn textbox_id(app: &str, element: &str) -> egui::Id {
    egui::Id::new(("api panel textbox", app, element))
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

    /// Take the panels as they are now: each textbox the program gave a
    /// value since takes it, unless it's being typed in.
    fn take(&mut self, shown: Vec<ShownPanel>, ctx: &egui::Context) {
        let gone = |app: &String| !shown.iter().any(|s| &s.app == app);
        self.texts.retain(|(app, _), _| !gone(app));
        self.selections.retain(|(app, _), _| !gone(app));
        for panel in &shown {
            for (id, value) in &panel.fresh {
                if !ctx.memory(|m| m.has_focus(textbox_id(&panel.app, id))) {
                    self.texts.insert((panel.app.clone(), id.clone()), value.clone());
                }
            }
        }
        if self.picked.as_ref().is_none_or(|picked| gone(picked)) {
            self.picked = shown.first().map(|s| s.app.clone());
        }
        self.shown = shown;
    }
}

impl App {
    /// Every frame (`api_tick`): the panels as the API has them, and the
    /// programs told when theirs comes into view or leaves it.
    pub(super) fn panels_tick(&mut self, ctx: &egui::Context) {
        let Some(server) = self.api.server() else {
            if self.panels.any() {
                self.panels = PanelsState::default();
            }
            return;
        };
        if let Some(shown) = server.panels(&mut self.panels.revision) {
            self.panels.take(shown, ctx);
        }
        // (Drawn in this pass or the last: it's in view.)
        let pass = ctx.cumulative_pass_nr();
        let state = &mut self.panels;
        let now = state.drawn.as_ref().filter(|(_, at)| at + 1 >= pass).map(|(app, _)| (app.clone(), server.connected(app)));
        if now == state.visible {
            return;
        }
        let tell = |app: &str, kind| server.panel_event(app, &Event { kind, ..Event::default() });
        if let Some((was, true)) = &state.visible
            && now.as_ref().is_none_or(|(app, _)| app != was)
        {
            tell(was, EventKind::Hidden);
        }
        if let Some((app, true)) = &now {
            tell(app, EventKind::Shown);
        }
        state.visible = now;
    }

    /// The Apps tab: a chip for each program with a panel, and the picked
    /// one's panel, greyed while its program isn't listening.
    pub(super) fn apps_ui(&mut self, ui: &mut egui::Ui) {
        let Some(server) = self.api.server() else {
            ui.weak("Programs can show panels here through the local API. Turn it on in Preferences > Integrations.");
            return;
        };
        let state = &mut self.panels;
        if state.shown.is_empty() {
            ui.weak("No program has a panel here yet. A program allowed to Show a panel (Preferences > Integrations) puts one here with PUT /v1/panel.");
            return;
        }
        state.art.frame(ui.ctx(), self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup));
        ui.horizontal_wrapped(|ui| {
            for shown in &state.shown {
                let picked = state.picked.as_ref() == Some(&shown.app);
                if ui.selectable_label(picked, &shown.name).clicked() {
                    state.picked = Some(shown.app.clone());
                }
            }
        });
        let Some(shown) = state.shown.iter().find(|s| state.picked.as_ref() == Some(&s.app)) else { return };
        state.drawn = Some((shown.app.clone(), ui.ctx().cumulative_pass_nr()));
        let connected = server.connected(&shown.app);
        let mut close = false;
        ui.horizontal(|ui| {
            let (dot, hover) = if connected {
                (egui::Color32::from_rgb(80, 200, 120), "Listening")
            } else {
                (ui.visuals().weak_text_color(), "Not running")
            };
            ui.label(egui::RichText::new("●").color(dot)).on_hover_text(hover);
            ui.strong(if shown.panel.title.is_empty() { &shown.name } else { &shown.panel.title });
            ui.weak(format!("from {}", shown.name));
            if !connected {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.add(super::icon_button("x")).on_hover_text("Close this panel").clicked();
                });
            }
        });
        if !connected {
            ui.weak(format!("{} isn't running. Its panel comes back when it does.", shown.name));
        }
        ui.separator();
        let mut drawing = Drawing {
            app: &shown.app,
            texts: &mut state.texts,
            selections: &mut state.selections,
            art: &mut state.art,
            songs: &self.songs,
            wishes: &self.wishlist.list.wishes,
            events: Vec::new(),
            acts: Vec::new(),
        };
        let elements = &shown.panel.elements;
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
        for event in &events {
            server.panel_event(&shown.app, event);
        }
        if close {
            server.close_panel(&shown.app);
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
    app: &'a str,
    texts: &'a mut HashMap<(String, String), String>,
    selections: &'a mut HashMap<(String, String), Selection>,
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
                    let text = self.texts.entry((self.app.to_string(), id.clone())).or_default();
                    let edit = if *multiline { egui::TextEdit::multiline(text) } else { egui::TextEdit::singleline(text) };
                    let width = if in_row { 200.0 } else { ui.available_width() };
                    let response = ui.add(edit.id(textbox_id(self.app, id)).hint_text(hint.as_str()).desired_width(width));
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
        let selection = self.selections.entry((self.app.to_string(), id.to_string())).or_default();
        let linked = items.iter().any(|item| item.link.is_some());
        let (art, songs, wishes) = (&mut *self.art, self.songs, self.wishes);
        let (mut picked, mut acts) = (None, Vec::new());
        let list_id = egui::Id::new(("api panel list", self.app, id));
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
