//! How often each song's been played and skipped, and when last
//! (`stats.json` in the library's folder, kept by `SongStore`).
//!
//! Each device counts its own (`Counts`, by song key, then device ID), so
//! once they sync no count needs merging: a song's totals are its
//! devices' summed, its times the latest (`PlayStats::totals`). A reset
//! (`PlayStats::reset`) is when a song's counting starts again: counts
//! from before it are left out, and the device's own start over.
//!
//! A play is getting past `PLAYED_AT` of the song (or to its end); a skip
//! is leaving it for another between `SKIP_FROM` and `SKIP_UNTIL`
//! seconds in, unplayed (`Listening`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How far through a song it counts as played.
pub const PLAYED_AT: f64 = 0.5;
/// When leaving a song for another counts as skipping it (seconds in).
pub const SKIP_FROM: f64 = 2.0;
pub const SKIP_UNTIL: f64 = 20.0;

/// One device's count of one song, since `since`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Counts {
    #[serde(rename = "p", skip_serializing_if = "is_zero")]
    pub plays: u32,
    #[serde(rename = "s", skip_serializing_if = "is_zero")]
    pub skips: u32,
    /// When it was last played and skipped (Unix seconds).
    #[serde(rename = "lp", skip_serializing_if = "Option::is_none")]
    pub last_played: Option<i64>,
    #[serde(rename = "ls", skip_serializing_if = "Option::is_none")]
    pub last_skipped: Option<i64>,
    /// When this count started: the song's reset, if it had one.
    #[serde(rename = "since", skip_serializing_if = "is_zero_i64")]
    pub since: i64,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}

/// A song's counts, every device's together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub plays: u32,
    pub skips: u32,
    pub last_played: Option<i64>,
    pub last_skipped: Option<i64>,
}

/// Every song's counts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayStats {
    /// By song key, then device ID.
    pub counts: BTreeMap<String, BTreeMap<String, Counts>>,
    /// When each song's counting last started again, by key.
    pub resets: BTreeMap<String, i64>,
}

impl PlayStats {
    /// Song `key`'s totals: its devices' counts since its reset.
    pub fn totals(&self, key: &str) -> Totals {
        let reset = self.resets.get(key).copied().unwrap_or(0);
        let mut totals = Totals::default();
        for counts in self.counts.get(key).into_iter().flat_map(BTreeMap::values).filter(|c| c.since >= reset) {
            totals.plays += counts.plays;
            totals.skips += counts.skips;
            totals.last_played = totals.last_played.max(counts.last_played);
            totals.last_skipped = totals.last_skipped.max(counts.last_skipped);
        }
        totals
    }

    /// Device `device`'s count of song `key`, started over if the song's
    /// been reset since it began.
    fn own(&mut self, key: &str, device: &str) -> &mut Counts {
        let reset = self.resets.get(key).copied().unwrap_or(0);
        let counts = self.counts.entry(key.to_string()).or_default().entry(device.to_string()).or_default();
        if counts.since < reset {
            *counts = Counts { since: reset, ..Counts::default() };
        }
        counts
    }

    pub fn played(&mut self, key: &str, device: &str, now: i64) {
        let counts = self.own(key, device);
        counts.plays += 1;
        counts.last_played = Some(now);
    }

    pub fn skipped(&mut self, key: &str, device: &str, now: i64) {
        let counts = self.own(key, device);
        counts.skips += 1;
        counts.last_skipped = Some(now);
    }

    /// Start counting song `key` again, from `now`. Whether it had any.
    pub fn reset(&mut self, key: &str, now: i64) -> bool {
        let had = self.totals(key) != Totals::default();
        if had {
            self.resets.insert(key.to_string(), now);
        }
        had
    }

    /// Drop song `key`'s counts (it's gone from the library).
    pub fn forget(&mut self, key: &str) {
        self.counts.remove(key);
        self.resets.remove(key);
    }
}

/// What leaving a song, or listening on, counts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Played(String),
    Skipped(String),
}

/// The song playing, as it's listened to: whether it's counted as played
/// yet this time through, and how far in it got.
#[derive(Debug, Default)]
pub struct Listening {
    key: Option<String>,
    at: f64,
    played: bool,
}

impl Listening {
    /// Song `key` started (or a song not in the library: `None`): a skip
    /// of the one before, if it was left unplayed in the skip window.
    pub fn start(&mut self, key: Option<String>) -> Option<Heard> {
        let left = std::mem::replace(self, Listening { key, ..Listening::default() });
        let skipped = !left.played && (SKIP_FROM..SKIP_UNTIL).contains(&left.at);
        left.key.filter(|_| skipped).map(Heard::Skipped)
    }

    /// Where it is now (seconds in, of `length`; `finished` at its end): a
    /// play, the moment it gets past `PLAYED_AT` or to its end. Going back
    /// before that (looping, or seeking) lets it count again.
    pub fn at(&mut self, position: f64, length: f64, finished: bool) -> Option<Heard> {
        let past = finished || (length > 0.0 && position >= length * PLAYED_AT);
        if !finished {
            self.at = position;
        }
        let newly = past && !self.played;
        self.played = past;
        self.key.clone().filter(|_| newly).map(Heard::Played)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_add_up_and_the_latest_time_wins() {
        let mut stats = PlayStats::default();
        stats.played("a", "pc", 10);
        stats.played("a", "pc", 20);
        stats.played("a", "phone", 15);
        stats.skipped("a", "phone", 30);
        assert_eq!(stats.totals("a"), Totals { plays: 3, skips: 1, last_played: Some(20), last_skipped: Some(30) });
        assert_eq!(stats.totals("b"), Totals::default());
    }

    #[test]
    fn a_reset_leaves_out_counts_from_before_it() {
        let mut stats = PlayStats::default();
        stats.played("a", "pc", 10);
        stats.played("a", "phone", 10);
        assert!(stats.reset("a", 50));
        assert_eq!(stats.totals("a"), Totals::default());
        assert!(!stats.reset("a", 60), "nothing to reset");
        stats.played("a", "pc", 70);
        assert_eq!(stats.totals("a"), Totals { plays: 1, last_played: Some(70), ..Totals::default() }, "the phone's old count stays out");
        assert_eq!(stats.counts["a"]["pc"].since, 50);
    }

    #[test]
    fn a_play_is_past_halfway_once_each_time_through() {
        let mut listening = Listening::default();
        assert_eq!(listening.start(Some("a".into())), None);
        assert_eq!(listening.at(10.0, 100.0, false), None);
        assert_eq!(listening.at(50.0, 100.0, false), Some(Heard::Played("a".into())));
        assert_eq!(listening.at(60.0, 100.0, false), None);
        assert_eq!(listening.at(100.0, 100.0, true), None, "already counted");
        // (Looped back to the start.)
        assert_eq!(listening.at(1.0, 100.0, false), None);
        assert_eq!(listening.at(70.0, 100.0, false), Some(Heard::Played("a".into())));
    }

    #[test]
    fn reaching_the_end_counts_as_played() {
        let mut listening = Listening::default();
        listening.start(Some("a".into()));
        assert_eq!(listening.at(0.0, 0.0, true), Some(Heard::Played("a".into())), "a length it doesn't know");
    }

    #[test]
    fn leaving_early_but_not_at_once_is_a_skip() {
        let mut listening = Listening::default();
        listening.start(Some("a".into()));
        listening.at(1.0, 100.0, false);
        assert_eq!(listening.start(Some("b".into())), None, "too soon");
        listening.at(5.0, 100.0, false);
        assert_eq!(listening.start(Some("c".into())), Some(Heard::Skipped("b".into())));
        listening.at(25.0, 100.0, false);
        assert_eq!(listening.start(None), None, "listened too long");
        listening.at(5.0, 100.0, false);
        assert_eq!(listening.start(Some("d".into())), None, "not a library song");
    }

    #[test]
    fn a_short_song_played_through_is_not_a_skip() {
        let mut listening = Listening::default();
        listening.start(Some("a".into()));
        listening.at(5.0, 8.0, false);
        assert_eq!(listening.start(Some("b".into())), None);
    }
}
