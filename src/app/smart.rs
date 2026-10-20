//! Smart playlists and rulesets on the `App` side (the engine is
//! `crate::smart`): keeping each smart playlist's songs what its rules say
//! as the library changes, the rules editor (a modal; on touch screens it
//! only shows the rules, unless `EDIT_RULES_ON_TOUCH`), and the Rulesets
//! section of Preferences.

use std::collections::BTreeMap;
use std::sync::Arc;

use eframe::egui;

use super::{fit_width, App};
use crate::playlist::{Playlist, PlaylistEntry};
use crate::select_list;
use crate::settings_sync;
use crate::smart::{self, Field, Group, Kind, Limit, Op, Order, Rule, Rules, Ruleset, Split, SplitBy, Unit};
use crate::song_store::Tag;

/// Whether rules can be edited on a touch screen (they can always be
/// looked at): off until editing there is good to use.
const EDIT_RULES_ON_TOUCH: bool = false;

/// The icon smart playlists have.
pub(super) const SMART_ICON: &str = "sparkle";

/// Whether rules can be edited here (not on a touch screen, for now).
pub(super) fn rules_editable(ctx: &egui::Context) -> bool {
    !select_list::touch(ctx) || EDIT_RULES_ON_TOUCH
}

/// How many of a ruleset's playlists Preferences lists.
const SHOWN_GROUPS: usize = 200;

/// What the editor's rules are for.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Editing {
    /// A smart playlist, by ID (`None`: a new one).
    Playlist(Option<String>),
    /// A ruleset, by ID (`None`: a new one).
    Ruleset(Option<String>),
}

/// The rules editor: what it's editing, the name and rules as edited so
/// far, and what they come to (worked out again when they change).
pub(super) struct RulesEditor {
    editing: Editing,
    name: String,
    rules: Rules,
    read_only: bool,
    preview: Option<(u64, Rules, String)>,
}

impl App {
    /// Work each smart playlist's songs out again if the library changed
    /// (or `force`d: a playlist's rules did), keeping the one playing on
    /// the song it's at (or letting go of the playlist, if that song's no
    /// longer on it).
    pub(super) fn refresh_smart(&mut self, force: bool) {
        let revision = self.songs.revision();
        if !force && self.smart_seen == Some(revision) {
            return;
        }
        self.smart_seen = Some(revision);
        let now = settings_sync::now();
        let mut replanned = false;
        for (i, stored) in self.playlists.lists.iter_mut().enumerate() {
            let Some(rules) = &stored.playlist.smart else { continue };
            let entries: Vec<PlaylistEntry> = rules
                .songs(&self.songs, now)
                .into_iter()
                .map(|key| PlaylistEntry { path: self.songs.path(&key), soundfont: None, missing: !self.songs.has(&key) })
                .collect();
            let old = &stored.playlist.entries;
            if old.len() == entries.len() && old.iter().zip(&entries).all(|(a, b)| a.path == b.path && a.missing == b.missing) {
                continue;
            }
            if self.now_playing.as_ref().is_some_and(|np| np.list == i) {
                let playing = self.now_playing.as_ref().and_then(|np| old.get(np.entry)).map(|e| e.path.clone());
                match playing.and_then(|path| entries.iter().position(|e| e.path == path)) {
                    Some(at) => {
                        if let Some(np) = &mut self.now_playing {
                            np.entry = at;
                            np.history = vec![at];
                            np.upcoming = None;
                        }
                    }
                    None => self.now_playing = None,
                }
                replanned = true;
            }
            stored.playlist.entries = entries;
        }
        if replanned {
            self.plan_upcoming();
        }
    }

    /// Open the rules of playlist `list`, or of a new smart playlist.
    pub(super) fn edit_playlist_rules(&mut self, ctx: &egui::Context, list: Option<usize>) {
        let (editing, name, rules) = match list.and_then(|l| self.playlists.lists.get(l)) {
            Some(stored) => (Editing::Playlist(Some(stored.id.clone())), stored.playlist.name.clone(), stored.playlist.smart.clone().unwrap_or_default()),
            None => (Editing::Playlist(None), self.unique_playlist_name("Smart playlist"), Rules::default()),
        };
        self.open_rules_editor(ctx, editing, name, rules, false);
    }

    fn open_rules_editor(&mut self, ctx: &egui::Context, editing: Editing, name: String, rules: Rules, builtin: bool) {
        let read_only = builtin || !rules_editable(ctx);
        self.rules_editor = Some(RulesEditor { editing, name, rules, read_only, preview: None });
    }

    /// A playlist of the songs smart playlist `list` has now, which stays
    /// as it is.
    pub(super) fn freeze_playlist(&mut self, list: usize) {
        let Some(stored) = self.playlists.lists.get(list) else { return };
        let name = self.unique_playlist_name(&format!("{} (copy)", stored.playlist.name));
        let songs = stored.playlist.entries.iter().map(|e| e.path.clone()).collect();
        self.create_playlist(&name, songs);
        self.status = format!("Saved as {name}.");
    }

    /// The rules editor, while it's open.
    pub(super) fn rules_editor_ui(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.rules_editor else { return };
        let revision = self.songs.revision();
        if editor.preview.as_ref().is_none_or(|(seen, rules, _)| *seen != revision || *rules != editor.rules) {
            editor.preview = Some((revision, editor.rules.clone(), summary(&editor.rules, &self.songs)));
        }
        let ruleset = matches!(editor.editing, Editing::Ruleset(_));
        let new = matches!(editor.editing, Editing::Playlist(None) | Editing::Ruleset(None));
        let mut done = None;
        let response = egui::Modal::new(egui::Id::new("rules_editor")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 560.0));
            ui.heading(match (ruleset, new) {
                (false, true) => "New smart playlist",
                (false, false) => "Smart playlist",
                (true, true) => "New ruleset",
                (true, false) => "Ruleset",
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.6).show(ui, |ui| {
                ui.add_enabled_ui(!editor.read_only, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut editor.name);
                    });
                    rules_ui(ui, &mut editor.rules, self.songs.tags(), ruleset);
                });
            });
            ui.add_space(4.0);
            if let Some((_, _, summary)) = &editor.preview {
                ui.weak(summary);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if editor.read_only {
                    if ui.button("Close").clicked() {
                        done = Some(false);
                    }
                    if !rules_editable(ctx) {
                        ui.weak("Rules can be edited on a computer.");
                    }
                    return;
                }
                let named = !editor.name.trim().is_empty();
                if ui.add_enabled(named, egui::Button::new("Save")).clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
        });
        if response.should_close() && done.is_none() {
            done = Some(false);
        }
        let Some(save) = done else { return };
        let Some(editor) = self.rules_editor.take() else { return };
        if save {
            self.save_rules(editor);
        }
    }

    fn save_rules(&mut self, editor: RulesEditor) {
        let RulesEditor { editing, name, rules, .. } = editor;
        let name = name.trim().to_string();
        match editing {
            Editing::Playlist(id) => {
                let playlist = Playlist { name, entries: Vec::new(), smart: Some(rules) };
                let at = match id.and_then(|id| self.playlists.find(&id)) {
                    Some(at) => {
                        self.playlists.lists[at].playlist.name = playlist.name;
                        self.playlists.lists[at].playlist.smart = playlist.smart;
                        at
                    }
                    None => match self.playlists.create(&playlist.name, Vec::new()) {
                        Ok(at) => {
                            self.playlists.lists[at].playlist.smart = playlist.smart;
                            self.groupings.show(super::groupings::PLAYLISTS);
                            self.open_playlist(Some(at));
                            at
                        }
                        Err(e) => {
                            self.status = format!("Couldn't create playlist: {e}");
                            return;
                        }
                    },
                };
                self.refresh_smart(true);
                self.save_playlist(at);
            }
            Editing::Ruleset(id) => {
                let id = id.unwrap_or_else(|| format!("{:016x}", smart::new_seed()));
                self.config.rulesets.insert(id, Ruleset { name, rules });
                self.save_config();
            }
        }
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            log::warn!("saving the config: {e}");
        }
    }

    /// The playlists ruleset `id` (with `rules`) makes, worked out again
    /// when the library or the rules change.
    pub(super) fn groups_of(&mut self, id: &str, rules: &Rules) -> Arc<Vec<Group>> {
        let revision = self.songs.revision();
        if let Some((seen, cached, groups)) = self.ruleset_groups.get(id)
            && *seen == revision
            && cached == rules
        {
            return groups.clone();
        }
        let groups = Arc::new(rules.groups(&self.songs, settings_sync::now()));
        self.ruleset_groups.insert(id.to_string(), (revision, rules.clone(), groups.clone()));
        groups
    }

    /// Every ruleset: the built-in ones, then the user's, by ID.
    pub(super) fn all_rulesets(&self) -> Vec<(String, Ruleset)> {
        let builtin = smart::builtin().map(|(id, ruleset)| (id.to_string(), ruleset));
        builtin.into_iter().chain(self.config.rulesets.iter().map(|(id, r)| (id.clone(), r.clone()))).collect()
    }

    /// Preferences' Rulesets: the built-in ones and the user's, each
    /// opening to the playlists it makes.
    pub(super) fn rulesets_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("Rulesets");
        ui.weak("A ruleset makes a playlist for each artist, album, genre... of the songs its rules pick. By Artist and By Album are built in.");
        let builtin = smart::builtin().map(|(id, ruleset)| (id.to_string(), ruleset, true));
        let own = self.config.rulesets.iter().map(|(id, ruleset)| (id.clone(), ruleset.clone(), false));
        let all: Vec<(String, Ruleset, bool)> = builtin.into_iter().chain(own).collect();
        let (mut open, mut delete) = (None, None);
        for (id, ruleset, builtin) in all {
            egui::CollapsingHeader::new(&ruleset.name).id_salt(("ruleset", &id)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let label = if builtin || !rules_editable(ui.ctx()) { "View rules" } else { "Edit rules" };
                    if ui.button(label).clicked() {
                        open = Some((id.clone(), ruleset.clone(), builtin));
                    }
                    if !builtin && ui.button("Delete").clicked() {
                        delete = Some(id.clone());
                    }
                });
                let groups = self.groups_of(&id, &ruleset.rules);
                ui.weak(format!("{} playlist{}", groups.len(), if groups.len() == 1 { "" } else { "s" }));
                for group in groups.iter().take(SHOWN_GROUPS) {
                    let subtitle = group.subtitle.as_deref().map(|s| format!(" \u{b7} {s}")).unwrap_or_default();
                    ui.label(format!("{}{subtitle} ({})", group.title, group.songs.len()));
                }
                if groups.len() > SHOWN_GROUPS {
                    ui.weak(format!("and {} more", groups.len() - SHOWN_GROUPS));
                }
            });
        }
        if rules_editable(ui.ctx()) && ui.button("New ruleset...").clicked() {
            let rules = Rules { split: Some(Split { by: SplitBy::Genre, min: 1 }), ..Rules::default() };
            self.open_rules_editor(ui.ctx(), Editing::Ruleset(None), "By Genre".into(), rules, false);
        }
        if let Some((id, ruleset, builtin)) = open {
            self.open_rules_editor(ui.ctx(), Editing::Ruleset(Some(id)), ruleset.name, ruleset.rules, builtin);
        }
        if let Some(id) = delete {
            self.config.rulesets.remove(&id);
            self.ruleset_groups.remove(&id);
            self.save_config();
        }
    }
}

/// The ruleset playlists last worked out, by ruleset ID: for which library
/// revision and rules.
pub(super) type RulesetGroups = BTreeMap<String, (u64, Rules, Arc<Vec<Group>>)>;

/// What `rules` come to in `store` now, in a line.
fn summary(rules: &Rules, store: &crate::song_store::SongStore) -> String {
    let now = settings_sync::now();
    if rules.split.is_some() {
        let groups = rules.groups(store, now);
        let songs: usize = groups.iter().map(|g| g.songs.len()).sum();
        format!("{} playlist{}, {songs} songs in all", groups.len(), if groups.len() == 1 { "" } else { "s" })
    } else {
        let songs = rules.songs(store, now).len();
        format!("{songs} song{} now", if songs == 1 { "" } else { "s" })
    }
}

/// Edit `rules` (a ruleset's, with what it splits by, if `ruleset`).
fn rules_ui(ui: &mut egui::Ui, rules: &mut Rules, tags: &BTreeMap<String, Tag>, ruleset: bool) {
    ui.horizontal(|ui| {
        ui.label("Songs matching");
        egui::ComboBox::from_id_salt("rules_match").selected_text(if rules.any { "any" } else { "all" }).show_ui(ui, |ui| {
            ui.selectable_value(&mut rules.any, false, "all");
            ui.selectable_value(&mut rules.any, true, "any");
        });
        ui.label("of these rules:");
    });
    let mut remove = None;
    for (i, rule) in rules.rules.iter_mut().enumerate() {
        ui.horizontal_wrapped(|ui| {
            ui.push_id(i, |ui| {
                rule_ui(ui, rule, tags);
                if ui.add(super::icon_button("x")).on_hover_text("Remove this rule").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        rules.rules.remove(i);
    }
    if rules.rules.is_empty() {
        ui.weak("No rules: every song.");
    }
    if ui.button("Add a rule").clicked() {
        rules.rules.push(Rule::new(Field::Artist));
    }
    ui.add_space(4.0);

    let mut limited = rules.limit.is_some();
    ui.horizontal_wrapped(|ui| {
        if ui.checkbox(&mut limited, "Limit to").changed() {
            rules.limit = limited.then_some(Limit { amount: 25.0, unit: Unit::Items, by: Order::Random });
        }
        if let Some(limit) = &mut rules.limit {
            ui.add(egui::DragValue::new(&mut limit.amount).range(0.0..=100_000.0).speed(1.0));
            pick(ui, "limit_unit", &mut limit.unit, &Unit::ALL, |u| u.title());
            ui.label("picked by");
            pick(ui, "limit_by", &mut limit.by, &Order::ALL, |o| o.title());
        }
    });
    ui.horizontal(|ui| {
        ui.label("In order of");
        pick(ui, "rules_order", &mut rules.order, &Order::ALL, |o| o.title());
    });
    let random = rules.order == Order::Random || rules.limit.as_ref().is_some_and(|l| l.by == Order::Random);
    if random
        && ui.button("Re-seed").on_hover_text("Pick a new random order (the same on all your devices)").clicked()
    {
        rules.seed = smart::new_seed();
    }
    if ruleset && let Some(split) = &mut rules.split {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("A playlist for each");
            pick(ui, "split_by", &mut split.by, &SplitBy::ALL, |s| s.title());
            ui.label("with at least");
            ui.add(egui::DragValue::new(&mut split.min).range(1..=1000));
            ui.label("songs");
        });
    }
}

/// One rule: field, op and its values.
fn rule_ui(ui: &mut egui::Ui, rule: &mut Rule, tags: &BTreeMap<String, Tag>) {
    let before = rule.field;
    pick(ui, "field", &mut rule.field, &Field::ALL, |f| f.title());
    if rule.field.kind() != before.kind() {
        *rule = Rule::new(rule.field);
    }
    let kind = rule.field.kind();
    pick(ui, "op", &mut rule.op, kind.ops(), |o| o.title());
    if rule.op.values() == 0 {
        return;
    }
    if kind == Kind::Tag {
        let name = tags.get(&rule.value).map_or("(pick a tag)", |t| t.name.as_str());
        egui::ComboBox::from_id_salt("tag").selected_text(name).show_ui(ui, |ui| {
            let mut sorted: Vec<(&String, &Tag)> = tags.iter().collect();
            sorted.sort_by_key(|(_, t)| t.name.to_lowercase());
            for (id, tag) in sorted {
                ui.selectable_value(&mut rule.value, id.clone(), &tag.name);
            }
        });
        return;
    }
    let hint = match (rule.field, rule.op) {
        (Field::Length, _) => "m:ss",
        (_, Op::Before | Op::After) => "2026-10-20",
        (_, Op::InLast | Op::NotInLast) => "days",
        _ => "",
    };
    let width = if kind == Kind::Text { 160.0 } else { 80.0 };
    ui.add(egui::TextEdit::singleline(&mut rule.value).hint_text(hint).desired_width(width));
    if rule.op == Op::InRange {
        ui.label("to");
        ui.add(egui::TextEdit::singleline(&mut rule.to).hint_text(hint).desired_width(width));
    }
}

/// A combo box of `options`, named by `title`.
fn pick<T: Copy + PartialEq>(ui: &mut egui::Ui, id: &str, value: &mut T, options: &[T], title: impl Fn(T) -> &'static str) {
    egui::ComboBox::from_id_salt(id).selected_text(title(*value)).show_ui(ui, |ui| {
        for &option in options {
            ui.selectable_value(value, option, title(option));
        }
    });
}
