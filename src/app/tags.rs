//! Song tags (`song_store::Tag`): the Tags submenu of a song's right-click
//! menu, and the Tags modal, where they're made, renamed, colored, given
//! icons and deleted. They sync with the library (`song_sync.rs`).

use std::path::PathBuf;

use eframe::egui;

use super::{fit_width, icon, App};
use crate::song_store::{SongStore, Tag, TagShow, TAG_ICONS};

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
    let glyph = icon(tag.icon_or_default());
    let text = match tag.show {
        TagShow::Name => tag.name.clone(),
        TagShow::Icon => glyph.to_string(),
        TagShow::Both => format!("{glyph}  {}", tag.name),
    };
    egui::RichText::new(text).color(egui::Color32::from_rgb(tag.color[0], tag.color[1], tag.color[2]))
}

/// The button that picks `tag`'s icon (from `TAG_ICONS`) and how it's
/// shown, in a popup under it.
fn icon_picker(ui: &mut egui::Ui, tag: &mut Tag) {
    let size = ui.spacing().interact_size.y;
    let glyph = egui::RichText::new(icon(tag.icon_or_default())).size(size - 6.0);
    let glyph = if tag.icon.is_some() { glyph } else { glyph.weak() };
    let button = ui.add(egui::Button::new(glyph).min_size(egui::vec2(size, size))).on_hover_text("Its icon, and whether the app shows it");
    egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_max_width(6.0 * (size + 10.0));
        ui.label("Icon (Android's media controls show only this)");
        ui.horizontal_wrapped(|ui| {
            for name in TAG_ICONS {
                let selected = tag.icon.as_deref() == Some(name);
                let pick = egui::Button::new(egui::RichText::new(icon(name)).size(size - 4.0)).selected(selected).min_size(egui::vec2(size + 4.0, size + 4.0));
                if ui.add(pick).on_hover_text(name.replace('-', " ")).clicked() {
                    tag.icon = (!selected).then(|| name.to_string());
                }
            }
        });
        ui.separator();
        ui.label("Show in the app");
        ui.horizontal(|ui| {
            for show in TagShow::ALL {
                ui.selectable_value(&mut tag.show, show, show.title());
            }
        });
    });
}

/// What the Tags submenu asked for.
pub(super) enum TagAction {
    /// Give the songs the tag (`true`) or take it off them.
    Set(String, bool),
    /// A new tag, on the songs.
    New,
    /// Open the Tags modal.
    Edit,
}

/// The Tags submenu for the songs at `paths`: a checkbox for each tag (half
/// checked when some of them have it), then New tag and Edit tags.
pub(super) fn tags_menu(ui: &mut egui::Ui, store: &SongStore, paths: &[PathBuf]) -> Option<TagAction> {
    let mut action = None;
    ui.menu_button("Tags", |ui| {
        let mut tags: Vec<(&String, &Tag)> = store.tags().iter().collect();
        tags.sort_by_key(|(_, tag)| tag.name.to_lowercase());
        for (id, tag) in tags {
            let with = paths.iter().filter(|p| store.get(p).is_some_and(|s| s.tags.contains(id))).count();
            let mut on = with == paths.len();
            let checkbox = ui.add(egui::Checkbox::new(&mut on, tag_text(tag)).indeterminate(with > 0 && with < paths.len()));
            if checkbox.on_hover_text(&tag.name).clicked() {
                action = Some(TagAction::Set(id.clone(), on));
            }
        }
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

impl App {
    /// Do what the Tags submenu asked for, on the songs at `paths`.
    pub(super) fn tag_songs(&mut self, action: TagAction, paths: &[PathBuf]) {
        let (id, on) = match action {
            TagAction::Set(id, on) => (id, on),
            TagAction::New => (self.new_tag(), true),
            TagAction::Edit => {
                self.tags_open = true;
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

    /// Make a tag, named and colored to tell it apart, and open the Tags
    /// modal to name it: its ID.
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
        self.tags_open = true;
        self.tag_to_name = Some(id.clone());
        id
    }

    /// The Tags modal: each tag's color and name, and deleting it.
    pub(super) fn tags_ui(&mut self, ctx: &egui::Context) {
        if !self.tags_open {
            return;
        }
        let mut close = false;
        let mut changed: Vec<(String, Tag)> = Vec::new();
        let mut delete = None;
        let mut new = false;
        let to_name = self.tag_to_name.take();
        let response = egui::Modal::new(egui::Id::new("tags")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 360.0));
            ui.heading("Tags");
            ui.weak("Your own labels for songs: right-click songs to tag them, or use Song info's Tags. Each can have an icon, which its button in Android's media controls shows. With sync on, they're the same on all your devices.");
            ui.add_space(6.0);
            let mut tags: Vec<(String, Tag)> = self.songs.tags().iter().map(|(id, tag)| (id.clone(), tag.clone())).collect();
            tags.sort_by_key(|(_, tag)| tag.name.to_lowercase());
            if tags.is_empty() {
                ui.weak("No tags yet.");
            }
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for (id, mut tag) in tags {
                    ui.horizontal(|ui| {
                        let before = tag.clone();
                        ui.color_edit_button_srgb(&mut tag.color);
                        icon_picker(ui, &mut tag);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(icon("trash")).on_hover_text("Delete it (from every song)").clicked() {
                                delete = Some(id.clone());
                            }
                            let edit = egui::TextEdit::singleline(&mut tag.name).id_salt(&id).desired_width(f32::INFINITY).show(ui);
                            // A new tag: its name selected, to type over.
                            if to_name.as_deref() == Some(id.as_str()) {
                                edit.response.request_focus();
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
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("New tag").clicked() {
                    new = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        });
        let edited = !changed.is_empty() || delete.is_some();
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
            self.new_tag();
        }
        if close || response.should_close() {
            self.tags_open = false;
        }
    }
}
