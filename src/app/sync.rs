//! Sync in the app: the switch in Preferences > Online Library, and what
//! only the app does with what syncing brings (`device_sync.rs` does the
//! syncing itself, the wishlist included): putting synced settings in the config, finding the
//! playlist playing again once synced ones replace it, and saying what
//! moved.

use std::time::Instant;

use eframe::egui;

use super::App;
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

    /// The switch, under the identity in Preferences > Online Library.
    pub(super) fn sync_ui(&mut self, ui: &mut egui::Ui) {
        let signed_in = self.library_identity().is_some();
        let offered = self.library_server_info().is_none_or(|i| i.sync);
        let mut on = self.sync.engine.on();
        super::with_info(
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
            ui.weak("The server doesn't sync yet.");
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
        if self.sync.engine.on() {
            if self.sync.engine.held() {
                ui.weak("Paused while the update downloads.");
            }
            if let Some(error) = self.sync.engine.error() {
                ui.weak(format!("Couldn't sync: {error}"));
            }
            if let Some((done, total)) = self.songs_moving() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak(format!("Moving songs between your devices: {done} of {total}"));
                });
            }
            self.sync_choices_ui(ui);
            self.devices_ui(ui);
            self.blocked_songs_ui(ui);
            self.unheld_songs_ui(ui);
            let mut debug = self.config.sync_debug;
            if ui.checkbox(&mut debug, "Show what each sync changed (for testing)").changed() {
                self.config.sync_debug = debug;
                if let Err(e) = self.config.save() {
                    log::warn!("saving the config: {e}");
                }
            }
        }
    }

    /// Which settings this device syncs: each category, opening to its
    /// settings.
    fn sync_choices_ui(&mut self, ui: &mut egui::Ui) {
        let mut chosen: Vec<(&'static settings_sync::Setting, bool)> = Vec::new();
        egui::CollapsingHeader::new("What this device syncs").id_salt("sync_choices").show(ui, |ui| {
            ui.weak("Unticked: this device keeps its own, and doesn't change the others'.");
            for category in settings_sync::CATEGORIES {
                let choices = self.sync.engine.choices();
                let on = category.settings.iter().filter(|s| settings_sync::syncs(choices, s)).count();
                let id = ui.make_persistent_id(("sync_category", category.name));
                egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                    .show_header(ui, |ui| {
                        let mut all = on == category.settings.len();
                        let mixed = on > 0 && !all;
                        let checkbox = egui::Checkbox::new(&mut all, category.name).indeterminate(mixed);
                        if ui.add(checkbox).changed() {
                            chosen.extend(category.settings.iter().map(|s| (s, all)));
                        }
                    })
                    .body(|ui| {
                        for setting in category.settings {
                            let mut on = settings_sync::syncs(choices, setting);
                            if ui.checkbox(&mut on, setting.name).changed() {
                                chosen.push((setting, on));
                            }
                        }
                    });
            }
        });
        if chosen.is_empty() {
            return;
        }
        let link = self.sync_link();
        let config = self.config.to_json().unwrap_or_default();
        self.sync.engine.choose(&link, &Here { songs: &mut self.songs, playlists: &mut self.playlists, wishlist: &mut self.wishlist.list }, chosen, &config);
    }

    /// The devices syncing songs, each but this one removable.
    fn devices_ui(&mut self, ui: &mut egui::Ui) {
        let devices = self.sync.engine.devices();
        if devices.is_empty() || !self.sync.engine.syncs_transfers() {
            return;
        }
        let me = self.sync.engine.device_id().unwrap_or_default();
        let mut forget = None;
        egui::CollapsingHeader::new(format!("Your devices ({})", devices.len())).id_salt("sync_devices").show(ui, |ui| {
            ui.weak(format!(
                "The devices your songs move between. One that doesn't sync for {} days is removed by itself; one removed comes back if it syncs again.",
                song_sync::FORGET_AFTER / (24 * 60 * 60)
            ));
            for device in devices {
                ui.horizontal(|ui| {
                    if device.id == me {
                        ui.weak("This device");
                    } else if ui.small_button("Remove").on_hover_text("Stop moving songs to it").clicked() {
                        forget = Some(device.id.clone());
                    }
                    ui.label(match device.name.as_deref() {
                        Some("") => "A device with no name",
                        Some(name) => name,
                        None => "A device on an older version",
                    });
                    if device.id != me {
                        ui.weak(format!("synced {}", library::ago(device.seen)));
                    }
                });
            }
        });
        if let Some(device) = forget {
            self.sync.engine.forget_device(&device);
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
        egui::CollapsingHeader::new(format!("Songs removed from this device ({})", blocked.len())).id_salt("sync_blocked").show(ui, |ui| {
            ui.weak("This device doesn't fetch these from your other devices.");
            for (key, name) in &blocked {
                ui.horizontal(|ui| {
                    if ui.small_button("Fetch again").clicked() {
                        unblock = Some(key.clone());
                    }
                    if library && ui.small_button("Remove from all devices").on_hover_text("Your other devices delete it too, until one imports it again").clicked() {
                        everywhere = Some(key.clone());
                    }
                    ui.label(name);
                });
            }
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
        let header = format!("Songs on none of your devices ({})", unheld.len());
        egui::CollapsingHeader::new(header).id_salt("sync_unheld").show(ui, |ui| {
            ui.weak("Every device removed these, or only devices that are gone had them. Your library still lists them, as unavailable.");
            clear = ui
                .button("Clear them from your library")
                .on_hover_text("Your other devices stop listing them too. Playlists keep them, as unavailable.")
                .clicked();
            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                for key in unheld {
                    ui.label(self.songs.get_by_key(key).map_or(key.as_str(), |song| song.name.as_str()));
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

