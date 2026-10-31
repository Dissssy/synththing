//! How often each song's been played and skipped, and when last
//! (`stats.json` in the library's folder, kept by `SongStore`).
//!
//! Each device counts its own (`Counts`, by song key, then device ID), so
//! once they sync no count needs merging: a song's totals are its
//! devices' summed, its times the latest (`PlayStats::totals`). A reset
//! (`PlayStats::reset`) is when a song's counting starts again: counts
//! from before it are left out, and the device's own start over.
//!
//! Synced as a blob of its own (`NAME`): a leaf per song per device
//! (`<key>/<device>`, its `Counts`) and per reset song (`<key>/reset`).
//! Each device writes only its own counts, so no two write the same leaf
//! but for a reset, where the latest wins.
//!
//! A play is getting past a point in the song (or to its end); a skip is
//! leaving it for another, unplayed, in a window near its start (`Listening`,
//! by the user's `Cutoffs`: each a share of the song or seconds into it).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::settings_sync::Fields;

/// The synced data's name on the server.
pub const NAME: &str = "stats";
/// A song's reset leaf, beside its devices' (`<key>/reset`).
const RESET: &str = "reset";

/// A point in a song: a share of it, or seconds into it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cutoff {
    /// 0 to 100.
    Percent(f64),
    Seconds(f64),
}

impl Cutoff {
    /// Seconds into a song `length` long (a share of one whose length
    /// isn't known is never reached).
    pub fn at(self, length: f64) -> f64 {
        match self {
            Cutoff::Percent(p) if length > 0.0 => length * p / 100.0,
            Cutoff::Percent(_) => f64::INFINITY,
            Cutoff::Seconds(s) => s,
        }
    }
}

/// When a song counts as played, and when leaving it counts as a skip
/// (Preferences > Playback, `Config::play_counts`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cutoffs {
    pub played: Cutoff,
    pub skip_from: Cutoff,
    pub skip_until: Cutoff,
}

impl Default for Cutoffs {
    fn default() -> Self {
        Self { played: Cutoff::Percent(50.0), skip_from: Cutoff::Seconds(2.0), skip_until: Cutoff::Seconds(20.0) }
    }
}

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

/// The stats' leaves, to sync.
pub fn leaves(stats: &PlayStats) -> BTreeMap<String, Value> {
    let counts = stats.counts.iter().flat_map(|(key, devices)| {
        devices.iter().map(move |(device, counts)| (format!("{key}/{device}"), serde_json::to_value(counts).unwrap_or_default()))
    });
    let resets = stats.resets.iter().map(|(key, at)| (format!("{key}/{RESET}"), Value::from(*at)));
    counts.chain(resets).collect()
}

/// The stats `fields` say.
pub fn read(fields: &Fields) -> PlayStats {
    let mut stats = PlayStats::default();
    for (path, field) in fields {
        let Some((key, device)) = path.rsplit_once('/') else { continue };
        if device == RESET {
            if let Some(at) = field.value.as_i64() {
                stats.resets.insert(key.to_string(), at);
            }
        } else if let Ok(counts) = serde_json::from_value(field.value.clone()) {
            stats.counts.entry(key.to_string()).or_default().insert(device.to_string(), counts);
        }
    }
    stats
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
    length: f64,
    played: bool,
}

impl Listening {
    /// Song `key` started (or a song not in the library: `None`): a skip
    /// of the one before, if it was left unplayed in the skip window.
    pub fn start(&mut self, key: Option<String>, cutoffs: &Cutoffs) -> Option<Heard> {
        let left = std::mem::replace(self, Listening { key, ..Listening::default() });
        let skipped = !left.played && (cutoffs.skip_from.at(left.length)..cutoffs.skip_until.at(left.length)).contains(&left.at);
        left.key.filter(|_| skipped).map(Heard::Skipped)
    }

    /// Where it is now (seconds in, of `length`; `finished` at its end): a
    /// play, the moment it gets past `cutoffs.played` or to its end. Going
    /// back before that (looping, or seeking) lets it count again.
    pub fn at(&mut self, position: f64, length: f64, finished: bool, cutoffs: &Cutoffs) -> Option<Heard> {
        let past = finished || position >= cutoffs.played.at(length);
        if !finished {
            self.at = position;
        }
        self.length = length;
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
    fn leaves_read_back_as_they_were() {
        let mut stats = PlayStats::default();
        stats.played("a.mp3", "pc", 10);
        stats.skipped("a.mp3", "phone", 20);
        stats.reset("a.mp3", 30);
        stats.played("a.mp3", "pc", 40);
        let fields: Fields = leaves(&stats).into_iter().map(|(path, value)| (path, crate::settings_sync::Field { value, changed: 0 })).collect();
        assert_eq!(fields.keys().collect::<Vec<_>>(), ["a.mp3/pc", "a.mp3/phone", "a.mp3/reset"]);
        assert_eq!(read(&fields), stats);
    }

    #[test]
    fn a_big_library_played_on_two_devices_fits_one_blob() {
        // 5,000 songs, each counted on two devices: deflated and sealed,
        // well within what a blob can be (about 430 KB of 768).
        let mut stats = PlayStats::default();
        for i in 0..5000u32 {
            // (Hashes, as keys are: they don't compress.)
            let key = format!("{}.mp3", crate::library::hex(&<sha2::Sha256 as sha2::Digest>::digest(i.to_le_bytes())));
            for (device, plays) in [("0123456789abcdef", i % 40 + 1), ("fedcba9876543210", i % 7 + 1)] {
                for n in 0..plays {
                    stats.played(&key, device, 1_790_000_000 + i64::from(i * 97 + n));
                }
                stats.skipped(&key, device, 1_790_000_000 + i64::from(i * 31));
            }
        }
        let fields: Fields = leaves(&stats).into_iter().map(|(path, value)| (path, crate::settings_sync::Field { value, changed: 1_790_000_000 })).collect();
        let plain = serde_json::to_vec(&fields).unwrap();
        let sealed = crate::library::sync::seal(&[1; 32], &crate::library::sync::pack(&plain)).unwrap();
        assert!(sealed.len() < crate::library::sync::MAX_DATA * 3 / 4, "{} KB sealed, from {} KB", sealed.len() / 1024, plain.len() / 1024);
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
        assert_eq!(listening.start(Some("a".into()), &Cutoffs::default()), None);
        assert_eq!(listening.at(10.0, 100.0, false, &Cutoffs::default()), None);
        assert_eq!(listening.at(50.0, 100.0, false, &Cutoffs::default()), Some(Heard::Played("a".into())));
        assert_eq!(listening.at(60.0, 100.0, false, &Cutoffs::default()), None);
        assert_eq!(listening.at(100.0, 100.0, true, &Cutoffs::default()), None, "already counted");
        // (Looped back to the start.)
        assert_eq!(listening.at(1.0, 100.0, false, &Cutoffs::default()), None);
        assert_eq!(listening.at(70.0, 100.0, false, &Cutoffs::default()), Some(Heard::Played("a".into())));
    }

    #[test]
    fn reaching_the_end_counts_as_played() {
        let mut listening = Listening::default();
        listening.start(Some("a".into()), &Cutoffs::default());
        assert_eq!(listening.at(0.0, 0.0, true, &Cutoffs::default()), Some(Heard::Played("a".into())), "a length it doesn't know");
    }

    #[test]
    fn leaving_early_but_not_at_once_is_a_skip() {
        let mut listening = Listening::default();
        listening.start(Some("a".into()), &Cutoffs::default());
        listening.at(1.0, 100.0, false, &Cutoffs::default());
        assert_eq!(listening.start(Some("b".into()), &Cutoffs::default()), None, "too soon");
        listening.at(5.0, 100.0, false, &Cutoffs::default());
        assert_eq!(listening.start(Some("c".into()), &Cutoffs::default()), Some(Heard::Skipped("b".into())));
        listening.at(25.0, 100.0, false, &Cutoffs::default());
        assert_eq!(listening.start(None, &Cutoffs::default()), None, "listened too long");
        listening.at(5.0, 100.0, false, &Cutoffs::default());
        assert_eq!(listening.start(Some("d".into()), &Cutoffs::default()), None, "not a library song");
    }

    #[test]
    fn cutoffs_in_either_unit() {
        let cutoffs = Cutoffs { played: Cutoff::Seconds(30.0), skip_from: Cutoff::Percent(0.0), skip_until: Cutoff::Percent(10.0) };
        let mut listening = Listening::default();
        listening.start(Some("a".into()), &cutoffs);
        assert_eq!(listening.at(29.0, 300.0, false, &cutoffs), None);
        assert_eq!(listening.at(30.0, 300.0, false, &cutoffs), Some(Heard::Played("a".into())));
        listening.start(Some("b".into()), &cutoffs);
        listening.at(25.0, 300.0, false, &cutoffs);
        assert_eq!(listening.start(Some("c".into()), &cutoffs), Some(Heard::Skipped("b".into())), "within 10% of 300 s");
        assert_eq!(Cutoff::Percent(50.0).at(0.0), f64::INFINITY, "no length, no share of it");
    }

    #[test]
    fn a_short_song_played_through_is_not_a_skip() {
        let mut listening = Listening::default();
        listening.start(Some("a".into()), &Cutoffs::default());
        listening.at(5.0, 8.0, false, &Cutoffs::default());
        assert_eq!(listening.start(Some("b".into()), &Cutoffs::default()), None);
    }
}
