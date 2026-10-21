//! The playing song's playback options (Song info > Options,
//! `fields::Section::Options`): where it starts and stops, its volume
//! adjust, and where it was left, for a song that remembers that.
//! (Skipping when shuffling is the play order's: `song_list::Rules`,
//! `pick_next`.)

use super::App;
use crate::audio::AudioCommand;
use crate::engine::EngineView;
use crate::fields::{Facts, Field};

/// A remembered position this close to either end is no position: the
/// song starts from its start again.
const POSITION_MARGIN: f64 = 5.0;

impl App {
    fn playing_facts(&self) -> Option<Facts<'_>> {
        self.current_song.as_deref().and_then(|p| self.songs.get(p)).map(Facts::of)
    }

    /// Set the output volume: the slider's, by the playing song's adjust.
    pub(super) fn send_volume(&self) {
        let adjust = self.playing_facts().and_then(|f| f.number(Field::VolumeAdjust)).unwrap_or(0.0);
        self.send(AudioCommand::SetVolume(self.volume * (1.0 + adjust as f32 / 100.0)));
    }

    /// The song just sent to play: from where it was left, else its start
    /// time, at its volume.
    pub(super) fn song_options_loaded(&mut self) {
        self.playing_at = None;
        // (A preview from Song info plays from where it asked.)
        let previewed = self.seek_on_load.take().filter(|(path, _)| self.current_song.as_ref() == Some(path)).map(|(_, at)| at);
        let Some(facts) = self.playing_facts() else {
            self.send_volume();
            return;
        };
        let left = self.current_song.as_deref().and_then(|p| self.songs.get(p)).and_then(|s| s.position).filter(|_| facts.flag(Field::RememberPosition));
        if let Some(at) = previewed.or(left).or_else(|| facts.number(Field::StartTime)).filter(|&at| at > 0.0) {
            self.send(AudioCommand::Seek(at));
        }
        self.send_volume();
    }

    /// Keep up with the playing song: where it is (to remember), and its
    /// stop time (it ends there, as at its end).
    pub(super) fn song_options_tick(&mut self, view: &EngineView) {
        // (Until the song sent is the one playing, the view's another's.)
        if view.loads < self.song_loads {
            return;
        }
        let Some(facts) = self.playing_facts() else { return };
        let stop = facts.number(Field::StopTime).filter(|&s| s > 0.0);
        let start = facts.number(Field::StartTime).unwrap_or(0.0);
        if view.finished {
            self.playing_at = None;
            return;
        }
        self.playing_at = Some((view.position, view.length));
        // (Once each time it gets there: what's done about it seeks or
        // loads, which moves the view on to its next generation.)
        if let Some(stop) = stop
            && !view.paused
            && view.position >= stop
            && self.stopped_in != Some(view.generation)
        {
            self.stopped_in = Some(view.generation);
            self.playing_at = None;
            if self.loop_mode == crate::playlist::LoopMode::One {
                self.send(AudioCommand::Seek(start));
            } else if self.has_next() {
                self.next_track();
            } else {
                self.send(AudioCommand::TogglePause);
                self.send(AudioCommand::Seek(start));
            }
        }
    }

    /// Note where the playing song was left, if it remembers that: as
    /// another starts, and as the app closes.
    pub(super) fn remember_position(&mut self) {
        let Some(path) = self.current_song.clone() else { return };
        let Some(key) = self.songs.key(&path) else { return };
        if !self.playing_facts().is_some_and(|f| f.flag(Field::RememberPosition)) {
            return;
        }
        let at = self.playing_at.filter(|(at, length)| *at > POSITION_MARGIN && *at < length - POSITION_MARGIN).map(|(at, _)| at);
        self.songs.set_position(&key, at);
        self.songs_changed();
    }
}
