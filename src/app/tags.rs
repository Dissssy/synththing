//! Song tags (`song_store::Tag`): given to songs from the Tags submenu of
//! their right-click menu, Song info's Tags page and the quick Tags dialog
//! (the phone's Now playing); made, renamed, colored, given icons and
//! deleted in Preferences > Library > Tags, where starred songs' color
//! and icon are picked too (`song_store::StarLook`). They sync with the
//! library (`song_sync.rs`).

use std::path::PathBuf;

use eframe::egui::{self, AtomExt as _};

use super::preferences::PrefTab;
use super::{fit_width, icon, setting_row, settings_group, App};
use crate::config::QuickToggle;
use crate::song_store::{SongStore, StarLook, Tag, TagShow, STARRED, TAG_ICONS};

/// The colors new tags take, in turn.
const PALETTE: [[u8; 3]; 8] = [
    [0xd9, 0x48, 0x48],
    [0xe0, 0x8a, 0x2c],
    [0xd8, 0xc0, 0x3a],
    [0x4c, 0xa8, 0x5a],
    [0x2f, 0xa3, 0xa8],
    [0x3f, 0x7c, 0xd6],
    [0x8a, 0x5c, 0xd6],
    [0xc9, 0x5c, 0xa8],
];

/// A tag as the app shows it (`Tag::show`): its name, its icon, or both,
/// in its color.
pub(super) fn tag_text(tag: &Tag) -> egui::RichText {
    egui::RichText::new(tag_label(tag)).color(egui::Color32::from_rgb(tag.color[0], tag.color[1], tag.color[2]))
}

/// A tag's text as `Tag::show` says: its name, its icon, or both (the
/// Songs list's chips, menus).
pub(crate) fn tag_label(tag: &Tag) -> String {
    let glyph = icon(tag.icon_or_default());
    match tag.show {
        TagShow::Name => tag.name.clone(),
        TagShow::Icon => glyph.to_string(),
        TagShow::Both => format!("{glyph} {}", tag.name),
    }
}

/// The button that picks a tag's (or the star's) icon from `TAG_ICONS`,
/// in a popup under it (picked again: none, showing `default`).
fn icon_picker(ui: &mut egui::Ui, picked: &mut Option<String>, default: &str) {
    let size = ui.spacing().interact_size.y;
    let glyph = egui::RichText::new(icon(picked.as_deref().unwrap_or(default))).size(size - 6.0);
    let glyph = if picked.is_some() { glyph } else { glyph.weak() };
    let button = ui.add(egui::Button::new(glyph).min_size(egui::vec2(size, size))).on_hover_text("Its icon (Android's media controls show only this)");
    egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_max_width(6.0 * (size + 10.0));
        ui.horizontal_wrapped(|ui| {
            for name in TAG_ICONS {
                let selected = picked.as_deref() == Some(name);
                let pick = egui::Button::new(egui::RichText::new(icon(name)).size(size - 4.0)).selected(selected).min_size(egui::vec2(size + 4.0, size + 4.0));
                if ui.add(pick).on_hover_text(name.replace('-', " ")).clicked() {
                    *picked = (!selected).then(|| name.to_string());
                }
            }
        });
    });
}

/// Starred songs' row in Preferences > Library > Tags: their color (the
/// theme's star color until one's picked) and icon, how many there are,
/// and back to the usual look.
fn star_row(ui: &mut egui::Ui, star: &mut StarLook, count: usize) {
    let [r, g, b, _] = crate::theme::palette(ui.ctx()).star.0.to_srgba_unmultiplied();
    let mut color = star.color.unwrap_or([r, g, b]);
    if ui.color_edit_button_srgb(&mut color).on_hover_text("Starred songs' color (until you pick one: the theme's star color)").changed() {
        star.color = Some(color);
    }
    icon_picker(ui, &mut star.icon, crate::song_store::STAR_ICON);
    ui.label("Starred").on_hover_text("Built in: the mark at the left of each song");
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let custom = *star != StarLook::default();
        if ui.add_enabled(custom, super::row_icon_button(ui, "arrow-counter-clockwise")).on_hover_text("Back to the theme's star color and the alien").clicked() {
            *star = StarLook::default();
        }
        ui.weak(count.to_string()).on_hover_text(format!("{count} song{} starred", if count == 1 { " is" } else { "s are" }));
    });
}

/// What a Tags list (`tag_checklist`, the Tags submenu) asked for.
pub(super) enum TagAction {
    /// Give the songs the tag (`true`) or take it off them.
    Set(String, bool),
    /// A new tag, on the songs, named in Preferences.
    New,
    /// Edit the tags: Preferences > Library > Tags.
    Edit,
}

/// Star (Unstar, when every one is starred) for the songs at `paths`,
/// then the Tags submenu: their tags to switch (`tag_checklist`, without
/// the star), then New tag and Edit tags.
pub(super) fn tags_menu(ui: &mut egui::Ui, store: &SongStore, paths: &[PathBuf]) -> Option<TagAction> {
    let mut action = None;
    let starred = paths.iter().all(|p| store.is_starred(p));
    let count = if paths.len() > 1 { format!(" ({})", paths.len()) } else { String::new() };
    let label = format!("{} {}{count}", icon(store.star().icon_name()), if starred { "Unstar" } else { "Star" });
    if ui.button(label).clicked() {
        action = Some(TagAction::Set(STARRED.into(), !starred));
    }
    ui.menu_button("Tags", |ui| {
        action = tag_checks(ui, store, paths, false, |ui, text, on, mixed| ui.add(egui::Checkbox::new(on, text).indeterminate(mixed)));
        if !store.tags().is_empty() {
            ui.separator();
        }
        if ui.button("New tag").clicked() {
            action = Some(TagAction::New);
        }
        if ui.button("Edit tags...").clicked() {
            action = Some(TagAction::Edit);
        }
    });
    action
}

/// The star (with `star`) and each tag, sorted by name, as a checkbox
/// (`add`) on or off by whether the songs at `paths` have it (`mixed`:
/// only some do): which one was switched.
fn tag_checks(
    ui: &mut egui::Ui,
    store: &SongStore,
    paths: &[PathBuf],
    star: bool,
    mut add: impl FnMut(&mut egui::Ui, egui::RichText, &mut bool, bool) -> egui::Response,
) -> Option<TagAction> {
    let mut tags: Vec<(&str, String, egui::RichText)> = store.tags().iter().map(|(id, tag)| (id.as_str(), tag.name.clone(), tag_text(tag))).collect();
    tags.sort_by_key(|(_, name, _)| name.to_lowercase());
    if star {
        let text = egui::RichText::new(format!("{}  Starred", icon(store.star().icon_name())));
        tags.insert(0, (crate::song_store::STARRED, "Starred".into(), text));
    }
    let mut action = None;
    for (id, name, text) in tags {
        let with = paths.iter().filter(|p| store.get(p).is_some_and(|s| s.tags.iter().any(|t| t == id))).count();
        let mut on = with == paths.len();
        if add(ui, text, &mut on, with > 0 && with < paths.len()).on_hover_text(name).clicked() {
            action = Some(TagAction::Set(id.to_string(), on));
        }
    }
    action
}

/// The songs at `paths`' star and tags as a settings group of switches
/// (Song info's Tags page, the quick Tags dialog), changed straight away;
/// then New tag and Edit tags, unless `editing` says why they can't be.
pub(super) fn tag_checklist(ui: &mut egui::Ui, store: &SongStore, paths: &[PathBuf], editing: Result<(), &str>) -> Option<TagAction> {
    let mut action = settings_group("Tags")
        .icon("tag")
        .hint("Changed straight away. Preferences > Playback puts a tag's button in the phone's notification.")
        .show(ui, |ui| {
            let action = tag_checks(ui, store, paths, true, |ui, text, on, mixed| {
                setting_row(ui, |ui| ui.add(egui::Checkbox::new(on, text).indeterminate(mixed)))
            });
            if store.tags().is_empty() {
                setting_row(ui, |ui| ui.weak("No tags of your own yet."));
            }
            action
        })
        .flatten();
    ui.horizontal(|ui| {
        let why = editing.err().unwrap_or("");
        if ui.add_enabled(editing.is_ok(), egui::Button::new("New tag")).on_disabled_hover_text(why).clicked() {
            action = Some(TagAction::New);
        }
        if ui.add_enabled(editing.is_ok(), egui::Button::new("Edit tags...")).on_disabled_hover_text(why).clicked() {
            action = Some(TagAction::Edit);
        }
    });
    action
}

/// A star or tag toggle (`App::tag_toggle`) as an icon button:
/// filled in the tag's color (the star's for the star) while the song has
/// it, outlined otherwise. With its hover text; a click means toggle it.
pub(super) fn quick_toggle_button(ui: &mut egui::Ui, store: &SongStore, toggle: &QuickToggle, glyph: f32, size: egui::Vec2) -> egui::Response {
    let star = toggle.tag == STARRED;
    let mut text = egui::RichText::new(icon(&toggle.icon)).size(glyph);
    if toggle.on {
        let color = match store.tags().get(&toggle.tag) {
            Some(tag) if !star => egui::Color32::from_rgb(tag.color[0], tag.color[1], tag.color[2]),
            _ => crate::select_list::star_color(ui.ctx()),
        };
        text = text.family(egui::FontFamily::Name(super::fonts::PHOSPHOR_FILL.into())).color(color);
    }
    let hover = match (star, toggle.on) {
        (true, true) => "Starred (click: unstar)".to_string(),
        (true, false) => "Star this song".to_string(),
        (false, true) => format!("Tagged {} (click: untag)", toggle.label),
        (false, false) => format!("Tag it {}", toggle.label),
    };
    ui.add(egui::Button::new(text.atom_grow(true)).min_size(size)).on_hover_text(hover)
}

impl App {
    /// The song playing's toggle for the tag `id` (`STARRED`: its star):
    /// None with no song in the library playing, or for a deleted tag.
    pub(super) fn quick_toggle(&self, id: &str) -> Option<QuickToggle> {
        self.current_song.as_deref().and_then(|p| self.songs.get(p))?;
        self.tag_toggle(id)
    }

    /// The toggle for the tag `id` (`STARRED`: the star), on when the song
    /// playing has it (off with none playing): None for a deleted tag.
    pub(super) fn tag_toggle(&self, id: &str) -> Option<QuickToggle> {
        let (label, glyph) = if id == STARRED {
            ("Star".to_string(), self.songs.star().icon_name())
        } else {
            let tag = self.songs.tags().get(id)?;
            (tag.name.clone(), tag.icon_or_default())
        };
        let song = self.current_song.as_deref().and_then(|p| self.songs.get(p));
        let on = song.is_some_and(|s| s.tags.iter().any(|t| t == id));
        Some(QuickToggle { tag: id.into(), label, icon: glyph.into(), on })
    }

    /// Do what a Tags list asked for, on the songs at `paths`.
    pub(super) fn tag_songs(&mut self, action: TagAction, paths: &[PathBuf]) {
        let (id, on) = match action {
            TagAction::Set(id, on) => (id, on),
            TagAction::New => {
                let id = self.new_tag();
                self.edit_tags(Some(id.clone()));
                (id, true)
            }
            TagAction::Edit => {
                self.edit_tags(None);
                return;
            }
        };
        for path in paths {
            let Some(key) = self.songs.key(path) else { continue };
            let mut tags = self.songs.get(path).map(|s| s.tags.clone()).unwrap_or_default();
            tags.retain(|t| *t != id);
            if on {
                tags.push(id.clone());
            }
            self.songs.set_song_tags(&key, tags);
        }
        self.songs_changed();
        // (What a playlist plays next can hang on what's starred.)
        self.plan_upcoming();
    }

    /// Go to Preferences > Library > Tags, closing the dialogs over it
    /// (Song info, quick Tags), with `to_name`'s name selected to type over.
    fn edit_tags(&mut self, to_name: Option<String>) {
        self.tags_for = None;
        self.song_details = None;
        self.tag_to_name = to_name;
        self.open_preferences(PrefTab::Library);
    }

    /// Make a tag, named and colored to tell it apart: its ID.
    fn new_tag(&mut self) -> String {
        let id = crate::library::new_nonce();
        let tags = self.songs.tags();
        let name = (tags.len() + 1..).map(|n| format!("Tag {n}")).find(|name| tags.values().all(|t| t.name != *name)).unwrap_or_default();
        // The first color no tag has (round again once they all do).
        let color = PALETTE
            .iter()
            .min_by_key(|color| tags.values().filter(|t| t.color == **color).count())
            .copied()
            .unwrap_or(PALETTE[0]);
        self.songs.set_tag(&id, Tag { name, color, icon: None, show: TagShow::Name });
        self.songs_changed();
        id
    }

    /// The quick Tags dialog, for the songs `tags_for` (the phone's Now
    /// playing opens it for the song playing): their star and tags to
    /// switch, and the way to Preferences to edit them.
    pub(super) fn tags_ui(&mut self, ctx: &egui::Context) {
        let Some(paths) = self.tags_for.clone() else { return };
        let mut close = false;
        let mut action = None;
        let response = egui::Modal::new(egui::Id::unique("tags")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 360.0));
            if let [path] = paths.as_slice() {
                ui.heading(self.song_title(path));
            }
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                action = tag_checklist(ui, &self.songs, &paths, Ok(()));
            });
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| close = ui.button("Close").clicked());
        });
        if close || response.should_close() {
            self.tags_for = None;
        }
        if let Some(action) = action {
            self.tag_songs(action, &paths);
        }
    }

    /// Preferences > Library > Tags: starred songs' color and icon first,
    /// then each tag's color, icon, name, how it's shown (the Songs list's
    /// chips, menus), how many songs have it, and deleting it; and New tag.
    pub(super) fn tags_preferences_ui(&mut self, ui: &mut egui::Ui) {
        let mut changed: Vec<(String, Tag)> = Vec::new();
        let mut delete = None;
        let mut new = false;
        let to_name = self.tag_to_name.take();
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (_, song) in self.songs.songs() {
            for tag in &song.tags {
                *counts.entry(tag.as_str()).or_default() += 1;
            }
        }
        let mut tags: Vec<(String, Tag)> = self.songs.tags().iter().map(|(id, tag)| (id.clone(), tag.clone())).collect();
        tags.sort_by_key(|(_, tag)| tag.name.to_lowercase());
        let mut star = self.songs.star().clone();
        settings_group("Tags")
            .icon("tag")
            .hint("Your own labels for songs, the same on all your devices with sync on. A song's menu, or Song info's Tags, tags it.")
            .show(ui, |ui| {
                let count = counts.get(STARRED).copied().unwrap_or(0);
                setting_row(ui, |ui| star_row(ui, &mut star, count));
                for (id, mut tag) in tags {
                    let count = counts.get(id.as_str()).copied().unwrap_or(0);
                    setting_row(ui, |ui| {
                        let before = tag.clone();
                        ui.color_edit_button_srgb(&mut tag.color);
                        icon_picker(ui, &mut tag.icon, TAG_ICONS[0]);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if super::confirm_button(ui, ("tag", &id), super::row_icon_button(ui, "trash"), "Delete it (from every song)", "Really delete?") {
                                delete = Some(id.clone());
                            }
                            egui::ComboBox::from_id_salt(("tag_show", &id))
                                .width(64.0)
                                .selected_text(match tag.show {
                                    TagShow::Name => "Name",
                                    TagShow::Icon => "Icon",
                                    TagShow::Both => "Both",
                                })
                                .show_ui(ui, |ui| {
                                    for show in TagShow::ALL {
                                        ui.selectable_value(&mut tag.show, show, show.title());
                                    }
                                })
                                .response
                                .on_hover_text("What the app shows of it: its name, its icon, or both");
                            ui.weak(count.to_string()).on_hover_text(format!("{count} song{} have it", if count == 1 { "" } else { "s" }));
                            let edit = egui::TextEdit::singleline(&mut tag.name).id_salt(&id).desired_width(f32::INFINITY).show(ui);
                            // A new tag: its name selected, to type over.
                            if to_name.as_deref() == Some(id.as_str()) {
                                edit.response.request_focus();
                                edit.response.scroll_to_me(Some(egui::Align::Center));
                                let mut state = edit.state;
                                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(tag.name.chars().count()));
                                state.cursor.set_char_range(Some(all));
                                state.store(ui.ctx(), edit.response.id);
                            }
                        });
                        if tag != before {
                            changed.push((id.clone(), tag));
                        }
                    });
                }
                setting_row(ui, |ui| {
                    new = ui.button("New tag").clicked();
                });
            });
        let edited = !changed.is_empty() || delete.is_some() || star != *self.songs.star();
        self.songs.set_star(star);
        for (id, tag) in changed {
            self.songs.set_tag(&id, tag);
        }
        if let Some(id) = delete {
            self.songs.remove_tag(&id);
        }
        if edited {
            self.songs_changed();
        }
        if new {
            let id = self.new_tag();
            self.tag_to_name = Some(id);
        }
    }
}
