//! Smart playlists and rulesets on the `App` side (the engine is
//! `crate::smart`): keeping each smart playlist's songs what its rules say
//! as the library changes, the rules editor (a modal; on touch screens it
//! only shows the rules, unless `EDIT_RULES_ON_TOUCH`), and the Rulesets
//! section of Preferences.

use std::collections::BTreeMap;
use std::sync::Arc;

use eframe::egui;

use super::{fit_width, App};
use crate::playlist::{self, Playlist, PlaylistEntry};
use crate::select_list;
use crate::settings_sync;
use crate::fields::{self, Field, Kind};
use crate::smart::{self, Example, Group, Limit, Op, Order, Rule, Rules, Ruleset, Split, SplitBy, Unit};
use crate::latest::Latest;
use crate::song_store::{SongStore, Tag};
use crate::wishlist::Wishes;

/// Whether rules can be edited on a touch screen (they can always be
/// looked at): off until editing there is good to use.
const EDIT_RULES_ON_TOUCH: bool = false;

/// The icon smart playlists have.
pub(super) const SMART_ICON: &str = "engine";

/// Whether rules can be edited here (not on a touch screen, for now).
pub(super) fn rules_editable(ctx: &egui::Context) -> bool {
    !select_list::touch(ctx) || EDIT_RULES_ON_TOUCH
}


/// What the editor's rules are for.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Editing {
    /// A smart playlist, by ID (`None`: a new one).
    Playlist(Option<String>),
    /// A ruleset, by ID (`None`: a new one).
    Ruleset(Option<String>),
}

/// How many example songs the rules editor shows that match, and that
/// don't, and for how long before showing others.
const EXAMPLES_MATCHING: usize = 2;
const EXAMPLES_OTHERS: usize = 1;
const EXAMPLES_SECS: f64 = 4.0;

/// The rules editor: what it's editing, the name and rules as edited so
/// far, and what they come to (worked out again, on a thread of its own,
/// when they change).
pub(super) struct RulesEditor {
    editing: Editing,
    name: String,
    rules: Rules,
    read_only: bool,
    worker: Latest<(), (u64, GroupsJob), Preview>,
    /// What the preview was last asked for from (`App::groups_revision`,
    /// and the rules).
    asked: Option<(u64, Rules)>,
    preview: Option<Preview>,
    /// The example songs shown, and which round of them
    /// (`EXAMPLES_SECS`).
    examples: Option<(u64, Vec<Example>)>,
    /// What of the search it was made from couldn't be a rule.
    left_out: Vec<String>,
}

impl App {
    /// Work each smart playlist's songs out again if the library changed
    /// (or `force`d: a playlist's rules did). The one playing keeps its
    /// place and what's planned next, and its song stays on it while it
    /// plays even if it no longer matches (see `playlist::carry_over`).
    pub(super) fn refresh_smart(&mut self, force: bool) {
        let revision = self.groups_revision();
        if !force && self.smart_seen == Some(revision) {
            return;
        }
        self.smart_seen = Some(revision);
        let mut replanned = false;
        for i in 0..self.playlists.lists.len() {
            replanned |= self.refresh_smart_list(i);
        }
        if replanned {
            self.extend_upcoming();
        }
    }

    /// Each frame: once the song kept on a smart playlist past its rules
    /// (see `refresh_smart`) isn't playing there any more, it goes.
    pub(super) fn smart_held_tick(&mut self) {
        let Some(held) = &self.smart_held else { return };
        let playing = self.now_playing.as_ref().and_then(|np| {
            let stored = self.playlists.lists.get(np.list)?;
            Some((&stored.id, &stored.playlist.entries.get(np.entry)?.path))
        });
        if playing == Some((&held.0, &held.1)) {
            return;
        }
        let list = self.playlists.find(&held.0);
        self.smart_held = None;
        if let Some(list) = list
            && self.refresh_smart_list(list)
        {
            self.extend_upcoming();
        }
    }

    /// Work smart playlist `list`'s songs out again; whether the one
    /// playing changed (so what comes next wants filling in again).
    fn refresh_smart_list(&mut self, list: usize) -> bool {
        let now = settings_sync::now();
        let Some(stored) = self.playlists.lists.get_mut(list) else { return false };
        let Some(rules) = &stored.playlist.smart else { return false };
        let mut entries: Vec<PlaylistEntry> = rules
            .songs(&self.songs, &self.wishlist.list.wishes, now, self.config.sort_with_articles)
            .into_iter()
            .map(|key| PlaylistEntry { path: self.songs.path(&key), soundfont: None, missing: !self.songs.has(&key) })
            .collect();
        let playing = self.now_playing.as_mut().filter(|np| np.list == list);
        if let Some(np) = playing {
            let kept;
            (entries, kept) = playlist::carry_over(np, &stored.playlist.entries, entries);
            self.smart_held.take_if(|(id, _)| *id == stored.id);
            if kept {
                self.smart_held = Some((stored.id.clone(), entries[np.entry].path.clone()));
            }
        }
        let old = &stored.playlist.entries;
        if old.len() == entries.len() && old.iter().zip(&entries).all(|(a, b)| a.path == b.path && a.missing == b.missing) {
            return false;
        }
        stored.playlist.entries = entries;
        self.now_playing.as_ref().is_some_and(|np| np.list == list)
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
        self.rules_editor = Some(RulesEditor {
            editing,
            name,
            rules,
            read_only,
            worker: Latest::new("rules-preview", make_preview),
            asked: None,
            preview: None,
            examples: None,
            left_out: Vec::new(),
        });
    }

    /// Open a new smart playlist's rules made from the Songs search
    /// `query`: its filters, saying what else it had (words to search for,
    /// filters that didn't parse) that couldn't be a rule.
    pub(super) fn smart_playlist_from_search(&mut self, ctx: &egui::Context, query: &str) {
        let rules = Rules { seed: smart::new_seed(), ..crate::search::parse(query, self.songs.tags()).rules };
        let name = self.unique_playlist_name("Smart playlist");
        self.open_rules_editor(ctx, Editing::Playlist(None), name, rules, false);
        if let Some(editor) = &mut self.rules_editor {
            editor.left_out = crate::search::left_out(query, self.songs.tags()).into_iter().map(str::to_string).collect();
        }
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
        let revision = self.groups_revision();
        let Some(editor) = &mut self.rules_editor else { return };
        if let Some((_, preview)) = editor.worker.take().last() {
            editor.preview = Some(preview);
            editor.examples = None;
        }
        if editor.asked.as_ref().is_none_or(|(seen, rules)| *seen != revision || *rules != editor.rules) {
            let job = GroupsJob {
                rules: editor.rules.clone(),
                store: self.songs.clone(),
                wishes: self.wishlist.list.wishes.clone(),
                now: settings_sync::now(),
                ignore: self.config.group_ignore.clone(),
                articles: self.config.sort_with_articles,
            };
            editor.worker.ask((), (revision, job), ctx);
            editor.asked = Some((revision, editor.rules.clone()));
        }
        let round = (ctx.input(|i| i.time) / EXAMPLES_SECS) as u64;
        if let Some(preview) = &editor.preview
            && editor.examples.as_ref().is_none_or(|(seen, _)| *seen != round)
        {
            let mut keys = sample(&preview.matching, EXAMPLES_MATCHING, round);
            keys.extend(sample(&preview.others, EXAMPLES_OTHERS, round));
            let examples = preview.rules.check(&self.songs, &self.wishlist.list.wishes, settings_sync::now(), &keys);
            editor.examples = Some((round, examples));
        }
        let now = ctx.input(|i| i.time);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(EXAMPLES_SECS - now % EXAMPLES_SECS));
        let naming = super::naming(&self.config, &self.songs);
        let names: Vec<String> = editor
            .examples
            .iter()
            .flat_map(|(_, examples)| examples)
            .map(|e| super::title_of(&mut self.song_info, &naming, &self.songs.path(&e.key)))
            .collect();
        let ruleset = matches!(editor.editing, Editing::Ruleset(_));
        let new = matches!(editor.editing, Editing::Playlist(None) | Editing::Ruleset(None));
        let mut done = None;
        let response = egui::Modal::new(egui::Id::unique("rules_editor")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 560.0));
            ui.heading(match (ruleset, new) {
                (false, true) => "New smart playlist",
                (false, false) => "Smart playlist",
                (true, true) => "New ruleset",
                (true, false) => "Ruleset",
            });
            if !editor.left_out.is_empty() {
                let words = editor.left_out.join("  ");
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Not in the rules: {words}. Only filters (artist:..., type:..., year:...) can be rules: a word searched for has to say which field it's in."),
                );
            }
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
            if let Some(preview) = &editor.preview {
                ui.horizontal(|ui| {
                    ui.weak(&preview.summary);
                    if editor.asked.as_ref().is_some_and(|(seen, rules)| (*seen, rules) != (preview.revision, &preview.rules)) {
                        ui.spinner();
                    }
                });
                if let Some((_, examples)) = &editor.examples
                    && !examples.is_empty()
                {
                    ui.add_space(4.0);
                    examples_ui(ui, &preview.rules, examples, &names, self.songs.tags());
                }
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
                let playlist = Playlist { name, smart: Some(rules), ..Playlist::default() };
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

    pub(super) fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            log::warn!("saving the config: {e}");
        }
    }

    /// What smart playlists and rulesets' playlists are worked out from
    /// changes: the library, the characters grouping ignores, or how names
    /// sort.
    fn groups_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // (The wishlist too: what's wished is a rule.)
        (self.songs.revision(), self.wishlist.list.revision(), &self.config.group_ignore, self.config.sort_with_articles).hash(&mut hasher);
        hasher.finish()
    }

    /// The playlists ruleset `id` (with `rules`) makes, as last worked
    /// out: worked out again on another thread when the library, the
    /// rules or grouping change, repainting `ctx` once they are. `None`
    /// until they first are.
    pub(super) fn groups_of(&mut self, ctx: &egui::Context, id: &str, rules: &Rules) -> Option<Arc<Vec<Group>>> {
        let revision = self.groups_revision();
        let (songs, wishes, config) = (&self.songs, &self.wishlist.list.wishes, &self.config);
        self.ruleset_groups.get(ctx, id, revision, rules, || GroupsJob {
            rules: rules.clone(),
            store: songs.clone(),
            wishes: wishes.clone(),
            now: settings_sync::now(),
            ignore: config.group_ignore.clone(),
            articles: config.sort_with_articles,
        })
    }

    /// What each ruleset leaves out of its playlists for want of what it
    /// splits by (`Rules::missing`), for those leaving any out, and the
    /// revision it was worked out for: worked out again when the library
    /// or the rulesets change.
    pub(super) fn missing_info(&mut self) -> (u64, Arc<Vec<Missing>>) {
        let revision = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (self.groups_revision(), serde_json::to_string(&self.config.rulesets).unwrap_or_default(), &self.config.hidden_rulesets).hash(&mut hasher);
            hasher.finish()
        };
        if let Some((seen, missing)) = &self.missing_info
            && *seen == revision
        {
            return (revision, missing.clone());
        }
        let _timing = crate::frame_timing::scope("missing_info");
        let now = settings_sync::now();
        let missing: Vec<Missing> = self
            .shown_rulesets()
            .into_iter()
            .filter_map(|(id, ruleset)| {
                let by = ruleset.rules.split.as_ref()?.by;
                let keys = ruleset.rules.missing(&self.songs, &self.wishlist.list.wishes, now);
                (!keys.is_empty()).then_some(Missing { id, by, keys })
            })
            .collect();
        let missing = Arc::new(missing);
        self.missing_info = Some((revision, missing.clone()));
        (revision, missing)
    }

    /// Every ruleset: the built-in ones, then the user's, by ID.
    pub(super) fn all_rulesets(&self) -> Vec<(String, Ruleset)> {
        smart::rulesets(&self.config.rulesets)
    }

    /// The rulesets Groupings shows: all but the hidden ones.
    pub(super) fn shown_rulesets(&self) -> Vec<(String, Ruleset)> {
        let mut rulesets = self.all_rulesets();
        rulesets.retain(|(id, _)| !self.config.hidden_rulesets.contains(id));
        rulesets
    }

    /// Preferences' Rulesets: a row for each, built-in ones then the
    /// user's, with how many playlists it makes; New ruleset; what they
    /// leave out.
    pub(super) fn rulesets_ui(&mut self, ui: &mut egui::Ui) {
        let builtin = smart::builtin().map(|(id, ruleset)| (id.to_string(), ruleset, true));
        let own = self.config.rulesets.iter().map(|(id, ruleset)| (id.clone(), ruleset.clone(), false));
        let all: Vec<(String, Ruleset, bool)> = builtin.into_iter().chain(own).collect();
        let (mut open, mut delete, mut new, mut edit, mut hide) = (None, None, false, None, None);
        let (_, missing) = self.missing_info();
        let hint = "A ruleset makes a playlist for each artist, album, genre... of the songs its rules pick. By Artist and By Album are built in.";
        super::settings_group("Rulesets").icon(SMART_ICON).hint(hint).show(ui, |ui| {
            let editable = rules_editable(ui.ctx());
            for (id, ruleset, builtin) in all {
                let groups = self.groups_of(ui.ctx(), &id, &ruleset.rules).map(|g| g.len());
                let hidden = self.config.hidden_rulesets.contains(&id);
                super::setting_row(ui, |ui| {
                    let (eye, hover) = if hidden { ("eye-slash", "Hidden from Groupings: click to show it") } else { ("eye", "Shown in Groupings: click to hide it") };
                    if ui.add(super::row_icon_button(ui, eye)).on_hover_text(hover).clicked() {
                        hide = Some((id.clone(), !hidden));
                    }
                    let name = egui::RichText::new(&ruleset.name);
                    ui.label(if hidden { name.weak() } else { name });
                    if builtin {
                        ui.label(egui::RichText::new(super::icon("lock-simple")).color(ui.visuals().weak_text_color()))
                            .on_hover_text("Built in: its rules can be viewed, not changed");
                    }
                    if let Some(each) = missing.iter().find(|m| m.id == id) {
                        let n = each.keys.len();
                        let warning = egui::Button::new(egui::RichText::new(super::icon("warning")).color(ui.visuals().warn_fg_color)).frame(false);
                        let hover = format!(
                            "{n} song{} without {}, which it leaves out. Click to fill it in for them all at once in Song info; \
                             the filter button in Songs shows only them.",
                            if n == 1 { "" } else { "s" },
                            lacking(each.by)
                        );
                        if ui.add(warning).on_hover_text(hover).clicked() {
                            edit = Some(each.keys.iter().map(|k| self.songs.path(k)).collect());
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !builtin && super::confirm_button(ui, ("ruleset", &id), super::row_icon_button(ui, "trash"), "Delete", "Really delete?") {
                            delete = Some(id.clone());
                        }
                        let (name, hover) = if builtin || !editable { ("list-magnifying-glass", "View rules") } else { ("pencil-simple", "Edit rules") };
                        if ui.add(super::row_icon_button(ui, name)).on_hover_text(hover).clicked() {
                            open = Some((id.clone(), ruleset.clone(), builtin));
                        }
                        match groups {
                            Some(n) => ui.weak(format!("{n} playlist{}", if n == 1 { "" } else { "s" })),
                            None => ui.spinner(),
                        };
                    });
                });
            }
            if editable {
                super::setting_row(ui, |ui| new = ui.button(format!("{}  New ruleset...", super::icon("plus"))).clicked());
            }
        });
        if edit.is_some() {
            self.details_for = edit;
        }
        if let Some((id, hidden)) = hide {
            if hidden {
                self.config.hidden_rulesets.insert(id);
            } else {
                self.config.hidden_rulesets.remove(&id);
            }
            self.save_config();
        }
        if new {
            let rules = Rules { split: Some(Split { by: SplitBy::Genre, min: 1 }), ..Rules::default() };
            self.open_rules_editor(ui.ctx(), Editing::Ruleset(None), "By Genre".into(), rules, false);
        }
        if let Some((id, ruleset, builtin)) = open {
            self.open_rules_editor(ui.ctx(), Editing::Ruleset(Some(id)), ruleset.name, ruleset.rules, builtin);
        }
        if let Some(id) = delete {
            self.config.hidden_rulesets.remove(&id);
            self.config.rulesets.remove(&id);
            self.ruleset_groups.remove(&id);
            self.save_config();
        }
    }
}

/// What a ruleset leaves out for want of what it splits by: its ID, what
/// it splits by, and the songs (keys).
pub(super) struct Missing {
    pub id: String,
    pub by: SplitBy,
    pub keys: Vec<String>,
}

/// What a song a ruleset split `by` leaves out lacks: "an album".
fn lacking(by: SplitBy) -> &'static str {
    match by {
        SplitBy::Artist => "an artist",
        SplitBy::AlbumArtist => "an album artist or artist",
        SplitBy::Album => "an album",
        SplitBy::Composer => "a composer",
        SplitBy::Genre => "a genre",
        SplitBy::Year => "a year",
        SplitBy::Tag => "a tag",
    }
}

/// What a ruleset's playlists are worked out from, on the thread.
pub(super) struct GroupsJob {
    rules: Rules,
    store: SongStore,
    wishes: Wishes,
    now: i64,
    ignore: String,
    articles: bool,
}

/// What they were worked out from (the revision, `App::groups_revision`,
/// and the rules), and what they came to.
type Made = (u64, Rules, Arc<Vec<Group>>);

/// The rulesets' playlists, by ruleset ID: as last worked out, and asked
/// for of the thread that works them out.
pub(super) struct RulesetGroups {
    made: BTreeMap<String, Made>,
    asked: BTreeMap<String, (u64, Rules)>,
    worker: Latest<String, (u64, GroupsJob), Made>,
}

impl Default for RulesetGroups {
    fn default() -> Self {
        Self { made: BTreeMap::new(), asked: BTreeMap::new(), worker: Latest::new("ruleset-groups", make_groups) }
    }
}

fn make_groups((revision, job): (u64, GroupsJob)) -> Made {
    let groups = job.rules.groups(&job.store, &job.wishes, job.now, &job.ignore, job.articles);
    (revision, job.rules, Arc::new(groups))
}

impl RulesetGroups {
    /// Ruleset `id`'s playlists as last worked out, asking for them again
    /// (from `job`) if that wasn't for `revision` and `rules`.
    fn get(&mut self, ctx: &egui::Context, id: &str, revision: u64, rules: &Rules, job: impl FnOnce() -> GroupsJob) -> Option<Arc<Vec<Group>>> {
        for (id, made) in self.worker.take() {
            self.made.insert(id, made);
        }
        if self.asked.get(id).is_none_or(|(seen, cached)| *seen != revision || cached != rules) {
            self.worker.ask(id.to_string(), (revision, job()), ctx);
            self.asked.insert(id.to_string(), (revision, rules.clone()));
        }
        self.made.get(id).map(|(_, _, groups)| groups.clone())
    }

    /// Ruleset `id`'s playlists as last worked out, if they have been.
    pub(super) fn known(&self, id: &str) -> Option<&[Group]> {
        self.made.get(id).map(|(_, _, groups)| groups.as_slice())
    }

    /// Forgets ruleset `id` (deleted).
    fn remove(&mut self, id: &str) {
        self.made.remove(id);
        self.asked.remove(id);
    }
}

/// What `rules` come to in `store` now, in a line.
/// What rules come to, for the rules editor: what they were worked out
/// from (`App::groups_revision`, and the rules), how many songs (and
/// playlists) they make, and the songs they match and don't (the limit
/// aside), to show some of.
struct Preview {
    revision: u64,
    rules: Rules,
    summary: String,
    matching: Vec<String>,
    others: Vec<String>,
}

fn make_preview((revision, job): (u64, GroupsJob)) -> Preview {
    let GroupsJob { rules, store, wishes, now, ignore, articles } = job;
    let summary = if rules.split.is_some() {
        let groups = rules.groups(&store, &wishes, now, &ignore, articles);
        let songs: usize = groups.iter().map(|g| g.songs.len()).sum();
        format!("{} playlist{}, {songs} songs in all", groups.len(), if groups.len() == 1 { "" } else { "s" })
    } else {
        let songs = rules.songs(&store, &wishes, now, articles).len();
        format!("{songs} song{} now", if songs == 1 { "" } else { "s" })
    };
    let (matching, others) = rules.sort_out(&store, &wishes, now);
    Preview { revision, rules, summary, matching, others }
}

/// Up to `n` of `keys`, a different few each `round`.
fn sample(keys: &[String], n: usize, round: u64) -> Vec<String> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    if keys.len() <= n {
        return keys.to_vec();
    }
    let mut picked: Vec<usize> = Vec::new();
    for k in 0u64.. {
        if picked.len() == n {
            break;
        }
        let mut hasher = DefaultHasher::new();
        (round, k).hash(&mut hasher);
        let i = (hasher.finish() % keys.len() as u64) as usize;
        if !picked.contains(&i) {
            picked.push(i);
        }
    }
    picked.into_iter().map(|i| keys[i].clone()).collect()
}

/// Example songs (`names`) checked against `rules`: whether each
/// matches, and each rule's values for it and whether they match.
fn examples_ui(ui: &mut egui::Ui, rules: &Rules, examples: &[Example], names: &[String], tags: &BTreeMap<String, Tag>) {
    let mark = |ui: &mut egui::Ui, matches: bool| {
        let (glyph, color) = if matches { (super::icon("check"), ui.visuals().strong_text_color()) } else { (super::icon("x"), ui.visuals().warn_fg_color) };
        ui.colored_label(color, glyph);
    };
    ui.weak("For example:");
    for (example, name) in examples.iter().zip(names) {
        ui.horizontal_wrapped(|ui| {
            mark(ui, example.matches);
            ui.label(name).on_hover_text(if example.matches { "Matches the rules" } else { "Doesn't match the rules" });
            for (rule, check) in rules.rules.iter().zip(&example.checks) {
                ui.weak("\u{b7}");
                mark(ui, check.matches);
                let fields = std::iter::once(rule.field).chain(rule.compared_with());
                let text = match rule.field.kind() {
                    Kind::Tag => tags.get(&rule.value).map_or("a tag", |t| t.name.as_str()).to_string(),
                    _ if check.values.is_empty() => rule.field.label().to_string(),
                    _ => fields.zip(&check.values).map(|(f, v)| format!("{} {v}", f.label())).collect::<Vec<_>>().join(", "),
                };
                ui.weak(text);
            }
        });
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
    pick(ui, "field", &mut rule.field, &fields::rule_fields(), Field::label);
    if rule.field.kind() != before.kind() {
        *rule = Rule::new(rule.field);
    }
    let kind = rule.field.kind();
    pick(ui, "op", &mut rule.op, smart::ops(kind), |o| o.title());
    if rule.op.values() == 0 {
        return;
    }
    if rule.op.compares_fields(kind) {
        let shown = rule.compared_with().map_or("a value", Field::label);
        egui::ComboBox::from_id_salt("against").selected_text(shown).show_ui(ui, |ui| {
            ui.selectable_value(&mut rule.against, None, "a value");
            for field in fields::rule_fields().into_iter().filter(|f| f.kind() == kind) {
                ui.selectable_value(&mut rule.against, Some(field), field.label());
            }
        })
        .response
        .on_hover_text("Compare with a value, or with another of the song's fields");
        if rule.compared_with().is_some() {
            return;
        }
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
    let (hint, formats) = match (rule.field, rule.op) {
        (_, Op::InLast | Op::NotInLast) => ("30d", "How long ago: 12h, 30d, 2w, 6mo or 1y (a bare number is days)"),
        (Field::Imported | Field::LastPlayed | Field::LastSkipped, _) => ("2026-10", "A year, month or day: 2026, 2026-10 or 2026-10-20"),
        (Field::Length, _) => ("6:00", "6:00, 1:02:30, 6m, 90s or 1h2m (a bare number is minutes)"),
        (Field::Size, _) => ("MB", "MB, or with a unit: 800kb, 1.2gb"),
        _ => ("", ""),
    };
    // (A value that doesn't parse shows so.)
    let parses = |text: &str| match (kind, rule.op) {
        (Kind::Date, Op::InLast | Op::NotInLast) => smart::parse_age(text).is_some(),
        (Kind::Date, _) => smart::parse_period(text).is_some(),
        (Kind::Number, _) => smart::parse(rule.field, text).is_some(),
        _ => true,
    };
    let width = if kind == Kind::Text { 160.0 } else { 80.0 };
    let value_ui = |ui: &mut egui::Ui, text: &mut String| {
        let bad = !text.trim().is_empty() && !parses(text);
        let mut edit = egui::TextEdit::singleline(text).hint_text(hint).desired_width(width);
        if bad {
            edit = edit.text_color(ui.visuals().warn_fg_color);
        }
        let response = ui.add(edit);
        if !formats.is_empty() {
            response.on_hover_text(if bad { format!("That isn't one. {formats}") } else { formats.to_string() });
        }
    };
    value_ui(ui, &mut rule.value);
    if rule.op == Op::InRange {
        ui.label("to");
        value_ui(ui, &mut rule.to);
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
