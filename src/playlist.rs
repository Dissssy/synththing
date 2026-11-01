//! Saved playlists: named, ordered lists of songs from the song library
//! (`song_store.rs`), each entry optionally carrying its own soundfont
//! override (MIDI only), persisted one JSON file per playlist under
//! `<config dir>/playlists/`, naming each song by its key in the library.
//! Each file is named by the playlist's ID (random, the same on every
//! device it syncs to: `playlist_sync.rs`); its name is only a field.
//! A smart playlist keeps its rules (`smart.rs`) instead of its songs:
//! they're worked out again here whenever the library changes
//! (`App::refresh_smart`).
//!
//! This module is the data model and the pure "what plays next" logic; the
//! panel UI, drag-and-drop and actually loading/playing tracks live in
//! `app.rs`. Index bookkeeping after edits (`index_after_move`/
//! `index_after_remove`) is here too so it can be tested without a GUI.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::together::{self, Member};

#[derive(Clone, Debug)]
pub struct PlaylistEntry {
    /// The song's file in the library.
    pub path: PathBuf,
    /// Soundfont to use for this entry instead of the one selected in the
    /// Soundfonts panel. Only meaningful for MIDI entries.
    pub soundfont: Option<PathBuf>,
    /// Cached `!path.exists()` (the song isn't on this device), refreshed
    /// on load/add rather than stat'ing every row every frame.
    pub missing: bool,
}

impl PlaylistEntry {
    pub fn new(path: PathBuf) -> Self {
        let missing = !path.exists();
        Self { path, soundfont: None, missing }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Playlist {
    pub name: String,
    /// A smart playlist's are its rules' songs, as last worked out.
    pub entries: Vec<PlaylistEntry>,
    /// A smart playlist's rules.
    pub smart: Option<crate::smart::Rules>,
    /// The picture the user picked for it, by its key (`images.rs`).
    pub image: Option<String>,
}

/// A playlist as saved: each song by its key in the library.
#[derive(Serialize, Deserialize)]
struct PlaylistFile {
    name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entries: Vec<EntryFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    smart: Option<crate::smart::Rules>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct EntryFile {
    song: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    soundfont: Option<PathBuf>,
}

/// A playlist and its ID (its file's name).
pub struct StoredPlaylist {
    pub id: String,
    pub playlist: Playlist,
}

pub struct Library {
    dir: PathBuf,
    /// The song library's folder, where the entries' songs are.
    songs: PathBuf,
    pub lists: Vec<StoredPlaylist>,
}

impl Library {
    /// Load every `*.json` in `dir`, sorted by name, their songs in the
    /// library in `songs`. Unreadable files are skipped and reported back
    /// rather than failing the whole load. A file not named by an ID (put
    /// there by hand) is given one.
    pub fn load(dir: PathBuf, songs: PathBuf) -> (Self, Vec<String>) {
        let mut lists = Vec::new();
        let mut errors = Vec::new();
        if let Ok(read_dir) = fs::read_dir(&dir) {
            for entry in read_dir.flatten() {
                let mut file = entry.path();
                if file.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let mut id = file.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
                if !is_id(&id) {
                    id = new_id();
                    let renamed = dir.join(format!("{id}.json"));
                    if let Err(e) = fs::rename(&file, &renamed) {
                        errors.push(format!("{}: {e}", file.display()));
                        continue;
                    }
                    file = renamed;
                }
                let parsed = fs::read_to_string(&file)
                    .map_err(anyhow::Error::from)
                    .and_then(|s| serde_json::from_str::<PlaylistFile>(&s).map_err(Into::into));
                match parsed {
                    Ok(saved) => {
                        let entries = saved
                            .entries
                            .into_iter()
                            .map(|e| PlaylistEntry { soundfont: e.soundfont, ..PlaylistEntry::new(crate::song_store::path_of(&songs, &e.song)) })
                            .collect();
                        lists.push(StoredPlaylist { id, playlist: Playlist { name: saved.name, entries, smart: saved.smart, image: saved.image } });
                    }
                    Err(e) => errors.push(format!("{}: {e}", file.display())),
                }
            }
        }
        lists.sort_by_key(|l| l.playlist.name.to_lowercase());
        (Self { dir, songs, lists }, errors)
    }

    /// Create and save a new playlist, returning its index.
    pub fn create(&mut self, name: &str, songs: Vec<PathBuf>) -> Result<usize> {
        let playlist = Playlist {
            name: name.to_string(),
            entries: songs.into_iter().map(PlaylistEntry::new).collect(),
            ..Playlist::default()
        };
        self.put(&new_id(), playlist)
    }

    /// Save `playlist` as the one with `id`, replacing it if there is one
    /// (else added at the end); its index.
    pub fn put(&mut self, id: &str, playlist: Playlist) -> Result<usize> {
        let idx = match self.find(id) {
            Some(idx) => {
                self.lists[idx].playlist = playlist;
                idx
            }
            None => {
                self.lists.push(StoredPlaylist { id: id.to_string(), playlist });
                self.lists.len() - 1
            }
        };
        self.save(idx)?;
        Ok(idx)
    }

    /// The index of the playlist with `id`.
    pub fn find(&self, id: &str) -> Option<usize> {
        self.lists.iter().position(|l| l.id == id)
    }

    /// The song library's folder.
    pub fn songs_dir(&self) -> &Path {
        &self.songs
    }

    fn file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn save(&self, idx: usize) -> Result<()> {
        let Some(stored) = self.lists.get(idx) else {
            return Ok(());
        };
        let entries = match stored.playlist.smart {
            Some(_) => Vec::new(),
            None => stored
                .playlist
                .entries
                .iter()
                .filter_map(|e| Some(EntryFile { song: crate::song_store::key_in(&self.songs, &e.path)?.to_string(), soundfont: e.soundfont.clone() }))
                .collect(),
        };
        let saved = PlaylistFile { name: stored.playlist.name.clone(), entries, smart: stored.playlist.smart.clone(), image: stored.playlist.image.clone() };
        let json = serde_json::to_string_pretty(&saved)?;
        fs::create_dir_all(&self.dir).with_context(|| format!("creating {}", self.dir.display()))?;
        let file = self.file(&stored.id);
        fs::write(&file, json).with_context(|| format!("writing {}", file.display()))
    }

    pub fn delete(&mut self, idx: usize) -> Result<()> {
        if idx >= self.lists.len() {
            return Ok(());
        }
        let stored = self.lists.remove(idx);
        let file = self.file(&stored.id);
        match fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("deleting {}", file.display()))
            }
            _ => Ok(()),
        }
    }
}

/// A playlist ID: 16 lowercase hex digits.
pub fn is_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn new_id() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        // (No randomness: the time will do, for one device's playlists.)
        bytes = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64).to_le_bytes();
    }
    crate::library::hex(&bytes)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopMode {
    #[default]
    Off,
    /// Repeat the current track forever (handled gaplessly by the engine).
    One,
    /// Wrap from the end of the playlist back to the start. Outside a
    /// playlist there's nothing to wrap, so it behaves like `One`.
    All,
}

impl LoopMode {
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

/// Which playlist entry is playing, plus the shuffle history for this pass
/// through the list (entries already played, oldest first, current last).
#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub list: usize,
    pub entry: usize,
    pub history: Vec<usize>,
    /// The entries already picked to play next, in order (decided as soon
    /// as this one starts, even when shuffling, so the first can be
    /// preloaded and the queue can show what's coming). Empty when nothing
    /// follows, or not decided yet.
    pub upcoming: Vec<usize>,
}

impl NowPlaying {
    pub fn new(list: usize, entry: usize) -> Self {
        Self { list, entry, history: vec![entry], upcoming: Vec::new() }
    }

    /// On to `next`, in a list of `len` entries: shuffle history keeps the
    /// pass's entries (starting again once every one's had its turn), and
    /// otherwise only the one playing, so a looping list's doesn't grow.
    pub fn advance(&mut self, next: usize, len: usize, shuffle: bool) {
        if shuffle {
            if self.history.len() >= len {
                self.history.clear();
            }
            self.history.push(next);
        } else {
            self.history = vec![next];
        }
        self.entry = next;
    }
}

/// The entry to play after `current` in a list whose entries can each be
/// played on to or not (`playable`: "Only play starred songs" leaves the
/// others out), or `None` when playback should stop. `history` is this
/// pass's already-played entries and `members` the songs that play
/// together (each entry's set, if it's in one; only consulted when
/// shuffling); `random` is any random number.
pub fn next_entry(
    playable: &[bool],
    members: &[Member],
    current: usize,
    loop_mode: LoopMode,
    shuffle: bool,
    history: &[usize],
    random: u64,
) -> Option<usize> {
    let rows: Vec<usize> = (0..playable.len()).filter(|&i| playable[i]).collect();
    if rows.is_empty() {
        return None;
    }
    if !shuffle {
        return rows.iter().copied().find(|&i| i > current).or_else(|| (loop_mode == LoopMode::All).then_some(rows[0]));
    }
    let unplayed: Vec<usize> = rows.iter().copied().filter(|i| !history.contains(i)).collect();
    if let Some(i) = together::next_in_set(members, &unplayed, current) {
        return Some(i);
    }
    let unplayed = together::firsts(members, &unplayed);
    if !unplayed.is_empty() {
        return Some(unplayed[(random % unplayed.len() as u64) as usize]);
    }
    if loop_mode != LoopMode::All {
        return None;
    }
    // New pass: anything but the track that just finished, so a reshuffle
    // never plays the same song twice in a row (unless it's the only one).
    let others: Vec<usize> = together::firsts(members, &rows).into_iter().filter(|&i| i != current).collect();
    if others.is_empty() {
        return Some(rows[0]);
    }
    Some(others[(random % others.len() as u64) as usize])
}

/// The entry "Previous" goes to: the one before it in shuffle history, else
/// the one before it in list order (wrapping only when looping all).
pub fn previous_entry(len: usize, current: usize, loop_mode: LoopMode, shuffle: bool, history: &[usize]) -> Option<usize> {
    if len == 0 {
        return None;
    }
    if shuffle {
        return history.iter().rev().nth(1).copied();
    }
    if current > 0 {
        Some(current - 1)
    } else if loop_mode == LoopMode::All {
        Some(len - 1)
    } else {
        None
    }
}

/// The list's new order after moving the entries at `moving` (in their
/// current relative order) to sit together just before position `before`
/// of the current list: the old index of each entry, in new order.
pub fn order_after_move(len: usize, moving: &[usize], before: usize) -> Vec<usize> {
    let mut moving: Vec<usize> = moving.iter().copied().filter(|&i| i < len).collect();
    moving.sort_unstable();
    moving.dedup();
    let mut rest: Vec<usize> = (0..len).filter(|i| moving.binary_search(i).is_err()).collect();
    let insert_at = rest.iter().take_while(|&&i| i < before).count();
    rest.splice(insert_at..insert_at, moving);
    rest
}

/// For a new order from `order_after_move`: where each old index ended up.
pub fn new_positions(order: &[usize]) -> Vec<usize> {
    let mut positions = vec![0; order.len()];
    for (new, &old) in order.iter().enumerate() {
        positions[old] = new;
    }
    positions
}

/// The comment line marking a playlist entry's soundfont override in an
/// exported .m3u (other players skip `#` lines, so it's harmless there).
const M3U_SOUNDFONT: &str = "#SYNTHTHING-SOUNDFONT:";

/// `playlist` as an extended M3U (UTF-8): a `#EXTINF` line per entry with
/// its length in whole seconds (-1 when unknown, as M3U expects) and name
/// (`described` gives both), its soundfont override if any, then its
/// absolute path.
pub fn to_m3u(playlist: &Playlist, mut described: impl FnMut(&Path) -> (String, Option<f64>)) -> String {
    let mut out = String::from("#EXTM3U\n");
    out.push_str(&format!("#PLAYLIST:{}\n", playlist.name));
    for entry in &playlist.entries {
        let (name, length) = described(&entry.path);
        let seconds = length.map_or(-1, |s| s.round() as i64);
        out.push_str(&format!("#EXTINF:{seconds},{name}\n"));
        if let Some(sf) = &entry.soundfont {
            out.push_str(&format!("{M3U_SOUNDFONT}{}\n", sf.display()));
        }
        out.push_str(&format!("{}\n", entry.path.display()));
    }
    out
}

/// Entries from an M3U/M3U8 file's text: every non-comment line is a file
/// (relative paths are relative to `base`, the file's folder); web URLs are
/// skipped. Soundfont overrides written by `to_m3u` come back too.
pub fn from_m3u(text: &str, base: &Path) -> Vec<PlaylistEntry> {
    let resolve = |line: &str| {
        let path = PathBuf::from(line);
        if path.is_relative() { base.join(path) } else { path }
    };
    let mut entries = Vec::new();
    let mut soundfont: Option<PathBuf> = None;
    for line in text.trim_start_matches('\u{FEFF}').lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(sf) = line.strip_prefix(M3U_SOUNDFONT) {
            soundfont = Some(resolve(sf.trim()));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if line.contains("://") {
            soundfont = None;
            continue;
        }
        let mut entry = PlaylistEntry::new(resolve(line));
        entry.soundfont = soundfont.take();
        entries.push(entry);
    }
    entries
}

/// Replace a playlist's entries (as working a smart playlist out again
/// does) without losing the place in it: the song playing stays on the list
/// (put back, before the next old entry still there, if it no longer
/// belongs), and the history and the songs planned next follow their songs
/// to where they are now, leaving out those gone. Returns the entries to
/// use, and whether the playing song had to be put back.
pub fn carry_over(np: &mut NowPlaying, old: &[PlaylistEntry], mut new: Vec<PlaylistEntry>) -> (Vec<PlaylistEntry>, bool) {
    let Some(playing) = old.get(np.entry) else {
        np.history.clear();
        np.upcoming.clear();
        return (new, false);
    };
    let kept = !new.iter().any(|e| e.path == playing.path);
    if kept {
        let at = old[np.entry + 1..]
            .iter()
            .find_map(|after| new.iter().position(|e| e.path == after.path))
            .unwrap_or(new.len());
        new.insert(at, playing.clone());
    }
    let places: std::collections::HashMap<&Path, usize> = new.iter().enumerate().map(|(i, e)| (e.path.as_path(), i)).collect();
    let place = |i: &usize| old.get(*i).and_then(|e| places.get(e.path.as_path()).copied());
    np.entry = places[playing.path.as_path()];
    np.history = np.history.iter().filter_map(place).collect();
    np.upcoming = np.upcoming.iter().filter_map(place).collect();
    (new, kept)
}

/// Where an index ends up after removing entry `removed`; `None` if it was
/// the removed one.
pub fn index_after_remove(idx: usize, removed: usize) -> Option<usize> {
    match idx.cmp(&removed) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some(idx - 1),
        std::cmp::Ordering::Less => Some(idx),
    }
}

/// Small xorshift for shuffle, not worth a `rand` dependency for picking
/// the next song.
pub struct Rng(u64);

impl Rng {
    pub fn from_time() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(names: &[&str]) -> Vec<PlaylistEntry> {
        names.iter().map(|n| PlaylistEntry::new(PathBuf::from(n))).collect()
    }

    fn names(entries: &[PlaylistEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.path.to_str().unwrap()).collect()
    }

    #[test]
    fn carry_over_keeps_the_place_and_whats_planned() {
        let old = entries(&["a", "b", "c", "d", "e"]);
        // Playing c, shuffled: a and c played, e then b planned.
        let mut np = NowPlaying { list: 0, entry: 2, history: vec![0, 2], upcoming: vec![4, 1] };
        let (new, kept) = carry_over(&mut np, &old, entries(&["e", "a", "b", "c", "d"]));
        assert!(!kept);
        assert_eq!(names(&new), ["e", "a", "b", "c", "d"]);
        assert_eq!((np.entry, &np.history[..], &np.upcoming[..]), (3, &[1, 3][..], &[0, 2][..]));
    }

    #[test]
    fn carry_over_keeps_the_song_playing_though_it_left() {
        let old = entries(&["a", "b", "c", "d", "e"]);
        // Playing c (which stops matching), with d gone too.
        let mut np = NowPlaying { list: 0, entry: 2, history: vec![0, 2], upcoming: vec![3, 4, 1] };
        let (new, kept) = carry_over(&mut np, &old, entries(&["a", "b", "e"]));
        assert!(kept);
        assert_eq!(names(&new), ["a", "b", "c", "e"], "back before the next one still there");
        assert_eq!((np.entry, &np.history[..], &np.upcoming[..]), (2, &[0, 2][..], &[3, 1][..]));
        // Once something else plays, working it out again lets c go.
        np.entry = 3;
        np.upcoming = vec![1];
        let (new, kept) = carry_over(&mut np, &new, entries(&["a", "b", "e"]));
        assert!(!kept);
        assert_eq!(names(&new), ["a", "b", "e"]);
        assert_eq!((np.entry, &np.history[..], &np.upcoming[..]), (2, &[0][..], &[1][..]));
    }

    #[test]
    fn sequential_next_stops_or_wraps() {
        assert_eq!(next_entry(&[true; 3], &[None; 3], 0, LoopMode::Off, false, &[], 0), Some(1));
        assert_eq!(next_entry(&[true; 3], &[None; 3], 2, LoopMode::Off, false, &[], 0), None);
        assert_eq!(next_entry(&[true; 3], &[None; 3], 2, LoopMode::All, false, &[], 0), Some(0));
        assert_eq!(next_entry(&[true; 0], &[None; 0], 0, LoopMode::All, false, &[], 0), None);
    }

    #[test]
    fn only_the_playable_entries_are_played_on_to() {
        let starred = [true, false, true, false];
        assert_eq!(next_entry(&starred, &[None; 3], 0, LoopMode::Off, false, &[], 0), Some(2));
        assert_eq!(next_entry(&starred, &[None; 3], 1, LoopMode::Off, false, &[], 0), Some(2), "after one picked by hand");
        assert_eq!(next_entry(&starred, &[None; 3], 2, LoopMode::Off, false, &[], 0), None);
        assert_eq!(next_entry(&starred, &[None; 3], 2, LoopMode::All, false, &[], 0), Some(0));
        for r in 0..10 {
            assert_eq!(next_entry(&starred, &[None; 3], 0, LoopMode::Off, true, &[0], r), Some(2));
            assert_eq!(next_entry(&starred, &[None; 3], 2, LoopMode::All, true, &[0, 2], r), Some(0), "never the one just played");
        }
        assert_eq!(next_entry(&[false; 3], &[None; 3], 0, LoopMode::All, true, &[], 0), None);
    }

    #[test]
    fn shuffle_visits_every_entry_once_per_pass() {
        let mut rng = Rng(12345);
        let mut history = vec![0];
        let mut current = 0;
        while let Some(next) = next_entry(&[true; 5], &[None; 5], current, LoopMode::Off, true, &history, rng.next()) {
            assert!(!history.contains(&next));
            history.push(next);
            current = next;
        }
        let mut sorted = history.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn shuffle_new_pass_never_repeats_the_last_track() {
        for r in 0..20 {
            let next = next_entry(&[true; 3], &[None; 3], 1, LoopMode::All, true, &[0, 2, 1], r).unwrap();
            assert_ne!(next, 1);
        }
        assert_eq!(next_entry(&[true; 1], &[None; 1], 0, LoopMode::All, true, &[0], 7), Some(0));
    }

    #[test]
    fn shuffle_plays_songs_together_in_order() {
        let members = [None, Some(("x", 1)), None, Some(("x", 0))];
        for r in 0..20 {
            assert_ne!(next_entry(&[true; 4], &members, 0, LoopMode::Off, true, &[0], r), Some(1), "from its first");
            assert_eq!(next_entry(&[true; 4], &members, 3, LoopMode::Off, true, &[0, 3], r), Some(1), "then the rest of it");
            assert_ne!(next_entry(&[true; 4], &members, 1, LoopMode::All, true, &[0, 3, 2, 1], r), Some(1), "a new pass too");
        }
        assert_eq!(next_entry(&[true; 4], &members, 0, LoopMode::Off, false, &[0], 0), Some(1), "in the list's order, not shuffling");
    }

    #[test]
    fn previous_follows_history_when_shuffling() {
        assert_eq!(previous_entry(5, 3, LoopMode::Off, true, &[4, 1, 3]), Some(1));
        assert_eq!(previous_entry(5, 3, LoopMode::Off, true, &[3]), None);
        assert_eq!(previous_entry(5, 0, LoopMode::Off, false, &[]), None);
        assert_eq!(previous_entry(5, 0, LoopMode::All, false, &[]), Some(4));
    }

    #[test]
    fn moving_several_keeps_their_order_and_lands_before_the_target() {
        // Move entries 1 and 3 to just before 0.
        assert_eq!(order_after_move(5, &[3, 1], 0), [1, 3, 0, 2, 4]);
        // To the end.
        assert_eq!(order_after_move(5, &[0, 2], 5), [1, 3, 4, 0, 2]);
        // Into the middle, "before 4" in the current list.
        assert_eq!(order_after_move(5, &[0, 1], 4), [2, 3, 0, 1, 4]);
        // Dropping a block onto itself changes nothing.
        assert_eq!(order_after_move(4, &[1, 2], 2), [0, 1, 2, 3]);
        let order = order_after_move(5, &[3, 1], 0);
        assert_eq!(new_positions(&order), [2, 0, 3, 1, 4]);
    }

    #[test]
    fn m3u_round_trips_paths_and_soundfont_overrides() {
        // Absolute paths on this platform ("C:/..." is relative on Linux).
        let root = if cfg!(windows) { "C:/" } else { "/" };
        let at = |path: &str| PathBuf::from(format!("{root}{path}"));
        let base = at("music/lists");
        let base = base.as_path();
        let mut playlist = Playlist { name: "Mix".into(), ..Playlist::default() };
        let mut first = PlaylistEntry::new(at("music/a.mid"));
        first.soundfont = Some(at("sf/piano.sf2"));
        playlist.entries.push(first);
        playlist.entries.push(PlaylistEntry::new(at("music/b.mp3")));
        let text = to_m3u(&playlist, |p| {
            (p.file_stem().unwrap().to_string_lossy().into_owned(), (p.extension().unwrap() == "mp3").then_some(61.4))
        });
        assert!(text.starts_with("#EXTM3U\n#PLAYLIST:Mix\n#EXTINF:-1,a\n#SYNTHTHING-SOUNDFONT:"), "{text}");
        assert!(text.contains("#EXTINF:61,b\n"));

        let back = from_m3u(&text, base);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].path, at("music/a.mid"));
        assert_eq!(back[0].soundfont, Some(at("sf/piano.sf2")));
        assert_eq!(back[1].soundfont, None);

        // Plain M3U from elsewhere: relative paths, a BOM, URLs skipped.
        let other = from_m3u("\u{FEFF}# comment\nsongs/c.ogg\nhttp://radio.example/stream\n\nd.wav\n", base);
        let paths: Vec<_> = other.iter().map(|e| e.path.clone()).collect();
        assert_eq!(paths, [base.join("songs/c.ogg"), base.join("d.wav")]);
    }

    #[test]
    fn playlists_are_files_named_by_id() {
        let root = std::env::temp_dir().join(format!("synththing-playlist-ids-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, songs) = (root.join("playlists"), root.join("music"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("By hand.json"), r#"{ "name": "By hand", "entries": [] }"#).unwrap();
        let (mut library, errors) = Library::load(dir.clone(), songs.clone());
        assert!(errors.is_empty(), "{errors:?}");
        let by_hand = library.lists[0].id.clone();
        assert!(is_id(&by_hand), "{by_hand}");
        assert!(dir.join(format!("{by_hand}.json")).exists() && !dir.join("By hand.json").exists());

        let made = library.create("a/b: c?", vec![songs.join("x.mid")]).unwrap();
        let id = library.lists[made].id.clone();
        assert!(is_id(&id) && id != by_hand);
        let renamed = Playlist { name: "Renamed".into(), ..Playlist::default() };
        assert_eq!(library.put(&id, renamed).unwrap(), made);
        let (library, _) = Library::load(dir.clone(), songs);
        assert_eq!(library.lists.iter().map(|l| l.playlist.name.as_str()).collect::<Vec<_>>(), ["By hand", "Renamed"]);
        assert_eq!(library.find(&id), Some(1));
        let _ = std::fs::remove_dir_all(&root);
    }
}
