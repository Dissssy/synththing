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
mod sprite_editor;
mod tour;
mod welcome;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    self, Binding, DebugVar, LogLevel, PlaybackRequest, SettingDescriptor, SettingKind, SettingValue,
};
use crate::playlist::{Library, LoopMode, NowPlaying, Rng};
use crate::song_info::{format_length, SongInfoCache};
use crate::updater::{self, UpdateState, Updater};
use crate::watch::{FolderWatch, Watched};
use crate::changelog;
use crate::script_host::{self, ScriptHost};
use crate::visualizer::{DisplayMode, NotesSnapshot, SampleTap, ShowOutput, VisualizerPanel};

const VISUALIZER_WIDTH: usize = 800;
const VISUALIZER_HEIGHT: usize = 320;

/// Extensions the song browser lists. MIDI files drive the synth; everything
/// else is decoded as plain audio (rodio/symphonia figure out the actual
/// format themselves, this list is just what shows up in the picker).
const SONG_EXTENSIONS: &[&str] = &["mid", "midi", "wav", "mp3", "ogg", "flac", "m4a", "aac"];
const MIDI_EXTENSIONS: &[&str] = &["mid", "midi"];

/// The songs listed and added from folders: MIDI files, plus audio files
/// when that's turned on in Preferences.
fn listed_song_extensions(config: &Config) -> &'static [&'static str] {
    if config.show_audio_files { SONG_EXTENSIONS } else { MIDI_EXTENSIONS }
}

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
    /// Sent to the script thread (which saves the script's data first):
    /// the answer, once it comes.
    waiting: Option<std::sync::mpsc::Receiver<Result<PathBuf, String>>>,
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
    visualizer: VisualizerPanel,
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
    /// The changelog window: `Some(since)` lists only the versions newer
    /// than that (What's new, after an update), `None` all of them
    /// (Help > Changelog...).
    changelog: Option<Option<changelog::Version>>,
    /// The welcome window and the starter pack download.
    welcome: welcome::Welcome,
    /// The guided tour (Help > Tour).
    tour: tour::Tour,
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
    /// View > Layout > Save current layout as...: the name being typed.
    layout_save: Option<String>,
    /// A very large MIDI waiting for "play anyway" (path, notes), and the
    /// last one that got it.
    heavy_prompt: Option<(PathBuf, usize)>,
    heavy_confirmed: Option<PathBuf>,
    /// Game controllers, read every frame; this frame's input.
    gamepads: crate::gamepad::Gamepads,
    pad_frame: crate::gamepad::PadFrame,
    /// Songs sent to the audio thread, and the latest one waiting to reach
    /// the script: (its load number, path, notes, id).
    loads_sent: u64,
    pending_script_song: Option<(u64, PathBuf, Arc<crate::midi_notes::NoteList>, String)>,
    /// The reference spot to show (section, block), when it was asked for,
    /// and whether it still needs scrolling to.
    docs_target: Option<((usize, usize), Instant, bool)>,
    /// A section to bring to the front once the dock is free.
    pending_focus: Option<Section>,
    /// The Sprite Editor tab (see `app/sprite_editor.rs`).
    sprite: sprite_editor::SpriteEditorState,
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
        let browser = FileBrowser::new("songs", config.browse_start_dir(), listed_song_extensions(&config));

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
        let mut config = config;
        if config.layout.is_none() && config.active_layout.is_none() {
            // A first launch starts on the Listening layout.
            config.active_layout = layout::PRESETS.first().map(|p| p.name.to_string());
        }
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
                ScriptHost::spawn(source.clone(), active_path, sample_rate),
                VISUALIZER_WIDTH,
                VISUALIZER_HEIGHT,
            ),
            scripts_dir,
            available_scripts,
            active_script,
            editor_text: source,
            completion: CompletionWorker::new(lua_visualizer::host_global_names()),
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
            changelog: None,
            welcome: welcome::Welcome::default(),
            tour: tour::Tour::default(),
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
            layout_save: None,
            heavy_prompt: None,
            heavy_confirmed: None,
            gamepads: crate::gamepad::Gamepads::new(),
            pad_frame: crate::gamepad::PadFrame::default(),
            loads_sent: 0,
            pending_script_song: None,
            docs_target: None,
            pending_focus: None,
            sprite: sprite_editor::SpriteEditorState::default(),
            sample_rate,
            editor: editor::EditorState::default(),
            editor_saved: String::new(),
            recording: recording::Recording::new(),
            last_recording: None,
        };
        if let Some(path) = app.visualizer.script().path().map(Path::to_path_buf) {
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

    /// After an update, show what's new since the version that ran last,
    /// and remember this one. A fresh install shows nothing; a config from
    /// before versions were recorded (`ran_before`, no `last_version`)
    /// gets this version's changes.
    pub fn whats_new_on_launch(&mut self, ran_before: bool) {
        let Some(current) = changelog::current_version() else { return };
        let last = match self.config.last_version.as_deref() {
            Some(text) => changelog::parse_version(text),
            None if ran_before => changelog::previous_version(current),
            None => None,
        };
        if let Some(last) = last
            && last < current
            && changelog::newer_than(last).next().is_some()
        {
            self.changelog = Some(Some(last));
        }
        let version = changelog::format_version(current);
        if self.config.last_version.as_deref() != Some(version.as_str()) {
            self.config.last_version = Some(version);
            if let Err(e) = self.config.save() {
                log::warn!("couldn't save the config: {e:#}");
            }
        }
    }

    /// What's new (since a version), or the whole changelog.
    fn changelog_ui(&mut self, ctx: &egui::Context) {
        let Some(since) = self.changelog else { return };
        let mut close = false;
        let mut show_all = false;
        let response = egui::Modal::new(egui::Id::new("changelog")).show(ctx, |ui| {
            ui.set_width(560.0);
            match since {
                Some(since) => {
                    ui.heading("What's new");
                    ui.weak(format!(
                        "synththing v{}, since v{} (the version you had)",
                        env!("CARGO_PKG_VERSION"),
                        changelog::format_version(since)
                    ));
                }
                None => {
                    ui.heading("Changelog");
                }
            }
            ui.add_space(6.0);
            let releases: Vec<&changelog::Release> = match since {
                Some(since) => changelog::newer_than(since).collect(),
                None => changelog::releases().iter().collect(),
            };
            egui::ScrollArea::vertical().id_salt("changelog_scroll").max_height(440.0).show(ui, |ui| {
                for release in releases {
                    ui.label(egui::RichText::new(&release.heading).heading().size(17.0));
                    for block in &release.blocks {
                        match block {
                            changelog::Block::Heading(text) => {
                                ui.add_space(2.0);
                                ui.strong(text);
                            }
                            changelog::Block::Bullet(text) => {
                                ui.label(inline_code_job(ui, "- ", text));
                            }
                            changelog::Block::Paragraph(text) => {
                                ui.label(inline_code_job(ui, "", text));
                            }
                        }
                    }
                    ui.add_space(10.0);
                }
            });
            ui.add_space(6.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
                if since.is_some() && ui.button("Full changelog").clicked() {
                    show_all = true;
                }
            });
        });
        if show_all {
            self.changelog = Some(None);
        } else if close || response.should_close() {
            self.changelog = None;
        }
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

    /// Open `section` if it's closed, and make it the shown tab of its
    /// group.
    fn reveal_section(&mut self, section: Section) {
        if self.dock_in_use {
            self.pending_layout.push((section, true));
            self.pending_focus = Some(section);
            return;
        }
        self.set_open(section, true);
        if let Some(path) = self.dock.find_tab(&section) {
            let _ = self.dock.set_active_tab(path);
        }
    }

    /// Show where `name` is documented in the Scripting Reference.
    fn open_reference(&mut self, name: &str) -> bool {
        let Some(target) = lua_docs::find(name) else { return false };
        self.docs_target = Some((target, Instant::now(), true));
        self.reveal_section(Section::Reference);
        true
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
        self.store_in_active_layout();
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save layout: {e}");
        }
    }

    /// The named layout the showing slot is on, if any.
    fn active_layout(&self) -> Option<&String> {
        if self.dock_is_fullscreen_layout {
            self.config.fullscreen_active_layout.as_ref()
        } else {
            self.config.active_layout.as_ref()
        }
    }

    fn set_active_layout(&mut self, name: Option<String>) {
        if self.dock_is_fullscreen_layout {
            self.config.fullscreen_active_layout = name;
        } else {
            self.config.active_layout = name;
        }
    }

    /// Changes to the arrangement go into the active named layout: a
    /// built-in's override (dropped when it matches the original again),
    /// or a saved layout itself.
    fn store_in_active_layout(&mut self) {
        let Some(name) = self.active_layout().cloned() else { return };
        if let Some(preset) = layout::PRESETS.iter().find(|p| p.name == name) {
            if layout::same_arrangement(&self.dock, &(preset.build)()) {
                self.config.layout_overrides.remove(&name);
            } else {
                self.config.layout_overrides.insert(name, self.dock.clone());
            }
        } else if let Some(saved) = self.config.layout_presets.iter_mut().find(|p| p.name == name) {
            saved.dock = self.dock.clone();
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

    /// View > Layout: the built-in arrangements (with Reset when changed),
    /// the user's saved ones, and saving the current one. The active one is
    /// marked; changes made while it's active are kept in it.
    fn layout_menu_ui(&mut self, ui: &mut egui::Ui) {
        let active = self.active_layout().cloned();
        let mut switch = None;
        let mut reset = None;
        for preset in layout::PRESETS {
            let changed = self.config.layout_overrides.contains_key(preset.name);
            ui.horizontal(|ui| {
                let label = if changed { format!("{} (changed)", preset.name) } else { preset.name.to_string() };
                if ui
                    .selectable_label(active.as_deref() == Some(preset.name), label)
                    .on_hover_text(preset.description)
                    .clicked()
                {
                    switch = Some(preset.name.to_string());
                    ui.close();
                }
                if changed && ui.small_button("Reset").on_hover_text("Back to how this layout comes").clicked() {
                    reset = Some(preset.name.to_string());
                }
            });
        }
        let mut remove = None;
        if !self.config.layout_presets.is_empty() {
            ui.separator();
            for (n, saved) in self.config.layout_presets.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.selectable_label(active.as_deref() == Some(saved.name.as_str()), &saved.name).clicked() {
                        switch = Some(saved.name.clone());
                        ui.close();
                    }
                    if ui.small_button("x").on_hover_text("Remove this saved layout").clicked() {
                        remove = Some(n);
                    }
                });
            }
        }
        if active.is_none() {
            ui.separator();
            ui.weak("Showing: your own arrangement (Custom)");
        }
        ui.separator();
        if ui.button("Save current layout as...").clicked() {
            self.layout_save = Some(String::new());
            ui.close();
        }

        if let Some(name) = reset {
            self.config.layout_overrides.remove(&name);
            if active.as_deref() == Some(name.as_str())
                && let Some(preset) = layout::PRESETS.iter().find(|p| p.name == name)
            {
                self.apply_layout((preset.build)());
            } else if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save the layouts: {e}");
            }
            self.status = format!("Reset the {name} layout.");
        }
        if let Some(n) = remove {
            let removed = self.config.layout_presets.remove(n);
            if active.as_deref() == Some(removed.name.as_str()) {
                self.set_active_layout(None);
            }
            self.status = format!("Removed the saved layout \"{}\".", removed.name);
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save the layout list: {e}");
            }
        }
        if let Some(name) = switch {
            self.switch_layout(&name);
        }
    }

    /// Show the named layout (as the user last left it), after keeping the
    /// current arrangement in the layout it belongs to.
    fn switch_layout(&mut self, name: &str) {
        if self.dock_in_use {
            return;
        }
        self.save_layout();
        let dock = if let Some(preset) = layout::PRESETS.iter().find(|p| p.name == name) {
            self.config.layout_overrides.get(name).cloned().map(layout::sanitize).unwrap_or_else(preset.build)
        } else if let Some(saved) = self.config.layout_presets.iter().find(|p| p.name == name) {
            layout::sanitize(saved.dock.clone())
        } else {
            return;
        };
        self.set_active_layout(Some(name.to_string()));
        self.apply_layout(dock);
        self.status = format!("Layout: {name}.");
    }

    /// Switch the arrangement to `dock` (in whichever slot is showing: the
    /// windowed layout, or fullscreen's own) and remember it.
    fn apply_layout(&mut self, dock: egui_dock::DockState<Section>) {
        if self.dock_in_use {
            return;
        }
        self.dock = dock;
        if self.is_open_in_dock(Section::Editor) {
            // It already has the reference where the layout put it.
            self.editor_opened_this_run = true;
        }
        self.refresh_open_sections();
        self.save_layout();
    }

    fn is_open_in_dock(&self, section: Section) -> bool {
        layout::is_open(&self.dock, section)
    }

    /// View > Layout > Save current layout as...: name it.
    fn layout_save_ui(&mut self, ctx: &egui::Context) {
        let Some(name) = &mut self.layout_save else { return };
        let mut save = false;
        let mut close = false;
        let taken = self.config.layout_presets.iter().any(|p| p.name.eq_ignore_ascii_case(name.trim()));
        let built_in = layout::PRESETS.iter().any(|p| p.name.eq_ignore_ascii_case(name.trim()));
        let response = egui::Modal::new(egui::Id::new("save_layout")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("Save layout");
            ui.label("Name:");
            let field = ui.text_edit_singleline(name);
            field.request_focus();
            if built_in {
                ui.colored_label(ui.visuals().warn_fg_color, "That's a built-in layout's name; pick another.");
            } else if taken {
                ui.weak("A saved layout has this name; saving replaces it.");
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                save = true;
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.add_enabled(!name.trim().is_empty() && !built_in, egui::Button::new("Save")).clicked() {
                    save = true;
                }
            });
        });
        if save && !name.trim().is_empty() && !built_in {
            let name = name.trim().to_string();
            let saved = layout::SavedLayout { name: name.clone(), dock: self.dock.clone() };
            match self.config.layout_presets.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&name)) {
                Some(existing) => *existing = saved,
                None => self.config.layout_presets.push(saved),
            }
            // From now on, changes go into it.
            self.set_active_layout(Some(name.clone()));
            self.status = match self.config.save() {
                Ok(()) => format!("Saved the layout \"{name}\" (View > Layout)."),
                Err(e) => format!("Couldn't save the layout: {e}"),
            };
            close = true;
        }
        if close || response.should_close() {
            self.layout_save = None;
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
                self.visualizer.script_mut().load(Some(path.clone()), source);
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
                self.set_loop_mode(self.loop_mode.cycle());
            }
            if ui.selectable_label(self.shuffle, "Shuffle").clicked() {
                self.set_shuffle(!self.shuffle);
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
        self.starter_progress_ui(ui);
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
                section_checkbox(ui, Section::Sprites);
                ui.separator();
                ui.menu_button("Layout", |ui| self.layout_menu_ui(ui));
            });
            for (section, open) in toggled {
                self.set_open(section, open);
            }
            ui.menu_button("Help", |ui| {
                if ui.button("Welcome...").on_hover_text("The welcome window, and the starter pack").clicked() {
                    self.open_welcome();
                }
                if ui.button("Tour").on_hover_text("A walk through the app, one thing at a time").clicked() {
                    self.start_tour();
                }
                if ui.button("Check for updates...").clicked() {
                    self.launch_update_check = false;
                    self.updates_open = true;
                    self.updater.check();
                }
                if ui.button("Changelog...").clicked() {
                    self.changelog = Some(None);
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
        let answer = rename.waiting.as_ref().and_then(|waiting| waiting.try_recv().ok());
        let waiting = rename.waiting.is_some() && answer.is_none();
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
            if waiting {
                ui.weak("Renaming, once the script finishes what it's doing...");
            } else {
                ui.weak("Its settings file is renamed along with it.");
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.add_enabled(!waiting, egui::Button::new("Rename")).clicked() {
                    submit = true;
                }
                if ui.add_enabled(!waiting, egui::Button::new("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
        if let Some(answer) = answer {
            rename.waiting = None;
            let path = rename.path.clone();
            match answer {
                Ok(new_path) => {
                    editor::rename_history(&path, &new_path);
                    self.available_scripts = lua_visualizer::list_scripts(&self.scripts_dir);
                    self.active_script = self.available_scripts.iter().position(|p| *p == new_path);
                    self.visualizer.script_mut().renamed(new_path.clone());
                    self.status = format!("Renamed script to {}", lua_visualizer::display_name(&new_path));
                    self.rename = None;
                }
                Err(e) => rename.error = Some(e),
            }
            return;
        }
        if waiting {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        if cancel || response.should_close() {
            self.rename = None;
            return;
        }
        if !submit {
            return;
        }
        let name = rename.name.clone();
        // Edits still waiting are saved under the old name first; the
        // script thread saves the script's data, then moves it all along.
        self.flush_editor();
        let answer = self.visualizer.script().rename(name);
        if let Some(rename) = &mut self.rename {
            rename.waiting = Some(answer);
        }
    }

    /// The running script's error, if any, with a "Go to line" button when
    /// it points at a line. `open_editor`: the button also opens the editor
    /// (for the copy shown above the visualizer).
    fn script_error_ui(&mut self, ui: &mut egui::Ui, open_editor: bool) {
        let Some(error) = self.visualizer.script().error() else {
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
            || self.changelog.is_some()
            || self.welcome.open
            || self.song_info_open.is_some()
            || self.recording.prompt_open
            || self.editor.history_open
            || self.layout_save.is_some()
            || self.heavy_prompt.is_some()
            || self.sprite.new_sprite.is_some()
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
        let current = self.visualizer.script().path().map(Path::to_path_buf);
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
                self.visualizer.script_mut().set_source(text, None);
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
        if let Some(section) = self.pending_focus.take() {
            self.reveal_section(section);
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
        let picker = ui.scope(|ui| self.script_picker_ui(ui)).response.rect;
        self.tour.mark(tour::Target::ScriptPicker, picker);

        ui.horizontal(|ui| {
            let fullscreen = ui
                .button("Fullscreen visualizer")
                .on_hover_text("Just the visualizer, filling the screen, with mouse and keyboard going to the script. F11 toggles it, Esc exits.");
            self.tour.mark(tour::Target::Fullscreen, fullscreen.rect);
            if fullscreen.clicked() {
                let ctx = ui.ctx().clone();
                self.enter_dedicated(&ctx);
            }
            self.recording_buttons_ui(ui, &playback);
            if let Some(path) = self.visualizer.script().path() {
                ui.weak(path.display().to_string());
            }
        });

        self.script_error_ui(ui, true);

        let channels = ui.scope(|ui| self.channels_ui(ui, &mut notes)).response.rect;
        self.tour.mark(tour::Target::Channels, channels);

        let mode = if self.fullscreen { DisplayMode::Fullscreen } else { DisplayMode::Window };
        self.show_visualizer(ui, &notes, &playback, mode);
    }

    /// Draw (and run) the visualizer into the rest of `ui`, and pass on
    /// whatever it asked for.
    fn show_visualizer(&mut self, ui: &mut egui::Ui, notes: &NotesSnapshot, playback: &EngineView, mode: DisplayMode) {
        let transport = self.script_transport();
        self.visualizer.script_mut().set_app_log(self.config.script_log_to_app);
        self.visualizer.set_pads(self.pad_frame.clone());
        let fixed_size = self.recording.frame_size();
        let (hold, timestep) = self.pace_recording();
        self.visualizer_output = self.visualizer.show(
            ui,
            &self.tap,
            notes,
            playback,
            transport,
            self.preferences_open,
            mode,
            fixed_size,
            hold,
            timestep,
        );
        self.feed_recording(ui);
        self.visualizer_drawn = true;
        let effects = self.visualizer.take_effects();

        // A script can ask to mute/unmute a channel itself (e.g. a game
        // script silencing a dead player's channel), same command the GUI's
        // own checkboxes send, so it's subject to the same "never disable
        // the last channel" rule.
        for (channel, enabled) in effects.channel_requests {
            self.send(AudioCommand::SetChannelEnabled(channel, enabled));
        }

        // Notes the script played, for the live synth.
        for command in effects.live_commands {
            self.send(AudioCommand::Live(command));
        }

        // set_paused / seek from the script. `paused` tracks the effect of
        // earlier requests this frame, since the engine's view won't
        // reflect them until next frame.
        let mut paused = playback.paused;
        for request in effects.playback_requests {
            match request {
                PlaybackRequest::Pause(pause) if pause != paused => {
                    self.send(AudioCommand::TogglePause);
                    paused = pause;
                }
                PlaybackRequest::Pause(_) => {}
                PlaybackRequest::Seek(seconds) => self.send(AudioCommand::Seek(seconds)),
                PlaybackRequest::PlayTrack(entry) => {
                    if let Some(list) = self.script_playlist() {
                        self.play_entry(list, entry);
                    }
                }
                PlaybackRequest::NextTrack => self.next_track(),
                PlaybackRequest::PreviousTrack => self.previous_track(playback.position),
                PlaybackRequest::Speed(speed) => self.send(AudioCommand::SetSpeed(speed)),
                PlaybackRequest::Loop(mode) => self.set_loop_mode(mode),
                PlaybackRequest::Shuffle(on) => self.set_shuffle(on),
            }
        }
    }

    /// A song was just sent to the audio thread: the script gets it (its
    /// notes, id, path and a new `song_loads`) once the audio thread reports
    /// it loaded, so everything `playback()` says changes in the same frame
    /// (`sync_script_song`).
    pub(super) fn song_for_script(&mut self, path: &Path, notes: Arc<crate::midi_notes::NoteList>, id: String) {
        self.loads_sent += 1;
        self.pending_script_song = Some((self.loads_sent, path.to_path_buf(), notes, id));
    }

    /// Hand the script the song the audio thread now reports as loaded.
    fn sync_script_song(&mut self, view: &EngineView) {
        if self.pending_script_song.as_ref().is_some_and(|(target, ..)| view.loads >= *target)
            && let Some((_, path, notes, id)) = self.pending_script_song.take()
        {
            self.visualizer.script().set_song(path, id, notes);
        }
    }

    /// The playlist scripts see (`playlist()`): the one playing, else the
    /// one open in the Playlists tab.
    fn script_playlist(&self) -> Option<usize> {
        self.now_playing
            .as_ref()
            .map(|np| np.list)
            .or(self.viewed_playlist)
            .filter(|&l| l < self.playlists.lists.len())
    }

    /// What scripts see of the playlist and the loop/shuffle modes.
    fn script_transport(&mut self) -> lua_visualizer::Transport {
        let playlist = self.script_playlist().map(|list| {
            let stored = &self.playlists.lists[list].playlist;
            let playing = self.now_playing.as_ref().filter(|np| np.list == list);
            let entries = stored
                .entries
                .iter()
                .map(|entry| lua_visualizer::PlaylistEntryView {
                    name: nice_name(&entry.path),
                    length: self.song_info.get(&entry.path).and_then(|info| info.length),
                    missing: entry.missing,
                })
                .collect();
            lua_visualizer::PlaylistView {
                name: stored.name.clone(),
                playing: playing.is_some(),
                current: playing.map(|np| np.entry),
                entries,
            }
        });
        lua_visualizer::Transport { loop_mode: self.loop_mode, shuffle: self.shuffle, playlist }
    }

    fn set_loop_mode(&mut self, mode: LoopMode) {
        if mode != self.loop_mode {
            self.loop_mode = mode;
            self.plan_upcoming();
            self.save_playback_modes();
        }
    }

    fn set_shuffle(&mut self, on: bool) {
        if on != self.shuffle {
            self.shuffle = on;
            // Start a fresh pass from whatever's playing now.
            if let Some(np) = &mut self.now_playing {
                np.history = vec![np.entry];
            }
            self.plan_upcoming();
            self.save_playback_modes();
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
        let name = self.visualizer.script().path().map(nice_name).unwrap_or_else(|| "script".to_string());
        self.status = if self.visualizer.script_mut().restart() {
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

    /// The script has been on one frame (or loading) for a while: say so in
    /// the visualizer's bottom-left corner, with a button to stop it.
    fn script_busy_ui(&mut self, ctx: &egui::Context) {
        let rect = self.visualizer_output.image_rect;
        let Some(busy) = self.visualizer.script().busy_for() else { return };
        if !self.visualizer_drawn || busy < script_host::BUSY_NOTICE_AFTER || !rect.is_positive() {
            return;
        }
        let name = self.visualizer.script().path().map(nice_name).unwrap_or_else(|| "The script".to_string());
        let mut stop = false;
        egui::Area::new(egui::Id::new("script_busy_notice"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(rect.left_bottom() + egui::vec2(12.0, -12.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(egui::Color32::from_rgb(230, 180, 80), "!");
                        ui.label(format!("{name} hasn't finished a frame in {:.1} s", busy.as_secs_f32()));
                        stop = ui
                            .button("Stop")
                            .on_hover_text(format!(
                                "Stop it now (the watchdog would at {} s). Fix it and save, or press Restart (F5), to run it again.",
                                script_host::APP_WATCHDOG_LIMIT.as_secs()
                            ))
                            .clicked();
                    });
                });
            });
        if stop {
            self.visualizer.script().stop();
            self.status = format!("Stopped {name}.");
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
                    // Grouped: the bundled visualizers and games, then the
                    // user's own (and copies of templates and examples).
                    let category_of = |path: &PathBuf| {
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .and_then(lua_visualizer::bundled_category)
                            .filter(|c| matches!(*c, "visualizers" | "games"))
                    };
                    for group in [Some("visualizers"), Some("games"), None] {
                        let members: Vec<(usize, &PathBuf)> = self
                            .available_scripts
                            .iter()
                            .enumerate()
                            .filter(|(_, path)| category_of(path) == group)
                            .collect();
                        if members.is_empty() {
                            continue;
                        }
                        ui.label(egui::RichText::new(lua_visualizer::category_title(group.unwrap_or(""))).weak().small());
                        for (i, path) in members {
                            let name = lua_visualizer::display_name(path);
                            if ui.selectable_label(Some(i) == self.active_script, name).clicked() {
                                picked_idx = Some(i);
                            }
                        }
                    }
                });

            let mut new_from: Option<&'static str> = None;
            egui::ComboBox::from_id_salt("visualizer_new_script")
                .selected_text("New")
                .show_ui(ui, |ui| {
                    let mut heading = "";
                    for (category, name, content) in lua_visualizer::new_script_templates() {
                        if category != heading {
                            heading = category;
                            ui.label(egui::RichText::new(lua_visualizer::category_title(category)).weak().small());
                        }
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
                .script()
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
            if let Some(path) = self.visualizer.script().path()
                && ui.button("Rename").on_hover_text("Rename this script's file (its settings come along)").clicked()
            {
                rename = Some(path.to_path_buf());
            }
        });
        if let Some(path) = rename {
            let name = lua_visualizer::display_name(&path);
            self.rename = Some(RenameScript { path, name, error: None, focused_once: false, waiting: None });
        }
        if restart {
            self.restart_script();
        }
        if let Some(idx) = picked_idx {
            self.load_script(idx);
        }
        if let Some(source) = restore_bundled {
            if let Some(path) = self.visualizer.script().path().map(Path::to_path_buf) {
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
        let mut warn_heavy = self.config.warn_heavy_midi.unwrap_or(true);
        let mut script_log = self.config.script_log_to_app;
        let mut audio_files = self.config.show_audio_files;
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
            ui.strong("Songs");
            ui.checkbox(&mut audio_files, "Show audio files (MP3, WAV, OGG, FLAC, ...)");
            ui.weak(
                "synththing is built around MIDI. Audio files play, and visualizers that only use the \
                 sound (waveform, spectrum) work with them, but there are no notes in them: channel \
                 muting, per-song soundfonts, and visualizers and games that read the notes don't do \
                 anything with them. Off: only MIDI files are listed, and adding a folder adds only \
                 its MIDI files (an audio file dragged in by itself is still added).",
            );

            ui.add_space(10.0);
            ui.strong("Loading");
            ui.checkbox(&mut warn_heavy, "Warn before playing a very large MIDI file").on_hover_text(format!(
                "Over {} notes: scripts that go through every note can stall on them.",
                crate::song_info::thousands(crate::song_info::HEAVY_MIDI_NOTES)
            ));
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
            ui.strong("Scripts");
            ui.checkbox(&mut script_log, "Copy script logs to the app log");
            ui.weak(
                "What visualizer scripts log() shows in their Script Settings tab; with this on, \
                 it also goes to Help > Log... (and the log file), labelled with the script's \
                 name. A script can turn it on for itself (script_options), or for single \
                 messages (log_app).",
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
            || warn_heavy != self.config.warn_heavy_midi.unwrap_or(true)
            || script_log != self.config.script_log_to_app
            || audio_files != self.config.show_audio_files
        {
            self.config.show_audio_files = audio_files;
            self.browser.set_extensions(listed_song_extensions(&self.config));
            self.config.script_log_to_app = script_log;
            self.config.warn_heavy_midi = (!warn_heavy).then_some(false);
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
        let actions = self.visualizer.script().actions();
        if actions.is_empty() {
            self.binding_capture = None;
            return;
        }
        ui.separator();
        ui.strong("Controls");
        ui.weak(
            "Click a binding, then press the new key or controller button. Keys work while the visualizer \
             has focus; controllers while synththing's window does.",
        );
        let pads = &self.pad_frame.connected;
        ui.weak(if pads.is_empty() { "No controller connected.".to_string() } else { format!("Controllers: {}", pads.join(", ")) });

        let mut change: Option<(usize, Vec<Binding>)> = None;
        let mut start_capture: Option<Option<(usize, Option<usize>)>> = None;
        egui::Grid::new("script_controls").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            for (i, action) in actions.iter().enumerate() {
                ui.label(&action.name);
                ui.horizontal_wrapped(|ui| {
                    for (slot, key) in action.bindings.iter().enumerate() {
                        let capturing = self.binding_capture == Some((i, Some(slot)));
                        let label = if capturing { "press a key or button...".to_string() } else { key.label().to_string() };
                        if ui.selectable_label(capturing, label).clicked() {
                            start_capture = Some((!capturing).then_some((i, Some(slot))));
                        }
                        if ui.small_button("x").on_hover_text("Remove this binding").clicked() {
                            let mut bindings = action.bindings.clone();
                            bindings.remove(slot);
                            change = Some((i, bindings));
                        }
                    }
                    let adding = self.binding_capture == Some((i, None));
                    if ui
                        .selectable_label(adding, if adding { "press a key or button..." } else { "+" })
                        .on_hover_text("Add another key or controller button")
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
            let pressed = ui
                .input(|input| {
                    input.events.iter().find_map(|event| match event {
                        egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                        _ => None,
                    })
                })
                .map(Binding::Key)
                .or_else(|| self.pad_frame.pressed.first().copied().map(Binding::Pad));
            if let Some(key) = pressed {
                captured = true;
                self.binding_capture = None;
                let reserved = matches!(key, Binding::Key(k) if crate::visualizer::RESERVED_KEYS.contains(&k));
                if !reserved {
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
            self.visualizer.script_mut().set_action_bindings(i, bindings);
        }
    }

    /// The active script's settings widgets, its `debug_locals()` snapshot,
    /// and its log/error history.
    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let descriptors = self.visualizer.script().settings();
        let mut changed: Option<(String, SettingValue)> = None;
        let mut clear_log = false;

        // The whole tab scrolls: performance, controls, settings, variables
        // and the log.
        egui::ScrollArea::vertical()
            .id_salt("visualizer_settings_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Performance: render time against the 60 fps budget.
                let perf = self.visualizer.script().perf_summary();
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
                    self.visualizer.script_mut().retry_full_frame_rate();
                }
                self.controls_ui(ui);
                ui.separator();

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
                match self.visualizer.script().debug_snapshot() {
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
                        let entries = self.visualizer.script().log_entries();
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
            self.visualizer.script().set_setting(key, value);
        }
        if clear_log {
            self.visualizer.script_mut().clear_log();
        }
    }

    /// The scripting-API reference: one embedded, structured page (see
    /// `lua_docs.rs`) rather than a hosted wiki, the whole API is a few
    /// dozen functions, not a sprawling product.
    fn docs_ui(&mut self, ui: &mut egui::Ui) {
        // A spot asked for from the editor: scrolled to once, highlighted
        // for a moment.
        const HIGHLIGHT_SECS: f32 = 2.5;
        let target = self.docs_target.as_mut();
        let (target_at, fade, mut scroll) = match target {
            Some((at, since, scroll)) => {
                let left = 1.0 - since.elapsed().as_secs_f32() / HIGHLIGHT_SECS;
                (Some(*at), left, std::mem::take(scroll))
            }
            None => (None, 0.0, false),
        };
        egui::ScrollArea::vertical().id_salt("docs_scroll").auto_shrink([false, false]).show(ui, |ui| {
            for (s, section) in lua_docs::sections().iter().enumerate() {
                ui.heading(section.title);
                for (b, block) in section.blocks.iter().enumerate() {
                    let response = ui
                        .vertical(|ui| match block {
                            lua_docs::Block::P(text) => {
                                ui.label(inline_code_job(ui, "", text));
                            }
                            lua_docs::Block::Code(code) => {
                                ui.code(*code);
                            }
                            lua_docs::Block::Bullets(items) => {
                                for item in *items {
                                    ui.label(inline_code_job(ui, "- ", item));
                                }
                            }
                        })
                        .response;
                    if target_at == Some((s, b)) {
                        if std::mem::take(&mut scroll) {
                            response.scroll_to_me(Some(egui::Align::TOP));
                        }
                        if fade > 0.0 {
                            let color = ui.visuals().selection.stroke.color.gamma_multiply(fade);
                            ui.painter().rect_stroke(response.rect.expand(3.0), 3.0, egui::Stroke::new(2.0, color), egui::StrokeKind::Outside);
                            ui.ctx().request_repaint();
                        }
                    }
                    ui.add_space(4.0);
                }
                ui.separator();
            }
        });
        if fade <= 0.0 && target_at.is_some() {
            self.docs_target = None;
        }
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
        self.sync_script_song(view);
        self.pad_frame = self.gamepads.poll(ctx.input(|i| i.focused));
        if !self.pad_frame.down.is_empty() || !self.pad_frame.released.is_empty() {
            ctx.request_repaint();
        }

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
        self.tour.begin_frame();

        if self.dedicated.is_some() {
            self.dedicated_ui(ui, &shared);
        } else {
            egui::Panel::top("top_bar").show(ui, |ui| self.top_bar_ui(ui));
            egui::Panel::bottom("controls_panel").show(ui, |ui| {
                self.controls_bar_ui(ui, view);
                self.tour.mark(tour::Target::Controls, ui.min_rect());
            });
            self.dock_ui(ui, &shared);
            self.autosave_layout(&ctx);
            self.preferences_ui(&ctx);
            self.updates_ui(&ctx);
            self.log_ui(&ctx);
            self.rename_ui(&ctx);
            self.credits_ui(&ctx);
            self.changelog_ui(&ctx);
            self.welcome_ui(&ctx);
            self.song_info_ui(&ctx);
            self.history_ui(&ctx);
            self.layout_save_ui(&ctx);
            self.new_sprite_ui(&ctx);
        }
        self.editor_tick(&ctx);
        // Outside the layout: F9 can ask for ffmpeg from the dedicated
        // fullscreen too.
        self.ffmpeg_prompt_ui(&ctx);
        self.heavy_midi_ui(&ctx);
        self.poll_recordings(view);
        if self.status != self.logged_status {
            log::info!(target: "synththing::status", "{}", self.status);
            self.logged_status = self.status.clone();
        }
        self.apply_cursor_confinement(&ctx);
        self.keep_loaded(&ctx);
        self.poll_starter(&ctx);
        self.script_busy_ui(&ctx);
        self.tour_ui(&ctx, &shared);

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
        // Stop the script thread, which saves the script's data.
        self.visualizer.script_mut().shutdown();
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
        app.tour.mark(tour::Target::Tab(*tab), ui.max_rect());
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
            Section::Sprites => app.sprite_editor_ui(ui),
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
    if let Some(info) = info.filter(|i| i.is_heavy()) {
        ui.colored_label(egui::Color32::from_rgb(230, 80, 80), "!").on_hover_text(format!(
            "A very large MIDI file: {} notes. It plays fine, but a visualizer script going through every \
             note can stall on it.",
            crate::song_info::thousands(info.notes.unwrap_or_default())
        ));
    }
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
