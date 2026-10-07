//! Saved playlists: named, ordered lists of songs, each entry optionally
//! carrying its own soundfont override (MIDI only), persisted one JSON file
//! per playlist under `<config dir>/playlists/`.
//!
//! This module is the data model and the pure "what plays next" logic; the
//! panel UI, drag-and-drop and actually loading/playing tracks live in
//! `app.rs`. Index bookkeeping after edits (`index_after_move`/
//! `index_after_remove`) is here too so it can be tested without a GUI.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub path: PathBuf,
    /// Soundfont to use for this entry instead of the one selected in the
    /// Soundfonts panel. Only meaningful for MIDI entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soundfont: Option<PathBuf>,
    /// Cached `!path.exists()`, refreshed on load/add rather than stat'ing
    /// every row every frame.
    #[serde(skip)]
    pub missing: bool,
}

impl PlaylistEntry {
    pub fn new(path: PathBuf) -> Self {
        let missing = !path.exists();
        Self { path, soundfont: None, missing }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Playlist {
    pub name: String,
    #[serde(default)]
    pub entries: Vec<PlaylistEntry>,
}

/// A playlist plus the file it's saved in. The file name is fixed at
/// creation; renaming only changes `playlist.name`, so a rename never has to
/// touch the filesystem beyond the usual save.
pub struct StoredPlaylist {
    file: PathBuf,
    pub playlist: Playlist,
}

pub struct Library {
    dir: PathBuf,
    pub lists: Vec<StoredPlaylist>,
}

impl Library {
    /// Load every `*.json` in `dir`, sorted by name. Unreadable files are
    /// skipped and reported back rather than failing the whole load.
    pub fn load(dir: PathBuf) -> (Self, Vec<String>) {
        let mut lists = Vec::new();
        let mut errors = Vec::new();
        if let Ok(read_dir) = fs::read_dir(&dir) {
            for entry in read_dir.flatten() {
                let file = entry.path();
                if file.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let parsed = fs::read_to_string(&file)
                    .map_err(anyhow::Error::from)
                    .and_then(|s| serde_json::from_str::<Playlist>(&s).map_err(Into::into));
                match parsed {
                    Ok(mut playlist) => {
                        for e in &mut playlist.entries {
                            e.missing = !e.path.exists();
                        }
                        lists.push(StoredPlaylist { file, playlist });
                    }
                    Err(e) => errors.push(format!("{}: {e}", file.display())),
                }
            }
        }
        lists.sort_by_key(|l| l.playlist.name.to_lowercase());
        (Self { dir, lists }, errors)
    }

    /// Create and save a new playlist, returning its index.
    pub fn create(&mut self, name: &str, songs: Vec<PathBuf>) -> Result<usize> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let stem = file_stem_for(name);
        let mut file = self.dir.join(format!("{stem}.json"));
        let mut n = 2;
        while file.exists() || self.lists.iter().any(|l| l.file == file) {
            file = self.dir.join(format!("{stem}-{n}.json"));
            n += 1;
        }
        let playlist = Playlist {
            name: name.to_string(),
            entries: songs.into_iter().map(PlaylistEntry::new).collect(),
        };
        self.lists.push(StoredPlaylist { file, playlist });
        let idx = self.lists.len() - 1;
        self.save(idx)?;
        Ok(idx)
    }

    pub fn save(&self, idx: usize) -> Result<()> {
        let Some(stored) = self.lists.get(idx) else {
            return Ok(());
        };
        let json = serde_json::to_string_pretty(&stored.playlist)?;
        fs::write(&stored.file, json)
            .with_context(|| format!("writing {}", stored.file.display()))
    }

    pub fn delete(&mut self, idx: usize) -> Result<()> {
        if idx >= self.lists.len() {
            return Ok(());
        }
        let stored = self.lists.remove(idx);
        if stored.file.exists() {
            fs::remove_file(&stored.file)
                .with_context(|| format!("deleting {}", stored.file.display()))?;
        }
        Ok(())
    }
}

/// A filesystem-safe file stem for a playlist name, anything outside
/// `[A-Za-z0-9 _-]` becomes `_`, so names like "a/b: c?" can't escape the
/// playlists folder or trip Windows' reserved characters.
fn file_stem_for(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-') { c } else { '_' })
        .collect();
    if stem.trim().is_empty() { "playlist".to_string() } else { stem }
}

/// Every file directly in `dir` (not subfolders) whose extension is in
/// `extensions`, sorted case-insensitively by file name, the "new playlist
/// from current folder" contents.
pub fn songs_in_folder(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut songs: Vec<PathBuf> = read_dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| extensions.iter().any(|x| ext.eq_ignore_ascii_case(x)))
        })
        .collect();
    songs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    songs
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
    /// The entry already picked to play next (decided as soon as this one
    /// starts, even when shuffling, so it can be preloaded). `None` when
    /// nothing follows, or not decided yet.
    pub upcoming: Option<usize>,
}

impl NowPlaying {
    pub fn new(list: usize, entry: usize) -> Self {
        Self { list, entry, history: vec![entry], upcoming: None }
    }
}

/// The entry to play after `current` in a list of `len`, or `None` when
/// playback should stop. `history` is this pass's already-played entries
/// (only consulted when shuffling); `random` is any random number.
pub fn next_entry(
    len: usize,
    current: usize,
    loop_mode: LoopMode,
    shuffle: bool,
    history: &[usize],
    random: u64,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    if !shuffle {
        return if current + 1 < len {
            Some(current + 1)
        } else if loop_mode == LoopMode::All {
            Some(0)
        } else {
            None
        };
    }
    let unplayed: Vec<usize> = (0..len).filter(|i| !history.contains(i)).collect();
    if !unplayed.is_empty() {
        return Some(unplayed[(random % unplayed.len() as u64) as usize]);
    }
    if loop_mode != LoopMode::All {
        return None;
    }
    // New pass: anything but the track that just finished, so a reshuffle
    // never plays the same song twice in a row (unless it's the only one).
    if len == 1 {
        return Some(0);
    }
    let others: Vec<usize> = (0..len).filter(|&i| i != current).collect();
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
/// its length in whole seconds (-1 when unknown, as M3U expects) and name,
/// its soundfont override if any, then its absolute path.
pub fn to_m3u(playlist: &Playlist, mut length_of: impl FnMut(&Path) -> Option<f64>) -> String {
    let mut out = String::from("#EXTM3U\n");
    out.push_str(&format!("#PLAYLIST:{}\n", playlist.name));
    for entry in &playlist.entries {
        let seconds = length_of(&entry.path).map_or(-1, |s| s.round() as i64);
        let name = entry.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
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

    #[test]
    fn sequential_next_stops_or_wraps() {
        assert_eq!(next_entry(3, 0, LoopMode::Off, false, &[], 0), Some(1));
        assert_eq!(next_entry(3, 2, LoopMode::Off, false, &[], 0), None);
        assert_eq!(next_entry(3, 2, LoopMode::All, false, &[], 0), Some(0));
        assert_eq!(next_entry(0, 0, LoopMode::All, false, &[], 0), None);
    }

    #[test]
    fn shuffle_visits_every_entry_once_per_pass() {
        let mut rng = Rng(12345);
        let mut history = vec![0];
        let mut current = 0;
        while let Some(next) = next_entry(5, current, LoopMode::Off, true, &history, rng.next()) {
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
            let next = next_entry(3, 1, LoopMode::All, true, &[0, 2, 1], r).unwrap();
            assert_ne!(next, 1);
        }
        assert_eq!(next_entry(1, 0, LoopMode::All, true, &[0], 7), Some(0));
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
        let mut playlist = Playlist { name: "Mix".into(), entries: Vec::new() };
        let mut first = PlaylistEntry::new(at("music/a.mid"));
        first.soundfont = Some(at("sf/piano.sf2"));
        playlist.entries.push(first);
        playlist.entries.push(PlaylistEntry::new(at("music/b.mp3")));
        let text = to_m3u(&playlist, |p| (p.extension().unwrap() == "mp3").then_some(61.4));
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
    fn file_stems_are_filesystem_safe() {
        assert_eq!(file_stem_for("chill / study: mix?"), "chill _ study_ mix_");
        assert_eq!(file_stem_for("   "), "playlist");
        assert_eq!(file_stem_for("../../etc"), "______etc");
    }
}
