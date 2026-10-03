//! The visualizer script runs on a thread of its own, so a slow or stuck
//! script never freezes the window. The app sends it messages (a new
//! source, a setting, a frame to draw) through [`ScriptHost`], and reads
//! back finished frames and a [`ScriptStatus`] snapshot (error, settings,
//! controls, log, ...) the script thread publishes after each one. While a
//! frame takes long, the app keeps showing the last one, says the script
//! is busy, and can stop it ([`ScriptHost::stop`]).
//!
//! A Lua state can't move between threads, so the `LuaVisualizer` is
//! created on the script thread and stays there. Headless runs
//! (`headless.rs`) don't need any of this and use it directly.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::engine::EngineView;
use crate::live::LiveCommand;
use crate::lua_visualizer::{
    self, Action, Binding, DebugSnapshot, HALF_RATE_INTERVAL, LogEntry, LuaVisualizer, PerfSummary,
    PlaybackRequest, ScriptOptions, SettingDescriptor, SettingValue, Transport,
};
use crate::midi_notes::NoteList;
use crate::visualizer::{CursorRequest, NotesSnapshot, StereoFrame, Visualizer, VisualizerInput};

/// How long the watchdog lets a script's code run in the app: longer than
/// in headless runs, as the app stays responsive meanwhile and the busy
/// notice offers to stop it sooner.
pub const APP_WATCHDOG_LIMIT: Duration = Duration::from_secs(10);

/// How long a script can be busy before the app says so. A script busy
/// this long is also stopped first by anything that replaces it (another
/// script, an edit, a restart).
pub const BUSY_NOTICE_AFTER: Duration = Duration::from_secs(1);

/// One frame for the script to draw.
pub struct FrameRequest {
    pub width: usize,
    pub height: usize,
    /// A buffer to draw into (an old frame's), to save allocating one.
    pub pixels: Vec<u32>,
    pub samples: Vec<StereoFrame>,
    pub notes: NotesSnapshot,
    pub playback: EngineView,
    pub input: VisualizerInput,
    pub transport: Transport,
    /// Seconds per frame for DT and TIME instead of the wall clock
    /// (recording).
    pub timestep: Option<f64>,
}

/// A frame the script drew.
pub struct FrameDone {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    /// The audio it was given, handed back (for recording).
    pub samples: Vec<StereoFrame>,
    pub effects: Effects,
}

/// What the script asked the app to do while drawing a frame.
#[derive(Default)]
pub struct Effects {
    pub channel_requests: Vec<(u8, bool)>,
    pub live_commands: Vec<LiveCommand>,
    pub playback_requests: Vec<PlaybackRequest>,
}

/// What the app shows of the running script, as of the last thing it did.
#[derive(Clone, Default)]
pub struct ScriptStatus {
    pub running: bool,
    pub error: Option<String>,
    pub settings: Vec<SettingDescriptor>,
    pub actions: Vec<Action>,
    pub perf: PerfSummary,
    pub debug: Option<DebugSnapshot>,
    pub log: Vec<LogEntry>,
    pub options: ScriptOptions,
    pub cursor: CursorRequest,
}

enum Message {
    Load { path: Option<PathBuf>, source: String },
    SetSource { source: String, save_error: Option<String> },
    Restart,
    Rename { new_name: String, reply: Sender<Result<PathBuf, String>> },
    SetSong { path: PathBuf, id: String, notes: Arc<NoteList> },
    SetAppLog(bool),
    SetSetting { key: String, value: SettingValue },
    SetActionBindings { index: usize, bindings: Vec<Binding> },
    ClearLog,
    RetryFullFrameRate,
    Frame(Box<FrameRequest>),
}

/// Between the threads: the status, and how long the script's been busy.
#[derive(Default)]
struct Shared {
    status: ScriptStatus,
    /// When the script thread started what it's doing now (drawing a
    /// frame, loading a script); `None` while it waits for the next thing.
    busy_since: Option<Instant>,
}

/// The app's side: owns the script thread.
pub struct ScriptHost {
    messages: Option<Sender<Message>>,
    frames: Receiver<FrameDone>,
    shared: Arc<Mutex<Shared>>,
    /// Stops the script's code running now (`LuaVisualizer::stop_flag`).
    stop: Arc<AtomicBool>,
    /// Set on the way out: the script thread skips whatever's still queued.
    quit: Arc<AtomicBool>,
    /// For the script thread to wake the app up when a frame is done.
    repaint: Arc<OnceLock<egui::Context>>,
    thread: Option<JoinHandle<()>>,
    /// The running script's file. The app is the one that sets it, so its
    /// copy is always current.
    path: Option<PathBuf>,
    /// A frame was asked for and hasn't come back yet.
    in_flight: bool,
    last_request: Option<Instant>,
    /// The "copy script logs to the app log" preference, as last sent.
    app_log: bool,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ScriptHost {
    /// Start the script thread, running `source` (from `path`).
    pub fn spawn(source: String, path: Option<PathBuf>, sample_rate: u32) -> Self {
        Self::spawn_with_watchdog(source, path, sample_rate, APP_WATCHDOG_LIMIT)
    }

    fn spawn_with_watchdog(source: String, path: Option<PathBuf>, sample_rate: u32, limit: Duration) -> Self {
        let (message_tx, message_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let quit: Arc<AtomicBool> = Arc::default();
        let repaint: Arc<OnceLock<egui::Context>> = Arc::default();
        let worker = Worker {
            messages: message_rx,
            frames: frame_tx,
            shared: Arc::clone(&shared),
            quit: Arc::clone(&quit),
            repaint: Arc::clone(&repaint),
            log_seen: None,
        };
        let first_path = path.clone();
        // A big stack: deeply recursive script code (and the Rust it calls
        // into) runs here now, not on the main thread.
        let thread = std::thread::Builder::new()
            .name("script".into())
            .stack_size(16 << 20)
            .spawn(move || {
                lock(&worker.shared).busy_since = Some(Instant::now());
                let visualizer = LuaVisualizer::with_watchdog(source, first_path, sample_rate, limit);
                let _ = stop_tx.send(visualizer.stop_flag());
                worker.run(visualizer);
            })
            .expect("couldn't start the script thread");
        // The stop flag is the visualizer's own, handed over as soon as
        // it's made (before its script's top level runs).
        let stop = stop_rx.recv().unwrap_or_default();
        Self {
            messages: Some(message_tx),
            frames: frame_rx,
            shared,
            stop,
            quit,
            repaint,
            thread: Some(thread),
            path,
            in_flight: false,
            last_request: None,
            app_log: false,
        }
    }

    fn send(&self, message: Message) {
        if let Some(messages) = &self.messages {
            let _ = messages.send(message);
        }
    }

    /// Something is about to replace the running script: stop it first if
    /// it's been busy for a while, rather than wait for it.
    fn stop_if_stuck(&self) {
        if self.busy_for().is_some_and(|busy| busy >= BUSY_NOTICE_AFTER) {
            self.stop();
        }
    }

    /// Wake the app (this context) when a frame finishes.
    pub fn set_repaint_context(&self, ctx: &egui::Context) {
        let _ = self.repaint.set(ctx.clone());
    }

    // --- what the app reads -----------------------------------------------

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn status(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared)
    }

    pub fn is_running(&self) -> bool {
        self.status().status.running
    }

    pub fn error(&self) -> Option<String> {
        self.status().status.error.clone()
    }

    pub fn settings(&self) -> Vec<SettingDescriptor> {
        self.status().status.settings.clone()
    }

    pub fn actions(&self) -> Vec<Action> {
        self.status().status.actions.clone()
    }

    pub fn perf_summary(&self) -> PerfSummary {
        self.status().status.perf
    }

    pub fn debug_snapshot(&self) -> Option<DebugSnapshot> {
        self.status().status.debug.clone()
    }

    pub fn log_entries(&self) -> Vec<LogEntry> {
        self.status().status.log.clone()
    }

    pub fn options(&self) -> ScriptOptions {
        self.status().status.options
    }

    pub fn cursor(&self) -> CursorRequest {
        self.status().status.cursor
    }

    /// How long the script has been busy with what it's doing now, if it
    /// is (a frame, or loading).
    pub fn busy_for(&self) -> Option<Duration> {
        self.status().busy_since.map(|since| since.elapsed())
    }

    // --- what the app asks for ----------------------------------------------

    /// Stop the script's code running now, as the watchdog would: it stays
    /// stopped until it's changed or restarted.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Run another script (`source`, from `path`).
    pub fn load(&mut self, path: Option<PathBuf>, source: String) {
        self.stop_if_stuck();
        self.path = path.clone();
        self.send(Message::Load { path, source });
    }

    /// Apply new source for the running script, already written to its
    /// file (`save_error`: how that went).
    pub fn set_source(&mut self, source: String, save_error: Option<String>) {
        self.stop_if_stuck();
        self.send(Message::SetSource { source, save_error });
    }

    /// Start the running script over. False if nothing is running.
    pub fn restart(&mut self) -> bool {
        if !self.is_running() {
            return false;
        }
        self.stop_if_stuck();
        self.send(Message::Restart);
        true
    }

    /// Rename the running script's file (and its sidecars), once its
    /// unsaved data is written out. The answer arrives on the receiver;
    /// call `renamed` with the new path when it's a success.
    pub fn rename(&self, new_name: String) -> Receiver<Result<PathBuf, String>> {
        let (reply, answer) = mpsc::channel();
        self.send(Message::Rename { new_name, reply });
        answer
    }

    /// The running script's file is now `path` (`rename` succeeded).
    pub fn renamed(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    /// The song now playing: its file, id and notes.
    pub fn set_song(&self, path: PathBuf, id: String, notes: Arc<NoteList>) {
        self.send(Message::SetSong { path, id, notes });
    }

    /// The "copy script logs to the app log" preference (sent on a change).
    pub fn set_app_log(&mut self, on: bool) {
        if on != self.app_log {
            self.app_log = on;
            self.send(Message::SetAppLog(on));
        }
    }

    /// A value changed in Script Settings (saved to the script's sidecar).
    pub fn set_setting(&self, key: String, value: SettingValue) {
        // Shown right away, not a frame later, so the widget doesn't jump.
        if let Some(d) = self.status().status.settings.iter_mut().find(|d| d.key == key) {
            d.value = value.clone();
        }
        self.send(Message::SetSetting { key, value });
    }

    /// Rebind action `index` (from `actions()`), and save it.
    pub fn set_action_bindings(&self, index: usize, bindings: Vec<Binding>) {
        if let Some(action) = self.status().status.actions.get_mut(index) {
            action.bindings = bindings.clone();
        }
        self.send(Message::SetActionBindings { index, bindings });
    }

    pub fn clear_log(&self) {
        self.status().status.log.clear();
        self.send(Message::ClearLog);
    }

    pub fn retry_full_frame_rate(&self) {
        self.send(Message::RetryFullFrameRate);
    }

    // --- frames ---------------------------------------------------------------

    /// Whether a frame can be asked for now: none is being drawn, and a
    /// script running at the 30 fps fallback is due another.
    pub fn ready_for_frame(&self) -> bool {
        !self.in_flight
            && (!self.perf_summary().half_rate
                || self.last_request.is_none_or(|last| last.elapsed() >= HALF_RATE_INTERVAL))
    }

    pub fn request_frame(&mut self, request: FrameRequest) {
        self.in_flight = true;
        self.last_request = Some(Instant::now());
        self.send(Message::Frame(Box::new(request)));
    }

    /// The frame asked for, if it's done, waiting up to `wait` for it.
    pub fn take_frame(&mut self, wait: Duration) -> Option<FrameDone> {
        if !self.in_flight {
            return None;
        }
        let done = if wait.is_zero() { self.frames.try_recv().ok() } else { self.frames.recv_timeout(wait).ok() };
        if done.is_some() {
            self.in_flight = false;
        }
        done
    }

    /// Stop the script thread, saving the script's data, and wait for it.
    pub fn shutdown(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        self.stop();
        self.messages = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ScriptHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The script thread's side.
struct Worker {
    messages: Receiver<Message>,
    frames: Sender<FrameDone>,
    shared: Arc<Mutex<Shared>>,
    quit: Arc<AtomicBool>,
    repaint: Arc<OnceLock<egui::Context>>,
    /// The log revision last published (the log is only copied when it
    /// changes).
    log_seen: Option<(u64, u64)>,
}

impl Worker {
    fn run(mut self, mut visualizer: LuaVisualizer) {
        let stop = visualizer.stop_flag();
        self.publish(&visualizer);
        while let Ok(message) = self.messages.recv() {
            if self.quit.load(Ordering::Relaxed) {
                break;
            }
            // A stop is for whatever was running when it was asked for.
            stop.store(false, Ordering::Relaxed);
            lock(&self.shared).busy_since = Some(Instant::now());
            let frame_size = match &message {
                Message::Frame(request) => Some((request.width, request.height)),
                _ => None,
            };
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| handle(&mut visualizer, message)));
            let done = match outcome {
                Ok(done) => done,
                // A bug on synththing's side, not the script's: say so, and
                // keep going (a frame still comes back, blank).
                Err(cause) => {
                    let what = cause
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| cause.downcast_ref::<String>().cloned())
                        .unwrap_or_default();
                    log::error!(target: "synththing::script", "the script thread hit a bug: {what}");
                    frame_size.map(|(width, height)| FrameDone {
                        width,
                        height,
                        pixels: vec![0; width * height],
                        samples: Vec::new(),
                        effects: Effects::default(),
                    })
                }
            };
            self.publish(&visualizer);
            if let Some(done) = done {
                let _ = self.frames.send(done);
            }
            if let Some(ctx) = self.repaint.get() {
                ctx.request_repaint();
            }
        }
        // The visualizer drops here, on its own thread, saving its store.
    }

    /// Copy what the app shows into the shared status, and mark the
    /// script idle.
    fn publish(&mut self, visualizer: &LuaVisualizer) {
        let revision = visualizer.log_revision();
        let log = (self.log_seen != Some(revision)).then(|| visualizer.log_entries());
        self.log_seen = Some(revision);
        let status = ScriptStatus {
            running: visualizer.is_running(),
            error: visualizer.error().map(str::to_string),
            settings: visualizer.settings(),
            actions: visualizer.actions(),
            perf: visualizer.perf_summary(),
            debug: visualizer.debug_snapshot(),
            log: Vec::new(),
            options: visualizer.options(),
            cursor: visualizer.cursor_request(),
        };
        let mut shared = lock(&self.shared);
        let log = log.unwrap_or_else(|| std::mem::take(&mut shared.status.log));
        shared.status = ScriptStatus { log, ..status };
        shared.busy_since = None;
    }
}

/// Carry out one message; a frame comes back drawn.
fn handle(visualizer: &mut LuaVisualizer, message: Message) -> Option<FrameDone> {
    match message {
        Message::Load { path, source } => {
            visualizer.set_path(path);
            visualizer.set_source(source);
        }
        Message::SetSource { source, save_error } => visualizer.set_source_after_save(source, save_error),
        Message::Restart => {
            visualizer.restart();
        }
        Message::Rename { new_name, reply } => {
            let result = match visualizer.path().map(Path::to_path_buf) {
                None => Err("the script has no file to rename".to_string()),
                Some(path) => {
                    // Unsaved script data goes to the old name first, then moves along.
                    visualizer.flush_store();
                    match lua_visualizer::rename_script(&path, &new_name) {
                        Ok(new_path) => {
                            visualizer.renamed_to(new_path.clone());
                            Ok(new_path)
                        }
                        Err(e) => Err(format!("{e:#}")),
                    }
                }
            };
            let _ = reply.send(result);
        }
        Message::SetSong { path, id, notes } => {
            visualizer.set_note_list(notes);
            visualizer.set_song(Some(&path), Some(id));
        }
        Message::SetAppLog(on) => visualizer.set_app_log(on),
        Message::SetSetting { key, value } => visualizer.set_setting(&key, value),
        Message::SetActionBindings { index, bindings } => visualizer.set_action_bindings(index, bindings),
        Message::ClearLog => visualizer.clear_log(),
        Message::RetryFullFrameRate => visualizer.retry_full_frame_rate(),
        Message::Frame(request) => {
            let FrameRequest { width, height, mut pixels, samples, notes, playback, input, transport, timestep } =
                *request;
            visualizer.set_transport(transport);
            visualizer.set_fixed_timestep(timestep);
            pixels.resize(width * height, 0);
            visualizer.render(&mut pixels, width, height, &samples, &notes, &playback, &input);
            let effects = Effects {
                channel_requests: visualizer.take_channel_requests(),
                live_commands: visualizer.take_live_commands(),
                playback_requests: visualizer.take_playback_requests(),
            };
            return Some(FrameDone { width, height, pixels, samples, effects });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(width: usize, height: usize) -> FrameRequest {
        FrameRequest {
            width,
            height,
            pixels: Vec::new(),
            samples: Vec::new(),
            notes: NotesSnapshot::default(),
            playback: EngineView::default(),
            input: VisualizerInput::default(),
            transport: Transport::default(),
            timestep: None,
        }
    }

    fn draw(host: &mut ScriptHost) -> FrameDone {
        host.request_frame(request(4, 3));
        host.take_frame(Duration::from_secs(5)).expect("a frame")
    }

    #[test]
    fn frames_and_status_come_back() {
        let script = "local n = setting_int('n', 3, 1, 9)\nfunction render(w, h) log('frame ' .. FRAME) \
                      clear({r = 0x11, g = 0x22, b = 0x33}) pixel(0, 0, {r = 255, g = 255, b = 255}) set_channel_enabled(2, false) end";
        let mut host = ScriptHost::spawn(script.to_string(), None, 44_100);
        assert!(host.ready_for_frame());
        let done = draw(&mut host);
        assert!(host.ready_for_frame());
        assert_eq!((done.width, done.height, done.pixels.len()), (4, 3, 12));
        assert_eq!(done.pixels[0], 0xffffff, "{:x?} {:?}", done.pixels, host.error());
        assert_eq!(done.pixels[1], 0x112233);
        assert_eq!(done.effects.channel_requests, [(2, false)]);
        assert!(host.is_running());
        assert_eq!(host.error(), None);
        assert_eq!(host.settings().len(), 1);
        assert_eq!(host.log_entries().last().map(|e| e.message.clone()).as_deref(), Some("frame 1"));
        // Settings show the change at once.
        host.set_setting("n".into(), SettingValue::Int(7));
        assert!(matches!(host.settings()[0].value, SettingValue::Int(7)));
        // Messages stay in order with frames.
        host.set_source("function render() log('new') end".into(), None);
        draw(&mut host);
        assert_eq!(host.log_entries().last().map(|e| e.message.clone()).as_deref(), Some("new"));
        host.set_source("function render(".into(), None);
        draw(&mut host);
        assert!(host.error().is_some_and(|e| e.contains("compile") || e.contains("expected")), "{:?}", host.error());
    }

    #[test]
    fn a_busy_script_can_be_stopped_and_replaced() {
        let script = "function render() if FRAME == 2 then while true do end end end";
        let mut host = ScriptHost::spawn(script.to_string(), None, 44_100);
        draw(&mut host);
        assert_eq!(host.busy_for(), None);
        host.request_frame(request(4, 3));
        assert!(host.take_frame(Duration::from_millis(100)).is_none(), "still busy");
        assert!(host.busy_for().is_some());
        assert!(!host.ready_for_frame());
        host.stop();
        let done = host.take_frame(Duration::from_secs(3)).expect("stopped long before the watchdog");
        assert_eq!(done.pixels.len(), 12);
        assert!(host.error().is_some_and(|e| e.contains("Stop")), "{:?}", host.error());
        assert_eq!(host.busy_for(), None);

        // Busy past the notice: loading another script stops it first.
        host.restart();
        draw(&mut host);
        host.request_frame(request(4, 3));
        std::thread::sleep(BUSY_NOTICE_AFTER + Duration::from_millis(100));
        host.load(None, "function render() log('replaced') end".into());
        assert!(host.take_frame(Duration::from_secs(3)).is_some());
        draw(&mut host);
        assert_eq!(host.error(), None);
        assert_eq!(host.log_entries().last().map(|e| e.message.clone()).as_deref(), Some("replaced"));
    }

    #[test]
    fn the_watchdog_still_applies() {
        let script = "while true do end function render() end";
        let host = ScriptHost::spawn_with_watchdog(script.to_string(), None, 44_100, Duration::from_millis(300));
        let started = Instant::now();
        while host.error().is_none() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(host.error().is_some_and(|e| e.contains("watchdog")), "{:?}", host.error());
        assert!(!host.is_running());
    }

    #[test]
    fn shutting_down_saves_and_renames_work() {
        let dir = std::env::temp_dir().join(format!("synththing_host_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("saver.lua");
        let script = "function render() store_set('frames', FRAME) end";
        std::fs::write(&path, script).unwrap();
        let mut host = ScriptHost::spawn(script.to_string(), Some(path.clone()), 44_100);
        draw(&mut host);
        let answer = host.rename("kept".into());
        let renamed = answer.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        host.renamed(renamed.clone());
        assert_eq!(host.path(), Some(renamed.as_path()));
        assert!(renamed.exists() && !path.exists());
        draw(&mut host);
        // Even stuck in a loop, quitting stops it and saves.
        host.set_source("function render() store_set('frames', 99) while true do end end".into(), None);
        host.request_frame(request(4, 3));
        std::thread::sleep(Duration::from_millis(100));
        drop(host);
        let store = std::fs::read_to_string(dir.join("kept.lua.store.json")).unwrap();
        assert!(store.contains("99"), "{store}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
