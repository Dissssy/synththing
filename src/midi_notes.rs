//! Every note in a MIDI file as a whole, with its start and stop time, for
//! `notes_between()` in visualizer scripts.
//!
//! rustysynth's `MidiFile` doesn't expose its events, so this reads the
//! Standard MIDI File itself (once, on the loader thread, alongside
//! rustysynth's own parse): note-ons and note-offs, paired up per channel
//! and key, first-in first-out, with ticks turned into seconds through the
//! file's tempo changes, the same way the player times them. Times are song
//! seconds, the same clock as `playback().position`.
//!
//! It also keeps the file's timing: the tempo map and time signatures, for
//! `beat()`, `bar()`, `tempo()` and friends.

/// One note, start to stop, in song seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
    pub start: f64,
    pub stop: f64,
}

/// All of a file's notes, sorted by start time. A note's index in the list
/// is its id, stable for as long as the file is loaded.
#[derive(Clone, Debug, Default)]
pub struct NoteList {
    notes: Vec<Note>,
    /// The longest note, so a window query knows how far back to look for
    /// notes that started earlier but are still held.
    longest: f64,
    /// Tempo map and time signatures; `None` for files timed in SMPTE
    /// frames (which have no beats) and for plain audio.
    timing: Option<Timing>,
}

/// A MIDI file's musical timing: where the beats fall in song seconds.
/// Beats are quarter notes, counted from 0 at the start of the song.
#[derive(Clone, Debug)]
pub struct Timing {
    ticks_per_quarter: f64,
    /// `(tick, seconds at that tick, microseconds per quarter note)` from
    /// each tempo change on; the first is at tick 0.
    tempos: Vec<(u64, f64, f64)>,
    /// `(tick, numerator, denominator)` from each time signature change on;
    /// the first is at tick 0 (4/4 unless the file says otherwise).
    signatures: Vec<(u64, u8, u8)>,
}

impl Timing {
    fn tempo_index_at_seconds(&self, seconds: f64) -> usize {
        self.tempos.partition_point(|&(_, s, _)| s <= seconds).saturating_sub(1)
    }

    fn ticks_at(&self, seconds: f64) -> f64 {
        let seconds = seconds.max(0.0);
        let (tick, start, micros) = self.tempos[self.tempo_index_at_seconds(seconds)];
        tick as f64 + (seconds - start) * 1_000_000.0 / micros * self.ticks_per_quarter
    }

    /// Song position in beats (quarter notes) at `seconds`.
    pub fn beat_at(&self, seconds: f64) -> f64 {
        self.ticks_at(seconds) / self.ticks_per_quarter
    }

    /// The song time of beat `beat` (the inverse of `beat_at`).
    pub fn seconds_at_beat(&self, beat: f64) -> f64 {
        let ticks = beat.max(0.0) * self.ticks_per_quarter;
        let index = self.tempos.partition_point(|&(t, _, _)| (t as f64) <= ticks).saturating_sub(1);
        let (tick, start, micros) = self.tempos[index];
        start + (ticks - tick as f64) / self.ticks_per_quarter * micros / 1_000_000.0
    }

    /// Tempo at `seconds`, in quarter notes per minute.
    pub fn tempo_at(&self, seconds: f64) -> f64 {
        60_000_000.0 / self.tempos[self.tempo_index_at_seconds(seconds.max(0.0))].2
    }

    /// Time signature at `seconds`, as `(numerator, denominator)`.
    pub fn signature_at(&self, seconds: f64) -> (u8, u8) {
        let ticks = self.ticks_at(seconds);
        let index = self.signatures.partition_point(|&(t, _, _)| (t as f64) <= ticks).saturating_sub(1);
        let (_, numerator, denominator) = self.signatures[index];
        (numerator, denominator)
    }

    /// Bar number (1-based) at `seconds`, and how far into that bar it is,
    /// in beats (quarter notes, from 0). Follows time signature changes.
    pub fn bar_at(&self, seconds: f64) -> (u32, f64) {
        let ticks = self.ticks_at(seconds);
        let mut bars_before = 0.0;
        for (i, &(start, numerator, denominator)) in self.signatures.iter().enumerate() {
            let bar_ticks = self.ticks_per_quarter * 4.0 * f64::from(numerator) / f64::from(denominator.max(1));
            let end = self.signatures.get(i + 1).map_or(f64::INFINITY, |&(t, _, _)| t as f64);
            if ticks < end {
                let into = (ticks - start as f64) / bar_ticks;
                let bar = into.floor();
                let beat_in_bar = (into - bar) * bar_ticks / self.ticks_per_quarter;
                return ((bars_before + bar) as u32 + 1, beat_in_bar);
            }
            // A signature change mid-bar starts a new bar there (rounding up).
            bars_before += ((end - start as f64) / bar_ticks).ceil();
        }
        (1, 0.0)
    }
}

impl NoteList {
    /// The file's tempo map and time signatures, if it has beats.
    pub fn timing(&self) -> Option<&Timing> {
        self.timing.as_ref()
    }

    /// `(index, note)` for every note sounding at any point in
    /// `[t0, t1)`: starting inside it, or started earlier and still held at
    /// `t0`. In start order.
    pub fn between(&self, t0: f64, t1: f64) -> impl Iterator<Item = (usize, &Note)> {
        let from = self.notes.partition_point(|n| n.start < t0 - self.longest);
        let to = self.notes.partition_point(|n| n.start < t1);
        self.notes[from..to.max(from)]
            .iter()
            .enumerate()
            .map(move |(i, n)| (from + i, n))
            .filter(move |(_, n)| n.stop > t0 || (n.start >= t0 && n.start < t1))
    }

    /// Read the notes out of a Standard MIDI File.
    pub fn from_smf(bytes: &[u8]) -> Result<Self, String> {
        let mut r = Reader { bytes, pos: 0 };
        if r.take(4)? != b"MThd" {
            return Err("not a MIDI file (no MThd header)".into());
        }
        let header_len = r.u32()? as usize;
        let header_start = r.pos;
        let _format = r.u16()?;
        let tracks = r.u16()?;
        let division = r.u16()?;
        r.pos = header_start + header_len;

        // Every event that matters, from every track, with its tick.
        let mut events: Vec<(u64, usize, Event)> = Vec::new();
        let mut order = 0usize;
        for _ in 0..tracks {
            let id = r.take(4)?;
            let len = r.u32()? as usize;
            let end = r.pos.checked_add(len).filter(|&e| e <= bytes.len()).ok_or("truncated track")?;
            if id != b"MTrk" {
                r.pos = end; // unknown chunk: skip
                continue;
            }
            let mut track = Reader { bytes: &bytes[..end], pos: r.pos };
            read_track(&mut track, &mut events, &mut order)?;
            r.pos = end;
        }
        // Merge tracks by tick, keeping each track's own order for ties.
        events.sort_by_key(|&(tick, order, _)| (tick, order));

        let mut clock = TickClock::new(division);
        let smpte = division & 0x8000 != 0;
        let mut tempos: Vec<(u64, f64, f64)> = vec![(0, 0.0, 500_000.0)];
        let mut signatures: Vec<(u64, u8, u8)> = vec![(0, 4, 4)];
        let mut open: std::collections::HashMap<(u8, u8), std::collections::VecDeque<(f64, u8)>> =
            std::collections::HashMap::new();
        let mut notes = Vec::new();
        let mut end_time = 0.0f64;
        for &(tick, _, event) in &events {
            let time = clock.seconds(tick);
            end_time = end_time.max(time);
            match event {
                Event::Tempo(micros_per_quarter) => {
                    clock.set_tempo(tick, micros_per_quarter);
                    let entry = (tick, time, f64::from(micros_per_quarter.max(1)));
                    match tempos.last_mut() {
                        Some(last) if last.0 == tick => *last = entry,
                        _ => tempos.push(entry),
                    }
                }
                Event::TimeSignature(numerator, denominator) => {
                    let entry = (tick, numerator.max(1), denominator.max(1));
                    match signatures.last_mut() {
                        Some(last) if last.0 == tick => *last = entry,
                        _ => signatures.push(entry),
                    }
                }
                Event::NoteOn { channel, key, velocity } => {
                    open.entry((channel, key)).or_default().push_back((time, velocity));
                }
                Event::NoteOff { channel, key } => {
                    if let Some((start, velocity)) = open.get_mut(&(channel, key)).and_then(|q| q.pop_front()) {
                        notes.push(Note { channel, key, velocity, start, stop: time });
                    }
                }
                Event::Other => {}
            }
        }
        // Never released: held to the end of the song.
        for ((channel, key), queue) in open {
            for (start, velocity) in queue {
                notes.push(Note { channel, key, velocity, start, stop: end_time.max(start) });
            }
        }
        notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.channel.cmp(&b.channel)).then(a.key.cmp(&b.key)));
        let longest = notes.iter().map(|n| n.stop - n.start).fold(0.0, f64::max);
        let timing = (!smpte).then(|| Timing {
            ticks_per_quarter: f64::from(division.max(1)),
            tempos,
            signatures,
        });
        Ok(Self { notes, longest, timing })
    }
}

#[derive(Clone, Copy, Debug)]
enum Event {
    NoteOn { channel: u8, key: u8, velocity: u8 },
    NoteOff { channel: u8, key: u8 },
    /// Microseconds per quarter note.
    Tempo(u32),
    /// Numerator and denominator (already as a number, e.g. 8 for x/8).
    TimeSignature(u8, u8),
    Other,
}

fn read_track(r: &mut Reader, events: &mut Vec<(u64, usize, Event)>, order: &mut usize) -> Result<(), String> {
    let mut tick = 0u64;
    let mut running_status = 0u8;
    while r.pos < r.bytes.len() {
        tick += u64::from(r.vlq()?);
        let mut status = r.u8()?;
        let event = match status {
            0xFF => {
                let kind = r.u8()?;
                let len = r.vlq()? as usize;
                let data = r.take(len)?;
                match (kind, data) {
                    (0x51, [a, b, c]) => Event::Tempo(u32::from_be_bytes([0, *a, *b, *c])),
                    (0x58, [numerator, power, ..]) if *power < 8 => Event::TimeSignature(*numerator, 1 << power),
                    (0x2F, _) => {
                        events.push((tick, *order, Event::Other)); // end of track still marks time
                        *order += 1;
                        break;
                    }
                    _ => Event::Other,
                }
            }
            0xF0 | 0xF7 => {
                let len = r.vlq()? as usize;
                r.take(len)?;
                Event::Other
            }
            _ => {
                let first_data = if status < 0x80 {
                    // Running status: this byte is data, reuse the last status.
                    if running_status == 0 {
                        return Err("data byte with no running status".into());
                    }
                    let data = status;
                    status = running_status;
                    Some(data)
                } else {
                    running_status = status;
                    None
                };
                let data = |r: &mut Reader| -> Result<u8, String> {
                    match first_data {
                        Some(d) => Ok(d),
                        None => r.u8(),
                    }
                };
                let channel = status & 0x0F;
                match status & 0xF0 {
                    0x80 => {
                        let key = data(r)?;
                        r.u8()?;
                        Event::NoteOff { channel, key }
                    }
                    0x90 => {
                        let key = data(r)?;
                        let velocity = r.u8()?;
                        if velocity == 0 {
                            Event::NoteOff { channel, key }
                        } else {
                            Event::NoteOn { channel, key, velocity }
                        }
                    }
                    0xA0 | 0xB0 | 0xE0 => {
                        data(r)?;
                        r.u8()?;
                        Event::Other
                    }
                    0xC0 | 0xD0 => {
                        data(r)?;
                        Event::Other
                    }
                    _ => return Err(format!("unexpected status byte {status:#04x}")),
                }
            }
        };
        events.push((tick, *order, event));
        *order += 1;
    }
    Ok(())
}

/// Ticks to seconds, following tempo changes as they're reached (events are
/// fed in tick order).
#[derive(Clone, Copy)]
struct TickClock {
    /// Ticks per quarter note, or for SMPTE timing, ticks per second
    /// (and tempo is ignored).
    ticks_per_quarter: f64,
    smpte_ticks_per_second: Option<f64>,
    micros_per_quarter: f64,
    last_tick: u64,
    last_seconds: f64,
}

impl TickClock {
    fn new(division: u16) -> Self {
        let smpte_ticks_per_second = (division & 0x8000 != 0).then(|| {
            let fps = -((division >> 8) as u8 as i8) as f64;
            let ticks_per_frame = (division & 0xFF) as f64;
            fps * ticks_per_frame
        });
        Self {
            ticks_per_quarter: f64::from(division.max(1)),
            smpte_ticks_per_second,
            micros_per_quarter: 500_000.0,
            last_tick: 0,
            last_seconds: 0.0,
        }
    }

    fn seconds(&self, tick: u64) -> f64 {
        let delta = tick.saturating_sub(self.last_tick) as f64;
        match self.smpte_ticks_per_second {
            Some(per_second) => self.last_seconds + delta / per_second.max(1.0),
            None => self.last_seconds + delta * self.micros_per_quarter / 1_000_000.0 / self.ticks_per_quarter,
        }
    }

    fn set_tempo(&mut self, tick: u64, micros_per_quarter: u32) {
        self.last_seconds = self.seconds(tick);
        self.last_tick = tick;
        self.micros_per_quarter = f64::from(micros_per_quarter.max(1));
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.bytes.len()).ok_or("unexpected end of file")?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A variable-length quantity (at most 4 bytes).
    fn vlq(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.u8()?;
            value = (value << 7) | u32::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("variable-length number too long".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vlq(mut v: u32) -> Vec<u8> {
        let mut out = vec![(v & 0x7F) as u8];
        v >>= 7;
        while v > 0 {
            out.insert(0, (v & 0x7F) as u8 | 0x80);
            v >>= 7;
        }
        out
    }

    fn track(events: &[(u32, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (delta, bytes) in events {
            body.extend(vlq(*delta));
            body.extend_from_slice(bytes);
        }
        body.extend([0x00, 0xFF, 0x2F, 0x00]);
        let mut out = b"MTrk".to_vec();
        out.extend((body.len() as u32).to_be_bytes());
        out.extend(body);
        out
    }

    /// Format 1, 480 ticks per quarter: a tempo track (120 bpm, then 240 bpm
    /// from tick 960) and a note track with a repeated key, running status,
    /// a velocity-0 note-off and a note that's never released.
    fn sample_file() -> Vec<u8> {
        let mut bytes = b"MThd".to_vec();
        bytes.extend(6u32.to_be_bytes());
        bytes.extend([0, 1, 0, 2, 0x01, 0xE0]);
        bytes.extend(track(&[
            (0, &[0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]), // 500000 us/q
            (0, &[0xFF, 0x58, 0x04, 3, 2, 24, 8]),     // 3/4
            (960, &[0xFF, 0x51, 0x03, 0x03, 0xD0, 0x90]), // 250000 us/q
        ]));
        bytes.extend(track(&[
            (0, &[0x90, 60, 100]),   // C on at 0 s
            (480, &[0x80, 60, 0]),   // off at 0.5 s
            (0, &[0x90, 60, 90]),    // C again at 0.5 s
            (480, &[60, 0]),         // running status, vel 0 = off, at 1.0 s
            (480, &[0x91, 64, 80]),  // ch 1 E on at 1.25 s (tempo doubled)
            (480, &[0x81, 64, 0]),   // off at 1.5 s
            (0, &[0x92, 67, 70]),    // ch 2 G on at 1.5 s, never released
            (480, &[0xB0, 7, 100]),  // a controller, at 1.75 s
        ]));
        bytes
    }

    #[test]
    fn pairs_notes_through_tempo_changes() {
        let list = NoteList::from_smf(&sample_file()).unwrap();
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        let got: Vec<_> = list.notes.iter().map(|n| (n.channel, n.key, n.velocity, n.start, n.stop)).collect();
        assert_eq!(got.len(), 4, "{got:?}");
        let expected = [(0, 60, 100, 0.0, 0.5), (0, 60, 90, 0.5, 1.0), (1, 64, 80, 1.25, 1.5), (2, 67, 70, 1.5, 1.75)];
        for (g, e) in got.iter().zip(expected) {
            assert_eq!((g.0, g.1, g.2), (e.0, e.1, e.2));
            assert!(close(g.3, e.3) && close(g.4, e.4), "{g:?} vs {e:?}");
        }
    }

    #[test]
    fn timing_matches_rustysynth() {
        let bytes = sample_file();
        let list = NoteList::from_smf(&bytes).unwrap();
        let midi = rustysynth::MidiFile::new(&mut bytes.as_slice()).unwrap();
        let ours = list.notes.iter().map(|n| n.stop).fold(0.0, f64::max);
        assert!((ours - midi.get_length()).abs() < 1e-6, "ours {ours}, rustysynth {}", midi.get_length());
    }

    #[test]
    fn between_includes_held_notes_and_respects_the_window() {
        let list = NoteList::from_smf(&sample_file()).unwrap();
        let ids = |t0, t1| list.between(t0, t1).map(|(i, _)| i).collect::<Vec<_>>();
        assert_eq!(ids(0.0, 0.5), [0]);
        assert_eq!(ids(0.75, 0.8), [1]); // held since 0.5
        assert_eq!(ids(1.0, 1.3), [2]); // note 1 stops exactly at 1.0
        assert_eq!(ids(0.0, 10.0), [0, 1, 2, 3]);
        assert!(ids(5.0, 6.0).is_empty());
    }

    #[test]
    fn timing_follows_tempo_and_signature_changes() {
        let list = NoteList::from_smf(&sample_file()).unwrap();
        let timing = list.timing().unwrap();
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        // 120 bpm for the first 2 beats (1.0 s), then 240 bpm.
        assert!(close(timing.beat_at(0.5), 1.0));
        assert!(close(timing.beat_at(1.0), 2.0));
        assert!(close(timing.beat_at(1.25), 3.0));
        assert!(close(timing.tempo_at(0.9), 120.0) && close(timing.tempo_at(1.1), 240.0));
        for beat in [0.0, 1.5, 2.0, 3.7] {
            assert!(close(timing.beat_at(timing.seconds_at_beat(beat)), beat));
        }
        // 3/4: bars are 3 beats.
        assert_eq!(timing.signature_at(1.0), (3, 4));
        let (bar, into) = timing.bar_at(timing.seconds_at_beat(4.5));
        assert_eq!(bar, 2);
        assert!(close(into, 1.5));
    }

    #[test]
    fn rejects_garbage() {
        assert!(NoteList::from_smf(b"RIFF....").is_err());
        assert!(NoteList::from_smf(b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk\0\0\0\x10\x00\x90").is_err());
    }
}
