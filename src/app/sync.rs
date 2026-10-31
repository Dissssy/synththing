//! Sync in the app: the switch in Preferences > Account, and what
//! only the app does with what syncing brings (`device_sync.rs` does the
//! syncing itself, the wishlist included): putting synced settings in the config, finding the
//! playlist playing again once synced ones replace it, and saying what
//! moved.

use std::time::Instant;

use eframe::egui;

use super::{icon, row_icon_button, setting_row, settings_group, with_info, App};
use crate::device_sync::{self, DeviceSync, Here, Link, Report, Synced};
use crate::library;
use crate::settings_sync;
use crate::song_sync;

/// How long the debug toast shows.
const TOAST: std::time::Duration = std::time::Duration::from_secs(6);

#[derive(Default)]
pub struct SyncState {
    engine: DeviceSync,
    toast: Option<(String, Instant)>,
}

impl App {
    pub(super) fn sync_on(&self) -> bool {
        self.sync.engine.on()
    }

    /// Where this device reaches the hub directly, if it does.
    pub(super) fn hub_reached(&self) -> Option<&crate::song_sync::Hub> {
        self.sync.engine.hub_reached()
    }

    /// Whether the song library syncs here (so a song can be removed from
    /// every device).
    pub(super) fn syncs_library(&self) -> bool {
        self.sync.engine.syncs_library()
    }

    /// The songs here changed (`refresh_missing`).
    pub(super) fn songs_changed(&mut self) {
        self.sync.engine.songs_changed();
    }

    /// The authority and who's signed in, for syncing.
    pub(super) fn sync_link(&self) -> Link {
        let (server, server_key) = self.authority();
        let relay_song_mb = self.library_server_info().map(|i| i.relay_song_mb);
        Link { server, server_key, identity: self.library_identity(), relay_song_mb }
    }

    /// The notification stream opened again (`None`), or said synced data
    /// changed.
    pub(super) fn sync_server_seen(&mut self, changed: Option<library::sync::Changed>) {
        self.sync.engine.server_seen(changed);
    }

    /// The playlists changed here.
    pub(super) fn playlists_changed(&mut self) {
        self.sync.engine.playlists_changed(&self.songs, &self.playlists);
    }

    /// The songs' play stats changed here.
    pub(super) fn stats_changed(&mut self) {
        self.sync.engine.stats_changed(&self.songs);
    }

    /// The wishlist changed here.
    pub(super) fn wishlist_changed(&mut self) {
        self.sync.engine.wishlist_changed(&self.wishlist.list);
    }

    /// Each frame: sync what's due, and take in what synced. While `held`
    /// (an update is offered), nothing starts.
    pub(super) fn poll_sync(&mut self, held: bool) {
        let link = self.sync_link();
        let here = &mut Here { songs: &mut self.songs, playlists: &mut self.playlists, wishlist: &mut self.wishlist.list };
        let reports = self.sync.engine.poll(&link, here, held);
        for report in reports {
            match report {
                Report::Settings(synced) => self.settings_synced(synced),
                Report::Playlists(synced) => self.playlists_synced(synced),
                Report::Fetched(stored) => self.add_to_library([stored]),
                Report::LibraryChanged => self.refresh_missing(),
                Report::Status(text) => self.status = text,
                Report::Debug(text) => {
                    if self.config.sync_debug {
                        self.sync.toast = Some((text, Instant::now()));
                    }
                }
            }
        }
    }

    /// A settings sync is done: take its settings, with whatever was
    /// changed here while it ran.
    fn settings_synced(&mut self, synced: Synced) {
        let (config, applied) = match device_sync::synced_config(&self.config, &synced) {
            Ok(done) => done,
            Err(e) => return log::warn!("settings sync: {e}"),
        };
        let before = self.current_theme();
        let modes_changed = (config.loop_mode, config.shuffle) != (self.config.loop_mode, self.config.shuffle);
        self.config = config;
        if self.current_theme() != before {
            self.reapply_theme();
        }
        // (Loop and Shuffle in use are the app's own, saved as they change:
        // take the synced ones where they're remembered.)
        if modes_changed && self.config.remember.modes {
            self.loop_mode = self.config.loop_mode;
            self.shuffle = self.config.shuffle;
        }
        let toast = (self.config.sync_debug && !synced.changes().is_empty())
            .then(|| device_sync::describe("Settings", synced.changes(), str::to_string));
        self.sync.engine.settings_applied(synced, applied);
        if let Err(e) = self.config.save() {
            log::warn!("settings sync: {e}");
        }
        if let Some(text) = toast {
            self.sync.toast = Some((text, Instant::now()));
        }
    }

    /// A playlist sync is done: take its playlists, with whatever was
    /// changed here while it ran.
    fn playlists_synced(&mut self, synced: Synced) {
        let changes = synced.changes().to_vec();
        let playing = self.now_playing.as_ref().and_then(|np| self.playlists.lists.get(np.list)).map(|l| l.id.clone());
        let viewed = self.viewed_playlist.and_then(|v| self.playlists.lists.get(v)).map(|l| l.id.clone());
        let here = &mut Here { songs: &mut self.songs, playlists: &mut self.playlists, wishlist: &mut self.wishlist.list };
        match self.sync.engine.apply_playlists(synced, here) {
            Ok(touched) if !touched.is_empty() => self.playlists_replaced(&touched, playing, viewed),
            Ok(_) => {}
            Err(e) => {
                self.status = format!("Couldn't save a synced playlist: {e}");
                return;
            }
        }
        if self.config.sync_debug && !changes.is_empty() {
            let title = |id: &str| {
                let named = self.playlists.find(id).map(|i| self.playlists.lists[i].playlist.name.clone());
                named.unwrap_or_else(|| format!("a removed playlist ({id})"))
            };
            self.sync.toast = Some((device_sync::describe("Playlist", &changes, title), Instant::now()));
        }
    }

    /// Synced playlists replaced or removed these (by ID): find the one
    /// playing and the one open again, and start over in one that changed.
    fn playlists_replaced(&mut self, touched: &[String], playing: Option<String>, viewed: Option<String>) {
        self.playlist_selection.clear();
        self.now_playing = self.now_playing.take().and_then(|mut np| {
            let id = playing?;
            np.list = self.playlists.find(&id)?;
            if touched.contains(&id) {
                let len = self.playlists.lists[np.list].playlist.entries.len();
                (np.entry < len).then_some(())?;
                np.history = vec![np.entry];
                np.upcoming = None;
            }
            Some(np)
        });
        self.viewed_playlist = viewed
            .and_then(|id| self.playlists.find(&id))
            .or_else(|| (!self.playlists.lists.is_empty()).then_some(0));
        self.refresh_smart(true);
        self.refresh_missing();
        self.plan_upcoming();
    }

    /// Preferences > Account, under the identity: the switch and the
    /// devices, what this device syncs, and the songs no device fetches.
    pub(super) fn sync_ui(&mut self, ui: &mut egui::Ui) {
        settings_group("Sync").icon("arrows-clockwise").show(ui, |ui| self.sync_switch_ui(ui));
        if self.sync.engine.on() {
            self.sync_choices_ui(ui);
            self.blocked_songs_ui(ui);
            self.unheld_songs_ui(ui);
        }
    }

    /// The switch, what it's doing, the devices, and the testing toast.
    fn sync_switch_ui(&mut self, ui: &mut egui::Ui) {
        let signed_in = self.library_identity().is_some();
        let offered = self.library_server_info().is_none_or(|i| i.sync);
        let mut on = self.sync.engine.on();
        with_info(
            ui,
            "Keeps your settings and playlists the same on every device signed in as you that has \
             this on, and brings the songs in your playlists (or every song) over from your other \
             devices. What belongs to one device stays on it: folders, layouts, the audio buffer, the \
             mobile layout, the interface scale, updates, and the soundfont a playlist entry uses. \
             Everything is encrypted on this device first, with a key only your devices have, so the \
             server can't read it. Turning it on here takes what your other devices synced, if there's \
             anything; a song shows as unavailable until it's here.",
            |ui| ui.add_enabled(signed_in && offered, egui::Checkbox::new(&mut on, "Sync settings and playlists between my devices")),
        );
        if !offered {
            setting_row(ui, |ui| ui.weak("The server doesn't sync yet."));
        }
        if on != self.sync.engine.on() {
            if on {
                let link = self.sync_link();
                let config = self.config.to_json().unwrap_or_default();
                self.sync.engine.turn_on(&link, &Here { songs: &mut self.songs, playlists: &mut self.playlists, wishlist: &mut self.wishlist.list }, &config);
            } else {
                self.sync.engine.turn_off();
            }
        }
        if !self.sync.engine.on() {
            return;
        }
        if self.sync.engine.held() {
            setting_row(ui, |ui| ui.weak("Paused while the update downloads."));
        }
        if let Some(error) = self.sync.engine.error() {
            setting_row(ui, |ui| ui.weak(format!("Couldn't sync: {error}")));
        }
        if let Some((done, total)) = self.songs_moving() {
            setting_row(ui, |ui| {
                ui.spinner();
                ui.weak(format!("Moving songs between your devices: {done} of {total}"));
            });
        }
        self.devices_ui(ui);
        let mut debug = self.config.sync_debug;
        if setting_row(ui, |ui| ui.checkbox(&mut debug, "Show what each sync changed (for testing)")).changed() {
            self.config.sync_debug = debug;
            if let Err(e) = self.config.save() {
                log::warn!("saving the config: {e}");
            }
        }
    }

    /// Which settings this device syncs: a row for each category, with
    /// how many of its settings sync, opening to a row for each.
    fn sync_choices_ui(&mut self, ui: &mut egui::Ui) {
        let mut chosen: Vec<(&'static settings_sync::Setting, bool)> = Vec::new();
        let hint = "Unticked: this device keeps its own, and doesn't change the others'.";
        settings_group("What this device syncs").icon("sliders").hint(hint).closable("sync_choices").show(ui, |ui| {
            let choices = self.sync.engine.choices();
            for category in settings_sync::CATEGORIES {
                let on = category.settings.iter().filter(|s| settings_sync::syncs(choices, s)).count();
                let id = ui.make_persistent_id(("sync_category", category.name));
                let open = ui.ctx().data_mut(|d| *d.get_persisted_mut_or_default::<bool>(id));
                setting_row(ui, |ui| {
                    let caret = egui::Button::new(icon(if open { "caret-down" } else { "caret-right" })).frame(false);
                    if ui.add(caret).clicked() {
                        ui.ctx().data_mut(|d| d.insert_persisted(id, !open));
                    }
                    ui.label(egui::RichText::new(icon(category.icon)).color(ui.visuals().weak_text_color()));
                    let mut all = on == category.settings.len();
                    let mixed = on > 0 && !all;
                    let checkbox = egui::Checkbox::new(&mut all, category.name).indeterminate(mixed);
                    if ui.add(checkbox).changed() {
                        chosen.extend(category.settings.iter().map(|s| (s, all)));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(format!("{on} of {}", category.settings.len()));
                    });
                });
                if open {
                    for setting in category.settings {
                        let mut on = settings_sync::syncs(choices, setting);
                        setting_row(ui, |ui| {
                            ui.add_space(41.0);
                            if ui.checkbox(&mut on, setting.name).changed() {
                                chosen.push((setting, on));
                            }
                        });
                    }
                }
            }
        });
        if chosen.is_empty() {
            return;
        }
        let link = self.sync_link();
        let config = self.config.to_json().unwrap_or_default();
        self.sync.engine.choose(&link, &Here { songs: &mut self.songs, playlists: &mut self.playlists, wishlist: &mut self.wishlist.list }, chosen, &config);
    }

    /// The devices syncing songs, a row each, each but this one removable.
    fn devices_ui(&mut self, ui: &mut egui::Ui) {
        let devices = self.sync.engine.devices();
        if devices.is_empty() || !self.sync.engine.syncs_transfers() {
            return;
        }
        let me = self.sync.engine.device_id().unwrap_or_default().to_string();
        let hub = self.sync.engine.hub().cloned();
        let mut forget = None;
        let info = format!(
            "The devices your songs move between. One that doesn't sync for {} days is removed by itself; one removed comes back if it syncs again.",
            song_sync::FORGET_AFTER / (24 * 60 * 60)
        );
        with_info(ui, &info, |ui| ui.label(format!("Your devices ({})", devices.len())));
        for device in devices {
            setting_row(ui, |ui| {
                ui.add_space(24.0);
                ui.label(egui::RichText::new(icon("devices")).color(ui.visuals().weak_text_color()));
                ui.label(match device.name.as_deref() {
                    Some("") => "A device with no name",
                    Some(name) => name,
                    None => "A device on an older version",
                });
                if hub.as_ref().is_some_and(|hub| hub.device == device.id) {
                    ui.weak("hub").on_hover_text("Your hub: songs go straight to and from it, from devices that reach it");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if device.id == me {
                        ui.weak("this device");
                    } else {
                        if ui.add(row_icon_button(ui, "x")).on_hover_text("Remove: stop moving songs to it").clicked() {
                            forget = Some(device.id.clone());
                        }
                        ui.weak(format!("synced {}", library::ago(device.seen)));
                    }
                });
            });
        }
        if let Some(device) = forget {
            self.sync.engine.forget_device(&device);
        }
        if let Some(hub) = &hub {
            self.hub_ui(ui, hub, hub.device == me);
        }
    }

    /// How this device reaches the hub, and whether its addresses work from
    /// the internet, as the hub last had the server check.
    fn hub_ui(&mut self, ui: &mut egui::Ui, hub: &song_sync::HubLeaf, is_me: bool) {
        let weak = ui.visuals().weak_text_color();
        if !is_me {
            let checking = self.sync.engine.checking_hub();
            setting_row(ui, |ui| {
                ui.add_space(24.0);
                match self.hub_reached() {
                    Some(reached) => {
                        ui.label(egui::RichText::new(icon("plugs-connected")).color(weak));
                        ui.label(format!("This device reaches the hub directly, at {}", reached.base));
                    }
                    None => {
                        ui.label(egui::RichText::new(icon("plugs")).color(weak));
                        ui.label("This device doesn't reach the hub directly: songs go through the server");
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if checking {
                        ui.spinner();
                    } else if ui.add(row_icon_button(ui, "arrows-clockwise")).on_hover_text("Check again").clicked() {
                        self.sync.engine.check_hub_now();
                        // (And the hub its addresses from the internet.)
                        self.send_live(Some(hub.device.clone()), crate::hub::CHECK_TOPIC, serde_json::Value::Null);
                    }
                });
            });
        }
        for check in &hub.checks {
            setting_row(ui, |ui| {
                ui.add_space(24.0);
                let (glyph, color) = if check.works { ("globe", weak) } else { ("warning", ui.visuals().warn_fg_color) };
                ui.label(egui::RichText::new(icon(glyph)).color(color));
                let text = match &check.hint {
                    None => format!("{} works from the internet (checked {})", check.url, library::ago(check.at)),
                    Some(hint) => format!("{} doesn't work from the internet (checked {}): {hint}", check.url, library::ago(check.at)),
                };
                ui.add(egui::Label::new(text).wrap());
            });
        }
    }

    /// The songs removed here, which this device doesn't fetch from the
    /// others: each can be fetched again.
    fn blocked_songs_ui(&mut self, ui: &mut egui::Ui) {
        let blocked: Vec<(String, String)> =
            self.songs.blocked().map(|(key, song)| (key.to_string(), song.map_or_else(|| key.to_string(), |s| s.name.clone()))).collect();
        if blocked.is_empty() || !self.sync.engine.syncs_transfers() {
            return;
        }
        let mut unblock = None;
        let mut everywhere = None;
        let library = self.sync.engine.syncs_library();
        let title = format!("Songs removed from this device ({})", blocked.len());
        let hint = "This device doesn't fetch these from your other devices.";
        settings_group(&title).icon("prohibit").hint(hint).closable("sync_blocked").show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                for (key, name) in &blocked {
                    setting_row(ui, |ui| {
                        ui.add(egui::Label::new(name.as_str()).truncate());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if library
                                && ui
                                    .add(row_icon_button(ui, "trash"))
                                    .on_hover_text("Remove from all devices: your other devices delete it too, until one imports it again")
                                    .clicked()
                            {
                                everywhere = Some(key.clone());
                            }
                            if ui.add(row_icon_button(ui, "download-simple")).on_hover_text("Fetch again").clicked() {
                                unblock = Some(key.clone());
                            }
                        });
                    });
                }
            });
        });
        if let Some(key) = unblock {
            self.songs.unblock(&key);
            self.sync.engine.songs_changed();
        }
        if let Some(key) = everywhere {
            if let Err(e) = self.songs.remove_everywhere(&key) {
                self.status = format!("Couldn't remove a song: {e:#}");
            }
            self.refresh_missing();
        }
    }

    /// The songs in the library that no device has: they can be cleared
    /// from it, on every device.
    fn unheld_songs_ui(&mut self, ui: &mut egui::Ui) {
        let unheld = self.sync.engine.unheld();
        if unheld.is_empty() || !self.sync.engine.syncs_library() || !self.sync.engine.syncs_transfers() {
            return;
        }
        let mut clear = false;
        let title = format!("Songs on none of your devices ({})", unheld.len());
        let hint = "Every device removed these, or only devices that are gone had them. Your library still lists them, as unavailable.";
        settings_group(&title).icon("cloud-slash").hint(hint).closable("sync_unheld").show(ui, |ui| {
            setting_row(ui, |ui| {
                clear = ui
                    .button(format!("{}  Clear them from your library", icon("trash")))
                    .on_hover_text("Your other devices stop listing them too. Playlists keep them, as unavailable.")
                    .clicked();
            });
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                for key in unheld {
                    setting_row(ui, |ui| ui.add(egui::Label::new(self.songs.get_by_key(key).map_or(key.as_str(), |song| song.name.as_str())).truncate()));
                }
            });
        });
        if clear {
            self.sync.engine.clear_unheld(&mut self.songs);
            self.refresh_missing();
        }
    }

    /// Songs being moved between this device and the others: how many are
    /// done, of how many.
    pub(super) fn songs_moving(&self) -> Option<(usize, usize)> {
        self.sync.engine.moving()
    }

    /// The debug toast: what the last sync changed.
    pub(super) fn sync_toast_ui(&mut self, ctx: &egui::Context) {
        let Some((text, since)) = &self.sync.toast else { return };
        if super::toast::draw(ctx, "sync_toast", egui::Align2::CENTER_TOP, egui::vec2(0.0, 48.0), text, None, *since, TOAST) {
            self.sync.toast = None;
        }
    }
}

/// Songs being moved (`App::songs_moving`), as a spinner and a count.
pub(super) fn songs_moving_ui(ui: &mut egui::Ui, moving: Option<(usize, usize)>) {
    if let Some((done, total)) = moving {
        let hover = "Moving songs between your devices: done, of how many";
        ui.spinner().on_hover_text(hover);
        ui.weak(format!("{done}/{total}")).on_hover_text(hover);
    }
}

