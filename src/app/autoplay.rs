//! Autoplay from the Songs list (`song_list::Autoplay`): when a song ends,
//! what plays next is worked out from the list as it is then, under the
//! same Loop and Shuffle as playlists.

use eframe::egui;

use super::{icon_button, App};
use crate::playlist::LoopMode;
use crate::song_list::{Autoplay, Playing, Position, Rules};

/// What Autoplay is set to, for its button's hover and toast.
pub(super) fn text(mode: Autoplay) -> &'static str {
    match mode {
        Autoplay::Off => "Autoplay: off",
        Autoplay::Group => "Autoplay: the group",
        Autoplay::All => "Autoplay: the whole list",
    }
}

/// The Songs list's Autoplay button, set to `mode`; whether it was clicked.
pub(super) fn button_ui(ui: &mut egui::Ui, mode: Autoplay) -> bool {
    let glyph = if mode == Autoplay::Group { "stack" } else { "queue" };
    ui.add(icon_button(glyph).selected(mode != Autoplay::Off))
        .on_hover_text(format!("{}: when a song started here ends, play on down the list", text(mode)))
        .clicked()
}

impl App {
    pub(super) fn has_next(&self) -> bool {
        self.now_playing.is_some() || self.listed.is_some() || !self.queue.is_empty()
    }

    fn listed_rules(&self) -> Rules {
        Rules {
            autoplay: self.autoplay,
            loop_all: self.loop_mode == LoopMode::All,
            shuffle: self.shuffle,
            skip_collapsed: self.config.autoplay_skip_collapsed,
            only_starred: self.config.only_starred,
        }
    }

    /// A song clicked in the Songs list: it plays, leaving any
    /// playlist, and with Autoplay on the list plays on from it.
    pub(super) fn play_listed(&mut self, at: Position) {
        self.now_playing = None;
        self.listed = (self.autoplay != Autoplay::Off).then(|| Playing::new(at.clone()));
        self.plan_listed();
        self.request_play(&at.path, None, None, false);
    }

    /// Play through `tracks` from the one at `start`: an album's or
    /// artist's songs, to their end, leaving any playlist.
    pub(super) fn play_tracks(&mut self, tracks: Vec<std::path::PathBuf>, start: usize) {
        let tracks: Vec<_> = tracks.into_iter().map(|path| (path.clone(), super::plays(&self.songs, &path))).collect();
        let Some(playing) = Playing::tracks(tracks, start) else { return };
        let path = playing.at.path.clone();
        self.now_playing = None;
        self.listed = Some(playing);
        self.plan_listed();
        self.request_play(&path, None, None, false);
    }

    /// Turn Autoplay to `mode`. Turned on with a song from the list
    /// playing, the list plays on from it.
    pub(super) fn set_autoplay(&mut self, mode: Autoplay) {
        self.autoplay = mode;
        if mode == Autoplay::Off {
            // (An album playing through goes on.)
            self.listed.take_if(|p| !p.has_tracks());
        } else if self.listed.is_none()
            && self.now_playing.is_none()
            && let Some(at) = self.current_song.as_deref().and_then(|p| self.song_list.position_of(p))
        {
            self.listed = Some(Playing::new(at));
        }
        self.plan_listed();
        self.save_playback_modes();
    }

    /// Pick what plays after the song playing now (so the first can be
    /// preloaded, the queue can show them, and a shuffle's picks hold),
    /// from the list as it is.
    pub(super) fn plan_listed(&mut self) {
        if let Some(playing) = &mut self.listed {
            playing.upcoming.clear();
        }
        self.extend_listed();
    }

    /// Pick songs after the ones already picked, up to `planned_count`.
    fn extend_listed(&mut self) {
        let rules = self.listed_rules();
        let generation = self.song_list.generation();
        let count = self.planned_count();
        let Some(playing) = &self.listed else { return };
        let mut upcoming = playing.upcoming.clone();
        let mut after = playing.clone();
        for (at, new_pass) in &upcoming {
            after.advance(at.clone(), *new_pass);
        }
        while upcoming.len() < count {
            let Some((at, new_pass)) = self.song_list.next(&after, rules, self.rng.next()) else { break };
            after.advance(at.clone(), new_pass);
            upcoming.push((at, new_pass));
        }
        if let Some(playing) = &mut self.listed {
            playing.upcoming = upcoming;
            playing.planned = generation;
        }
    }

    /// On to the song picked to come next (see `plan_listed`); one that
    /// fails to load is skipped (see `loading.rs`).
    pub(super) fn listed_next(&mut self) {
        if self.listed.as_ref().is_some_and(|p| p.planned != self.song_list.generation()) {
            self.plan_listed();
        }
        let Some(mut playing) = self.listed.clone() else { return };
        if playing.upcoming.is_empty() {
            // Ran off the end: stay on the last song, finished.
            self.status = "End of the list.".to_string();
            return;
        }
        let (at, new_pass) = playing.upcoming.remove(0);
        playing.advance(at.clone(), new_pass);
        self.listed = Some(playing);
        self.extend_listed();
        self.request_play(&at.path, None, None, true);
    }

    /// Back to the song before this one; whether there was one.
    pub(super) fn listed_previous(&mut self) -> bool {
        let rules = self.listed_rules();
        let Some(mut playing) = self.listed.clone() else { return false };
        let Some(at) = self.song_list.previous(&playing, rules) else { return false };
        playing.history.pop();
        if playing.history.last() != Some(&at.path) {
            playing.history.push(at.path.clone());
        }
        playing.at = at.clone();
        playing.upcoming.clear();
        self.listed = Some(playing);
        self.plan_listed();
        self.request_play(&at.path, None, None, false);
        true
    }
}
