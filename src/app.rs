//! GUI state and layout, built on eframe/egui. Talks to the render thread
//! (`audio.rs`) over a command channel and reads playback state back from a
//! shared snapshot, it never touches the engine or audio directly.
//!
//! Layout: a menu bar on top, the playback controls always at the bottom,
//! and the sections in between as dockable tabs (egui_dock), drag them
//! around, split, float them out into windows. Closing a tab only hides that
//! section; the View menu brings it back (see `layout.rs` for where).

mod editor;
pub use editor::ApplyMode;
mod loading;
mod playlist_panel;
mod recording;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::{egui, Frame};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, DockState, TabViewer};

use crate::applog;
use crate::credits::{self, Contributors, ContributorsState};
use crate::audio::{AudioCommand, PlaybackShared, DEFAULT_BUFFER_MS, MAX_BUFFER_MS, MIN_BUFFER_MS};
use crate::config::{self, nice_name, Config, DEFAULT_PRELOAD_EXPIRY_SECS, PRELOAD_EXPIRY_RANGE};
use crate::engine::{EngineView, MAX_SPEED, MIN_SPEED};
use crate::filebrowser::{DraggedFile, FileBrowser};
use crate::layout::{self, Section};
use crate::loader::{self, Asset, AssetCache, SoundFontProbe};
use crate::lua_completion::CompletionWorker;
use crate::lua_docs;
use crate::lua_visualizer::{
    self, DebugVar, LogLevel, LuaVisualizer, PlaybackRequest, SettingDescriptor, SettingKind, SettingValue,
};
use crate::playlist::{Library, LoopMode, NowPlaying, Rng};
use crate::song_info::{format_length, SongInfoCache};
use crate::updater::{self, UpdateState, Updater};
use crate::watch::{FolderWatch, Watched};
use crate::visualizer::{DisplayMode, NotesSnapshot, SampleTap, ShowOutput, VisualizerPanel};

const VISUALIZER_WIDTH: usize = 800;
const VISUALIZER_HEIGHT: usize = 320;

/// Extensions the song browser lists. MIDI files drive the synth; everything
/// else is decoded as plain audio (rodio/symphonia figure out the actual
/// format themselves, this list is just what shows up in the picker).
const SONG_EXTENSIONS: &[&str] = &["mid", "midi", "wav", "mp3", "ogg", "flac", "m4a", "aac"];
const MIDI_EXTENSIONS: &[&str] = &["mid", "midi"];

/// "Previous" restarts the current track instead of going back a track
/// once it's been playing longer than this, the usual player behavior.
const PREVIOUS_RESTARTS_AFTER_SECS: f64 = 3.0;

/// The Rename script modal's state.
struct RenameScript {
    path: PathBuf,
    name: String,
    error: Option<String>,
    /// Focus the text box on the first frame only.
    focused_once: bool,
}

/// How long the "Esc to exit" hint shows after entering the dedicated
/// visualizer fullscreen, in seconds.
const DEDICATED_HINT_SECS: f64 = 3.0;

/// The dedicated visualizer fullscreen: just the visualizer, nothing else.
struct Dedicated {
    /// Whether the app was already fullscreen before, so leaving goes back
    /// to exactly that.
    was_fullscreen: bool,
    /// egui time it was entered, for the exit hint.
    since: f64,
}

/// Drag-and-drop payload for a row dragged out of the Soundfonts list (onto
/// a playlist entry, to set its override).
struct DraggedSoundfont(PathBuf);

/// How often (seconds) the dock layout is checked for changes to save:
/// dragging a tab or a divider doesn't announce itself, so it's polled.
const LAYOUT_AUTOSAVE_SECS: f64 = 1.0;

/// Fixed so the "Functions" dropdown's insert-at-cursor can load/store the
/// text cursor from a widget it hasn't drawn yet this frame.
const SCRIPT_EDITOR_ID: &str = "visualizer_script_editor_text";

pub struct App {
    commands: Sender<AudioCommand>,
    shared: Arc<Mutex<PlaybackShared>>,
    config: Config,
    /// Always-visible song file browser. Clicking a `.mid` plays it immediately.
    browser: FileBrowser,
    active_sf: Option<usize>,
    tap: SampleTap,
    visualizer: VisualizerPanel<LuaVisualizer>,
    /// Folder holding `.lua` visualizer scripts (bundled defaults + user's own).
    scripts_dir: PathBuf,
    available_scripts: Vec<PathBuf>,
    active_script: Option<usize>,
    /// Live text bound to the script editor; independent of what's actually
    /// running so a mid-edit syntax error doesn't touch what you're typing.
    editor_text: String,
    /// Background-computed autocomplete suggestions for the editor.
    completion: CompletionWorker,
    /// The word currently shown as "hover info" in the editor, set either
    /// by the mouse hovering a known identifier or by F1 at the text
    /// cursor's position; same display either way.
    editor_hover_info: Option<(String, String)>,
    /// The editor has been opened at least once this run, the first time,
    /// the scripting reference comes along as a tab next to it.
    editor_opened_this_run: bool,
    /// Output gain, 0.0..=1.0. Mirrored on the render thread; not applied to
    /// the visualizer tap.
    volume: f32,
    /// Ring-buffer cushion the render thread targets, in milliseconds.
    buffer_ms: u32,
    loop_mode: LoopMode,
    shuffle: bool,
    /// What the engine was last told about looping the current track, see
    /// `sync_engine_loop`.
    engine_loop: bool,
    status: String,
    /// True OS fullscreen. Still the docked tabs, from the windowed layout
    /// or fullscreen's own (see `sync_layout_slot`).
    fullscreen: bool,
    /// Path of the soundfont the engine currently has loaded, the selected
    /// one (`active_sf`), or a playlist entry's override.
    loaded_sf: Option<PathBuf>,
    /// Songs and soundfonts, loaded (and preloaded) in the background.
    assets: AssetCache,
    /// A song waiting on its file (and soundfont) to finish loading.
    pending_play: Option<loading::PendingPlay>,
    /// A soundfont waiting to finish loading before the engine switches to
    /// it. `Some(idx)`: it was clicked in the list and becomes the selected
    /// one too; `None`: just a swap for the playing song (its playlist
    /// override changed).
    pending_soundfont: Option<(Option<usize>, PathBuf)>,
    /// The song the engine has (or last had) loaded.
    current_song: Option<PathBuf>,
    /// Playlist tracks skipped in a row for failing to load, so a playlist
    /// of nothing but missing files stops instead of cycling forever.
    skips_in_a_row: usize,
    /// The playlist row the pointer is over this frame: (song, override).
    hovered_playlist_entry: Option<(PathBuf, Option<PathBuf>)>,
    /// The song the pointer's been resting on, and since when (egui time),
    /// for hover preloading's dwell delay.
    hover_since: Option<(PathBuf, f64)>,
    /// Preferences opened while playing: playback was paused for it and
    /// resumes when it closes.
    paused_for_preferences: bool,
    /// Whether the preferences modal was open last frame.
    preferences_were_open: bool,
    /// Set while the dedicated visualizer fullscreen is up.
    dedicated: Option<Dedicated>,
    /// What the visualizer reported this frame (focus, cursor request);
    /// default when it wasn't drawn.
    visualizer_output: ShowOutput,
    /// Whether the visualizer was drawn (and its script run) this frame and
    /// last frame: when it stops, notes the script was holding are let go.
    visualizer_drawn: bool,
    visualizer_was_drawn: bool,
    /// Whether the cursor is currently confined to the window for a script.
    cursor_confined: bool,
    updater: Updater,
    updates_open: bool,
    /// The startup update check is running: show its result only if it
    /// finds something new (and not a skipped version).
    launch_update_check: bool,
    /// The Log viewer (Help > Log...).
    log_open: bool,
    /// Lowest level shown in the Log viewer.
    log_min_level: log::Level,
    log_search: String,
    /// The status line as last written to the log, so each new status
    /// message is logged once.
    logged_status: String,
    /// The Rename script modal, while it's open.
    rename: Option<RenameScript>,
    /// Move the editor's text cursor to this line (1-based) and scroll to
    /// it, the next time the editor is drawn ("Go to line").
    editor_goto_line: Option<usize>,
    /// Lengths and details of songs in the lists, read in the background.
    song_info: SongInfoCache,
    /// The song whose info modal is open.
    song_info_open: Option<PathBuf>,
    /// Selected rows of the playlist on screen, sorted.
    playlist_selection: Vec<usize>,
    /// Where a Shift+click range starts: the last plain or Ctrl click.
    selection_anchor: Option<usize>,
    /// A Controls binding waiting for a key press: (action index, which of
    /// its keys to replace, or `None` to add one).
    binding_capture: Option<(usize, Option<usize>)>,
    /// Help > Credits & licenses.
    credits_open: bool,
    contributors: Contributors,
    credits_search: String,
    /// Watches the Songs folder and the scripts folder for changes on disk.
    /// Made on the first frame (it needs the egui context to wake the UI).
    folder_watch: Option<FolderWatch>,
    playlists: Library,
    /// The playlist shown in the Playlists panel (not necessarily the one
    /// playing).
    viewed_playlist: Option<usize>,
    /// Set while playing from a playlist; `None` for a song picked straight
    /// from the browser.
    now_playing: Option<NowPlaying>,
    /// Becomes true once the current playlist track is seen playing (not
    /// finished), so the "finished" that ends it advances exactly once, not
    /// again on a stale snapshot from before the next track loaded.
    advance_armed: bool,
    rng: Rng,
    /// Text being typed into the playlist rename box, while renaming.
    playlist_rename: Option<String>,
    /// Delete was clicked once; the next click confirms.
    confirm_delete_playlist: bool,
    /// The section layout. Moved out into a local while `DockArea` draws it
    /// (it needs `&mut` to both the layout and the app), see `dock_ui`.
    dock: DockState<Section>,
    /// True while `dock` is moved out; show/hide requests made meanwhile
    /// (e.g. "Show in Songs" from inside the Playlists tab) queue up in
    /// `pending_layout` and apply right after.
    dock_in_use: bool,
    pending_layout: Vec<(Section, bool)>,
    /// Which sections are open, refreshed after every layout change, so
    /// `is_open` still answers correctly while `dock` is moved out.
    open_sections: Vec<Section>,
    /// The layout as last saved, to tell when it's changed.
    saved_layout_json: String,
    last_layout_check: f64,
    /// Whether `dock` is fullscreen's own layout (true) or the windowed one,
    /// i.e. which config slot it's saved to. See `sync_layout_slot`.
    dock_is_fullscreen_layout: bool,
    preferences_open: bool,
    sample_rate: u32,
    /// The script editor's state (see `app/editor.rs`).
    editor: editor::EditorState,
    /// The script's text as last saved to (or read from) its file, to tell
    /// an outside change from our own save.
    editor_saved: String,
    /// Recording the visualizer to video (see `app/recording.rs`).
    recording: recording::Recording,
    /// The last recording saved, for "Show last recording".
    last_recording: Option<PathBuf>,
}

impl App {
    pub fn new(
        commands: Sender<AudioCommand>,
        shared: Arc<Mutex<PlaybackShared>>,
        config: Config,
        tap: SampleTap,
        sample_rate: u32,
    ) -> Self {
        let browser = FileBrowser::new("songs", config.browse_start_dir(), SONG_EXTENSIONS);

        let scripts_dir = lua_visualizer::scripts_dir().unwrap_or_else(|_| PathBuf::from("."));
        let available_scripts = lua_visualizer::list_scripts(&scripts_dir);
        let active_path = available_scripts.first().cloned();
        let source = active_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let active_script = active_path.is_some().then_some(0);

        let (playlists, playlist_errors) =
            Library::load(config::playlists_dir().unwrap_or_else(|_| PathBuf::from("playlists")));
        let viewed_playlist = (!playlists.lists.is_empty()).then_some(0);
        let dock = config.layout.clone().map(layout::sanitize).unwrap_or_else(layout::default_layout);
        let saved_layout_json = serde_json::to_string(&dock).unwrap_or_default();
        let (loop_mode, shuffle) = (config.loop_mode, config.shuffle);
        let status = match playlist_errors.first() {
            Some(e) => format!("Couldn't read a playlist ({e})"),
            None => "Click a song to play it, click a soundfont to load it.".to_string(),
        };

        let mut app = Self {
            commands,
            shared,
            config,
            browser,
            active_sf: None,
            tap,
            visualizer: VisualizerPanel::new(
                LuaVisualizer::new(source.clone(), active_path, sample_rate),
                VISUALIZER_WIDTH,
                VISUALIZER_HEIGHT,
            ),
            scripts_dir,
            available_scripts,
            active_script,
            editor_text: source,
            completion: CompletionWorker::new(),
            editor_hover_info: None,
            editor_opened_this_run: false,
            volume: 1.0,
            buffer_ms: DEFAULT_BUFFER_MS,
            loop_mode,
            shuffle,
            engine_loop: false,
            status,
            fullscreen: false,
            loaded_sf: None,
            assets: AssetCache::new(),
            pending_play: None,
            pending_soundfont: None,
            current_song: None,
            skips_in_a_row: 0,
            hovered_playlist_entry: None,
            hover_since: None,
            paused_for_preferences: false,
            preferences_were_open: false,
            dedicated: None,
            visualizer_output: ShowOutput::default(),
            visualizer_drawn: false,
            visualizer_was_drawn: false,
            cursor_confined: false,
            updater: Updater::new(),
            updates_open: false,
            launch_update_check: false,
            log_open: false,
            log_min_level: log::Level::Info,
            log_search: String::new(),
            logged_status: String::new(),
            rename: None,
            editor_goto_line: None,
            folder_watch: None,
            song_info: SongInfoCache::new(),
            song_info_open: None,
            playlist_selection: Vec::new(),
            selection_anchor: None,
            binding_capture: None,
            credits_open: false,
            contributors: Contributors::default(),
            credits_search: String::new(),
            playlists,
            viewed_playlist,
            now_playing: None,
            advance_armed: false,
            rng: Rng::from_time(),
            playlist_rename: None,
            confirm_delete_playlist: false,
            open_sections: Section::ALL.into_iter().filter(|&s| layout::is_open(&dock, s)).collect(),
            dock,
            dock_in_use: false,
            pending_layout: Vec::new(),
            saved_layout_json,
            last_layout_check: 0.0,
            dock_is_fullscreen_layout: false,
            preferences_open: false,
            sample_rate,
            editor: editor::EditorState::default(),
            editor_saved: String::new(),
            recording: recording::Recording::new(),
            last_recording: None,
        };
        if let Some(path) = app.visualizer.visualizer().path().map(Path::to_path_buf) {
            app.editor_opened(&path);
        }
        app
    }

    /// Look for a new release in the background, if that's enabled. Not in
    /// development builds (`cargo run`), which shouldn't offer to replace
    /// themselves with a release.
    pub fn check_for_updates_on_launch(&mut self) {
        if cfg!(debug_assertions) || !self.config.check_updates_on_launch.unwrap_or(true) {
            return;
        }
        self.launch_update_check = true;
        self.updater.check();
    }

    fn send(&self, command: AudioCommand) {
        let _ = self.commands.send(command);
    }

    /// Load the first existing soundfont from the retained list, so there is
    /// sound the moment a song is chosen.
    pub fn autoload_first_soundfont(&mut self) {
        if let Some(idx) = self.config.soundfonts.iter().position(|p| p.exists()) {
            self.activate_soundfont(idx);
        }
    }

    /// Force the visualizer open or closed, e.g. from a `--visualizer`
    /// startup flag.
    pub fn set_visualizer_open(&mut self, open: bool) {
        self.set_open(Section::Visualizer, open);
    }

    fn is_open(&self, section: Section) -> bool {
        self.open_sections.contains(&section)
    }

    /// Show (near its home spot) or hide a section's tab.
    fn set_open(&mut self, section: Section, open: bool) {
        if self.dock_in_use {
            self.pending_layout.push((section, open));
            return;
        }
        if open {
            layout::show(&mut self.dock, section);
            if section == Section::Editor && !self.editor_opened_this_run {
                self.editor_opened_this_run = true;
                layout::tab_alongside(&mut self.dock, Section::Reference, Section::Editor, false);
            }
        } else {
            layout::hide(&mut self.dock, section);
        }
        self.refresh_open_sections();
        self.save_layout();
    }

    fn refresh_open_sections(&mut self) {
        self.open_sections =
            Section::ALL.into_iter().filter(|&s| layout::is_open(&self.dock, s)).collect();
    }

    /// Persist the layout. Failure only costs remembering it next launch,
    /// so it's reported but not fatal.
    fn save_layout(&mut self) {
        self.saved_layout_json = serde_json::to_string(&self.dock).unwrap_or_default();
        let slot = if self.dock_is_fullscreen_layout {
            &mut self.config.fullscreen_layout
        } else {
            &mut self.config.layout
        };
        *slot = Some(self.dock.clone());
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save layout: {e}");
        }
    }

    /// Swap in the fullscreen layout or the windowed one, if the one showing
    /// isn't the one that should be: fullscreen's own only while fullscreen
    /// with the separate-layout preference on, the windowed one otherwise.
    /// The outgoing layout is saved to its own slot first, so neither ever
    /// overwrites the other; with the preference off, the fullscreen layout
    /// just sits untouched in the config until it's turned back on.
    fn sync_layout_slot(&mut self) {
        let want_fullscreen_layout = self.fullscreen && self.config.separate_fullscreen_layout;
        if want_fullscreen_layout == self.dock_is_fullscreen_layout || self.dock_in_use {
            return;
        }
        self.save_layout();
        self.dock = if want_fullscreen_layout {
            self.config.fullscreen_layout.clone().map(layout::sanitize).unwrap_or_else(layout::fullscreen_default)
        } else {
            self.config.layout.clone().map(layout::sanitize).unwrap_or_else(layout::default_layout)
        };
        self.dock_is_fullscreen_layout = want_fullscreen_layout;
        self.refresh_open_sections();
        self.save_layout();
    }

    /// Save the layout if it's changed (tabs dragged, dividers moved),
    /// checked about once a second, and never mid-drag.
    fn autosave_layout(&mut self, ctx: &egui::Context) {
        let (now, dragging) = ctx.input(|i| (i.time, i.pointer.any_down()));
        if dragging || now - self.last_layout_check < LAYOUT_AUTOSAVE_SECS {
            return;
        }
        self.last_layout_check = now;
        if serde_json::to_string(&self.dock).unwrap_or_default() != self.saved_layout_json {
            self.save_layout();
        }
    }

    /// Enter or leave true OS fullscreen. The layout shown is the windowed
    /// one, or fullscreen's own if that preference is on (`sync_layout_slot`).
    fn set_fullscreen(&mut self, ctx: &egui::Context, fullscreen: bool) {
        self.fullscreen = fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
    }

    // --- actions -------------------------------------------------------

    /// A song picked straight from the browser, leaves any playlist.
    fn play_path(&mut self, path: PathBuf) {
        self.now_playing = None;
        self.request_play(&path, None, None, false);
    }

    fn add_soundfont(&mut self, path: PathBuf) {
        let name = nice_name(&path);

        // Only refuse files that aren't SoundFont containers at all. A valid
        // container that this build of rustysynth can't render yet (e.g. a
        // compressed SF3) is still added to the list, with a note.
        let note = match loader::probe_soundfont(&path) {
            Ok(SoundFontProbe::Playable(soundfont)) => {
                // Already parsed it, so keep it rather than parse it again
                // when it's picked.
                self.assets.insert_ready(&path, Asset::SoundFont(soundfont), std::time::Instant::now());
                None
            }
            Ok(SoundFontProbe::Unplayable(reason)) => Some(reason),
            Ok(SoundFontProbe::NotSoundFont) => {
                self.status = format!("'{name}' is not a SoundFont file (no RIFF/sfbk header).");
                return;
            }
            Err(e) => {
                self.status = format!("Failed to read '{name}': {e}");
                return;
            }
        };

        if !self.config.add_soundfont(&path) {
            self.status = "That soundfont is already in the list.".to_string();
            return;
        }
        let save_err = self.config.save().err();

        self.status = match (note, save_err) {
            (Some(reason), _) => format!("Added '{name}', but {reason}"),
            (None, Some(e)) => format!("Added '{name}', but failed to save the list: {e}"),
            (None, None) => format!("Added soundfont: {name}"),
        };
    }

    fn remove_soundfont(&mut self, idx: usize) {
        if idx >= self.config.soundfonts.len() {
            return;
        }
        let name = nice_name(&self.config.soundfonts[idx]);
        self.config.remove_soundfont(idx);
        self.status = match self.config.save() {
            Err(e) => format!("Removed {name}, but failed to save list: {e}"),
            Ok(()) => format!("Removed soundfont: {name}"),
        };
        self.active_sf = shift_active(self.active_sf, idx);
        self.pending_soundfont = None;
    }

    fn open_soundfont_browser(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("SoundFont", &["sf2"])
            .set_directory(self.config.browse_start_dir())
            .pick_file()
        {
            self.add_soundfont(path);
        }
    }

    fn load_script(&mut self, idx: usize) {
        let Some(path) = self.available_scripts.get(idx).cloned() else {
            return;
        };
        // Edits still waiting go to the script they were made in.
        self.flush_editor();
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                self.editor_text = source.clone();
                self.completion.request(source.clone());
                self.editor_hover_info = None;
                let visualizer = self.visualizer.visualizer_mut();
                visualizer.set_path(Some(path.clone()));
                visualizer.set_source(source);
                self.active_script = Some(idx);
                self.editor_opened(&path);
            }
            Err(e) => self.status = format!("Failed to read script: {e}"),
        }
    }

    fn new_script_from_template(&mut self, content: &str) {
        match lua_visualizer::new_script_from_template(&self.scripts_dir, content) {
            Ok(path) => {
                self.available_scripts = lua_visualizer::list_scripts(&self.scripts_dir);
                if let Some(idx) = self.available_scripts.iter().position(|p| *p == path) {
                    self.load_script(idx);
                    self.set_open(Section::Editor, true);
                }
            }
            Err(e) => self.status = format!("Failed to create script: {e}"),
        }
    }

    // --- layout ----------------------------------------------------

    /// The bottom transport bar, VLC-style: track info, seek bar, then a
    /// button row (play/restart/stop, view toggles, loop) with speed/volume/
    /// buffer tucked underneath. Shown identically whether fullscreen or
    /// not, so it's the one thing that's always reachable.
    fn controls_bar_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.weak("Song:");
            ui.label(view.track_name.as_deref().unwrap_or("(none)"));
            if let Some(np) = &self.now_playing
                && let Some(list) = self.playlists.lists.get(np.list)
            {
                ui.weak(format!(
                    "({} {}/{})",
                    list.playlist.name,
                    np.entry + 1,
                    list.playlist.entries.len()
                ));
            }
            if !view.has_audio_file {
                ui.separator();
                ui.weak("SoundFont:");
                ui.label(view.soundfont_name.as_deref().unwrap_or("(none)"));
            }
            if !self.status.is_empty() {
                ui.separator();
                ui.weak(&self.status);
            }
        });

        ui.horizontal(|ui| {
            let mut position = view.position;
            let response = ui.add(
                egui::Slider::new(&mut position, 0.0..=view.length.max(0.001)).show_value(false),
            );
            if response.changed() {
                self.send(AudioCommand::Seek(position));
            }
            ui.monospace(format!("{} / {}", fmt_time(view.position), fmt_time(view.length)));
        });

        ui.horizontal_wrapped(|ui| {
            if ui
                .button(if view.paused { "Play" } else { "Pause" })
                .clicked()
            {
                self.send(AudioCommand::TogglePause);
            }
            if ui
                .button("Previous")
                .on_hover_text("Previous playlist track, or restart this one if it's been playing a few seconds")
                .clicked()
            {
                self.previous_track(view.position);
            }
            if ui.button("Stop").clicked() {
                self.send(AudioCommand::Seek(0.0));
                if !view.paused {
                    self.send(AudioCommand::TogglePause);
                }
            }
            if ui
                .add_enabled(self.now_playing.is_some(), egui::Button::new("Next"))
                .on_disabled_hover_text("Play something from a playlist to queue up a next track")
                .clicked()
            {
                self.next_track();
            }

            ui.separator();

            let fullscreen_label = if self.fullscreen { "Exit Fullscreen" } else { "Fullscreen" };
            if ui.button(fullscreen_label).clicked() {
                let ctx = ui.ctx().clone();
                self.set_fullscreen(&ctx, !self.fullscreen);
            }
            let visualizer = self.is_open(Section::Visualizer);
            if ui.selectable_label(visualizer, "Visualizer").clicked() {
                self.set_open(Section::Visualizer, !visualizer);
            }
            let playlists = self.is_open(Section::Playlists);
            if ui.selectable_label(playlists, "Playlist").clicked() {
                self.set_open(Section::Playlists, !playlists);
            }
            if ui
                .selectable_label(self.loop_mode != LoopMode::Off, self.loop_mode.label())
                .on_hover_text("Off → All (wrap the playlist) → One (repeat this track)")
                .clicked()
            {
                self.loop_mode = self.loop_mode.cycle();
                self.plan_upcoming();
                self.save_playback_modes();
            }
            if ui.selectable_label(self.shuffle, "Shuffle").clicked() {
                self.shuffle = !self.shuffle;
                // Start a fresh pass from whatever's playing now.
                if let Some(np) = &mut self.now_playing {
                    np.history = vec![np.entry];
                }
                self.plan_upcoming();
                self.save_playback_modes();
            }

            ui.separator();
            ui.weak(playback_state_text(view));
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("Speed");
            if ui.small_button("-").clicked() {
                self.send(AudioCommand::NudgeSpeed(-0.05));
            }
            let mut speed = view.speed;
            if ui
                .add(egui::Slider::new(&mut speed, MIN_SPEED..=MAX_SPEED))
                .changed()
            {
                self.send(AudioCommand::SetSpeed(speed));
            }
            if ui.small_button("+").clicked() {
                self.send(AudioCommand::NudgeSpeed(0.05));
            }
            if ui.small_button("1x").clicked() {
                self.send(AudioCommand::SetSpeed(1.0));
            }

            ui.separator();

            ui.label("Volume");
            if ui
                .add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false))
                .changed()
            {
                self.send(AudioCommand::SetVolume(self.volume));
            }
            ui.monospace(format!("{:>3.0}%", self.volume * 100.0));

            ui.separator();

            ui.label("Buffer");
            if ui
                .add(
                    egui::Slider::new(&mut self.buffer_ms, MIN_BUFFER_MS..=MAX_BUFFER_MS)
                        .suffix(" ms"),
                )
                .changed()
            {
                self.send(AudioCommand::SetBufferMs(self.buffer_ms));
            }
        });

        ui.add_space(4.0);
    }

    fn soundfont_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Add soundfont...").clicked() {
                self.open_soundfont_browser();
            }
        });

        let mut clicked_idx = None;
        let mut remove_idx = None;
        egui::ScrollArea::vertical()
            .id_salt("soundfonts_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, path) in self.config.soundfonts.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("x").clicked() {
                            remove_idx = Some(i);
                        }
                        ui.separator();
                        let mut label = nice_name(path);
                        if !path.exists() {
                            label.push_str("  (missing)");
                        }
                        let is_active = Some(i) == self.active_sf;
                        let response = ui
                            .add(
                                egui::Button::selectable(is_active, label)
                                    .sense(egui::Sense::click_and_drag()),
                            )
                            .on_hover_text("Click to use; drag onto a MIDI playlist entry to override its soundfont");
                        response.dnd_set_drag_payload(DraggedSoundfont(path.clone()));
                        if response.clicked() {
                            clicked_idx = Some(i);
                        }
                    });
                }
            });

        if let Some(i) = clicked_idx {
            self.activate_soundfont(i);
        }
        if let Some(i) = remove_idx {
            self.remove_soundfont(i);
        }
    }

    /// Desktop-style menu bar. View holds one checkbox per section,
    /// including the tool windows (Settings, Docs), so everything that can
    /// be shown is switched on and off from one place.
    fn top_bar_ui(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Preferences...").clicked() {
                    self.preferences_open = true;
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            let mut toggled = Vec::new();
            ui.menu_button("View", |ui| {
                let mut section_checkbox = |ui: &mut egui::Ui, section: Section| {
                    let mut open = self.open_sections.contains(&section);
                    if ui.checkbox(&mut open, section.title()).changed() {
                        toggled.push((section, open));
                    }
                };
                section_checkbox(ui, Section::Songs);
                section_checkbox(ui, Section::Soundfonts);
                section_checkbox(ui, Section::Playlists);
                ui.separator();
                section_checkbox(ui, Section::Visualizer);
                section_checkbox(ui, Section::Editor);
                section_checkbox(ui, Section::Settings);
                section_checkbox(ui, Section::Reference);
            });
            for (section, open) in toggled {
                self.set_open(section, open);
            }
            ui.menu_button("Help", |ui| {
                if ui.button("Check for updates...").clicked() {
                    self.launch_update_check = false;
                    self.updates_open = true;
                    self.updater.check();
                }
                if ui.button("Log...").clicked() {
                    self.log_open = true;
                }
                if ui.button("Credits & licenses...").clicked() {
                    self.credits_open = true;
                }
                ui.separator();
                ui.weak(format!("synththing v{}", env!("CARGO_PKG_VERSION")));
            });
        });
    }

    /// The app log viewer, as a modal: level filter, search, copy, and the
    /// log file's location.
    fn log_ui(&mut self, ctx: &egui::Context) {
        if !self.log_open {
            return;
        }
        let entries = applog::entries();
        let search = self.log_search.to_lowercase();
        let shown: Vec<&applog::Entry> = entries
            .iter()
            .filter(|e| e.level <= self.log_min_level)
            .filter(|e| search.is_empty() || e.line().to_lowercase().contains(&search))
            .collect();

        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("app_log")).show(ctx, |ui| {
            ui.set_width(720.0);
            ui.heading("Log");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("log_min_level")
                    .selected_text(match self.log_min_level {
                        log::Level::Error => "Errors",
                        log::Level::Warn => "Warnings and up",
                        _ => "Everything",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.log_min_level, log::Level::Info, "Everything");
                        ui.selectable_value(&mut self.log_min_level, log::Level::Warn, "Warnings and up");
                        ui.selectable_value(&mut self.log_min_level, log::Level::Error, "Errors");
                    });
                ui.add(egui::TextEdit::singleline(&mut self.log_search).hint_text("search...").desired_width(200.0));
                ui.weak(format!("{} of {}", shown.len(), entries.len()));
            });
            ui.separator();
            egui::ScrollArea::both()
                .id_salt("app_log_scroll")
                .max_height(420.0)
                .auto_shrink([false, true])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if shown.is_empty() {
                        ui.weak("(nothing)");
                    }
                    for entry in &shown {
                        let color = match entry.level {
                            log::Level::Error => egui::Color32::from_rgb(220, 90, 90),
                            log::Level::Warn => egui::Color32::from_rgb(220, 180, 80),
                            _ => ui.visuals().text_color(),
                        };
                        ui.add(egui::Label::new(egui::RichText::new(entry.line()).monospace().color(color)).extend());
                    }
                });
            ui.separator();
            if let Some(path) = applog::file_path() {
                ui.weak(format!("Also saved to {} (the previous run's is synththing.prev.log)", path.display()));
            }
            ui.horizontal(|ui| {
                if ui.button("Copy shown").clicked() {
                    let text: Vec<String> = shown.iter().map(|e| e.line()).collect();
                    ui.ctx().copy_text(text.join("\n"));
                }
                if let Some(path) = applog::file_path()
                    && ui.button("Open log folder").clicked()
                {
                    open_in_file_manager(&path);
                }
                if ui.button("Clear").on_hover_text("Clears this view; the log file keeps everything").clicked() {
                    applog::clear();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        });
        if close || response.should_close() {
            self.log_open = false;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    /// The Updates window: what the update check found, release notes, and
    /// installing. Opened from Help, or by the startup check when it finds
    /// a release that isn't skipped.
    fn updates_ui(&mut self, ctx: &egui::Context) {
        let state = self.updater.state();
        if self.launch_update_check {
            match &state {
                UpdateState::Checking => {}
                UpdateState::Available(release) => {
                    self.launch_update_check = false;
                    let skipped = self.config.skipped_update.as_deref() == Some(release.version.to_string().as_str());
                    self.updates_open |= !skipped;
                }
                _ => self.launch_update_check = false,
            }
        }
        if matches!(state, UpdateState::Checking | UpdateState::Downloading { .. }) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if !self.updates_open {
            return;
        }

        let current = updater::current_version();
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("updates")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Updates");
            ui.add_space(4.0);
            match &state {
                UpdateState::Idle => {
                    if ui.button("Check for updates").clicked() {
                        self.updater.check();
                    }
                }
                UpdateState::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking for updates...");
                    });
                }
                UpdateState::UpToDate => {
                    ui.label(format!("You're on the latest version (v{current})."));
                }
                UpdateState::Available(release) => {
                    ui.heading(format!("synththing v{} is available", release.version));
                    ui.weak(format!("You have v{current}."));
                    if !release.notes.trim().is_empty() {
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                            ui.label(release.notes.trim());
                        });
                    }
                    ui.hyperlink_to("Release page", &release.page_url);
                    ui.add_space(6.0);
                    let dev_build = cfg!(debug_assertions);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!dev_build, egui::Button::new("Install"))
                            .on_disabled_hover_text(
                                "This is a development build (cargo run); updates install into release builds.",
                            )
                            .clicked()
                        {
                            self.updater.install(release.clone());
                        }
                        if ui.button("Skip this version").clicked() {
                            self.config.skipped_update = Some(release.version.to_string());
                            if let Err(e) = self.config.save() {
                                self.status = format!("Couldn't save preferences: {e}");
                            }
                            close = true;
                        }
                        if ui.button("Not now").clicked() {
                            close = true;
                        }
                    });
                }
                UpdateState::Downloading { release, downloaded } => {
                    ui.label(format!("Downloading v{}...", release.version));
                    let total = release.size().max(1);
                    ui.add(
                        egui::ProgressBar::new(*downloaded as f32 / total as f32)
                            .text(format!("{:.1} / {:.1} MB", *downloaded as f64 / 1e6, total as f64 / 1e6)),
                    );
                }
                UpdateState::Installed(version) => {
                    ui.label(format!("v{version} is installed. It runs the next time synththing starts."));
                    ui.horizontal(|ui| {
                        if ui.button("Restart now").clicked() {
                            match updater::relaunch() {
                                Ok(()) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                                Err(e) => self.status = format!("Couldn't restart: {e}"),
                            }
                        }
                        if ui.button("Later").clicked() {
                            close = true;
                        }
                    });
                }
                UpdateState::Failed(error) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
                    if ui.button("Try again").clicked() {
                        self.updater.check();
                    }
                }
            }
            // Every state but Available has its own way out above, so a
            // plain Close for the rest. (A download carries on in the
            // background if this closes, and Help > Check for updates
            // shows how it went.)
            if !matches!(state, UpdateState::Available(_) | UpdateState::Installed(_)) {
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            }
        });
        if close || response.should_close() {
            self.updates_open = false;
        }
    }

    /// Help > Credits & licenses: contributors (from GitHub), the font, Lua,
    /// and every library in the app with its license text.
    fn credits_ui(&mut self, ctx: &egui::Context) {
        if !self.credits_open {
            return;
        }
        self.contributors.fetch_once();
        let contributors = self.contributors.state();
        if matches!(contributors, ContributorsState::Fetching) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        let third_party = credits::third_party();
        let repo_url = format!("https://github.com/{}", updater::REPO);

        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("credits")).show(ctx, |ui| {
            ui.set_width(660.0);
            ui.heading("Credits & licenses");
            ui.horizontal(|ui| {
                ui.weak(format!("synththing v{}", env!("CARGO_PKG_VERSION")));
                ui.hyperlink_to(&repo_url, &repo_url);
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("synththing itself is released under the WTFPL: do whatever you want with it.");
                ui.hyperlink_to("License", format!("{repo_url}/blob/master/LICENSE"));
            });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("credits_scroll").max_height(480.0).auto_shrink([false, true]).show(
                ui,
                |ui| {
                    ui.strong("Contributors");
                    match &contributors {
                        ContributorsState::NotFetched | ContributorsState::Fetching => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.weak("Loading from GitHub...");
                            });
                        }
                        ContributorsState::Done(list) if list.is_empty() => {
                            ui.weak("(none listed yet)");
                        }
                        ContributorsState::Done(list) => {
                            for contributor in list {
                                ui.horizontal(|ui| {
                                    ui.hyperlink_to(&contributor.login, &contributor.html_url);
                                    let s = if contributor.contributions == 1 { "" } else { "s" };
                                    ui.weak(format!("{} commit{s}", contributor.contributions));
                                });
                            }
                        }
                        ContributorsState::Failed(error) => {
                            ui.weak(format!("Couldn't load the list ({error})."));
                            ui.hyperlink_to("See them on GitHub", format!("{repo_url}/graphs/contributors"));
                        }
                    }

                    ui.separator();
                    ui.strong("Font");
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Script text uses Monogram by Vinícius Menézio (datagoblin), released to the public domain (CC0). Attribution isn't required, but it's given gladly.");
                        ui.hyperlink_to("datagoblin.itch.io/monogram", "https://datagoblin.itch.io/monogram");
                    });
                    egui::CollapsingHeader::new("Monogram's credits").id_salt("credits_monogram").show(ui, |ui| {
                        ui.label(egui::RichText::new(credits::MONOGRAM_CREDITS.trim()).monospace());
                    });

                    ui.separator();
                    ui.strong("Lua");
                    ui.label("Visualizer scripts run on Lua 5.4, by Lua.org, PUC-Rio (MIT license), through mlua.");
                    egui::CollapsingHeader::new("Lua's license").id_salt("credits_lua").show(ui, |ui| {
                        ui.label(egui::RichText::new(credits::LUA_LICENSE.trim()).monospace());
                    });

                    ui.separator();
                    ui.strong(format!("Libraries ({})", third_party.packages.len()));
                    ui.weak("Everything compiled into this build, with its license. Click one for its license text.");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.credits_search)
                            .hint_text("search by name or license...")
                            .desired_width(240.0),
                    );
                    let search = self.credits_search.to_lowercase();
                    for package in &third_party.packages {
                        let matches = search.is_empty()
                            || package.name.to_lowercase().contains(&search)
                            || package.license.to_lowercase().contains(&search);
                        if !matches {
                            continue;
                        }
                        egui::CollapsingHeader::new(format!(
                            "{} {}   {}",
                            package.name, package.version, package.license
                        ))
                        .id_salt(("credits_package", &package.name, &package.version))
                        .show(ui, |ui| {
                            if !package.authors.is_empty() {
                                ui.weak(format!("By {}", package.authors.join(", ")));
                            }
                            if let Some(repository) = &package.repository {
                                ui.hyperlink_to(repository, repository);
                            }
                            for text in third_party.texts_for(package) {
                                ui.label(egui::RichText::new(text).monospace().small());
                            }
                        });
                    }
                },
            );
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if close || response.should_close() {
            self.credits_open = false;
        }
    }

    /// The song info modal: every detail read from one file.
    fn song_info_ui(&mut self, ctx: &egui::Context) {
        let Some(path) = self.song_info_open.clone() else {
            return;
        };
        let info = self.song_info.get(&path);
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("song_info")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(nice_name(&path));
            ui.weak(path.display().to_string());
            ui.add_space(6.0);
            match &info {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("Reading...");
                    });
                }
                Some(info) if info.details.is_empty() => {
                    ui.weak("Nothing to show for this file.");
                }
                Some(info) => {
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        egui::Grid::new("song_info_grid").num_columns(2).spacing([16.0, 4.0]).striped(true).show(
                            ui,
                            |ui| {
                                for (key, value) in &info.details {
                                    ui.strong(key);
                                    ui.add(egui::Label::new(value).wrap());
                                    ui.end_row();
                                }
                            },
                        );
                    });
                }
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if close || response.should_close() {
            self.song_info_open = None;
        }
    }

    /// The Rename script modal: a new name for the running script's file.
    fn rename_ui(&mut self, ctx: &egui::Context) {
        let Some(rename) = &mut self.rename else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::new("rename_script")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Rename script");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut rename.name).desired_width(260.0));
                ui.label(".lua");
                if !rename.focused_once {
                    edit.request_focus();
                    rename.focused_once = true;
                }
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
            });
            if let Some(error) = &rename.error {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
            }
            ui.weak("Its settings file is renamed along with it.");
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Rename").clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel || response.should_close() {
            self.rename = None;
            return;
        }
        if !submit {
            return;
        }
        let (path, name) = (rename.path.clone(), rename.name.clone());
        // Unsaved script data goes to the old name first, then moves along.
        self.flush_editor();
        self.visualizer.visualizer_mut().flush_store();
        match lua_visualizer::rename_script(&path, &name) {
            Ok(new_path) => {
                editor::rename_history(&path, &new_path);
                self.available_scripts = lua_visualizer::list_scripts(&self.scripts_dir);
                self.active_script = self.available_scripts.iter().position(|p| *p == new_path);
                self.visualizer.visualizer_mut().renamed_to(new_path.clone());
                self.status = format!("Renamed script to {}", lua_visualizer::display_name(&new_path));
                self.rename = None;
            }
            Err(e) => {
                if let Some(rename) = &mut self.rename {
                    rename.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    /// The running script's error, if any, with a "Go to line" button when
    /// it points at a line. `open_editor`: the button also opens the editor
    /// (for the copy shown above the visualizer).
    fn script_error_ui(&mut self, ui: &mut egui::Ui, open_editor: bool) {
        let Some(error) = self.visualizer.visualizer().error().map(str::to_string) else {
            return;
        };
        let line = lua_visualizer::error_line(&error);
        ui.horizontal_wrapped(|ui| {
            if let Some(line) = line
                && ui.small_button(format!("Go to line {line}")).clicked()
            {
                self.editor_goto_line = Some(line);
                if open_editor {
                    self.set_open(Section::Editor, true);
                }
            }
            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
        });
    }

    /// Whether one of the app's modals (Preferences, Log, Updates, Rename
    /// script) is up. They get the keyboard: app shortcuts and the global
    /// keys pause.
    fn any_modal_open(&self) -> bool {
        self.preferences_open
            || self.log_open
            || self.updates_open
            || self.rename.is_some()
            || self.credits_open
            || self.song_info_open.is_some()
            || self.recording.prompt_open
            || self.editor.history_open
    }

    /// Pick up files added, removed or changed on disk in the folders on
    /// screen: the Songs browser's current folder and the scripts folder.
    fn refresh_changed_folders(&mut self, ctx: &egui::Context) {
        let watch = self.folder_watch.get_or_insert_with(|| FolderWatch::new(ctx.clone()));
        watch.watch(Watched::Songs, self.browser.cwd());
        watch.watch(Watched::Scripts, &self.scripts_dir);
        for which in watch.poll() {
            match which {
                Watched::Songs => {
                    self.song_info.forget_folder(self.browser.cwd());
                    self.browser.rescan();
                }
                Watched::Scripts => self.rescan_scripts(),
            }
        }
    }

    /// The scripts folder changed: refresh the picker, and if the running
    /// script's own file changed (edited in another editor, say), load the
    /// new version into the editor and visualizer. The app's own saves
    /// leave the file matching the editor, so they don't reload anything.
    fn rescan_scripts(&mut self) {
        let current = self.visualizer.visualizer().path().map(Path::to_path_buf);
        self.available_scripts = lua_visualizer::list_scripts(&self.scripts_dir);
        self.active_script = current.as_ref().and_then(|p| self.available_scripts.iter().position(|q| q == p));
        let Some(path) = current else {
            return;
        };
        let name = lua_visualizer::display_name(&path);
        // Compared with what was last saved, not the editor: the editor
        // can hold edits that aren't applied (saved) yet.
        match std::fs::read_to_string(&path) {
            Ok(text) if text != self.editor_saved && self.editor_dirty() => {
                self.editor_saved = text;
                self.status = format!(
                    "{name} changed on disk; kept your edits that aren't applied yet (applying them overwrites the file)."
                );
            }
            Ok(text) if text != self.editor_saved => {
                self.editor_text = text.clone();
                self.editor_saved = text.clone();
                self.completion.request(text.clone());
                self.visualizer.visualizer_mut().set_source(text);
                self.status = format!("Reloaded {name} (changed on disk).");
            }
            Ok(_) => {}
            Err(_) if !path.exists() => {
                self.status = format!("{name}.lua was moved or deleted; it keeps running until another script is picked.");
            }
            Err(e) => log::warn!("couldn't re-read {}: {e}", path.display()),
        }
    }

    /// Remember Loop and Shuffle for next launch.
    fn save_playback_modes(&mut self) {
        self.config.loop_mode = self.loop_mode;
        self.config.shuffle = self.shuffle;
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save loop/shuffle: {e}");
        }
    }

    /// The section tabs, filling everything between the menu bar and the
    /// controls. With every tab closed, a pointer to the View menu instead.
    fn dock_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        if self.open_sections.is_empty() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.weak(
                        "Nothing open. Use the View menu at the top to show Songs (pick something \
                         to play), Playlists, the Visualizer, and more.",
                    );
                });
            });
            return;
        }

        // DockArea needs `&mut` to the layout while the tabs it draws need
        // `&mut self`, so the layout is moved out for the duration.
        let mut dock = std::mem::replace(&mut self.dock, DockState::new(Vec::new()));
        self.dock_in_use = true;
        let mut tabs = SectionTabs { app: self, shared, closed: false };
        egui::CentralPanel::default().show(ui, |ui| {
            DockArea::new(&mut dock)
                .id(egui::Id::new("section_dock"))
                .show_add_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut tabs);
        });
        let closed = tabs.closed;
        self.dock_in_use = false;
        self.dock = dock;

        self.refresh_open_sections();
        if closed {
            self.save_layout();
        }
        for (section, open) in std::mem::take(&mut self.pending_layout) {
            self.set_open(section, open);
        }
    }

    /// The editor tab: plus the script picker when the visualizer (which
    /// normally hosts it) is closed.
    fn editor_section_ui(&mut self, ui: &mut egui::Ui) {
        if !self.is_open(Section::Visualizer) {
            self.script_picker_ui(ui);
        }
        self.script_error_ui(ui, false);
        self.visualizer_editor_ui(ui);
    }

    fn songs_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("Set as default")
                .on_hover_text("Open the song browser in this folder from now on")
                .clicked()
            {
                let dir = self.browser.cwd().to_path_buf();
                self.status = match self.config.set_browse_dir(dir) {
                    Ok(()) => "Default song folder updated.".to_string(),
                    Err(e) => format!("Failed to save default folder: {e}"),
                };
            }
        });
        let cache = &mut self.song_info;
        let info_open = &mut self.song_info_open;
        let clicked = self.browser.ui(ui, &mut |ui, path| song_info_widgets(ui, cache, path, info_open));
        if let Some(path) = clicked {
            self.play_path(path);
        }
    }

    /// Script picker + channel toggles + error banner + the live preview.
    fn visualizer_preview_ui(
        &mut self,
        ui: &mut egui::Ui,
        mut notes: NotesSnapshot,
        playback: EngineView,
    ) {
        self.script_picker_ui(ui);

        ui.horizontal(|ui| {
            if ui
                .button("Fullscreen visualizer")
                .on_hover_text("Just the visualizer, filling the screen, with mouse and keyboard going to the script. F11 toggles it, Esc exits.")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.enter_dedicated(&ctx);
            }
            self.recording_buttons_ui(ui, &playback);
            if let Some(path) = self.visualizer.visualizer().path() {
                ui.weak(path.display().to_string());
            }
        });

        self.script_error_ui(ui, true);

        self.channels_ui(ui, &mut notes);

        let mode = if self.fullscreen { DisplayMode::Fullscreen } else { DisplayMode::Window };
        self.show_visualizer(ui, &notes, &playback, mode);
    }

    /// Draw (and run) the visualizer into the rest of `ui`, and pass on
    /// whatever it asked for.
    fn show_visualizer(&mut self, ui: &mut egui::Ui, notes: &NotesSnapshot, playback: &EngineView, mode: DisplayMode) {
        let fixed_size = self.recording.frame_size();
        let hold = self.pace_recording();
        self.visualizer_output =
            self.visualizer.show(ui, &self.tap, notes, playback, self.preferences_open, mode, fixed_size, hold);
        self.feed_recording(ui);
        self.visualizer_drawn = true;

        // A script can ask to mute/unmute a channel itself (e.g. a game
        // script silencing a dead player's channel), same command the GUI's
        // own checkboxes send, so it's subject to the same "never disable
        // the last channel" rule.
        for (channel, enabled) in self.visualizer.visualizer_mut().take_channel_requests() {
            self.send(AudioCommand::SetChannelEnabled(channel, enabled));
        }

        // Notes the script played, for the live synth.
        for command in self.visualizer.visualizer_mut().take_live_commands() {
            self.send(AudioCommand::Live(command));
        }

        // set_paused / seek from the script. `paused` tracks the effect of
        // earlier requests this frame, since the engine's view won't
        // reflect them until next frame.
        let mut paused = playback.paused;
        for request in self.visualizer.visualizer_mut().take_playback_requests() {
            match request {
                PlaybackRequest::Pause(pause) if pause != paused => {
                    self.send(AudioCommand::TogglePause);
                    paused = pause;
                }
                PlaybackRequest::Pause(_) => {}
                PlaybackRequest::Seek(seconds) => self.send(AudioCommand::Seek(seconds)),
            }
        }
    }

    /// Enter the dedicated visualizer fullscreen: OS fullscreen with only
    /// the visualizer, which gets keyboard focus.
    fn enter_dedicated(&mut self, ctx: &egui::Context) {
        let since = ctx.input(|i| i.time);
        self.dedicated = Some(Dedicated { was_fullscreen: self.fullscreen, since });
        if !self.fullscreen {
            self.set_fullscreen(ctx, true);
        }
        self.visualizer.request_focus();
    }

    /// Start the running visualizer script over from scratch (F5, or the
    /// Restart button).
    fn restart_script(&mut self) {
        let name = self.visualizer.visualizer().path().map(nice_name).unwrap_or_else(|| "script".to_string());
        self.status = if self.visualizer.visualizer_mut().restart() {
            format!("Restarted {name}.")
        } else {
            "No script running to restart.".to_string()
        };
    }

    /// F11 out of the dedicated fullscreen: back to the normal layout, and
    /// out of OS fullscreen too unless the app was fullscreen before. (Esc
    /// skips that and always goes straight back to windowed.)
    fn exit_dedicated(&mut self, ctx: &egui::Context) {
        if let Some(dedicated) = self.dedicated.take()
            && !dedicated.was_fullscreen
        {
            self.set_fullscreen(ctx, false);
        }
    }

    /// The dedicated fullscreen's whole window: the visualizer edge to edge,
    /// plus a brief "Esc to exit" hint.
    fn dedicated_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            self.show_visualizer(ui, &shared.notes, &shared.view, DisplayMode::Dedicated);
        });
        let ctx = ui.ctx().clone();
        let elapsed = ctx.input(|i| i.time) - self.dedicated.as_ref().map_or(0.0, |d| d.since);
        if elapsed < DEDICATED_HINT_SECS {
            egui::Area::new(egui::Id::new("dedicated_exit_hint"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 24.0))
                .interactable(false)
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label("Press Esc to exit");
                    });
                });
        }
    }

    /// Confine the cursor to the window while a script asks for it, but only
    /// in the dedicated fullscreen, while the visualizer has focus and the
    /// window is focused. Released the moment any of that stops being true
    /// (Escape included), so a script can never trap the cursor.
    fn apply_cursor_confinement(&mut self, ctx: &egui::Context) {
        let window_focused = ctx.input(|i| i.focused);
        let want = self.dedicated.is_some()
            && self.visualizer_output.focused
            && self.visualizer_output.cursor.confined
            && window_focused
            && !self.preferences_open;
        if want != self.cursor_confined {
            self.cursor_confined = want;
            let grab = if want { egui::viewport::CursorGrab::Confined } else { egui::viewport::CursorGrab::None };
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(grab));
        }
    }

    /// Active script picker, "New" from template, and "Restore default" for
    /// a bundled script. Shown above the visualizer, or above the editor
    /// when the visualizer is off.
    fn script_picker_ui(&mut self, ui: &mut egui::Ui) {
        let mut picked_idx = None;
        let mut restore_bundled: Option<String> = None;
        let mut restart = false;
        let mut rename: Option<PathBuf> = None;
        ui.horizontal(|ui| {
            ui.label("Script:");
            let current = self
                .active_script
                .and_then(|i| self.available_scripts.get(i))
                .map(|p| lua_visualizer::display_name(p))
                .unwrap_or_else(|| "(none)".to_string());
            egui::ComboBox::from_id_salt("visualizer_script_picker")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (i, path) in self.available_scripts.iter().enumerate() {
                        let name = lua_visualizer::display_name(path);
                        if ui
                            .selectable_label(Some(i) == self.active_script, name)
                            .clicked()
                        {
                            picked_idx = Some(i);
                        }
                    }
                });

            let mut new_from: Option<&'static str> = None;
            egui::ComboBox::from_id_salt("visualizer_new_script")
                .selected_text("New")
                .show_ui(ui, |ui| {
                    for (name, content) in lua_visualizer::new_script_templates() {
                        if ui.selectable_label(false, name).clicked() {
                            new_from = Some(content);
                        }
                    }
                });
            if let Some(content) = new_from {
                self.new_script_from_template(content);
            }

            // Offer a re-sync when the active script is a bundled default
            // (they're only written on first run, so an older copy on disk
            // won't have newer host features).
            if let Some(name) = self
                .visualizer
                .visualizer()
                .path()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                && let Some(bundled) = lua_visualizer::bundled_default(name)
                && ui.button("Restore default").clicked()
            {
                restore_bundled = Some(bundled.to_string());
            }
            if ui
                .button("Restart")
                .on_hover_text("Start the running script over from scratch: its state resets, its settings stay (F5)")
                .clicked()
            {
                restart = true;
            }
            if let Some(path) = self.visualizer.visualizer().path()
                && ui.button("Rename").on_hover_text("Rename this script's file (its settings come along)").clicked()
            {
                rename = Some(path.to_path_buf());
            }
        });
        if let Some(path) = rename {
            let name = lua_visualizer::display_name(&path);
            self.rename = Some(RenameScript { path, name, error: None, focused_once: false });
        }
        if restart {
            self.restart_script();
        }
        if let Some(idx) = picked_idx {
            self.load_script(idx);
        }
        if let Some(source) = restore_bundled {
            if let Some(path) = self.visualizer.visualizer().path().map(Path::to_path_buf) {
                self.editor_opened(&path);
            }
            self.editor_text = source;
            self.completion.request(self.editor_text.clone());
            self.apply_editor();
        }
    }

    /// Per-channel mute toggles for the current MIDI file (left-click
    /// toggle, right-click solo, "All" to re-enable everything). `notes` is
    /// updated optimistically so this frame's preview already reflects it.
    fn channels_ui(&mut self, ui: &mut egui::Ui, notes: &mut NotesSnapshot) {
        if !notes.detected_channels.is_empty() {
            let mut toggle: Option<(u8, bool)> = None;
            let mut solo: Option<u8> = None;
            let mut enable_all = false;

            // At least one channel always has to stay enabled, muting the
            // last one is never useful, just confusing silence.
            let enabled_count = notes
                .detected_channels
                .iter()
                .filter(|&&c| notes.enabled_channels[c as usize])
                .count();

            ui.horizontal_wrapped(|ui| {
                ui.label("Channels:")
                    .on_hover_text("Left-click to toggle, right-click to solo");
                if ui.small_button("All").clicked() {
                    enable_all = true;
                }
                for &channel in &notes.detected_channels {
                    let mut enabled = notes.enabled_channels[channel as usize];
                    let is_only_one_left = enabled && enabled_count == 1;
                    let response = ui.add_enabled(
                        !is_only_one_left,
                        egui::Checkbox::new(&mut enabled, format!("{}", channel + 1)),
                    );
                    if response.changed() {
                        toggle = Some((channel, enabled));
                    }
                    if response.secondary_clicked() {
                        solo = Some(channel);
                    }
                }
            });

            if enable_all {
                for &c in &notes.detected_channels {
                    notes.enabled_channels[c as usize] = true;
                }
                self.send(AudioCommand::EnableAllChannels);
            } else if let Some(channel) = solo {
                for (i, slot) in notes.enabled_channels.iter_mut().enumerate() {
                    *slot = i == channel as usize;
                }
                self.send(AudioCommand::SoloChannel(channel));
            } else if let Some((channel, enabled)) = toggle {
                notes.enabled_channels[channel as usize] = enabled;
                self.send(AudioCommand::SetChannelEnabled(channel, enabled));
            }
        }
    }

    /// While Preferences is open, playback and the visualizer script pause;
    /// closing it resumes playback only if it was playing beforehand.
    fn pause_for_preferences(&mut self, view: &EngineView) {
        if self.preferences_open && !self.preferences_were_open {
            let playing = !view.paused && !view.finished && (view.has_midi || view.has_audio_file);
            if playing {
                self.send(AudioCommand::TogglePause);
            }
            self.paused_for_preferences = playing;
        } else if !self.preferences_open && self.preferences_were_open {
            if self.paused_for_preferences && view.paused {
                self.send(AudioCommand::TogglePause);
            }
            self.paused_for_preferences = false;
        }
        self.preferences_were_open = self.preferences_open;
    }

    /// App-wide preferences, as a modal over everything else.
    fn preferences_ui(&mut self, ctx: &egui::Context) {
        if !self.preferences_open {
            return;
        }
        let mut separate = self.config.separate_fullscreen_layout;
        let mut expiry = self.config.preload_expiry_secs.unwrap_or(DEFAULT_PRELOAD_EXPIRY_SECS);
        let mut show_experimental = self.config.show_experimental;
        let mut hover_preload = self.config.preload_on_hover;
        let mut check_updates = self.config.check_updates_on_launch.unwrap_or(true);
        let loaded = self.assets.summary();
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("preferences")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.heading("Preferences");
            ui.add_space(8.0);

            ui.strong("Layout");
            ui.checkbox(&mut separate, "Separate layout for fullscreen");
            ui.weak(
                "Off: fullscreen shows the same tabs as the window, and rearranging them in \
                 either changes both. On: fullscreen remembers its own arrangement (starting \
                 with just the visualizer). Turning this off keeps that arrangement saved for \
                 if you turn it back on.",
            );

            ui.add_space(10.0);
            ui.strong("Updates");
            ui.checkbox(&mut check_updates, "Check for updates when synththing starts");
            ui.weak(
                "Looks for a newer release on GitHub and asks before installing anything. \
                 Help > Check for updates does it any time.",
            );

            ui.add_space(10.0);
            ui.strong("Loading");
            ui.horizontal(|ui| {
                ui.label("Unload preloaded files after");
                ui.add(egui::Slider::new(&mut expiry, PRELOAD_EXPIRY_RANGE).suffix(" s").logarithmic(true));
            });
            ui.weak(
                "Songs and soundfonts load in the background, and the next playlist track is \
                 loaded ahead of time. Anything loaded that nothing has needed for this long \
                 (not playing, not up next, not hovered) is unloaded to free memory.",
            );

            ui.add_space(10.0);
            self.recording_preferences_ui(ui);

            ui.add_space(10.0);
            ui.checkbox(&mut show_experimental, "Show experimental settings");
            if show_experimental {
                ui.add_space(4.0);
                ui.strong("Experimental");
                ui.checkbox(&mut hover_preload, "Preload songs on hover");
                ui.weak(
                    "Start loading a song once the pointer rests on it in Songs or a playlist, \
                     so it starts sooner when clicked. Uses more memory while browsing.",
                );
                egui::CollapsingHeader::new(format!("Loaded right now ({})", loaded.len()))
                    .id_salt("preferences_loaded_files")
                    .show(ui, |ui| {
                        if loaded.is_empty() {
                            ui.weak("(nothing)");
                        }
                        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                            for (path, kind, state) in &loaded {
                                ui.label(format!("{} ({kind:?}, {state})", nice_name(path)))
                                    .on_hover_text(path.display().to_string());
                            }
                        });
                    });
            }

            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if close || response.should_close() {
            self.preferences_open = false;
        }
        let mut changed = false;
        let expiry = Some(expiry).filter(|&s| s != DEFAULT_PRELOAD_EXPIRY_SECS);
        if expiry != self.config.preload_expiry_secs
            || show_experimental != self.config.show_experimental
            || hover_preload != self.config.preload_on_hover
            || check_updates != self.config.check_updates_on_launch.unwrap_or(true)
        {
            self.config.check_updates_on_launch = (!check_updates).then_some(false);
            self.config.preload_expiry_secs = expiry;
            self.config.show_experimental = show_experimental;
            self.config.preload_on_hover = hover_preload;
            changed = true;
        }
        // (A layout preference change saves the config itself, below.)
        if changed
            && separate == self.config.separate_fullscreen_layout
            && let Err(e) = self.config.save()
        {
            self.status = format!("Couldn't save preferences: {e}");
        }
        if separate != self.config.separate_fullscreen_layout {
            self.config.separate_fullscreen_layout = separate;
            // Saves the config (with the new preference) along the way when
            // the layout actually switches; otherwise save it directly.
            self.sync_layout_slot();
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
    }

    /// The running script's input actions and their keys: click a key and
    /// press another to rebind it, x to remove, + to add, Reset for the
    /// script's defaults. Saved per script.
    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        let actions = self.visualizer.visualizer().actions();
        if actions.is_empty() {
            self.binding_capture = None;
            return;
        }
        ui.separator();
        ui.strong("Controls");
        ui.weak("Click a key, then press the new one. Controls work while the visualizer has focus.");

        let mut change: Option<(usize, Vec<egui::Key>)> = None;
        let mut start_capture: Option<Option<(usize, Option<usize>)>> = None;
        egui::Grid::new("script_controls").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            for (i, action) in actions.iter().enumerate() {
                ui.label(&action.name);
                ui.horizontal_wrapped(|ui| {
                    for (slot, key) in action.bindings.iter().enumerate() {
                        let capturing = self.binding_capture == Some((i, Some(slot)));
                        let label = if capturing { "press a key...".to_string() } else { key.name().to_string() };
                        if ui.selectable_label(capturing, label).clicked() {
                            start_capture = Some((!capturing).then_some((i, Some(slot))));
                        }
                        if ui.small_button("x").on_hover_text("Remove this key").clicked() {
                            let mut bindings = action.bindings.clone();
                            bindings.remove(slot);
                            change = Some((i, bindings));
                        }
                    }
                    let adding = self.binding_capture == Some((i, None));
                    if ui
                        .selectable_label(adding, if adding { "press a key..." } else { "+" })
                        .on_hover_text("Add another key")
                        .clicked()
                    {
                        start_capture = Some((!adding).then_some((i, None)));
                    }
                    if action.bindings != action.defaults
                        && ui.small_button("Reset").on_hover_text("Back to the script's keys").clicked()
                    {
                        change = Some((i, action.defaults.clone()));
                    }
                });
                ui.end_row();
            }
        });

        // Waiting for a key: the first press (not a reserved key) binds it.
        let mut captured = false;
        if let Some((i, slot)) = self.binding_capture
            && let Some(action) = actions.get(i)
        {
            let pressed = ui.input(|input| {
                input.events.iter().find_map(|event| match event {
                    egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                    _ => None,
                })
            });
            if let Some(key) = pressed {
                captured = true;
                self.binding_capture = None;
                if !crate::visualizer::RESERVED_KEYS.contains(&key) {
                    let mut bindings = action.bindings.clone();
                    match slot {
                        Some(slot) if slot < bindings.len() => bindings[slot] = key,
                        _ => bindings.push(key),
                    }
                    let mut seen = Vec::new();
                    bindings.retain(|k| {
                        let first = !seen.contains(k);
                        seen.push(*k);
                        first
                    });
                    change = Some((i, bindings));
                }
            }
        }
        // (A key that just got bound can also "click" the focused button;
        // don't let that start capturing again.)
        if let Some(capture) = start_capture
            && !captured
        {
            self.binding_capture = capture;
        }
        if let Some((i, bindings)) = change {
            self.visualizer.visualizer_mut().set_action_bindings(i, bindings);
        }
    }

    /// The active script's settings widgets, its `debug_locals()` snapshot,
    /// and its log/error history.
    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let descriptors = self.visualizer.visualizer().settings();
        let mut changed: Option<(String, SettingValue)> = None;
        let mut clear_log = false;

        // Performance: render time against the 60 fps budget.
        let perf = self.visualizer.visualizer().perf_summary();
        let mut retry = false;
        ui.horizontal_wrapped(|ui| {
            ui.strong("Performance");
            if perf.frames == 0 {
                ui.weak("(not rendered yet)");
                return;
            }
            let budget = 1000.0 / 60.0;
            let color = if perf.avg_ms > budget {
                egui::Color32::from_rgb(220, 90, 90)
            } else if perf.avg_ms > budget * 0.6 {
                egui::Color32::from_rgb(220, 180, 80)
            } else {
                ui.visuals().text_color()
            };
            ui.colored_label(color, format!("render() {:.1} ms avg, {:.1} ms worst", perf.avg_ms, perf.max_ms))
                .on_hover_text(format!(
                    "Over the last second. A 60 fps frame allows {budget:.1} ms; a script averaging more than that for 3 seconds runs at 30 fps until its code changes."
                ));
            if perf.half_rate {
                ui.colored_label(egui::Color32::from_rgb(220, 180, 80), "running at 30 fps");
                retry = ui.small_button("Try 60 fps again").clicked();
            } else {
                ui.weak("60 fps");
            }
        });
        if retry {
            self.visualizer.visualizer_mut().retry_full_frame_rate();
        }
        self.controls_ui(ui);
        ui.separator();

        egui::ScrollArea::vertical()
            .id_salt("visualizer_settings_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if descriptors.is_empty() {
                    ui.weak("This script hasn't registered any settings.");
                } else {
                    egui::Grid::new("visualizer_settings_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            for d in &descriptors {
                                ui.label(&d.key);
                                settings_row_ui(ui, d, &mut changed);
                                ui.end_row();
                            }
                        });
                }

                ui.separator();
                ui.heading("Variables");
                match self.visualizer.visualizer().debug_snapshot() {
                    Some(snapshot) => {
                        if let Some(label) = &snapshot.label {
                            ui.weak(format!("from: {label}"));
                        }
                        egui::ScrollArea::vertical()
                            .id_salt("visualizer_debug_vars_scroll")
                            .auto_shrink([false, false])
                            .max_height(160.0)
                            .show(ui, |ui| {
                                for (i, var) in snapshot.vars.iter().enumerate() {
                                    debug_var_ui(ui, var, i);
                                }
                            });
                    }
                    None => {
                        ui.weak("(nothing captured yet)");
                    }
                }

                ui.separator();
                ui.horizontal(|ui| {
                    ui.heading("Log");
                    if ui.small_button("Clear").clicked() {
                        clear_log = true;
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("visualizer_log_scroll")
                    .auto_shrink([false, false])
                    .max_height(180.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        let entries = self.visualizer.visualizer().log_entries();
                        if entries.is_empty() {
                            ui.weak("(nothing logged yet)");
                        }
                        for entry in &entries {
                            let color = if entry.level == LogLevel::Error {
                                egui::Color32::from_rgb(220, 90, 90)
                            } else {
                                ui.visuals().text_color()
                            };
                            let text = if entry.count > 1 {
                                format!("{} (x{})", entry.message, entry.count)
                            } else {
                                entry.message.clone()
                            };
                            ui.colored_label(color, text);
                        }
                    });
            });

        if let Some((key, value)) = changed {
            self.visualizer.visualizer_mut().set_setting(&key, value);
        }
        if clear_log {
            self.visualizer.visualizer_mut().clear_log();
        }
    }

    /// The scripting-API reference: one embedded, structured page (see
    /// `lua_docs.rs`) rather than a hosted wiki, the whole API is a few
    /// dozen functions, not a sprawling product.
    fn docs_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("docs_scroll").auto_shrink([false, false]).show(ui, |ui| {
            for section in lua_docs::sections() {
                ui.heading(section.title);
                for block in section.blocks {
                    match block {
                        lua_docs::Block::P(text) => {
                            ui.label(inline_code_job(ui, "", text));
                            ui.add_space(4.0);
                        }
                        lua_docs::Block::Code(code) => {
                            ui.code(*code);
                            ui.add_space(4.0);
                        }
                        lua_docs::Block::Bullets(items) => {
                            for item in *items {
                                ui.label(inline_code_job(ui, "- ", item));
                            }
                            ui.add_space(4.0);
                        }
                    }
                }
                ui.separator();
            }
        });
    }
}

/// One row of a `debug_locals()` snapshot: a leaf shows `name = value`, a
/// table gets a `CollapsingHeader` nesting its entries the same way.
/// `index` only matters for giving siblings with identical name+value text
/// distinct widget ids (position within the parent's own children list, not
/// a global counter).
fn debug_var_ui(ui: &mut egui::Ui, var: &DebugVar, index: usize) {
    if var.children.is_empty() {
        ui.label(format!("{} = {}", var.name, var.value));
        return;
    }
    egui::CollapsingHeader::new(format!("{} = {}", var.name, var.value))
        .id_salt((index, &var.name))
        .show(ui, |ui| {
            for (i, child) in var.children.iter().enumerate() {
                debug_var_ui(ui, child, i);
            }
        });
}

/// One setting's widget, matching `d.kind` to the right egui control.
fn settings_row_ui(ui: &mut egui::Ui, d: &SettingDescriptor, changed: &mut Option<(String, SettingValue)>) {
    match (&d.kind, &d.value) {
        (SettingKind::Bool, SettingValue::Bool(v)) => {
            let mut v = *v;
            if ui.checkbox(&mut v, "").changed() {
                *changed = Some((d.key.clone(), SettingValue::Bool(v)));
            }
        }
        (SettingKind::Int { min, max }, SettingValue::Int(v)) => {
            let mut v = *v;
            if ui.add(egui::Slider::new(&mut v, *min..=*max)).changed() {
                *changed = Some((d.key.clone(), SettingValue::Int(v)));
            }
        }
        (SettingKind::Float { min, max }, SettingValue::Float(v)) => {
            let mut v = *v;
            if ui.add(egui::Slider::new(&mut v, *min..=*max)).changed() {
                *changed = Some((d.key.clone(), SettingValue::Float(v)));
            }
        }
        (SettingKind::Color, SettingValue::Color(rgb)) => {
            let mut rgb = *rgb;
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                *changed = Some((d.key.clone(), SettingValue::Color(rgb)));
            }
        }
        (SettingKind::String, SettingValue::String(s)) => {
            let mut s = s.clone();
            if ui.text_edit_singleline(&mut s).changed() {
                *changed = Some((d.key.clone(), SettingValue::String(s)));
            }
        }
        (SettingKind::Selection { options, max_selections }, SettingValue::Selection(selected)) => {
            ui.vertical(|ui| {
                for option in options {
                    let mut checked = selected.contains(option);
                    if ui.checkbox(&mut checked, option).changed() {
                        let mut next = selected.clone();
                        if checked {
                            next.push(option.clone());
                            if next.len() > *max_selections {
                                next.remove(0); // evict the oldest selection
                            }
                        } else {
                            next.retain(|s| s != option);
                        }
                        *changed = Some((d.key.clone(), SettingValue::Selection(next)));
                    }
                }
            });
        }
        // Descriptor/value kind mismatch can't happen, both always come
        // from the same `SettingsStore` entry, but match exhaustively
        // rather than unwrap.
        _ => {
            ui.weak("(unsupported setting type)");
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        let ctx = ui.ctx().clone();
        let shared = self.shared.lock().unwrap().clone();
        let view = &shared.view;
        self.visualizer_drawn = false;

        // The OS is the final authority on fullscreen state, e.g. the user
        // could leave it some way other than our own button/key.
        if let Some(actual) = ctx.input(|i| i.viewport().fullscreen) {
            self.fullscreen = actual;
        }

        // App-wide keys, honored from anywhere: while typing, with the
        // visualizer focused, in either fullscreen (scripts never see them,
        // see visualizer::RESERVED_KEYS). Esc goes all the way back to
        // windowed, F11 toggles the dedicated visualizer fullscreen, F5
        // restarts the running script. Escape is only taken when there's a
        // fullscreen to leave, so it still does its usual job elsewhere.
        // Not while a modal is up: Escape closes the modal instead.
        if !self.any_modal_open() {
            let in_fullscreen = self.fullscreen || self.dedicated.is_some();
            let (escape, f11, f5, f9, f10) = ctx.input_mut(|i| {
                (
                    in_fullscreen && i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::F11),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::F5),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::F9),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::F10),
                )
            });
            if f9 {
                self.toggle_recording();
            }
            if f10 {
                self.toggle_recording_pause();
            }
            if escape {
                self.dedicated = None;
                self.set_fullscreen(&ctx, false);
            }
            if f11 {
                if self.dedicated.is_some() {
                    self.exit_dedicated(&ctx);
                } else {
                    self.enter_dedicated(&ctx);
                }
            }
            if f5 {
                self.restart_script();
            }
        }
        self.visualizer_output = ShowOutput::default();

        // Single-key shortcuts are suspended whenever some widget (namely the
        // song browser's search box) has keyboard focus, so typing "v" there
        // types a "v" instead of toggling the visualizer.
        let typing = ctx.memory(|m| m.focused().is_some()) || self.any_modal_open() || self.binding_capture.is_some();
        let mut toggle_fullscreen = false;
        let mut toggle_visualizer = false;
        let mut next = false;
        let mut previous = false;
        if !typing {
            ctx.input(|input| {
                if input.key_pressed(egui::Key::Space) {
                    self.send(AudioCommand::TogglePause);
                }
                if input.key_pressed(egui::Key::ArrowLeft) {
                    self.send(AudioCommand::SeekRelative(-5.0));
                }
                if input.key_pressed(egui::Key::ArrowRight) {
                    self.send(AudioCommand::SeekRelative(5.0));
                }
                if input.key_pressed(egui::Key::R) {
                    self.send(AudioCommand::Seek(0.0));
                }
                if input.key_pressed(egui::Key::V) {
                    toggle_visualizer = true;
                }
                if input.key_pressed(egui::Key::N) {
                    next = true;
                }
                if input.key_pressed(egui::Key::P) {
                    previous = true;
                }
                if input.key_pressed(egui::Key::F) {
                    toggle_fullscreen = true;
                }
            });
        }
        // `send_viewport_cmd` (inside `set_fullscreen`) takes the same lock
        // `ctx.input` holds for the duration of its closure above, calling
        // it from in there deadlocks, so it has to happen out here instead.
        if toggle_fullscreen {
            self.set_fullscreen(&ctx, !self.fullscreen);
        }
        if toggle_visualizer {
            self.set_open(Section::Visualizer, !self.is_open(Section::Visualizer));
        }
        if next && self.now_playing.is_some() {
            self.next_track();
        }
        if previous {
            self.previous_track(view.position);
        }

        self.pause_for_preferences(view);
        self.handle_os_file_drops(&ctx);
        self.poll_loads();
        self.refresh_changed_folders(&ctx);
        if self.song_info.poll() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if !self.preferences_open {
            self.playlist_tick(view);
        }

        // Which layout the dock shows depends on fullscreen (when the
        // separate-fullscreen-layout preference is on), so settle that before
        // drawing anything.
        self.sync_layout_slot();

        self.browser.clear_hovered();
        self.hovered_playlist_entry = None;

        if self.dedicated.is_some() {
            self.dedicated_ui(ui, &shared);
        } else {
            egui::Panel::top("top_bar").show(ui, |ui| self.top_bar_ui(ui));
            egui::Panel::bottom("controls_panel").show(ui, |ui| {
                self.controls_bar_ui(ui, view);
            });
            self.dock_ui(ui, &shared);
            self.autosave_layout(&ctx);
            self.preferences_ui(&ctx);
            self.updates_ui(&ctx);
            self.log_ui(&ctx);
            self.rename_ui(&ctx);
            self.credits_ui(&ctx);
            self.song_info_ui(&ctx);
            self.history_ui(&ctx);
        }
        self.editor_tick(&ctx);
        // Outside the layout: F9 can ask for ffmpeg from the dedicated
        // fullscreen too.
        self.ffmpeg_prompt_ui(&ctx);
        self.poll_recordings(view);
        if self.status != self.logged_status {
            log::info!(target: "synththing::status", "{}", self.status);
            self.logged_status = self.status.clone();
        }
        self.apply_cursor_confinement(&ctx);
        self.keep_loaded(&ctx);

        drag_ghost_ui(&ctx);

        // Keep the progress slider and visualizer animating without input;
        // stay fully idle otherwise. While a playlist is playing, keep
        // polling too, so the end of a track is noticed and the next one
        // starts even with the window idle in the background.
        // The visualizer went away (tab closed or hidden): its script can't
        // let go of notes it's holding any more, so let go for it.
        if self.visualizer_was_drawn && !self.visualizer_drawn {
            self.send(AudioCommand::Live(crate::live::LiveCommand::StopAll));
        }
        self.visualizer_was_drawn = self.visualizer_drawn;

        if self.dedicated.is_some() || self.is_open(Section::Visualizer) {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if !view.paused && (view.has_midi || view.has_audio_file) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    /// Last chance to save a layout change the once-a-second autosave
    /// hasn't caught yet.
    fn on_exit(&mut self) {
        self.flush_editor();
        // Don't lose a recording to closing the app: write it out first.
        if let Some(recorder) = self.recording.recorder.take()
            && let Err(e) = recorder.finish_now()
        {
            log::warn!("recording lost on exit: {e:#}");
        }
        self.recording.wait_for_saves();
        if serde_json::to_string(&self.dock).unwrap_or_default() != self.saved_layout_json {
            self.save_layout();
        }
    }
}

/// Draws each section's tab for `DockArea`.
struct SectionTabs<'a> {
    app: &'a mut App,
    shared: &'a PlaybackShared,
    /// A tab was closed this frame, so the layout should be saved.
    closed: bool,
}

impl TabViewer for SectionTabs<'_> {
    type Tab = Section;

    fn id(&mut self, tab: &mut Section) -> egui::Id {
        egui::Id::new(("section_tab", *tab))
    }

    fn title(&mut self, tab: &mut Section) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Section) {
        let app = &mut *self.app;
        match tab {
            Section::Songs => app.songs_ui(ui),
            Section::Soundfonts => app.soundfont_ui(ui),
            Section::Playlists => app.playlist_panel_ui(ui),
            Section::Visualizer => {
                app.visualizer_preview_ui(ui, self.shared.notes.clone(), self.shared.view.clone());
            }
            Section::Editor => app.editor_section_ui(ui),
            Section::Settings => app.settings_ui(ui),
            Section::Reference => app.docs_ui(ui),
        }
    }

    /// Closing a tab just hides the section, all its state lives in `App`,
    /// not the tab, and View brings it back.
    fn on_close(&mut self, _tab: &mut Section) -> OnCloseResponse {
        self.closed = true;
        OnCloseResponse::Close
    }

    /// Every section scrolls its own content where it needs to; a second,
    /// outer scroll area from the dock would just fight it.
    fn scroll_bars(&self, _tab: &Section) -> [bool; 2] {
        [false, false]
    }
}

/// Reference text as one wrapping label: `prefix`, then `text` with its
/// `inline code` spans in the code font and background.
fn inline_code_job(ui: &egui::Ui, prefix: &str, text: &str) -> egui::text::LayoutJob {
    let body = egui::TextFormat {
        font_id: egui::TextStyle::Body.resolve(ui.style()),
        color: ui.visuals().text_color(),
        ..Default::default()
    };
    let code = egui::TextFormat {
        font_id: egui::TextStyle::Monospace.resolve(ui.style()),
        color: ui.visuals().text_color(),
        background: ui.visuals().code_bg_color,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(prefix, 0.0, body.clone());
    for (span, is_code) in lua_docs::inline_spans(text) {
        job.append(span, 0.0, if is_code { code.clone() } else { body.clone() });
    }
    job
}

/// A song's length and an info button (greyed out when the file has no
/// details to show), for the end of a list row; laid out right to left.
fn song_info_widgets(ui: &mut egui::Ui, cache: &mut SongInfoCache, path: &Path, open: &mut Option<PathBuf>) {
    let info = cache.get(path);
    let has_details = info.as_ref().is_some_and(|i| !i.details.is_empty());
    let button = ui
        .add_enabled(has_details, egui::Button::new("i").small())
        .on_hover_text("Song info")
        .on_disabled_hover_text(if info.is_none() { "Reading..." } else { "No details for this file" });
    if button.clicked() {
        *open = Some(path.to_path_buf());
    }
    let length = match &info {
        None => "...".to_string(),
        Some(info) => info.length.map(format_length).unwrap_or_else(|| "--:--".to_string()),
    };
    ui.weak(length);
}

/// Char offset of the start of 1-based `line` in `text` (the end, if
/// `text` has fewer lines).
fn char_index_of_line(text: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    let mut newlines = 0;
    for (index, c) in text.chars().enumerate() {
        if c == '\n' {
            newlines += 1;
            if newlines == line - 1 {
                return index + 1;
            }
        }
    }
    text.chars().count()
}

/// Show `path` in the system file manager (selected, on Windows).
fn open_in_file_manager(path: &Path) {
    let result = if cfg!(windows) {
        std::process::Command::new("explorer").arg("/select,").arg(path).spawn()
    } else {
        let dir = path.parent().unwrap_or(path);
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        std::process::Command::new(opener).arg(dir).spawn()
    };
    if let Err(e) = result {
        log::warn!("couldn't open the file manager: {e}");
    }
}

/// While something is being dragged, a small label following the pointer
/// saying what, egui's drag-and-drop carries a payload but draws nothing
/// for it on its own.
fn drag_ghost_ui(ctx: &egui::Context) {
    use egui::DragAndDrop;
    let text = if let Some(file) = DragAndDrop::payload::<DraggedFile>(ctx) {
        nice_name(&file.0)
    } else if let Some(sf) = DragAndDrop::payload::<DraggedSoundfont>(ctx) {
        format!("SoundFont: {}", nice_name(&sf.0))
    } else if let Some(entry) = DragAndDrop::payload::<playlist_panel::DraggedEntry>(ctx) {
        nice_name(&entry.path)
    } else {
        return;
    };
    let Some(pos) = ctx.pointer_latest_pos() else {
        return;
    };
    egui::Area::new(egui::Id::new("drag_ghost"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + egui::vec2(14.0, 10.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(text);
            });
        });
}

/// The identifier (letters/digits/underscore) touching char offset
/// `char_index` into `source`, preferring the word starting at/after that
/// offset, falling back to the one ending at/before it (covers landing just
/// past the last character of a word). `None` if neither side is an
/// identifier character at all (whitespace, punctuation, ...).
fn identifier_at(source: &str, char_index: usize) -> Option<String> {
    let chars: Vec<char> = source.chars().collect();
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';

    let (mut start, mut end);
    if chars.get(char_index).is_some_and(|&c| is_ident(c)) {
        start = char_index;
        end = char_index;
        while end < chars.len() && is_ident(chars[end]) {
            end += 1;
        }
    } else if char_index > 0 && chars.get(char_index - 1).is_some_and(|&c| is_ident(c)) {
        start = char_index;
        end = char_index;
    } else {
        return None;
    }
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }

    (start < end).then(|| chars[start..end].iter().collect())
}

fn playback_state_text(view: &EngineView) -> &'static str {
    if !view.has_midi && !view.has_audio_file {
        "no song"
    } else if view.has_midi && !view.has_soundfont {
        "no soundfont"
    } else if view.finished {
        "done"
    } else if view.paused {
        "paused"
    } else {
        "playing"
    }
}

/// After removing item `removed`, adjust an "active" index that pointed into
/// the same list.
fn shift_active(active: Option<usize>, removed: usize) -> Option<usize> {
    match active {
        Some(a) if a == removed => None,
        Some(a) if a > removed => Some(a - 1),
        other => other,
    }
}

fn fmt_time(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn is_midi_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| MIDI_EXTENSIONS.iter().any(|m| ext.eq_ignore_ascii_case(m)))
}
