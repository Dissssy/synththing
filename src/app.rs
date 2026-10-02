//! GUI state and layout, built on eframe/egui. Talks to the render thread
//! (`audio.rs`) over a command channel and reads playback state back from a
//! shared snapshot, it never touches the engine or audio directly.
//!
//! Layout: a menu bar on top, the playback controls always at the bottom,
//! and the sections in between as dockable tabs (egui_dock), drag them
//! around, split, float them out into windows. Closing a tab only hides that
//! section; the View menu brings it back (see `layout.rs` for where).

mod loading;
mod playlist_panel;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::{egui, Frame};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, DockState, TabViewer};

use crate::audio::{AudioCommand, PlaybackShared, DEFAULT_BUFFER_MS, MAX_BUFFER_MS, MIN_BUFFER_MS};
use crate::config::{self, nice_name, Config, DEFAULT_PRELOAD_EXPIRY_SECS, PRELOAD_EXPIRY_RANGE};
use crate::engine::{EngineView, MAX_SPEED, MIN_SPEED};
use crate::filebrowser::{DraggedFile, FileBrowser};
use crate::layout::{self, Section};
use crate::loader::{self, Asset, AssetCache, SoundFontProbe};
use crate::lua_completion::{self, CompletionWorker};
use crate::lua_docs;
use crate::lua_highlight;
use crate::lua_visualizer::{
    self, DebugVar, LogLevel, LuaVisualizer, SettingDescriptor, SettingKind, SettingValue,
};
use crate::playlist::{Library, LoopMode, NowPlaying, Rng};
use crate::updater::{self, UpdateState, Updater};
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

/// How often (seconds) the dock layout is checked for changes to save —
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
    /// Whether the cursor is currently confined to the window for a script.
    cursor_confined: bool,
    updater: Updater,
    updates_open: bool,
    /// The startup update check is running: show its result only if it
    /// finds something new (and not a skipped version).
    launch_update_check: bool,
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
        let status = match playlist_errors.first() {
            Some(e) => format!("Couldn't read a playlist ({e})"),
            None => "Click a song to play it, click a soundfont to load it.".to_string(),
        };

        Self {
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
            loop_mode: LoopMode::Off,
            shuffle: false,
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
            cursor_confined: false,
            updater: Updater::new(),
            updates_open: false,
            launch_update_check: false,
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
        }
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

    /// Save the layout if it's changed (tabs dragged, dividers moved) —
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
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                self.editor_text = source.clone();
                self.completion.request(source.clone());
                self.editor_hover_info = None;
                let visualizer = self.visualizer.visualizer_mut();
                visualizer.set_path(Some(path));
                visualizer.set_source(source);
                self.active_script = Some(idx);
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
            }
            if ui.selectable_label(self.shuffle, "Shuffle").clicked() {
                self.shuffle = !self.shuffle;
                // Start a fresh pass from whatever's playing now.
                if let Some(np) = &mut self.now_playing {
                    np.history = vec![np.entry];
                }
                self.plan_upcoming();
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
                ui.separator();
                ui.weak(format!("synththing v{}", env!("CARGO_PKG_VERSION")));
            });
        });
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
        let mut open = true;
        let mut close = false;
        egui::Window::new("Updates")
            .open(&mut open)
            .collapsible(false)
            .default_width(420.0)
            .show(ctx, |ui| match &state {
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
            });
        if !open || close {
            self.updates_open = false;
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
            if let Some(error) = self.visualizer.visualizer().error() {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
            }
        }
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
        if let Some(path) = self.browser.ui(ui) {
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
            if let Some(path) = self.visualizer.visualizer().path() {
                ui.weak(path.display().to_string());
            }
        });

        if let Some(error) = self.visualizer.visualizer().error() {
            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
        }

        self.channels_ui(ui, &mut notes);

        let mode = if self.fullscreen { DisplayMode::Fullscreen } else { DisplayMode::Window };
        self.show_visualizer(ui, &notes, &playback, mode);
    }

    /// Draw (and run) the visualizer into the rest of `ui`, and pass on
    /// whatever it asked for.
    fn show_visualizer(&mut self, ui: &mut egui::Ui, notes: &NotesSnapshot, playback: &EngineView, mode: DisplayMode) {
        self.visualizer_output = self.visualizer.show(ui, &self.tap, notes, playback, self.preferences_open, mode);

        // A script can ask to mute/unmute a channel itself (e.g. a game
        // script silencing a dead player's channel), same command the GUI's
        // own checkboxes send, so it's subject to the same "never disable
        // the last channel" rule.
        for (channel, enabled) in self.visualizer.visualizer_mut().take_channel_requests() {
            self.send(AudioCommand::SetChannelEnabled(channel, enabled));
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
        });
        if restart {
            self.restart_script();
        }
        if let Some(idx) = picked_idx {
            self.load_script(idx);
        }
        if let Some(source) = restore_bundled {
            self.editor_text = source;
            self.recompile_editor();
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

    /// The script source editor: syntax-highlighted, scrollable so a long
    /// script doesn't just keep growing the panel, with a "Functions"
    /// insert-at-cursor list and hover/F1 lookup info against the same host
    /// API table (see `lua_completion`), including `debug_locals()` itself
    /// now; calling it by hand, from wherever in the script you want a
    /// snapshot, is the whole interface. (An earlier version added a gutter
    /// with clickable per-line markers to inject the call automatically —
    /// cut after it turned out laggy, a button per source line adds up, and
    /// lining a hand-rolled gutter up pixel-for-pixel with TextEdit's own
    /// line layout wasn't worth what it bought.)
    fn visualizer_editor_ui(&mut self, ui: &mut egui::Ui) {
        let editor_id = egui::Id::new(SCRIPT_EDITOR_ID);

        if let Some((word, detail)) = &self.editor_hover_info {
            ui.horizontal(|ui| {
                ui.strong(word);
                ui.label(detail);
            });
        } else {
            ui.weak("Hover a function, or press F1 at the text cursor, for info about it.");
        }

        ui.horizontal(|ui| {
            let mut insert = None;
            egui::ComboBox::from_id_salt("visualizer_editor_functions")
                .selected_text("Functions")
                .show_ui(ui, |ui| {
                    for suggestion in self.completion.suggestions() {
                        if ui
                            .selectable_label(false, &suggestion.label)
                            .on_hover_text(&suggestion.detail)
                            .clicked()
                        {
                            insert = Some(suggestion.label);
                        }
                    }
                });
            if let Some(name) = insert {
                self.insert_at_editor_cursor(ui.ctx(), editor_id, &name);
            }
        });

        egui::ScrollArea::vertical()
            .id_salt("visualizer_editor_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                    lua_highlight::layout(ui, buf.as_str(), wrap_width)
                };
                let output = egui::TextEdit::multiline(&mut self.editor_text)
                    .id(editor_id)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter)
                    .show(ui);

                if output.response.changed() {
                    self.recompile_editor();
                }

                // Mouse hovering a known identifier.
                if output.response.hovered()
                    && let Some(pointer) = ui.input(|i| i.pointer.hover_pos())
                {
                    let local = pointer - output.galley_pos;
                    let char_index = output.galley.cursor_from_pos(local).index.0;
                    if let Some(word) = identifier_at(&self.editor_text, char_index)
                        && let Some(detail) = lua_completion::lookup(&word)
                    {
                        self.editor_hover_info = Some((word, detail.to_string()));
                    }
                }

                // F1 at the text (writing) cursor's position, same display,
                // different trigger, for when the mouse isn't the one doing
                // the pointing.
                if output.response.has_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::F1))
                    && let Some(range) = output.cursor_range
                {
                    let char_index = range.primary.index.0;
                    self.editor_hover_info = identifier_at(&self.editor_text, char_index)
                        .and_then(|word| lua_completion::lookup(&word).map(|d| (word, d.to_string())));
                }
            });
    }

    /// Splice `text` into the editor at the text cursor's last-known
    /// position (from the previous frame, the editor widget itself hasn't
    /// drawn yet this frame when "Functions" is above it) and leave the
    /// cursor right after the inserted text.
    fn insert_at_editor_cursor(&mut self, ctx: &egui::Context, editor_id: egui::Id, text: &str) {
        let mut state = egui::widgets::text_edit::TextEditState::load(ctx, editor_id).unwrap_or_default();
        let char_index = state.cursor.char_range().map_or_else(
            || self.editor_text.chars().count(),
            |range| range.primary.index.0,
        );

        let byte_index =
            self.editor_text.char_indices().nth(char_index).map_or(self.editor_text.len(), |(b, _)| b);
        self.editor_text.insert_str(byte_index, text);

        let new_index = char_index + text.chars().count();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(new_index))));
        state.store(ctx, editor_id);

        self.recompile_editor();
    }

    fn recompile_editor(&mut self) {
        self.completion.request(self.editor_text.clone());
        self.visualizer.visualizer_mut().set_source_and_save(self.editor_text.clone());
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

    /// The active script's settings widgets, its `debug_locals()` snapshot,
    /// and its log/error history.
    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let descriptors = self.visualizer.visualizer().settings();
        let mut changed: Option<(String, SettingValue)> = None;
        let mut clear_log = false;

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
                            ui.label(*text);
                            ui.add_space(4.0);
                        }
                        lua_docs::Block::Code(code) => {
                            ui.code(*code);
                            ui.add_space(4.0);
                        }
                        lua_docs::Block::Bullets(items) => {
                            for item in *items {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(format!("- {item}"));
                                });
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
        if !self.preferences_open {
            let in_fullscreen = self.fullscreen || self.dedicated.is_some();
            let (escape, f11, f5) = ctx.input_mut(|i| {
                (
                    in_fullscreen && i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::F11),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::F5),
                )
            });
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
        let typing = ctx.memory(|m| m.focused().is_some()) || self.preferences_open;
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
        }
        self.apply_cursor_confinement(&ctx);
        self.keep_loaded(&ctx);

        drag_ghost_ui(&ctx);

        // Keep the progress slider and visualizer animating without input;
        // stay fully idle otherwise. While a playlist is playing, keep
        // polling too, so the end of a track is noticed and the next one
        // starts even with the window idle in the background.
        if self.dedicated.is_some() || self.is_open(Section::Visualizer) {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if !view.paused && (view.has_midi || view.has_audio_file) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    /// Last chance to save a layout change the once-a-second autosave
    /// hasn't caught yet.
    fn on_exit(&mut self) {
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
