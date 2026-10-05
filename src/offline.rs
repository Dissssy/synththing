//! Running a visualizer script without the app's clock: the song's audio is
//! made one video frame at a time and handed to the script, so every frame
//! gets its own picture and the sound lines up with it exactly, however
//! long each frame takes. [`Stage`] does one frame; `run-script`
//! (`headless.rs`) drives it from the command line, and [`Render`] drives
//! it on a thread of its own for the app's background renders (Record
//! window, Render), writing a video of a song or a whole playlist while the
//! app carries on.
//!
//! A render follows the same takes as a live recording (see
//! `app/recording.rs`): a script with `record_prepare` gets ready before
//! each song (those frames aren't in the video), starts the take with
//! `recording_ready()` and ends it with `recording_done()`, or `TAKE_TAIL`
//! after its song; any other script's take is its song, start to end.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::nice_name;
use crate::engine::{Engine, EngineView};
use crate::loader::{self, SoundFontProbe};
use crate::lua_visualizer::{LuaVisualizer, PlaybackRequest, RecordKind, RecordPhase, RecordingTake};
use crate::recorder::{Format, Recorder};
use crate::visualizer::{DisplayMode, StereoFrame, Visualizer, VisualizerInput};

pub const SAMPLE_RATE: u32 = 44_100;

/// How long a prepared take goes on after its song ends, if the script
/// doesn't call `recording_done()` first.
pub const TAKE_TAIL: Duration = Duration::from_secs(10);

/// How long a script may prepare before recording starts anyway, in
/// seconds of frames.
pub const PREPARE_LIMIT_SECONDS: f64 = 120.0;

/// A take ends past its song's length plus this much, whatever the script
/// does (a game left paused, say), so a render always finishes.
const OVERRUN_LIMIT_SECONDS: f64 = 120.0;

/// The engine and the script, driven one video frame at a time.
pub struct Stage {
    pub engine: Engine,
    pub visualizer: LuaVisualizer,
    pub width: usize,
    pub height: usize,
    /// The last frame drawn (`width * height`, `0x00RRGGBB`).
    pub pixels: Vec<u32>,
    audio: Vec<f32>,
    live: Vec<f32>,
    input: VisualizerInput,
}

impl Stage {
    pub fn new(source: String, script: Option<PathBuf>, width: usize, height: usize, fps: f64) -> Self {
        let mut visualizer = LuaVisualizer::new(source, script, SAMPLE_RATE);
        visualizer.set_fixed_timestep(Some(1.0 / fps));
        let samples_per_frame = (f64::from(SAMPLE_RATE) / fps).round().max(1.0) as usize;
        Self {
            engine: Engine::new(SAMPLE_RATE),
            visualizer,
            width,
            height,
            pixels: vec![0; width * height],
            audio: vec![0.0; samples_per_frame * 2],
            live: vec![0.0; samples_per_frame * 2],
            input: VisualizerInput { mode: DisplayMode::Window, ..Default::default() },
        }
    }

    /// One frame: `1/fps` seconds of the song (and of notes the script
    /// plays, mixed in as the app's output does), drawn by the script into
    /// `pixels`. Returns that audio and the playback state it was drawn at.
    pub fn frame(&mut self) -> (Vec<StereoFrame>, EngineView) {
        self.engine.render_into(&mut self.audio);
        if self.engine.live_busy() {
            self.engine.render_live(&mut self.live);
            for (sample, extra) in self.audio.iter_mut().zip(&self.live) {
                *sample += extra;
            }
        }
        // chunks_exact(2) rather than as_chunks::<2>(), like the rest of the code.
        #[allow(clippy::chunks_exact_to_as_chunks)]
        let samples: Vec<StereoFrame> = self.audio.chunks_exact(2).map(|s| (s[0], s[1])).collect();
        let view = self.engine.view();
        let mut notes = self.engine.notes_snapshot();
        notes.active.extend(self.engine.live_notes());
        self.visualizer.render(&mut self.pixels, self.width, self.height, &samples, &notes, &view, &self.input);
        (samples, view)
    }

    /// After a frame: the channel toggles and notes the script asked for
    /// are carried out, and its playback requests handed back.
    pub fn requests(&mut self) -> Vec<PlaybackRequest> {
        for (channel, enabled) in self.visualizer.take_channel_requests() {
            self.engine.set_channel_enabled(channel, enabled);
        }
        for command in self.visualizer.take_live_commands() {
            self.engine.live_command(command);
        }
        self.visualizer.take_playback_requests()
    }

    /// Carry out a pause, seek or speed change; false for anything else
    /// (there's no playlist to move through here, and recordings are the
    /// caller's).
    pub fn transport(&mut self, request: PlaybackRequest) -> bool {
        match request {
            PlaybackRequest::Pause(pause) => {
                if pause != self.engine.view().paused {
                    self.engine.toggle_pause();
                }
            }
            PlaybackRequest::Seek(seconds) => self.engine.seek(seconds),
            PlaybackRequest::Speed(speed) => self.engine.set_speed(speed),
            _ => return false,
        }
        true
    }

    /// Load `song` (with `soundfont`, for MIDI; else the first of
    /// `fallback_soundfonts` that exists), starting `paused` or not.
    pub fn load_song(
        &mut self,
        song: &Path,
        soundfont: Option<&Path>,
        fallback_soundfonts: &[PathBuf],
        paused: bool,
    ) -> Result<(), String> {
        let name = nice_name(song);
        let is_midi = song
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"));
        if !is_midi {
            let audio =
                loader::load_audio_file(song).map_err(|e| format!("couldn't read {}: {e}", song.display()))?;
            self.visualizer.set_song(Some(song), Some(audio.song_id.clone()));
            self.engine.load_audio_file(audio, name, paused);
            return Ok(());
        }
        let soundfont = soundfont
            .map(Path::to_path_buf)
            .or_else(|| fallback_soundfonts.iter().find(|p| p.exists()).cloned())
            .ok_or("a MIDI song needs a soundfont: pass --soundfont, or add one in the app")?;
        match loader::probe_soundfont(&soundfont) {
            Ok(SoundFontProbe::Playable(sf)) => self
                .engine
                .set_soundfont(sf, nice_name(&soundfont))
                .map_err(|e| format!("couldn't use {}: {e}", soundfont.display()))?,
            Ok(SoundFontProbe::Unplayable(reason)) => {
                return Err(format!("can't use {}: {reason}", soundfont.display()));
            }
            Ok(SoundFontProbe::NotSoundFont) => return Err(format!("{} isn't a soundfont", soundfont.display())),
            Err(e) => return Err(format!("couldn't read {}: {e}", soundfont.display())),
        }
        let midi = loader::load_midi(song).map_err(|e| format!("couldn't read {}: {e}", song.display()))?;
        self.engine.load_midi(midi.file, name, paused);
        self.visualizer.set_note_list(midi.notes);
        self.visualizer.set_song(Some(song), Some(midi.song_id));
        Ok(())
    }
}

/// One song of a render, and the soundfont it plays with.
#[derive(Clone, Debug)]
pub struct RenderSong {
    pub path: PathBuf,
    pub soundfont: Option<PathBuf>,
}

/// What to render.
#[derive(Clone, Debug)]
pub struct RenderJob {
    pub script: PathBuf,
    pub songs: Vec<RenderSong>,
    /// `Song` or `Playlist` (what `recording()` says).
    pub kind: RecordKind,
    /// The script plays itself, if it can (`record_auto`).
    pub auto: bool,
    pub format: Format,
    pub output: PathBuf,
    /// For a MIDI song with no soundfont of its own.
    pub fallback_soundfonts: Vec<PathBuf>,
}

/// How a render is going, for the app to show.
#[derive(Clone, Debug, Default)]
pub struct RenderStatus {
    /// The song being rendered (0-based) of how many, and its name.
    pub song: usize,
    pub songs: usize,
    pub name: String,
    pub position: f64,
    pub length: f64,
    /// The script is getting ready (nothing's being recorded).
    pub preparing: bool,
    /// Seconds of video so far.
    pub seconds: f64,
    /// The video is being finished (encoded and muxed).
    pub finishing: bool,
    /// A small copy of a recent frame: width, height, `0x00RRGGBB`, and a
    /// count that goes up when it changes.
    pub preview: Option<(usize, usize, Arc<Vec<u32>>)>,
    pub preview_revision: u64,
}

/// A render running on its own thread.
pub struct Render {
    status: Arc<Mutex<RenderStatus>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<PathBuf, String>>>,
    started: Instant,
}

/// The preview's longest side, in pixels.
const PREVIEW_SIZE: usize = 360;
/// How often the status (and preview) is updated.
const STATUS_EVERY: Duration = Duration::from_millis(250);

impl Render {
    /// Start rendering `job`, encoding with `ffmpeg`.
    pub fn start(job: RenderJob, ffmpeg: PathBuf) -> Self {
        let status = Arc::new(Mutex::new(RenderStatus { songs: job.songs.len(), ..Default::default() }));
        let stop: Arc<AtomicBool> = Arc::default();
        let thread = {
            let status = Arc::clone(&status);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("render".into())
                .stack_size(16 << 20)
                .spawn(move || render(job, &ffmpeg, &status, &stop))
                .ok()
        };
        Self { status, stop, thread, started: Instant::now() }
    }

    pub fn status(&self) -> RenderStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// When it started (for how fast it's going).
    pub fn started(&self) -> Instant {
        self.started
    }

    /// Stop early: what's rendered so far is kept as the video.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The result, once it's done: the video, or what went wrong.
    pub fn poll(&mut self) -> Option<Result<PathBuf, String>> {
        if !self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return self.thread.is_none().then(|| Err("the render couldn't start".to_string()));
        }
        let thread = self.thread.take()?;
        Some(thread.join().unwrap_or_else(|_| Err("the render hit a bug (see Help > Log)".to_string())))
    }

    /// Stop and wait for it (closing the app).
    pub fn stop_and_wait(mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The render thread: every song, each a take, into one video.
fn render(job: RenderJob, ffmpeg: &Path, status: &Mutex<RenderStatus>, stop: &AtomicBool) -> Result<PathBuf, String> {
    let source =
        std::fs::read_to_string(&job.script).map_err(|e| format!("couldn't read {}: {e}", job.script.display()))?;
    let format = job.format;
    let fps = f64::from(format.fps);
    let mut stage = Stage::new(source, Some(job.script.clone()), format.width, format.height, fps);
    // It reads its saved data, but a render never writes it.
    stage.visualizer.detach_store();
    if let Some(error) = stage.visualizer.error() {
        return Err(format!("the script has an error: {error}"));
    }
    let options = stage.visualizer.options();
    let prepared = options.record_prepare;
    let auto = job.auto && options.record_auto;
    let mut recorder = Recorder::start(ffmpeg, format, SAMPLE_RATE, job.output.clone())
        .map_err(|e| format!("{e:#}"))?
        .wait_for_encoder();
    let dt = 1.0 / fps;
    let mut last_status: Option<Instant> = None;

    'songs: for (index, song) in job.songs.iter().enumerate() {
        // Prepared: it waits for the script; otherwise it plays from the top.
        stage.load_song(&song.path, song.soundfont.as_deref(), &job.fallback_soundfonts, prepared)?;
        let mut take = RecordingTake {
            phase: if prepared { RecordPhase::Preparing } else { RecordPhase::Recording },
            kind: job.kind,
            auto,
        };
        stage.visualizer.set_recording(take.phase == RecordPhase::Recording, Some(take));
        let length = stage.engine.view().length;
        let mut prepared_frames = 0u32;
        let mut recorded = 0.0;
        let mut finished_for = 0.0;
        loop {
            if stop.load(Ordering::Relaxed) {
                break 'songs;
            }
            let preparing = take.phase == RecordPhase::Preparing;
            if preparing {
                prepared_frames += 1;
                if f64::from(prepared_frames) > PREPARE_LIMIT_SECONDS * fps {
                    take.phase = RecordPhase::Recording;
                    stage.visualizer.set_recording(true, Some(take));
                }
            }
            let preparing = take.phase == RecordPhase::Preparing;
            let (samples, view) = stage.frame();
            if !preparing {
                recorder.feed(dt, &samples, Some(&mut stage.pixels)).map_err(|e| format!("{e:#}"))?;
                recorded += dt;
            }

            let mut take_over = false;
            for request in stage.requests() {
                match request {
                    PlaybackRequest::RecordingReady if take.phase == RecordPhase::Preparing => {
                        take.phase = RecordPhase::Recording;
                        stage.visualizer.set_recording(true, Some(take));
                    }
                    PlaybackRequest::RecordingDone if prepared && take.phase == RecordPhase::Recording => {
                        take_over = true;
                    }
                    other => {
                        stage.transport(other);
                    }
                }
            }
            if view.finished {
                finished_for += dt;
            } else {
                finished_for = 0.0;
            }
            let over = if prepared {
                take_over || finished_for >= TAKE_TAIL.as_secs_f64()
            } else {
                view.finished
            };

            if last_status.is_none_or(|t| t.elapsed() >= STATUS_EVERY) || over {
                last_status = Some(Instant::now());
                let preview = preview(&stage.pixels, format.width, format.height);
                if let Ok(mut s) = status.lock() {
                    s.song = index;
                    s.name = nice_name(&song.path);
                    s.position = view.position;
                    s.length = length;
                    s.preparing = preparing;
                    s.seconds = recorder.seconds();
                    s.preview = Some(preview);
                    s.preview_revision += 1;
                }
            }
            if over || recorded > length + OVERRUN_LIMIT_SECONDS {
                break;
            }
        }
    }

    if let Ok(mut s) = status.lock() {
        s.finishing = true;
    }
    if recorder.seconds() <= 0.0 {
        let _ = recorder.finish_now();
        let _ = std::fs::remove_file(&job.output);
        return Err("stopped before anything was recorded".to_string());
    }
    recorder.finish_now().map_err(|e| format!("{e:#}"))?;
    Ok(job.output)
}

/// A small copy of a frame, its longest side `PREVIEW_SIZE` (nearest
/// neighbour).
fn preview(pixels: &[u32], width: usize, height: usize) -> (usize, usize, Arc<Vec<u32>>) {
    let scale = (width.max(height) as f64 / PREVIEW_SIZE as f64).max(1.0);
    let (w, h) = (((width as f64 / scale) as usize).max(1), ((height as f64 / scale) as usize).max(1));
    let mut small = Vec::with_capacity(w * h);
    for y in 0..h {
        let sy = ((y as f64 * scale) as usize).min(height - 1);
        for x in 0..w {
            let sx = ((x as f64 * scale) as usize).min(width - 1);
            small.push(pixels[sy * width + sx]);
        }
    }
    (w, h, Arc::new(small))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::Quality;

    /// A whole song rendered in the background, by a script that takes
    /// part (it prepares for a few frames first, then ends the take itself
    /// a second in), and stopping one early. Needs ffmpeg.
    #[test]
    #[ignore = "needs ffmpeg"]
    fn renders_a_song_in_the_background() {
        let ffmpeg = crate::ffmpeg::find().expect("no ffmpeg");
        let dir = std::env::temp_dir().join(format!("synththing-render-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("render_test.lua");
        std::fs::write(
            &script,
            "script_options({ record_prepare = true })
            local recorded = 0
            function render()
                clear({ r = 200, g = 0, b = 0 })
                local r = recording()
                if r.phase == 'preparing' and FRAME > 5 then
                    recording_ready()
                    set_paused(false)
                    recorded = 0
                end
                if r.phase == 'recording' then
                    recorded = recorded + 1
                    if recorded == 30 then recording_done() end
                end
            end",
        )
        .unwrap();
        let starter = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/starter");
        let song = RenderSong {
            path: starter.join("songs/Canon in D.mid"),
            soundfont: Some(starter.join("soundfonts/TimGM6mb.sf2")),
        };
        let job = RenderJob {
            script,
            songs: vec![song.clone(), song],
            kind: RecordKind::Playlist,
            auto: true,
            format: Format { width: 64, height: 36, fps: 30, quality: Quality::Standard },
            output: dir.join("render.mp4"),
            fallback_soundfonts: Vec::new(),
        };
        let mut render = Render::start(job.clone(), ffmpeg.clone());
        let result = loop {
            if let Some(result) = render.poll() {
                break result;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let output = result.unwrap();
        // Two takes of 30 frames each: two seconds.
        let status = render.status();
        assert!((status.seconds - 2.0).abs() < 0.05, "{}", status.seconds);
        assert!(std::fs::metadata(&output).unwrap().len() > 1000);

        // Stopped straight away: nothing recorded, no file.
        let mut stopped = Render::start(RenderJob { output: dir.join("stopped.mp4"), ..job }, ffmpeg);
        stopped.stop();
        let result = loop {
            if let Some(result) = stopped.poll() {
                break result;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(result.is_err() && !dir.join("stopped.mp4").exists(), "{result:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
