//! Writes the starter songs, `assets/starter/songs/*.mid`: synththing's own
//! arrangements of public-domain music (so they're ours, under the same
//! license as the rest of the repository), for the welcome starter pack.
//! Each instrument is on its own channel, so muting one is easy to hear.
//!
//!     cargo run --example gen_starter_songs

use std::fs;
use std::path::Path;

const PPQ: u32 = 480;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("starter").join("songs");
    fs::create_dir_all(&dir).unwrap();
    for (name, song) in [
        ("Frere Jacques (round).mid", frere_jacques()),
        ("Ode to Joy.mid", ode_to_joy()),
        ("Canon in D.mid", canon_in_d()),
        ("In the Hall of the Mountain King.mid", mountain_king()),
    ] {
        let bytes = song.to_smf();
        fs::write(dir.join(name), &bytes).unwrap();
        println!("{name}: {} bytes", bytes.len());
    }
}

// --- songs ----------------------------------------------------------------

/// A four-voice round, each voice two bars after the last, plus drums.
fn frere_jacques() -> Song {
    let mut song = Song::new("Frere Jacques (round)", 120.0, (4, 4));
    let tune = "C4:1 D4:1 E4:1 C4:1  C4:1 D4:1 E4:1 C4:1  E4:1 F4:1 G4:2  E4:1 F4:1 G4:2 \
                G4:.5 A4:.5 G4:.5 F4:.5 E4:1 C4:1  G4:.5 A4:.5 G4:.5 F4:.5 E4:1 C4:1 \
                C4:1 G3:1 C4:2  C4:1 G3:1 C4:2";
    let voices = [("Piano", 0, 0, 12), ("Flute", 1, 73, 24), ("Clarinet", 2, 71, 0), ("Cello", 3, 42, -12)];
    for (n, (name, channel, program, shift)) in voices.into_iter().enumerate() {
        let track = song.track(name, channel, program);
        let start = n as f64 * 8.0;
        let end = track.line(start, tune, 92, shift);
        track.line(end, tune, 92, shift);
    }
    let drums = song.track("Drums", 9, 0);
    drums.rock_beat(8.0, 88.0, false);
    drums.hit(88.0, 49, 100);
    song
}

/// The melody (piano, then trumpet), bass, string chords and drums.
fn ode_to_joy() -> Song {
    let mut song = Song::new("Ode to Joy", 112.0, (4, 4));
    let a = "E4:1 E4:1 F4:1 G4:1  G4:1 F4:1 E4:1 D4:1  C4:1 C4:1 D4:1 E4:1";
    let end1 = "E4:1.5 D4:.5 D4:2";
    let end2 = "D4:1.5 C4:.5 C4:2";
    let b = "D4:1 D4:1 E4:1 C4:1  D4:1 E4:.5 F4:.5 E4:1 C4:1  D4:1 E4:.5 F4:.5 E4:1 D4:1  C4:1 D4:1 G3:2";
    let tune = format!("{a} {end1} {a} {end2} {b} {a} {end2}");
    let melody = song.track("Piano", 0, 0);
    let end = melody.line(0.0, &tune, 96, 12);
    let brass = song.track("Trumpet", 1, 56);
    brass.line(end, &tune, 100, 0);

    // One chord (and bass note) per bar, root names.
    let bars = "C G C G  C G C G  G C G G  C G C GC";
    let strings = song.track("Strings", 2, 48);
    let mut at = 0.0;
    for _ in 0..2 {
        at = strings.chords(at, bars, 60, 0);
    }
    let bass = song.track("Bass", 3, 33);
    let mut at = 0.0;
    for _ in 0..2 {
        at = bass.bass(at, bars, 90);
    }
    let drums = song.track("Drums", 9, 0);
    drums.rock_beat(0.0, at, false);
    drums.hit(at, 49, 110);
    let melody = song.tracks.iter_mut().find(|t| t.name == "Trumpet").unwrap();
    melody.line(at, "C5:4", 100, 0);
    song
}

/// The ground bass, and three voices in canon over it.
fn canon_in_d() -> Song {
    let mut song = Song::new("Canon in D", 72.0, (4, 4));
    let ground = "D3:1 A2:1 B2:1 F#2:1  G2:1 D2:1 G2:1 A2:1";
    let cello = song.track("Cello", 3, 42);
    let mut at = 0.0;
    for _ in 0..9 {
        at = cello.line(at, ground, 84, 0);
    }
    cello.line(at, "D2:4", 84, 0);
    let chords = song.track("Harpsichord", 4, 6);
    let mut c = 0.0;
    for _ in 0..9 {
        c = chords.chords(c, "DABmF#m GDGA", 52, -1);
    }
    chords.chords(c, "D", 52, 0);
    let line = "F#5:2 E5:2 D5:2 C#5:2  B4:2 A4:2 B4:2 C#5:2 \
                D5:1 C#5:1 B4:1 A4:1  G4:1 F#4:1 G4:1 E4:1 \
                D4:.5 F#4:.5 A4:.5 G4:.5 F#4:.5 D4:.5 F#4:.5 E4:.5  D4:.5 B3:.5 D4:.5 A4:.5 G4:.5 B4:.5 A4:.5 G4:.5 \
                F#4:1 D4:1 E4:1 C#5:1  D5:1 F#5:1 A5:1 A4:1  B4:1 G4:1 A4:1 F#4:1  D4:1 D5:2 C#5:1";
    let voices = [("Violin", 0, 40, 0), ("Flute", 1, 73, 0), ("Harp", 2, 46, -12)];
    for (n, (name, channel, program, shift)) in voices.into_iter().enumerate() {
        let track = song.track(name, channel, program);
        let start = 8.0 + n as f64 * 8.0;
        let end = track.line(start, line, 80, shift);
        track.line(end, "D5:4", 80, shift);
    }
    song
}

/// The theme, over and over, faster and louder each time.
fn mountain_king() -> Song {
    let mut song = Song::new("In the Hall of the Mountain King", 92.0, (4, 4));
    let theme = "B3:.5 C#4:.5 D4:.5 E4:.5 F#4:.5 D4:.5 F#4:1  F4:.5 C#4:.5 F4:1 E4:.5 C4:.5 E4:1 \
                 B3:.5 C#4:.5 D4:.5 E4:.5 F#4:.5 D4:.5 F#4:.5 B4:.5  A4:.5 F#4:.5 D4:.5 F#4:.5 A4:2";
    let rounds = 6;
    let instruments = [("Bassoon", 0, 70), ("Pizzicato", 1, 45), ("Clarinet", 2, 71), ("Brass", 3, 61)];
    // Each round adds an instrument (the first four) and speeds up.
    for (n, (name, channel, program)) in instruments.into_iter().enumerate() {
        let track = song.track(name, channel, program);
        let shift = if n == 3 { 12 } else { 0 };
        for round in n.min(rounds)..rounds {
            track.line(round as f64 * 16.0, theme, 70 + round as u8 * 9, shift);
        }
    }
    let bass = song.track("Timpani", 4, 47);
    for round in 2..rounds {
        for beat in 0..16 {
            let note = if beat % 4 < 2 { "B2" } else { "F#2" };
            bass.line(round as f64 * 16.0 + beat as f64, &format!("{note}:.5"), 70 + round as u8 * 8, 0);
        }
    }
    let end = rounds as f64 * 16.0;
    for round in 0..rounds {
        song.tempo_at(round as f64 * 16.0, 92.0 + round as f64 * 22.0);
    }
    let drums = song.track("Drums", 9, 0);
    drums.rock_beat(48.0, end, true);
    drums.hit(end, 49, 120);
    drums.hit(end, 36, 120);
    for track in song.tracks.iter_mut().filter(|t| t.channel != 9) {
        let low = if track.name == "Timpani" { "B1:4" } else { "B2+F#3+B3:4" };
        track.line(end, low, 110, 0);
    }
    song
}

// --- a small Standard MIDI File writer --------------------------------------

struct Song {
    name: String,
    tempos: Vec<(f64, f64)>,
    signature: (u8, u8),
    tracks: Vec<Track>,
}

struct Track {
    name: String,
    channel: u8,
    program: u8,
    /// (tick, order, bytes): note-offs sort before note-ons at the same tick.
    events: Vec<(u32, u8, Vec<u8>)>,
}

impl Song {
    fn new(name: &str, bpm: f64, signature: (u8, u8)) -> Self {
        Self { name: name.into(), tempos: vec![(0.0, bpm)], signature, tracks: Vec::new() }
    }

    fn track(&mut self, name: &str, channel: u8, program: u8) -> &mut Track {
        self.tracks.push(Track { name: name.into(), channel, program, events: Vec::new() });
        self.tracks.last_mut().unwrap()
    }

    fn tempo_at(&mut self, beat: f64, bpm: f64) {
        self.tempos.retain(|&(b, _)| b != beat);
        self.tempos.push((beat, bpm));
    }

    fn to_smf(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(self.tracks.len() as u16 + 1).to_be_bytes());
        out.extend_from_slice(&(PPQ as u16).to_be_bytes());

        // Track 0: the name, time signature and tempo map.
        let mut meta: Vec<(u32, u8, Vec<u8>)> = vec![(0, 0, meta_event(0x03, self.name.as_bytes()))];
        let denominator_power = self.signature.1.trailing_zeros() as u8;
        meta.push((0, 0, meta_event(0x58, &[self.signature.0, denominator_power, 24, 8])));
        for &(beat, bpm) in &self.tempos {
            let micros = (60_000_000.0 / bpm).round() as u32;
            meta.push((ticks(beat), 1, meta_event(0x51, &micros.to_be_bytes()[1..])));
        }
        write_track(&mut out, meta);

        for track in &self.tracks {
            let mut events = vec![
                (0, 0, meta_event(0x03, track.name.as_bytes())),
                (0, 0, vec![0xC0 | track.channel, track.program]),
                (0, 0, vec![0xB0 | track.channel, 7, 100]),
            ];
            events.extend(track.events.iter().cloned());
            write_track(&mut out, events);
        }
        out
    }
}

impl Track {
    fn note(&mut self, beat: f64, length: f64, key: u8, velocity: u8) {
        let on = ticks(beat);
        // A hair short, so repeated notes are heard as separate notes.
        let off = ticks(beat + length).saturating_sub(PPQ / 32).max(on + 1);
        self.events.push((on, 2, vec![0x90 | self.channel, key, velocity]));
        self.events.push((off, 1, vec![0x80 | self.channel, key, 0]));
    }

    /// Notes from text ("C4:1 D#4:.5 r:1 C4+E4+G4:2", lengths in beats),
    /// starting at `start`; returns the beat after the last one.
    fn line(&mut self, start: f64, text: &str, velocity: u8, shift: i32) -> f64 {
        let mut at = start;
        for token in text.split_whitespace() {
            let (keys, length) = token.split_once(':').expect(token);
            let length: f64 = length.parse().expect(token);
            if keys != "r" {
                for key in keys.split('+') {
                    let key = (note_number(key) as i32 + shift) as u8;
                    self.note(at, length, key, velocity);
                }
            }
            at += length;
        }
        at
    }

    /// A chord per bar from root names ("C", "F#m"; two names in one token,
    /// "GC", split the bar), in the octave around middle C.
    fn chords(&mut self, start: f64, bars: &str, velocity: u8, octave_shift: i32) -> f64 {
        let mut at = start;
        for bar in bars.split_whitespace() {
            let parts = split_chords(bar);
            let length = 4.0 / parts.len() as f64;
            for chord in parts {
                let (root, minor) = chord_root(&chord);
                let base = 60 + root as i32 + octave_shift * 12;
                let base = if base > 64 { base - 12 } else { base };
                for interval in [0, if minor { 3 } else { 4 }, 7] {
                    self.note(at, length, (base + interval) as u8, velocity);
                }
                at += length;
            }
        }
        at
    }

    /// The roots of `bars` (as in `chords`), as a walking bass in quarters.
    fn bass(&mut self, start: f64, bars: &str, velocity: u8) -> f64 {
        let mut at = start;
        for bar in bars.split_whitespace() {
            let parts = split_chords(bar);
            let beats = 4 / parts.len();
            for chord in parts {
                let (root, _) = chord_root(&chord);
                let key = 36 + root;
                for beat in 0..beats {
                    let key = if beat % 2 == 1 { key + 7 } else { key };
                    self.note(at, 1.0, key, velocity);
                    at += 1.0;
                }
            }
        }
        at
    }

    fn hit(&mut self, beat: f64, key: u8, velocity: u8) {
        self.note(beat, 0.5, key, velocity);
    }

    /// Kick, snare on 2 and 4, hi-hat eighths, from `start` to `end`.
    fn rock_beat(&mut self, start: f64, end: f64, driving: bool) {
        let mut beat = start;
        while beat < end - 1e-9 {
            let in_bar = (beat - start) % 4.0;
            if in_bar == 0.0 || in_bar == 2.0 || (driving && in_bar == 2.5) {
                self.hit(beat, 36, 100);
            }
            if in_bar == 1.0 || in_bar == 3.0 {
                self.hit(beat, 38, 90);
            }
            self.hit(beat, 42, if in_bar.fract() == 0.0 { 70 } else { 50 });
            beat += 0.5;
        }
    }
}

fn ticks(beat: f64) -> u32 {
    (beat * f64::from(PPQ)).round() as u32
}

fn meta_event(kind: u8, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0xFF, kind];
    bytes.extend(variable_length(data.len() as u32));
    bytes.extend_from_slice(data);
    bytes
}

fn variable_length(mut value: u32) -> Vec<u8> {
    let mut bytes = vec![(value & 0x7F) as u8];
    value >>= 7;
    while value > 0 {
        bytes.insert(0, (value & 0x7F) as u8 | 0x80);
        value >>= 7;
    }
    bytes
}

fn write_track(out: &mut Vec<u8>, mut events: Vec<(u32, u8, Vec<u8>)>) {
    events.sort_by_key(|(tick, order, _)| (*tick, *order));
    let mut data = Vec::new();
    let mut last = 0;
    for (tick, _, bytes) in events {
        data.extend(variable_length(tick - last));
        data.extend(bytes);
        last = tick;
    }
    data.extend([0x00, 0xFF, 0x2F, 0x00]);
    out.extend_from_slice(b"MTrk");
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend(data);
}

/// "C4" is middle C (60); "F#3", "Bb2".
fn note_number(name: &str) -> u8 {
    let (pitch, octave) = name.split_at(name.find(|c: char| c.is_ascii_digit() || c == '-').expect(name));
    let octave: i32 = octave.parse().expect(name);
    let (root, minor) = chord_root(pitch);
    assert!(!minor, "{name}");
    ((octave + 1) * 12 + root as i32) as u8
}

/// "F#m" -> (6, true).
fn chord_root(name: &str) -> (u8, bool) {
    let mut chars = name.chars();
    let letter = chars.next().expect(name);
    let mut root: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => panic!("not a note: {name}"),
    };
    let rest = chars.as_str();
    let rest = if let Some(r) = rest.strip_prefix('#') {
        root += 1;
        r
    } else if let Some(r) = rest.strip_prefix('b') {
        root -= 1;
        r
    } else {
        rest
    };
    (root.rem_euclid(12) as u8, rest == "m")
}

/// "GC" -> ["G", "C"]; "F#m" -> ["F#m"].
fn split_chords(bar: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in bar.chars() {
        if c.is_ascii_uppercase() {
            parts.push(c.to_string());
        } else {
            parts.last_mut().expect(bar).push(c);
        }
    }
    parts
}
