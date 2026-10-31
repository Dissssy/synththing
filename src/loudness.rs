//! How loud songs are, to even out their volume (Preferences > Playback):
//! EBU R128 integrated loudness, in LUFS, and the sample peak.
//!
//! A song's loudness comes from its ReplayGain tags when it has them
//! (`song_store::Meta::replay_gain`), else from measuring it here, once,
//! decoding the whole file on a thread of its own ([`Measuring`]); what's
//! measured is kept in the synced library (`song_store::Song::loudness`),
//! so one device measures each song for all of them. Playing, a song's
//! gain brings it to the target loudness ([`gain`]).

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use ebur128::{EbuR128, Mode};
use rodio::Source as _;
use serde::{Deserialize, Serialize};

use crate::fields::Facts;
use crate::song_store::{META_VERSION, Song, SongStore};

/// The loudness songs are brought to unless changed, in LUFS (what most
/// streaming services play at).
pub const DEFAULT_TARGET: f32 = -14.0;
/// The range Preferences offers for it.
pub const TARGET_RANGE: std::ops::RangeInclusive<f32> = -23.0..=-8.0;
/// The most a quiet song is boosted (+12 dB).
pub const MAX_GAIN: f32 = 4.0;
/// The loudness ReplayGain 2 brings songs to: a song's track gain is how
/// far it is from this.
const REPLAY_GAIN_REFERENCE: f32 = -18.0;
/// Frames fed to the meter at a time.
const BLOCK_FRAMES: usize = 1 << 14;

/// A song's loudness. Kept under short names: the synced library has one
/// for most songs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loudness {
    /// Integrated loudness (LUFS).
    #[serde(rename = "i")]
    pub lufs: f32,
    /// The loudest sample (1.0 is full scale), if it's known.
    #[serde(rename = "p", default, skip_serializing_if = "Option::is_none")]
    pub peak: Option<f32>,
}

impl Loudness {
    /// What ReplayGain tags say: a gain in dB ("-6.54 dB") and a peak
    /// ("0.988"), if they say it.
    pub fn from_replay_gain(gain: &str, peak: Option<&str>) -> Option<Self> {
        let number = |text: &str| text.trim().trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace()).trim().parse::<f32>().ok().filter(|n| n.is_finite());
        let gain = number(gain)?;
        Some(Self { lufs: REPLAY_GAIN_REFERENCE - gain, peak: peak.and_then(number).filter(|&p| p > 0.0) })
    }
}

/// An album's loudness, from its songs' and how long each is (seconds):
/// their loudness averaged as energy over the album's length, and the
/// loudest peak of theirs (unknown if any's unknown). `None` for no songs.
pub fn album(songs: impl IntoIterator<Item = (Loudness, f64)>) -> Option<Loudness> {
    let (mut energy, mut length, mut peak) = (0.0f64, 0.0f64, Some(0.0f32));
    for (loudness, seconds) in songs {
        let seconds = if seconds > 0.0 { seconds } else { 1.0 };
        energy += 10f64.powf(f64::from(loudness.lufs) / 10.0) * seconds;
        length += seconds;
        peak = peak.zip(loudness.peak).map(|(a, b)| a.max(b));
    }
    (length > 0.0).then(|| Loudness { lufs: (10.0 * (energy / length).log10()) as f32, peak })
}

/// The gain that brings a song this loud to `target` (LUFS): a boost
/// only as far as its peak stays under full scale, and no more than
/// `MAX_GAIN`.
pub fn gain(loudness: Loudness, target: f32) -> f32 {
    let gain = 10f32.powf((target - loudness.lufs) / 20.0);
    let ceiling = loudness.peak.map_or(MAX_GAIN, |peak| (1.0 / peak).clamp(1.0, MAX_GAIN));
    if gain.is_finite() { gain.min(ceiling) } else { 1.0 }
}

/// Measure the song at `path`, decoding all of it: with symphonia, else
/// ffmpeg (if there is one), as playing it would. Measured as it's
/// played: in stereo, mono on both sides.
pub fn measure(path: &Path) -> Result<Loudness, String> {
    measure_with_symphonia(path).or_else(|err| match crate::ffmpeg::find() {
        Some(ffmpeg) => measure_with_ffmpeg(&ffmpeg, path).map_err(|ffmpeg_err| format!("{err}; ffmpeg couldn't either: {ffmpeg_err}")),
        None => Err(err),
    })
}

fn measure_with_symphonia(path: &Path) -> Result<Loudness, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    // (The length and extension matter: the MP4 demuxer won't take an .m4a without them.)
    let mut builder = rodio::Decoder::builder().with_data(BufReader::new(file)).with_byte_len(len);
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        builder = builder.with_hint(ext);
    }
    let mut decoder = builder.build().map_err(|e| e.to_string())?;
    let channels = (decoder.channels().get() as usize).max(1);
    let mut meter = Meter::new(decoder.sample_rate().get())?;
    let mut block = Vec::with_capacity(BLOCK_FRAMES * channels);
    loop {
        block.clear();
        block.extend(decoder.by_ref().take(BLOCK_FRAMES * channels));
        if block.is_empty() {
            break;
        }
        meter.add(block.chunks_exact(channels).map(|f| (f[0], f[if channels > 1 { 1 } else { 0 }])))?;
    }
    meter.finish()
}

/// Have ffmpeg decode the first audio stream to stereo f32 at 48 kHz,
/// read as it comes.
fn measure_with_ffmpeg(ffmpeg: &Path, path: &Path) -> Result<Loudness, String> {
    const RATE: u32 = 48_000;
    let mut child = crate::ffmpeg::command(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-ac", "2", "-ar", &RATE.to_string(), "-f", "f32le", "-c:a", "pcm_f32le", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't run ffmpeg ({e})"))?;
    let mut stdout = child.stdout.take().ok_or("no output from ffmpeg")?;
    let mut meter = Meter::new(RATE)?;
    let mut bytes = vec![0u8; BLOCK_FRAMES * 8];
    let mut filled = 0;
    loop {
        let read = stdout.read(&mut bytes[filled..]).map_err(|e| e.to_string())?;
        filled += read;
        let whole = filled / 8 * 8;
        if read == 0 || whole == bytes.len() {
            let frames = bytes[..whole].as_chunks::<8>().0.iter().map(|&[a, b, c, d, e, f, g, h]| (f32::from_le_bytes([a, b, c, d]), f32::from_le_bytes([e, f, g, h])));
            meter.add(frames)?;
            bytes.copy_within(whole..filled, 0);
            filled -= whole;
        }
        if read == 0 {
            break;
        }
    }
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("ffmpeg couldn't decode it".into());
    }
    meter.finish()
}

/// An EBU R128 meter of stereo audio, and its peak.
struct Meter {
    meter: EbuR128,
    interleaved: Vec<f32>,
    frames: u64,
}

impl Meter {
    fn new(rate: u32) -> Result<Self, String> {
        let meter = EbuR128::new(2, rate, Mode::I | Mode::SAMPLE_PEAK).map_err(|e| e.to_string())?;
        Ok(Self { meter, interleaved: Vec::new(), frames: 0 })
    }

    fn add(&mut self, frames: impl Iterator<Item = (f32, f32)>) -> Result<(), String> {
        self.interleaved.clear();
        self.interleaved.extend(frames.flat_map(|(l, r)| [l, r]));
        self.frames += self.interleaved.len() as u64 / 2;
        self.meter.add_frames_f32(&self.interleaved).map_err(|e| e.to_string())
    }

    fn finish(self) -> Result<Loudness, String> {
        if self.frames == 0 {
            return Err("no audio data decoded".into());
        }
        let lufs = self.meter.loudness_global().map_err(|e| e.to_string())?;
        let peak = (0..2).filter_map(|ch| self.meter.sample_peak(ch).ok()).fold(0.0, f64::max);
        // (Silence is -inf LUFS: nothing to bring up.)
        if !lufs.is_finite() {
            return Err("it's silent".into());
        }
        Ok(Loudness { lufs: lufs as f32, peak: (peak > 0.0).then_some(peak as f32) })
    }
}

/// Measuring the songs here, one at a time, each its tags don't say it
/// for and no device has measured yet, into the song store (and so
/// synced): a background job (`background.rs`).
pub struct Measuring {
    done: Arc<dyn Fn() + Send + Sync>,
    measurer: Option<Measurer>,
    /// The song being measured, by key.
    measuring: Option<String>,
    /// The songs left to measure, as of a songs revision.
    left: Option<(u64, Vec<(String, PathBuf)>)>,
    /// The songs that couldn't be measured (this session).
    failed: HashSet<String>,
}

/// Whether `song` is still to be measured: its file's been read (by this
/// version, so its ReplayGain tags would be known), they say nothing, and
/// it's not MIDI (whose loudness is its soundfont's).
fn unmeasured(song: &Song) -> bool {
    song.loudness.is_none() && song.meta.as_ref().is_some_and(|m| m.version >= META_VERSION && m.replay_gain.is_none()) && !Facts::of(song).is_midi()
}

impl Measuring {
    /// `done` is called after each song's measured (to `tick` soon).
    pub fn new(done: impl Fn() + Send + Sync + 'static) -> Self {
        Self { done: Arc::new(done), measurer: None, measuring: None, left: None, failed: HashSet::new() }
    }

    /// Save what's been measured to `songs`, and ask for the next song
    /// once it's in. Whether any song was measured.
    pub fn tick(&mut self, songs: &mut SongStore) -> bool {
        let mut measured = false;
        while let Some((key, loudness)) = self.measurer.as_ref().and_then(Measurer::answer) {
            self.measuring = None;
            match loudness {
                Ok(loudness) => {
                    songs.set_loudness(&key, loudness);
                    measured = true;
                }
                Err(e) => {
                    log::info!("couldn't measure how loud {key} is: {e}");
                    self.failed.insert(key);
                }
            }
        }
        if self.measuring.is_some() {
            return measured;
        }
        let revision = songs.revision();
        if self.left.as_ref().is_none_or(|(seen, _)| *seen != revision) {
            let left = songs.songs().filter(|(_, song)| unmeasured(song)).filter_map(|(path, _)| Some((songs.key(&path)?, path))).collect();
            self.left = Some((revision, left));
        }
        let Some((_, left)) = self.left.as_mut() else { return measured };
        let Some((key, path)) = std::iter::from_fn(|| left.pop()).find(|(key, _)| !self.failed.contains(key)) else { return measured };
        if self.measurer.is_none() {
            let done = Arc::clone(&self.done);
            match Measurer::start(move || done()) {
                Ok(measurer) => self.measurer = Some(measurer),
                Err(e) => {
                    log::warn!("couldn't start measuring songs' loudness: {e}");
                    return measured;
                }
            }
        }
        if let Some(measurer) = &self.measurer {
            measurer.ask(key.clone(), path);
            self.measuring = Some(key);
        }
        measured
    }
}

/// Measures songs on a thread of its own, one at a time, as asked: by
/// key, with where the file is.
struct Measurer {
    requests: Sender<(String, PathBuf)>,
    results: Receiver<(String, Result<Loudness, String>)>,
}

impl Measurer {
    /// `done` is called after each song's measured (to look at the
    /// result soon).
    fn start(done: impl Fn() + Send + 'static) -> Result<Self, String> {
        let (requests, asked) = mpsc::channel::<(String, PathBuf)>();
        let (answer, results) = mpsc::channel();
        thread::Builder::new()
            .name("loudness".into())
            .spawn(move || {
                for (key, path) in asked {
                    if answer.send((key, measure(&path))).is_err() {
                        return;
                    }
                    done();
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { requests, results })
    }

    fn ask(&self, key: String, path: PathBuf) {
        let _ = self.requests.send((key, path));
    }

    /// A song measured since last asked, if one was.
    fn answer(&self) -> Option<(String, Result<Loudness, String>)> {
        self.results.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_replay_gain_tags() {
        let tagged = Loudness::from_replay_gain("-6.50 dB", Some("0.988")).unwrap();
        assert_eq!(tagged, Loudness { lufs: -11.5, peak: Some(0.988) });
        assert_eq!(Loudness::from_replay_gain("+2.1", None).unwrap().lufs, -20.1);
        assert_eq!(Loudness::from_replay_gain("loud", None), None);
    }

    #[test]
    fn gain_brings_songs_to_the_target_within_their_headroom() {
        let at = |lufs, peak| gain(Loudness { lufs, peak }, -14.0);
        assert!((at(-8.0, Some(1.0)) - 0.501).abs() < 0.001, "6 dB down");
        assert!((at(-20.0, Some(0.25)) - 1.995).abs() < 0.001, "6 dB up, with room to");
        assert!((at(-20.0, Some(0.8)) - 1.25).abs() < 0.001, "up only as far as its peak allows");
        assert_eq!(at(-40.0, None), MAX_GAIN, "never too far");
    }

    #[test]
    fn an_album_is_as_loud_as_its_songs_together() {
        let song = |lufs, peak| Loudness { lufs, peak: Some(peak) };
        let same = album([(song(-10.0, 0.5), 100.0), (song(-10.0, 0.9), 300.0)]).unwrap();
        assert!((same.lufs + 10.0).abs() < 1e-4);
        assert_eq!(same.peak, Some(0.9), "its loudest peak");
        // (Twice the energy for three quarters of it: about 2.4 dB louder.)
        let mixed = album([(song(-13.0, 0.5), 100.0), (song(-10.0, 0.5), 300.0)]).unwrap();
        assert!((mixed.lufs + 10.6).abs() < 0.1, "{mixed:?}");
        assert_eq!(album([(Loudness { lufs: -10.0, peak: None }, 1.0)]).unwrap().peak, None);
        assert_eq!(album([]), None);
    }

    #[test]
    fn measures_a_tone() {
        // A 1 kHz sine at full scale in one channel is -3 LUFS (the
        // standard's own check); in both, at half scale, -6.
        let dir = std::env::temp_dir().join(format!("synththing-loudness-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let rate = 44_100u32;
        let samples: Vec<i16> = (0..rate * 5)
            .flat_map(|i| {
                let s = ((i as f32 / rate as f32 * 1000.0 * std::f32::consts::TAU).sin() * 0.5 * f32::from(i16::MAX)) as i16;
                [s, s]
            })
            .collect();
        let mut wav = Vec::new();
        let data_len = (samples.len() * 2) as u32;
        wav.extend(b"RIFF");
        wav.extend((36 + data_len).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(rate.to_le_bytes());
        wav.extend((rate * 4).to_le_bytes());
        wav.extend(4u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data_len.to_le_bytes());
        wav.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        std::fs::write(&path, wav).unwrap();
        let measured = measure(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!((measured.lufs + 6.0).abs() < 0.1, "{measured:?}");
        assert!((measured.peak.unwrap() - 0.5).abs() < 0.01, "{measured:?}");
    }
}
