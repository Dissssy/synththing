//! Last.fm in the app (`lastfm.rs`): connecting an account in
//! Preferences > Integrations, and telling it what this device plays.
//! A device following another sends nothing: the one it follows does.

use super::App;
use super::{setting_row, settings_group, with_info};
use crate::engine::EngineView;
use crate::fields::{Facts, Field};
use crate::lastfm::{Connect, Connecting, Heard, Lastfm, Listen, Track};

#[derive(Default)]
pub(super) struct LastfmState {
    /// Sending to the account, while that's on.
    worker: Option<Lastfm>,
    /// Connecting an account, while that's going on.
    connecting: Option<Connecting>,
    /// Whether the approval page was opened for the connecting going on.
    opened: bool,
    /// The song playing, as it counts on Last.fm.
    pub(super) heard: Heard,
    /// The song load (`EngineView::loads`) it was last told is playing,
    /// until it pauses.
    shown: Option<u64>,
}

impl App {
    /// Keep Last.fm up to date with the song playing.
    pub(super) fn lastfm_tick(&mut self, view: &EngineView) {
        let settings = &self.config.lastfm;
        let key = settings.sending().map(|a| a.key.clone());
        if self.lastfm.worker.as_ref().map(|w| &w.key) != key.as_ref() {
            self.lastfm.worker = key.map(Lastfm::new);
            self.lastfm.shown = None;
        }
        let (now_playing, history) = (settings.now_playing, settings.history);
        if self.lastfm.worker.is_none() || self.following() {
            return;
        }
        let playing = !view.paused && !view.finished;
        let counted = self.lastfm.heard.at(view.position, view.length, view.finished);
        if !playing {
            self.lastfm.shown = None;
        }
        let show = now_playing && playing && self.lastfm.shown != Some(view.loads);
        if !show && !(history && counted) {
            return;
        }
        let Some(track) = self.lastfm_track(view) else { return };
        let Some(worker) = &self.lastfm.worker else { return };
        if show {
            worker.now_playing(track.clone());
            self.lastfm.shown = Some(view.loads);
        }
        if history && counted {
            let speed = if view.speed > 0.0 { view.speed } else { 1.0 };
            let into = if view.finished { view.length } else { view.position };
            let at = crate::lastfm::now() - (into / speed) as i64;
            worker.listened(Listen { track, at });
        }
    }

    /// The playing song, as Last.fm's told of it: none without an artist
    /// and a title (Last.fm needs both).
    fn lastfm_track(&self, view: &EngineView) -> Option<Track> {
        let song = self.songs.get(self.current_song.as_deref()?)?;
        let facts = Facts::of(song);
        let text = |field| facts.text(field).map(|t| t.into_owned());
        let meta = song.meta.as_ref();
        let length = meta.and_then(|m| m.length).unwrap_or(view.length);
        Some(Track {
            artist: text(Field::Artist)?,
            title: text(Field::Title)?,
            album: text(Field::Album),
            album_artist: text(Field::AlbumArtist),
            length: (length > 0.0).then_some(length.round() as u32),
            mbid: meta.and_then(|m| m.musicbrainz_recording.clone()).or_else(|| song.online.as_ref()?.recording.clone()),
        })
    }

    /// Preferences > Integrations > Last.fm.
    pub(super) fn lastfm_preferences_ui(&mut self, ui: &mut egui::Ui) {
        let before = self.config.lastfm.clone();
        let hint = "Last.fm keeps a history of the music you listen to, and shows your friends what's playing.";
        settings_group("Last.fm").icon("lastfm-logo").hint(hint).show(ui, |ui| {
            if self.config.lastfm.account.is_some() {
                self.lastfm_connected_ui(ui);
            } else {
                self.lastfm_connect_ui(ui);
            }
        });
        if self.config.lastfm != before {
            if before.account.is_some() && self.config.lastfm.account.is_none() {
                self.lastfm.worker = None;
                crate::lastfm::forget_waiting();
            }
            self.save_config();
        }
    }

    fn lastfm_connected_ui(&mut self, ui: &mut egui::Ui) {
        let status = self.lastfm.worker.as_ref().map(Lastfm::status).unwrap_or_default();
        let settings = &mut self.config.lastfm;
        let Some(account) = &settings.account else { return };
        let name = account.name.clone();
        setting_row(ui, |ui| {
            ui.label(format!("Connected as {name}"));
            if ui.button("Disconnect").on_hover_text("Forget the account here, and any listens not sent yet.").clicked() {
                settings.account = None;
            }
        });
        if settings.account.is_none() {
            return;
        }
        with_info(ui, "While a song plays on this device, your Last.fm profile shows it.", |ui| ui.checkbox(&mut settings.now_playing, "Show what's playing on Last.fm"));
        with_info(
            ui,
            "A song counts once it's longer than 30 seconds and you've heard half of it, or 4 minutes. Listens wait here while Last.fm can't be reached.",
            |ui| ui.checkbox(&mut settings.history, "Add songs I listen to to my Last.fm history"),
        );
        if status.revoked {
            setting_row(ui, |ui| {
                ui.colored_label(ui.visuals().error_fg_color, "Last.fm no longer lets synththing in. Disconnect, then connect again.");
            });
            return;
        }
        let mut line = Vec::new();
        if status.waiting > 0 {
            line.push(format!("{} {} waiting to send", status.waiting, if status.waiting == 1 { "listen" } else { "listens" }));
        }
        if let Some(sent) = status.sent {
            line.push(format!("last sent {}", super::editor::ago_text(sent.elapsed().as_secs())));
        }
        if !line.is_empty() {
            setting_row(ui, |ui| ui.weak(line.join(", ")));
        }
        if let Some(problem) = &status.problem {
            setting_row(ui, |ui| ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't reach Last.fm: {problem}")));
        }
    }

    fn lastfm_connect_ui(&mut self, ui: &mut egui::Ui) {
        let Some(connecting) = &self.lastfm.connecting else {
            let mut clicked = false;
            with_info(
                ui,
                "Only this device sends. It sends each song's title, artist, album and length, and when you played it, straight to Last.fm. Nothing goes through synththing's server.",
                |ui| {
                    let button = ui.button("Connect a Last.fm account");
                    clicked = button.clicked();
                    button
                },
            );
            if clicked {
                let ctx = ui.ctx().clone();
                self.lastfm.connecting = Some(Connecting::start(move || ctx.request_repaint()));
                self.lastfm.opened = false;
            }
            return;
        };
        match connecting.state() {
            Connect::Starting => {
                setting_row(ui, |ui| {
                    ui.spinner();
                    ui.label("Asking Last.fm...");
                });
            }
            Connect::Approving { url } => {
                if !self.lastfm.opened {
                    self.lastfm.opened = true;
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
                }
                setting_row(ui, |ui| {
                    ui.spinner();
                    ui.label("Allow synththing on the Last.fm page in your browser.");
                });
                setting_row(ui, |ui| {
                    if ui.button("Open the page again").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
                    }
                    if ui.button("Cancel").clicked() {
                        self.lastfm.connecting = None;
                    }
                });
            }
            Connect::Done(Ok(account)) => {
                self.lastfm.connecting = None;
                self.config.lastfm.account = Some(account);
                self.config.lastfm.now_playing = true;
                self.config.lastfm.history = true;
            }
            Connect::Done(Err(e)) => {
                setting_row(ui, |ui| ui.colored_label(ui.visuals().error_fg_color, format!("Couldn't connect: {e}")));
                setting_row(ui, |ui| {
                    if ui.button("Try again").clicked() {
                        self.lastfm.connecting = None;
                    }
                });
            }
        }
    }
}
