//! Recording the visualizer, with its sound, to a video file.
//!
//! The recorder is fed once per visualizer frame: the audio that played
//! since the last one (what the visualizer was given, so it's what was
//! heard, script notes included) and, when a new picture was drawn, the
//! frame buffer. It doesn't care where those come from or how fast, so the
//! same code records live in the app and renders offline (`run-script
//! --video`).
//!
//! * Video goes to ffmpeg as raw frames through a pipe, on a writer thread,
//!   encoded to H.264 in a temporary .mkv.
//! * Audio goes to a temporary WAV next to it.
//! * Timing follows the audio: after each feed, the video has exactly as
//!   many frames as the audio's length calls for (at the chosen frame
//!   rate), repeating the newest picture when the visualizer drew fewer, so
//!   picture and sound can't drift apart. While nothing is playing, silence
//!   is written so time still passes (an animated script keeps animating).
//! * If ffmpeg falls behind, frames are repeated instead of stalling the
//!   caller (live recording), or the caller waits (`wait_for_encoder`,
//!   offline rendering, where every frame must make it in).
//! * Finishing (on a background thread) closes ffmpeg, then muxes video and
//!   audio into the final .mp4 (AAC audio) and deletes the temporary files.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::ffmpeg;
use crate::visualizer::StereoFrame;

/// Recording sizes: the presets offered in Preferences, or any size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    #[serde(rename = "720p")]
    P720,
    #[default]
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "1440p")]
    P1440,
    #[serde(rename = "2160p")]
    P2160,
    /// Portrait, for phone-shaped videos (Shorts, Reels, TikTok).
    #[serde(rename = "720p-vertical")]
    V720,
    #[serde(rename = "1080p-vertical")]
    V1080,
    #[serde(rename = "1080-square")]
    Square1080,
    /// Any width and height (kept even, and within `CUSTOM_SIZES`).
    #[serde(rename = "custom")]
    Custom(u32, u32),
}

/// What a custom size's sides can be.
pub const CUSTOM_SIZES: std::ops::RangeInclusive<u32> = 64..=4096;

/// A custom size's side, made one the encoder takes: within `CUSTOM_SIZES`,
/// and even (4:2:0 color needs that).
fn custom_side(v: u32) -> u32 {
    v.clamp(*CUSTOM_SIZES.start(), *CUSTOM_SIZES.end()) & !1
}

impl Resolution {
    /// The presets, in the order they're offered.
    pub const ALL: [Resolution; 7] = [
        Resolution::P720,
        Resolution::P1080,
        Resolution::P1440,
        Resolution::P2160,
        Resolution::V720,
        Resolution::V1080,
        Resolution::Square1080,
    ];

    /// A custom size (see `custom_side`).
    pub fn custom(width: u32, height: u32) -> Self {
        Resolution::Custom(custom_side(width), custom_side(height))
    }

    pub fn size(self) -> (usize, usize) {
        match self {
            Resolution::P720 => (1280, 720),
            Resolution::P1080 => (1920, 1080),
            Resolution::P1440 => (2560, 1440),
            Resolution::P2160 => (3840, 2160),
            Resolution::V720 => (720, 1280),
            Resolution::V1080 => (1080, 1920),
            Resolution::Square1080 => (1080, 1080),
            // (a hand-edited config could hold anything)
            Resolution::Custom(w, h) => (custom_side(w) as usize, custom_side(h) as usize),
        }
    }

    pub fn label(self) -> String {
        match self {
            Resolution::P720 => "720p (1280 x 720)".into(),
            Resolution::P1080 => "1080p (1920 x 1080)".into(),
            Resolution::P1440 => "1440p (2560 x 1440)".into(),
            Resolution::P2160 => "4K (3840 x 2160)".into(),
            Resolution::V720 => "720p vertical (720 x 1280)".into(),
            Resolution::V1080 => "1080p vertical (1080 x 1920)".into(),
            Resolution::Square1080 => "Square (1080 x 1080)".into(),
            Resolution::Custom(..) => {
                let (w, h) = self.size();
                format!("Custom ({w} x {h})")
            }
        }
    }

    /// For `--video-size`: "720p", "1080p", "1440p", "2160p" or "4k",
    /// "vertical" (1080 x 1920), "720p-vertical", "square", or any
    /// "WIDTHxHEIGHT".
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.to_ascii_lowercase();
        match text.as_str() {
            "720p" | "720" => Some(Resolution::P720),
            "1080p" | "1080" => Some(Resolution::P1080),
            "1440p" | "1440" => Some(Resolution::P1440),
            "2160p" | "2160" | "4k" => Some(Resolution::P2160),
            "vertical" | "1080p-vertical" => Some(Resolution::V1080),
            "720p-vertical" => Some(Resolution::V720),
            "square" => Some(Resolution::Square1080),
            _ => {
                let (w, h) = text.split_once('x')?;
                let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
                (CUSTOM_SIZES.contains(&w) && CUSTOM_SIZES.contains(&h)).then(|| Resolution::custom(w, h))
            }
        }
    }
}

/// How the video is encoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    /// H.264, 4:2:0: plays everywhere (browsers, Discord, phones). Tuned for
    /// flat colors and hard edges, but color is stored at half resolution,
    /// so one-pixel colored details soften a little.
    #[default]
    #[serde(rename = "standard")]
    Standard,
    /// H.264, 4:4:4: full color resolution, so pixel edges stay exact. Plays
    /// in desktop players (VLC, mpv, Windows' own), not reliably in
    /// browsers or Discord's player.
    #[serde(rename = "sharp")]
    Sharp,
    /// Every pixel exactly (RGB H.264, lossless). Big files, for editing.
    #[serde(rename = "lossless")]
    Lossless,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Standard, Quality::Sharp, Quality::Lossless];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Standard => "Standard (plays everywhere)",
            Quality::Sharp => "Sharp pixels (4:4:4)",
            Quality::Lossless => "Lossless (huge files)",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "standard" => Some(Quality::Standard),
            "sharp" => Some(Quality::Sharp),
            "lossless" => Some(Quality::Lossless),
            _ => None,
        }
    }

    /// ffmpeg's encoder options for this quality.
    fn encoder_args(self) -> Vec<&'static str> {
        // RGB to YUV with the HD (BT.709) matrix, and tagged as such, so
        // players show the same colors the script drew; chroma averaged
        // over its area rather than smoothed.
        const TO_YUV420: &str = "scale=out_color_matrix=bt709:out_range=tv:flags=area+accurate_rnd+full_chroma_int,format=yuv420p";
        const TO_YUV444: &str = "scale=out_color_matrix=bt709:out_range=tv:flags=accurate_rnd+full_chroma_int,format=yuv444p";
        const TAGS: [&str; 6] = ["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];
        let mut args = match self {
            Quality::Standard => {
                vec!["-vf", TO_YUV420, "-c:v", "libx264", "-preset", "veryfast", "-tune", "animation", "-crf", "16"]
            }
            Quality::Sharp => vec![
                "-vf", TO_YUV444, "-c:v", "libx264", "-preset", "veryfast", "-tune", "animation", "-crf", "14",
                "-profile:v", "high444",
            ],
            Quality::Lossless => return vec!["-c:v", "libx264rgb", "-preset", "ultrafast", "-crf", "0"],
        };
        args.extend(TAGS);
        args
    }
}

pub const FRAME_RATES: [u32; 2] = [30, 60];
pub const DEFAULT_FPS: u32 = 60;

/// What a recording is: its size and frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub quality: Quality,
}

/// With nothing heard for this long, silence is written to keep time.
const IDLE_BEFORE_SILENCE: f64 = 0.1;
/// Longest step one feed counts: a gap longer than this (the visualizer
/// hidden, the app stalled) isn't recorded as time passing.
const MAX_STEP: f64 = 0.1;
/// Frames queued for the writer before the recorder starts repeating
/// instead.
const QUEUE_FRAMES: usize = 6;

enum VideoMsg {
    /// A new picture, shown for this many frames.
    Frame(Vec<u32>, u64),
    /// The previous picture for this many more frames (black if none yet).
    Repeat(u64),
}

pub struct Recorder {
    format: Format,
    sample_rate: u32,
    ffmpeg: PathBuf,
    frames: SyncSender<VideoMsg>,
    /// Buffers the writer is done with, to swap in for the next frame.
    returned: Receiver<Vec<u32>>,
    spares: Vec<Vec<u32>>,
    writer: JoinHandle<Result<()>>,
    audio: Wav,
    video_frames: u64,
    /// Frames owed to the writer that didn't fit in its queue.
    owed: u64,
    /// Video frames that repeated a picture rather than showing a new one.
    repeated: u64,
    /// Block until the encoder takes each frame, rather than repeating.
    wait_for_encoder: bool,
    idle: f64,
    paused: bool,
    temp_video: PathBuf,
    temp_audio: PathBuf,
    log_path: PathBuf,
    output: PathBuf,
}

impl Recorder {
    /// Start recording to `output` (an .mp4), with ffmpeg at `ffmpeg`.
    pub fn start(ffmpeg: &Path, format: Format, sample_rate: u32, output: PathBuf) -> Result<Self> {
        let stem = format!("synththing-recording-{}-{}", std::process::id(), unique_suffix());
        let temp = std::env::temp_dir();
        let temp_video = temp.join(format!("{stem}.mkv"));
        let temp_audio = temp.join(format!("{stem}.wav"));
        let log_path = temp.join(format!("{stem}.log"));

        let log = File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?;
        let mut child = ffmpeg::command(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "rawvideo", "-pix_fmt", "bgr0"])
            .args(["-video_size", &format!("{}x{}", format.width, format.height)])
            .args(["-framerate", &format.fps.to_string()])
            .args(["-i", "pipe:0"])
            .args(format.quality.encoder_args())
            .args(["-g", &(format.fps * 2).to_string()])
            .arg(&temp_video)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .with_context(|| format!("starting {}", ffmpeg.display()))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no pipe to ffmpeg"))?;

        let (frames, frames_rx) = mpsc::sync_channel(QUEUE_FRAMES);
        let (returned_tx, returned) = mpsc::channel();
        let pixels = format.width * format.height;
        let log_for_writer = log_path.clone();
        let writer = thread::Builder::new()
            .name("recording-writer".to_string())
            .spawn(move || write_video(child, stdin, frames_rx, returned_tx, pixels, &log_for_writer))
            .context("starting the recording thread")?;

        Ok(Self {
            format,
            sample_rate,
            ffmpeg: ffmpeg.to_path_buf(),
            frames,
            returned,
            spares: Vec::new(),
            writer,
            audio: Wav::create(&temp_audio, sample_rate)?,
            video_frames: 0,
            owed: 0,
            repeated: 0,
            wait_for_encoder: false,
            idle: 0.0,
            paused: false,
            temp_video,
            temp_audio,
            log_path,
            output,
        })
    }

    /// Wait for the encoder instead of repeating frames when it falls
    /// behind: for offline rendering, where nothing's live to keep up with.
    pub fn wait_for_encoder(mut self) -> Self {
        self.wait_for_encoder = true;
        self
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn output(&self) -> &Path {
        &self.output
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.idle = 0.0;
    }

    /// Video frames so far that repeated the previous picture (the
    /// visualizer didn't draw a new one in time).
    pub fn repeated_frames(&self) -> u64 {
        self.repeated
    }

    /// How many video frames a `feed(dt, samples)` with `incoming` samples
    /// would add: 0 means a new picture isn't needed yet. The app renders
    /// the visualizer only when this is above 0 and steps the script's time
    /// by that many frames, so every video frame gets its own picture,
    /// evenly spaced.
    pub fn frames_due(&self, dt: f64, incoming: usize) -> u64 {
        if self.paused {
            return 0;
        }
        let mut audio = self.audio.frames + incoming as u64;
        if incoming == 0 && self.idle + dt.clamp(0.0, MAX_STEP) > IDLE_BEFORE_SILENCE {
            audio += (dt.clamp(0.0, MAX_STEP) * self.sample_rate as f64).round() as u64;
        }
        (audio * u64::from(self.format.fps) / u64::from(self.sample_rate)).saturating_sub(self.video_frames)
    }

    /// Seconds recorded so far.
    pub fn seconds(&self) -> f64 {
        self.video_frames as f64 / self.format.fps as f64
    }

    /// One visualizer frame: `dt` seconds passed, `samples` played, and
    /// `pixels` is the new picture if one was drawn (`width * height`,
    /// `0x00RRGGBB`). A frame that's used is swapped out of `pixels` for a
    /// buffer of the same size with stale contents, so the caller must
    /// redraw it in full next time (visualizers do). Paused: nothing is
    /// recorded. An error means the encoder stopped; finish to clean up.
    pub fn feed(&mut self, dt: f64, samples: &[StereoFrame], pixels: Option<&mut Vec<u32>>) -> Result<()> {
        if self.paused {
            return Ok(());
        }
        if samples.is_empty() {
            self.idle += dt.clamp(0.0, MAX_STEP);
            if self.idle > IDLE_BEFORE_SILENCE {
                let frames = (dt.clamp(0.0, MAX_STEP) * self.sample_rate as f64).round() as u64;
                self.audio.silence(frames)?;
            }
        } else {
            self.idle = 0.0;
            self.audio.write(samples)?;
        }

        let due_total = self.audio.frames * u64::from(self.format.fps) / u64::from(self.sample_rate);
        let due = due_total.saturating_sub(self.video_frames);
        if due == 0 {
            return Ok(());
        }
        self.video_frames = due_total;
        self.repeated += if pixels.is_some() { due - 1 } else { due };

        while let Ok(spare) = self.returned.try_recv() {
            self.spares.push(spare);
        }
        let size = self.format.width * self.format.height;
        match pixels {
            Some(pixels) if pixels.len() == size => {
                let mut frame = self.spares.pop().unwrap_or_else(|| vec![0; size]);
                frame.resize(size, 0);
                std::mem::swap(pixels, &mut frame);
                if self.wait_for_encoder {
                    return self.frames.send(VideoMsg::Frame(frame, due)).map_err(|_| self.writer_gone());
                }
                self.send_owed()?;
                if self.owed > 0 {
                    // Still behind: this picture is skipped, the last one
                    // the writer has stands in.
                    self.spares.push(frame);
                    self.owed += due;
                } else {
                    match self.frames.try_send(VideoMsg::Frame(frame, due)) {
                        Ok(()) => {}
                        Err(TrySendError::Full(VideoMsg::Frame(frame, _))) => {
                            self.spares.push(frame);
                            self.owed += due;
                        }
                        Err(_) => return Err(self.writer_gone()),
                    }
                }
            }
            _ if self.wait_for_encoder => {
                return self.frames.send(VideoMsg::Repeat(due)).map_err(|_| self.writer_gone());
            }
            _ => {
                self.owed += due;
                self.send_owed()?;
            }
        }
        Ok(())
    }

    fn send_owed(&mut self) -> Result<()> {
        if self.owed == 0 {
            return Ok(());
        }
        match self.frames.try_send(VideoMsg::Repeat(self.owed)) {
            Ok(()) => self.owed = 0,
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => return Err(self.writer_gone()),
        }
        Ok(())
    }

    fn writer_gone(&self) -> anyhow::Error {
        anyhow!("the encoder stopped: {}", log_tail(&self.log_path))
    }

    /// Stop and write the file, on a background thread. Poll the result.
    pub fn finish(self) -> Finishing {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let output = self.output.clone();
            let result = self.finish_now().map(|()| output);
            let _ = tx.send(result);
        });
        Finishing { result: rx }
    }

    /// Stop and write the file, here and now.
    pub fn finish_now(self) -> Result<()> {
        let Recorder { frames, owed, writer, audio, ffmpeg, temp_video, temp_audio, log_path, output, .. } = self;
        let result = (|| {
            if owed > 0 {
                let _ = frames.send(VideoMsg::Repeat(owed));
            }
            drop(frames);
            writer.join().map_err(|_| anyhow!("the recording thread crashed"))??;
            let audio_frames = audio.finish()?;
            mux(&ffmpeg, &temp_video, (audio_frames > 0).then_some(temp_audio.as_path()), &output, &log_path)
        })();
        for temp in [&temp_video, &temp_audio, &log_path] {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
}

/// A recording being written out; `poll` until it says how it went.
pub struct Finishing {
    result: Receiver<Result<PathBuf>>,
}

impl Finishing {
    /// Wait for it to finish.
    pub fn wait(self) -> Result<PathBuf> {
        self.result.recv().unwrap_or_else(|_| Err(anyhow!("the recording was lost")))
    }

    pub fn poll(&self) -> Option<Result<PathBuf>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(anyhow!("the recording was lost"))),
        }
    }
}

/// The writer thread: frames from the channel into ffmpeg's stdin.
fn write_video(
    mut child: std::process::Child,
    stdin: std::process::ChildStdin,
    frames: Receiver<VideoMsg>,
    returned: mpsc::Sender<Vec<u32>>,
    pixels: usize,
    log_path: &Path,
) -> Result<()> {
    let mut stdin = BufWriter::with_capacity(1 << 20, stdin);
    let mut last: Vec<u8> = Vec::new();
    let mut failed = None;
    for msg in frames {
        if failed.is_some() {
            continue; // drain, so the sender never blocks forever
        }
        let repeats = match msg {
            VideoMsg::Frame(frame, repeats) => {
                last.clear();
                last.extend(frame.iter().flat_map(|p| p.to_le_bytes())); // B, G, R, 0: "bgr0"
                let _ = returned.send(frame);
                repeats
            }
            VideoMsg::Repeat(repeats) => {
                if last.is_empty() {
                    last = vec![0; pixels * 4];
                }
                repeats
            }
        };
        for _ in 0..repeats {
            if let Err(e) = stdin.write_all(&last) {
                failed = Some(e);
                break;
            }
        }
    }
    let flushed = stdin.flush();
    drop(stdin);
    let status = child.wait().context("waiting for ffmpeg")?;
    if failed.is_some() || flushed.is_err() || !status.success() {
        bail!("ffmpeg failed: {}", log_tail(log_path));
    }
    Ok(())
}

/// The video (and the audio, when there is any) into `output`.
fn mux(ffmpeg: &Path, video: &Path, audio: Option<&Path>, output: &Path, log_path: &Path) -> Result<()> {
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let log = File::create(log_path)?;
    let mut command = ffmpeg::command(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(video);
    if let Some(audio) = audio {
        command.arg("-i").arg(audio);
        command.args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "aac", "-b:a", "192k"]);
    }
    command.args(["-c:v", "copy", "-movflags", "+faststart"]).arg(output);
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .status()
        .with_context(|| format!("starting {}", ffmpeg.display()))?;
    if !status.success() {
        bail!("ffmpeg couldn't write the video: {}", log_tail(log_path));
    }
    Ok(())
}

/// The last few lines ffmpeg logged, for an error message.
fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(3)..].join(" / ");
    if tail.is_empty() { "no details".to_string() } else { tail }
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

/// 32-bit float stereo WAV, written as it goes; sizes filled in at the end.
struct Wav {
    file: BufWriter<File>,
    frames: u64,
    sample_rate: u32,
}

impl Wav {
    fn create(path: &Path, sample_rate: u32) -> Result<Self> {
        let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut wav = Self { file: BufWriter::new(file), frames: 0, sample_rate };
        wav.header(0)?;
        Ok(wav)
    }

    fn header(&mut self, data_bytes: u32) -> Result<()> {
        let rate = self.sample_rate;
        let f = &mut self.file;
        f.write_all(b"RIFF")?;
        f.write_all(&data_bytes.saturating_add(36).to_le_bytes())?;
        f.write_all(b"WAVEfmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&3u16.to_le_bytes())?; // IEEE float
        f.write_all(&2u16.to_le_bytes())?; // stereo
        f.write_all(&rate.to_le_bytes())?;
        f.write_all(&(rate * 8).to_le_bytes())?; // bytes per second
        f.write_all(&8u16.to_le_bytes())?; // bytes per frame
        f.write_all(&32u16.to_le_bytes())?; // bits per sample
        f.write_all(b"data")?;
        f.write_all(&data_bytes.to_le_bytes())?;
        Ok(())
    }

    fn write(&mut self, samples: &[StereoFrame]) -> Result<()> {
        for &(l, r) in samples {
            self.file.write_all(&l.to_le_bytes())?;
            self.file.write_all(&r.to_le_bytes())?;
        }
        self.frames += samples.len() as u64;
        Ok(())
    }

    fn silence(&mut self, frames: u64) -> Result<()> {
        for _ in 0..frames {
            self.file.write_all(&[0; 8])?;
        }
        self.frames += frames;
        Ok(())
    }

    /// Fill in the sizes and close; returns the frames written.
    fn finish(mut self) -> Result<u64> {
        // Past 4 GB (about three and a half hours) the sizes can't be
        // stored; the maximum makes ffmpeg read to the end anyway.
        let bytes = u32::try_from(self.frames * 8).unwrap_or(u32::MAX - 36);
        self.file.seek(SeekFrom::Start(0))?;
        self.header(bytes)?;
        self.file.flush()?;
        Ok(self.frames)
    }
}

/// A file name for a recording made now: `synththing 2026-10-02 14.05.33.mp4`
/// in `dir`, with a number added if that's taken.
pub fn output_path(dir: &Path) -> PathBuf {
    let stamp = local_timestamp();
    let mut path = dir.join(format!("synththing {stamp}.mp4"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("synththing {stamp} ({n}).mp4"));
        n += 1;
    }
    path
}

#[cfg(windows)]
fn local_timestamp() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime only writes the SYSTEMTIME it's given.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!("{:04}-{:02}-{:02} {:02}.{:02}.{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// UTC elsewhere (no local time zone lookup without another dependency).
#[cfg(not(windows))]
fn local_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}.{:02}.{:02}", rest / 3600, rest % 3600 / 60, rest % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs ffmpeg (downloaded by the app, or on the PATH).
    #[test]
    #[ignore = "needs ffmpeg"]
    fn records_picture_and_sound_in_step() {
        let ffmpeg = ffmpeg::find().expect("no ffmpeg");
        let dir = std::env::temp_dir().join(format!("synththing-rec-test-{}", std::process::id()));
        let output = dir.join("test.mp4");
        let format = Format { width: 64, height: 36, fps: 30, quality: Quality::Standard };
        let mut recorder = Recorder::start(&ffmpeg, format, 6000, output.clone()).unwrap();
        // Two seconds: a frame drawn every 1/60 s with 1/60 s of audio.
        let mut pixels = vec![0x00FF_0000u32; 64 * 36];
        let audio = vec![(0.25f32, -0.25f32); 6000 / 60];
        for i in 0..120 {
            pixels.fill(if i < 60 { 0x00FF_0000 } else { 0x0000_FF00 });
            recorder.feed(1.0 / 60.0, &audio, Some(&mut pixels)).unwrap();
        }
        let seconds = recorder.seconds();
        assert!((seconds - 2.0).abs() < 0.05, "{seconds}");
        recorder.finish_now().unwrap();
        assert!(std::fs::metadata(&output).unwrap().len() > 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolutions_parse_and_size() {
        assert_eq!(Resolution::parse("4K").map(Resolution::size), Some((3840, 2160)));
        assert_eq!(Resolution::parse("1080p"), Some(Resolution::P1080));
        assert_eq!(Resolution::parse("999p"), None);
        assert_eq!(Resolution::parse("vertical").map(Resolution::size), Some((1080, 1920)));
        assert_eq!(Resolution::parse("1081x1921").map(Resolution::size), Some((1080, 1920)), "kept even");
        assert_eq!(Resolution::parse("10x10"), None, "too small");
        assert_eq!(Resolution::custom(9000, 3).size(), (4096, 64));
        // Saved as before, plus the new ones.
        assert_eq!(serde_json::to_string(&Resolution::P1080).unwrap(), "\"1080p\"");
        let custom: Resolution = serde_json::from_str(&serde_json::to_string(&Resolution::custom(800, 600)).unwrap()).unwrap();
        assert_eq!(custom.size(), (800, 600));
    }
}
