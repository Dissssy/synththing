//! GUI state and layout, built on eframe/egui. Talks to the render thread
//! (`audio.rs`) over a command channel and reads playback state back from a
//! shared snapshot, it never touches the engine or audio directly.
//!
//! Layout: a menu bar on top, the playback controls always at the bottom,
//! and the sections in between as dockable tabs (egui_dock), drag them
//! around, split, float them out into windows. Closing a tab only hides that
//! section; the View menu brings it back (see `layout.rs` for where).

mod adding_soundfonts;
mod api;
mod autoplay;
mod branding;
mod close;
mod editor;
mod feedback;
#[cfg(not(target_os = "android"))]
mod file_manager;
mod flow;
pub mod fonts;
pub use editor::ApplyMode;
mod loading;
mod mobile;
mod preferences;
mod profiler;
mod queue;
use preferences::PrefTab;
mod pictures;
mod playlist_panel;
mod identity;
#[cfg(target_os = "linux")]
mod desktop_entry;
#[cfg(windows)]
mod install;
mod keyboard;
mod pairing;
mod sync;
mod tags;
mod copyright;
mod moderation;
mod notifications;
mod online;
mod overflow;
mod palette;
mod previews;
mod library;
mod lookup;
mod background;
mod groupings;
mod recording;
mod reorder;
mod repair;
mod script_windows;
mod smart;
mod song_details;
mod choose_art;
mod cleanup;
mod song_meta;
mod song_options;
mod plays;
mod song_names;
mod storage;
mod sprite_editor;
mod themes;
mod layouts;
mod title_bar;
mod toast;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod tray_panel;
mod tour;
mod usage;
mod welcome;
mod which_key;
mod wishlist;
mod panels;
mod panel_modal;
mod device_messages;
mod devices;
mod widgets;
mod controls_bar;
mod soundfonts_panel;
mod help;
mod log_panel;
mod updates;
mod credits_window;
mod dock;
mod songs;
mod visualizer_host;
mod script_bar;
mod dedicated;
mod dispatch;
mod reference;

use widgets::{CATEGORIES_HEIGHT, categories_body, page_ui, fit_width, window_room, with_info, setting_row, settings_group, LABEL_WIDTH, control_width, setting_combo, setting_slider, setting_label, info_icon, inline_code_job, colored_icon_button};
pub(crate) use widgets::{placeholder, icon, wrapping_combo, toggle_chip, chip, on_color, palette_color, icon_button, confirm_button, row_icon_button, row_icon_button_room};
use controls_bar::{ICON_BUTTON_WIDTH, drag_handle, square_corner, loop_text, shuffle_text, seek_bar_ui, status_color, status_outcome, STATUS_TARGET};
pub(crate) use controls_bar::play_button;
use updates::changelog_body_ui;
use songs::start_old_import;
use visualizer_host::ScriptArtRead;
pub(crate) use visualizer_host::lock_panel;
use script_bar::{RenameScript, debug_var_ui, settings_row_ui};
use dedicated::Dedicated;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::{egui, Frame};
use egui_dock::DockState;

use crate::commands::{self, Command, Keymap};
use crate::frame_timing;
use crate::credits::Contributors;
use crate::audio::{AudioCommand, PlaybackShared, DEFAULT_BUFFER_MS, MAX_BUFFER_MS, MIN_BUFFER_MS};
use crate::config::{self, nice_name, Config};
use crate::engine::{EngineView, MAX_SPEED, MIN_SPEED};
use crate::song_list::{self, Autoplay, DraggedFiles, SongList};
use crate::song_store::{self, SongStore};
use crate::select_list::{self, Selection};
use crate::layout::{self, Section};
use crate::loader::AssetCache;
use crate::lua_completion::CompletionWorker;
use crate::lua_visualizer;
use crate::playlist::{Library, LoopMode, NowPlaying, Rng};
use crate::fields::Field;
use crate::song_info::{format_length, SongInfo, SongInfoCache};
use crate::song_title::Naming;
use crate::updater::Updater;
use crate::watch::{FolderWatch, Watched};
use crate::script_host::ScriptHost;
use crate::visualizer::{SampleTap, ShowOutput, VisualizerPanel};

const VISUALIZER_WIDTH: usize = 800;
const VISUALIZER_HEIGHT: usize = 320;

/// Extensions the Songs list shows. MIDI files drive the synth; everything
/// else is decoded as plain audio (rodio/symphonia, or ffmpeg for what they
/// can't, figure out the actual format themselves; this list is just what
/// shows up in the picker).
pub(crate) const SONG_EXTENSIONS: &[&str] = &[
    "mid", "midi", "wav", "mp3", "ogg", "oga", "opus", "flac", "m4a", "aac", "aif", "aiff", "caf", "mka", "mp2", "wma", "ac3",
];
const MIDI_EXTENSIONS: &[&str] = &["mid", "midi"];

/// The songs listed and added from folders: MIDI files, plus audio files
/// when that's turned on in Preferences.
impl App {
    /// The song at `path`'s name, from its tags (Preferences > Browsing).
    fn song_title(&mut self, path: &Path) -> String {
        title_of(&mut self.song_info, &naming(&self.config, &self.songs), path)
    }

    /// The playing song's name, if a song is playing.
    fn current_title(&mut self) -> Option<String> {
        self.current_song.clone().map(|path| self.song_title(&path))
    }
}

/// How songs are named: by the title patterns, from their tags and the
/// names the library knows them by.
fn naming<'a>(config: &'a Config, songs: &'a SongStore) -> Naming<'a> {
    Naming::new(config.title_patterns.as_deref(), songs)
}

/// The song at `path` as the Songs list shows it: its name, file name, the
/// columns `keys` and its tags.
fn describe_song(info: Option<&SongInfo>, naming: &Naming, store: &SongStore, path: &Path, keys: &[&str]) -> song_list::Described {
    let song = naming.song(path, info);
    let values = keys.iter().map(|&key| Field::by_key(key).and_then(|field| song.value(field))).collect();
    let tags = store.tags_of(path).into_iter().map(|(_, tag)| (tags::tag_label(tag), tag.color)).collect();
    song_list::Described { name: naming.name(path, info), file_name: song.file_name.to_string(), values, tags, plays: plays(store, path) }
}

/// How the song at `path` plays (starred, skipped when shuffling, played
/// together with others).
fn plays(store: &SongStore, path: &Path) -> song_list::Plays {
    let flag = |field| store.get(path).is_some_and(|s| crate::fields::Facts::of(s).flag(field));
    song_list::Plays {
        starred: store.is_starred(path),
        skip_shuffle: flag(Field::SkipShuffle),
        together: store.together_of(path).map(|(set, at)| song_list::InSet { set: set.to_string(), at, of: store.together()[set].len() }),
    }
}

/// The song at `path`'s name, by `naming`.
fn title_of(cache: &mut SongInfoCache, naming: &Naming, path: &Path) -> String {
    naming.name(path, cache.get(path).as_deref())
}

/// "Previous" restarts the current track instead of going back a track
/// once it's been playing longer than this, the usual player behavior.
const PREVIOUS_RESTARTS_AFTER_SECS: f64 = 3.0;

/// How long the "Esc to exit" hint shows after entering the dedicated
/// visualizer fullscreen, in seconds.
const DEDICATED_HINT_SECS: f64 = 3.0;

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
    /// The song library (`song_store.rs`).
    songs: SongStore,
    /// The one-time import of the songs and playlists from before the
    /// library, while it runs.
    old_import: Option<std::sync::mpsc::Receiver<song_store::OldImport>>,
    /// The Songs tab: the library's songs. Clicking one plays it.
    song_list: SongList,
    /// The Groupings' state, for the session.
    groupings: groupings::Groupings,
    /// The store's and the song info's revisions when the songs' files were
    /// last looked at for what they say (`note_song_meta`).
    meta_seen: Option<(u64, u64)>,
    /// The library's revision the smart playlists were last worked out
    /// for (`smart::refresh_smart`).
    smart_seen: Option<u64>,
    /// The song kept on a smart playlist (by ID) while it plays there,
    /// though it no longer matches the rules (`smart::refresh_smart`).
    smart_held: Option<(String, PathBuf)>,
    /// The rules being edited (or looked at), if any.
    rules_editor: Option<smart::RulesEditor>,
    /// The rulesets' playlists, as Preferences last showed them.
    ruleset_groups: smart::RulesetGroups,
    /// What the rulesets leave out (`App::missing_info`), as last worked out.
    missing_info: Option<(u64, Arc<Vec<smart::Missing>>)>,
    /// The Songs list shows only the songs a ruleset leaves out.
    songs_missing_only: bool,
    /// Songs asked to be removed from the library, waiting on Remove.
    remove_songs: Vec<PathBuf>,
    /// Remove them from every device, not only this one.
    remove_everywhere: bool,
    active_sf: Option<usize>,
    /// Selected rows of the Soundfonts list.
    soundfont_selection: Selection,
    tap: SampleTap,
    /// Whoever listens to what plays, through the API.
    listeners: Arc<crate::audio_stream::Listeners>,
    /// The audio output, open while there's something to hear; sounds the
    /// app itself makes go to it too.
    output: crate::audio_output::AudioOutput,
    /// The visualizer's picture, shared with the windows that show it
    /// (`lock_panel`).
    visualizer: Arc<Mutex<VisualizerPanel>>,
    /// The running script: loading it, its settings, what it reads back.
    /// Its frames go to `visualizer`.
    script: ScriptHost,
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
    autoplay: Autoplay,
    /// What the engine was last told about looping the current track, see
    /// `sync_engine_loop`.
    engine_loop: bool,
    status: String,
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
    /// Soundfonts just added, still being parsed in the background.
    adding_soundfonts: adding_soundfonts::AddingSoundfonts,
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
    /// Set while the dedicated visualizer fullscreen is up.
    dedicated: Option<Dedicated>,
    /// What the visualizer reported this frame (focus, cursor request);
    /// default when it wasn't drawn.
    visualizer_output: ShowOutput,
    /// Whether the visualizer was drawn (and its script run) this frame and
    /// last frame: when it stops, notes the script was holding are let go.
    visualizer_drawn: bool,
    visualizer_was_drawn: bool,
    /// `ui` ran since the last `logic`: false while nothing's drawn (the
    /// window hidden or minimized), when what lives only on screen stops.
    ui_drawn: bool,
    /// The window's minimized (as of this frame's `logic`).
    minimized: bool,
    /// Whether the cursor is currently confined to the window for a script.
    cursor_confined: bool,
    updater: Updater,
    /// Branch builds: GitHub's list, and installing one (in the Updates
    /// window).
    branches: crate::channels::Branches,
    /// In a branch build's Updates window: asking before copying the
    /// stable settings over its own.
    confirm_recopy: bool,
    /// What the last thing done in the Updates window came to (an error,
    /// or a branch build removed), said there: the status bar is dimmed
    /// behind it. `true`: an error.
    updates_message: Option<(String, bool)>,
    /// The startup update check is running: show its result only if it
    /// finds something new (and not a skipped version).
    launch_update_check: bool,
    /// The Help page, open at a section.
    help: Option<help::HelpTab>,
    /// Lowest level shown in the Log viewer.
    log_min_level: log::Level,
    log_search: String,
    /// A failure showed on the status line since the recent messages were
    /// last looked at (a red dot on the line until they are).
    unseen_failure: bool,
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
    /// The songs whose details were asked for, to open next frame.
    details_for: Option<Vec<PathBuf>>,
    /// The song details open (`song_details.rs`).
    song_details: Option<song_details::SongDetails>,
    /// How to undo the last save of songs' details: each edit as it was.
    last_edit: Option<Vec<(String, Field, Option<String>)>>,
    /// The Clean up window, while it's open (`cleanup.rs`).
    cleanup: Option<cleanup::CleanUp>,
    /// The quick Tags dialog's songs, while it's open (`tags.rs`).
    tags_for: Option<Vec<PathBuf>>,
    /// The tag just made, to name in Preferences > Library > Tags.
    tag_to_name: Option<String>,
    /// Selected rows of the playlist on screen.
    playlist_selection: Selection,
    /// A Controls binding waiting for a key press: (action index, which of
    /// its keys to replace, or `None` to add one).
    binding_capture: Option<(usize, Option<usize>)>,
    /// The command palette, while it's open.
    palette: Option<palette::Palette>,
    /// The keys for commands, from the config's keymaps (`sync_keymap`).
    keymap: Arc<Keymap>,
    /// Preferences > Keyboard waiting for a key.
    keymap_capture: Option<keyboard::Capture>,
    /// A key sequence underway (its first presses).
    keymap_pending: Option<commands::Pending>,
    /// The list the keys steer (`select_list::set_keyboard`): the one in
    /// the tab in front.
    keyboard_list: Option<&'static str>,
    /// The script's Settings or Controls window, if open.
    script_window: Option<script_windows::ScriptWindow>,
    /// The category picked in each (Settings, Controls, Debug).
    script_window_tab: [usize; 3],
    /// The Settings window's presets: the user's for the script open
    /// (read when it changes), the one picked last (theirs or not), and the
    /// name typed for Save as preset.
    user_presets: Option<(PathBuf, Vec<lua_visualizer::SettingsPreset>)>,
    preset_picked: Option<(bool, String)>,
    preset_name: String,
    /// The tables opened in Debug > Variables.
    state_request: lua_visualizer::StateRequest,
    /// The Online Library tab.
    library: library::LibraryState,
    /// The script picker's previews (behind a RefCell: the picker's list is
    /// drawn from `&self`).
    script_previews: std::cell::RefCell<previews::LocalPreviews>,
    /// A panel's contents shown in a dialog over the layout.
    panel_modal: Option<panel_modal::PanelView>,
    /// Reports, and the Moderation window.
    moderation: moderation::ModerationState,
    feedback: feedback::FeedbackState,
    /// Preferences > Library > Storage.
    storage: storage::StorageState,
    repair: repair::RepairState,
    /// The identity's recovery email, new keys and recovery.
    identity: identity::IdentityState,
    /// Signing in another device, or this one with a link.
    pairing: pairing::PairingState,
    lookup: lookup::LookupState,
    /// The library's background work, once it's started here.
    background: Option<crate::background::Jobs>,
    choose_art: Option<choose_art::ChooseArt>,
    /// Settings sync between the identity's devices.
    sync: sync::SyncState,
    /// Notifications from the server.
    notifications: notifications::NotificationsState,
    /// A copyright notice on one of the user's uploads, open.
    copyright: copyright::CopyrightState,
    /// What to go through before the rest of the app (`flow.rs`).
    flow: flow::Flow,
    /// The flow's Usage data checkbox (`usage.rs`), until it's done.
    send_usage_data: bool,
    /// The welcome window and the starter pack download.
    welcome: welcome::Welcome,
    /// Installing synththing: the offer, and Preferences > App's group.
    #[cfg(windows)]
    install: install::Install,
    /// Adding synththing to the applications menu (Linux): the offer, and
    /// Preferences > App's group.
    #[cfg(target_os = "linux")]
    desktop_entry: desktop_entry::DesktopEntry,
    /// Started with Windows to open as the mini player: it opens on the
    /// first frame (`handle_tray`).
    #[cfg(windows)]
    open_mini_at_start: bool,
    /// The category showing in Preferences.
    preferences_tab: PrefTab,
    /// The Themes window, and your themes.
    themes: themes::ThemesState,
    /// The guided tour (Help > Tour).
    tour: tour::Tour,
    contributors: Contributors,
    credits_search: String,
    /// Watches the scripts folder for changes on disk.
    /// Made on the first frame (it needs the egui context to wake the UI).
    folder_watch: Option<FolderWatch>,
    playlists: Library,
    /// The playlist open in Groupings > Playlists (not necessarily the
    /// one playing); none: they're all shown.
    viewed_playlist: Option<usize>,
    /// The entry each playlist (by ID) last played, to continue from, for
    /// the session.
    playlist_positions: std::collections::HashMap<String, usize>,
    /// Set while playing from a playlist; `None` for a song picked straight
    /// from the browser.
    now_playing: Option<NowPlaying>,
    /// Set while autoplay goes down the Songs list (never with
    /// `now_playing`).
    listed: Option<song_list::Playing>,
    /// The play queue (see `queue.rs`).
    queue: Vec<PathBuf>,
    /// The song playing came from the queue: `now_playing` or `listed` is
    /// what it was queued after.
    playing_queued: bool,
    /// The Queue tab's selected rows.
    queue_selection: select_list::Selection,
    wishlist: wishlist::WishlistState,
    /// The Apps tab's panels (`panels.rs`).
    panels: panels::PanelsState,
    live: device_messages::LiveState,
    devices: devices::DevicesState,
    /// The Queue tab's rows as last described, and what from (a hash).
    queue_rows: Option<(u64, queue::QueueRows)>,
    /// Becomes true once the current playlist track is seen playing (not
    /// finished), so the "finished" that ends it advances exactly once, not
    /// again on a stale snapshot from before the next track loaded.
    advance_armed: bool,
    /// The engine's song load count once the current song's loaded
    /// (`EngineView::loads`): till then its view is another's.
    song_loads: u64,
    /// Where the current song is, and its length, while it plays: where
    /// it was left, for a song that remembers that (`song_options.rs`).
    playing_at: Option<(f64, f64)>,
    /// The current song as it's listened to, for its plays and skips
    /// (`plays.rs`).
    listening: crate::play_stats::Listening,
    /// The engine's generation (`EngineView::generation`) the current song
    /// last got to its stop time in, so it's acted on once.
    stopped_in: Option<u64>,
    /// Where to start the song at this path once it's loaded, over its own
    /// start: a preview from Song info's Options.
    seek_on_load: Option<(PathBuf, f64)>,
    rng: Rng,
    /// Text being typed into the playlist rename box, while renaming.
    playlist_rename: Option<String>,
    /// "Find in here" on the open playlist's page.
    playlist_find: String,
    /// What a picture's being picked for.
    picture_for: Option<pictures::PictureFor>,
    /// The section layout. Moved out into a local while `DockArea` draws it
    /// (it needs `&mut` to both the layout and the app), see `dock_ui`.
    dock: DockState<Section>,
    /// True while `dock` is moved out; show/hide requests made meanwhile
    /// (e.g. "Show in Songs" from inside a playlist) queue up in
    /// `pending_layout` and apply right after.
    dock_in_use: bool,
    /// Whether the window has the system's frame, as last set
    /// (`App::apply_window_frame`).
    native_frame: Option<bool>,
    pending_layout: Vec<(Section, bool)>,
    /// Which sections are open, refreshed after every layout change, so
    /// `is_open` still answers correctly while `dock` is moved out.
    open_sections: Vec<Section>,
    /// The layout as last saved, to tell when it's changed.
    saved_layout_json: String,
    last_layout_check: f64,
    preferences_open: bool,
    /// Layout > New layout...: what it's being made as.
    layout_new: Option<layouts::NewLayout>,
    /// Layout > Edit layouts...: open, and what's being done in it.
    layout_edit: Option<layouts::Editing>,
    /// A very large MIDI waiting for "play anyway" (path, notes), and the
    /// last one that got it.
    heavy_prompt: Option<(PathBuf, usize)>,
    heavy_confirmed: Option<PathBuf>,
    /// Game controllers, read every frame; this frame's input.
    gamepads: crate::gamepad::Gamepads,
    pad_frame: crate::gamepad::PadFrame,
    /// Songs sent to the audio thread, and the latest one waiting to reach
    /// the script: (its load number, path, notes, id, tags).
    loads_sent: u64,
    pending_script_song: Option<(u64, PathBuf, Arc<crate::midi_notes::NoteList>, String, lua_visualizer::SongTags)>,
    /// Songs' art read for the script (`album_art()`), on threads of
    /// their own: the sending side for them, and what they've read.
    script_art: (mpsc::Sender<ScriptArtRead>, mpsc::Receiver<ScriptArtRead>),
    /// The song playing's notes (its tempo map, for the transport's accent),
    /// as of when the audio thread reported it loaded.
    song_notes: Option<Arc<crate::midi_notes::NoteList>>,
    /// The transport's accent as last drawn (it eases toward the tempo's).
    accent: Option<[f32; 3]>,
    /// The points per pixel the window's minimum size (`MIN_WINDOW`) was
    /// last set for: set again when the UI scale changes.
    min_window_scale: f32,
    /// Whether the window's size is still to be checked against its new
    /// minimum (`keep_min_window`).
    min_window_check: bool,
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
    /// The wordmark in the menu bar, drawn for the current size and theme.
    wordmark: branding::Wordmark,
    /// The notification-area icon (`None` if it couldn't be made).
    #[cfg(windows)]
    tray: Option<crate::tray::Tray>,
    /// Raised when synththing is launched again for this config folder
    /// (`instance.rs`): the window comes back.
    second_launch: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The mini player, what clicking the tray icon shows.
    #[cfg(windows)]
    tray_panel: tray_panel::TrayPanel,
    /// The window's hidden in the tray.
    hidden: bool,
    /// The local app API's server and what it last told of.
    api: api::ApiState,
    /// Quit was decided: the next close goes through.
    quitting: bool,
    /// A question closing asks first (the first close; a recording on).
    close_prompt: Option<close::ClosePrompt>,
    /// Settings files that couldn't be read and were set aside
    /// (`config::set_aside_notices`), until the user's seen them.
    set_aside: Vec<(String, PathBuf)>,
    /// Close (or hide) once the background render finishes.
    close_after_render: Option<close::CloseAction>,
    /// The mini player's picture, for the first close's question.
    tray_picture: Option<egui::TextureHandle>,
    /// The last recording saved, for "Show last recording".
    last_recording: Option<PathBuf>,
    /// The mobile layout's page and what's open in it (`mobile.rs`).
    mobile_state: mobile::Mobile,
}

impl App {
    pub fn new(
        commands: Sender<AudioCommand>,
        shared: Arc<Mutex<PlaybackShared>>,
        config: Config,
        tap: SampleTap,
        listeners: Arc<crate::audio_stream::Listeners>,
        output: crate::audio_output::AudioOutput,
        sample_rate: u32,
    ) -> Self {
        let songs_dir = config::music_dir().unwrap_or_else(|_| PathBuf::from("music"));
        let old_import = (!SongStore::exists(&songs_dir)).then(|| start_old_import(songs_dir.clone()));
        let songs = SongStore::open(songs_dir.clone());

        let scripts_dir = lua_visualizer::scripts_dir().unwrap_or_else(|_| PathBuf::from("."));
        // (The bundled visualizers and games count as installed from the
        // official library: Update and Restore original work on them.)
        crate::library::set_official_publishers(config.library.official_publishers.clone());
        crate::library::mark_bundled(&scripts_dir);
        let available_scripts = lua_visualizer::list_scripts(&scripts_dir);
        // The script from last time, if that's remembered and it's still
        // there; else the first.
        let remembered = config.session.script.as_ref().filter(|_| config.remember.script);
        let active_index = remembered.and_then(|p| available_scripts.iter().position(|q| q == p)).unwrap_or(0);
        let active_path = available_scripts.get(active_index).cloned();
        let source = active_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let active_script = active_path.is_some().then_some(active_index);
        let mut script = ScriptHost::spawn(source.clone(), active_path, sample_rate);
        let visualizer =
            Arc::new(Mutex::new(VisualizerPanel::new(script.take_pipe(), VISUALIZER_WIDTH, VISUALIZER_HEIGHT)));
        #[cfg(windows)]
        let picture = tray_panel::Picture {
            panel: Arc::clone(&visualizer),
            tap: tap.clone(),
            playback: Arc::clone(&shared),
        };

        let (playlists, playlist_errors) =
            Library::load(config::playlists_dir().unwrap_or_else(|_| PathBuf::from("playlists")), songs_dir);
        let mut config = config;
        if config.layout.is_none() && config.active_layout.is_none() {
            // A first launch starts on the first template's layout, unlocked.
            let first = &layout::TEMPLATES[0];
            config.layout_presets.push(layout::SavedLayout { name: first.name.to_string(), dock: (first.build)() });
            config.active_layout = Some(first.name.to_string());
        }
        let dock = config.layout.clone().map(layout::sanitize).unwrap_or_else(layout::default_layout);
        let saved_layout_json = serde_json::to_string(&dock).unwrap_or_default();
        let (loop_mode, shuffle, autoplay) = if config.remember.modes {
            (config.loop_mode, config.shuffle, config.autoplay)
        } else {
            (LoopMode::Off, false, Autoplay::Off)
        };
        let volume = config.session.volume.filter(|_| config.remember.volume).map_or(1.0, |v| v.clamp(0.0, 1.0));
        let buffer_ms = config.buffer_ms.map_or(DEFAULT_BUFFER_MS, |ms| ms.clamp(MIN_BUFFER_MS, MAX_BUFFER_MS));
        let status = match playlist_errors.first() {
            Some(e) => format!("Couldn't read a playlist ({e})"),
            None => "Click a song to play it, a soundfont to load it.".to_string(),
        };
        #[cfg(windows)]
        let offer_install = install::Install::offer(&config);
        #[cfg(target_os = "linux")]
        let offer_desktop_entry = desktop_entry::DesktopEntry::offer(&config);

        let mut app = Self {
            commands,
            shared,
            config,
            songs,
            old_import,
            song_list: SongList::default(),
            groupings: groupings::Groupings::default(),
            meta_seen: None,
            smart_seen: None,
            smart_held: None,
            rules_editor: None,
            ruleset_groups: Default::default(),
            missing_info: None,
            songs_missing_only: false,
            remove_songs: Vec::new(),
            remove_everywhere: false,
            active_sf: None,
            soundfont_selection: Selection::default(),
            tap,
            listeners,
            output,
            visualizer: Arc::clone(&visualizer),
            script,
            scripts_dir,
            available_scripts,
            active_script,
            editor_text: source,
            completion: CompletionWorker::new(lua_visualizer::host_global_names()),
            editor_hover_info: None,
            editor_opened_this_run: false,
            volume,
            buffer_ms,
            loop_mode,
            autoplay,
            shuffle,
            engine_loop: false,
            status,
            loaded_sf: None,
            assets: AssetCache::new(),
            pending_play: None,
            pending_soundfont: None,
            adding_soundfonts: Default::default(),
            current_song: None,
            skips_in_a_row: 0,
            hovered_playlist_entry: None,
            hover_since: None,
            dedicated: None,
            visualizer_output: ShowOutput::default(),
            visualizer_drawn: false,
            visualizer_was_drawn: false,
            ui_drawn: false,
            minimized: false,
            cursor_confined: false,
            updater: Updater::new(),
            branches: crate::channels::Branches::default(),
            confirm_recopy: false,
            updates_message: None,
            launch_update_check: false,
            help: None,
            log_min_level: log::Level::Info,
            log_search: String::new(),
            unseen_failure: false,
            logged_status: String::new(),
            rename: None,
            editor_goto_line: None,
            folder_watch: None,
            song_info: match config::config_dir() {
                Ok(dir) => SongInfoCache::saved_in(dir.join("song-info.json")),
                Err(_) => SongInfoCache::new(),
            },
            details_for: None,
            song_details: None,
            last_edit: None,
            cleanup: None,
            tags_for: None,
            tag_to_name: None,
            playlist_selection: Selection::default(),
            binding_capture: None,
            palette: None,
            keymap: Arc::default(),
            keymap_capture: None,
            keymap_pending: None,
            keyboard_list: None,
            script_window: None,
            script_window_tab: [0; 3],
            user_presets: None,
            preset_picked: None,
            preset_name: String::new(),
            state_request: Default::default(),
            library: library::LibraryState::new(),
            script_previews: Default::default(),
            panel_modal: None,
            moderation: Default::default(),
            feedback: Default::default(),
            storage: Default::default(),
            repair: Default::default(),
            identity: Default::default(),
            pairing: Default::default(),
            lookup: Default::default(),
            background: None,
            choose_art: None,
            sync: Default::default(),
            notifications: Default::default(),
            copyright: Default::default(),
            flow: flow::Flow::default(),
            send_usage_data: false,
            welcome: welcome::Welcome::default(),
            #[cfg(windows)]
            install: install::Install::new(),
            #[cfg(target_os = "linux")]
            desktop_entry: desktop_entry::DesktopEntry::new(),
            #[cfg(windows)]
            open_mini_at_start: false,
            preferences_tab: PrefTab::default(),
            themes: themes::ThemesState::default(),
            tour: tour::Tour::default(),
            contributors: Contributors::default(),
            credits_search: String::new(),
            playlists,
            viewed_playlist: None,
            playlist_positions: std::collections::HashMap::new(),
            now_playing: None,
            listed: None,
            queue: Vec::new(),
            playing_queued: false,
            queue_selection: Default::default(),
            wishlist: Default::default(),
            panels: Default::default(),
            live: Default::default(),
            devices: Default::default(),
            queue_rows: None,
            advance_armed: false,
            song_loads: 0,
            playing_at: None,
            listening: Default::default(),
            stopped_in: None,
            seek_on_load: None,
            rng: Rng::from_time(),
            playlist_rename: None,
            playlist_find: String::new(),
            picture_for: None,
            open_sections: Section::ALL.into_iter().filter(|&s| layout::is_open(&dock, s)).collect(),
            dock,
            dock_in_use: false,
            native_frame: None,
            pending_layout: Vec::new(),
            saved_layout_json,
            last_layout_check: 0.0,
            preferences_open: false,
            layout_new: None,
            layout_edit: None,
            heavy_prompt: None,
            heavy_confirmed: None,
            gamepads: crate::gamepad::Gamepads::new(),
            pad_frame: crate::gamepad::PadFrame::default(),
            loads_sent: 0,
            pending_script_song: None,
            script_art: mpsc::channel(),
            song_notes: None,
            accent: None,
            min_window_scale: 0.0,
            min_window_check: false,
            docs_target: None,
            pending_focus: None,
            sprite: sprite_editor::SpriteEditorState::default(),
            sample_rate,
            editor: editor::EditorState::default(),
            editor_saved: String::new(),
            recording: recording::Recording::new(),
            wordmark: branding::Wordmark::default(),
            #[cfg(windows)]
            tray: None,
            second_launch: None,
            #[cfg(windows)]
            tray_panel: tray_panel::TrayPanel::new(picture),
            hidden: false,
            api: api::ApiState::default(),
            quitting: false,
            close_prompt: None,
            set_aside: Vec::new(),
            close_after_render: None,
            tray_picture: None,
            last_recording: None,
            mobile_state: mobile::Mobile::default(),
        };
        #[cfg(windows)]
        if offer_install {
            app.flow.add(flow::Step::Install);
        }
        #[cfg(target_os = "linux")]
        if offer_desktop_entry {
            app.flow.add(flow::Step::DesktopEntry);
        }
        if let Some(path) = app.script.path().map(Path::to_path_buf) {
            app.editor_opened(&path);
        }
        app.send(AudioCommand::SetBufferMs(app.buffer_ms));
        app.send(AudioCommand::SetVolume(app.volume));
        if let Some(speed) = app.config.session.speed.filter(|_| app.config.remember.speed) {
            app.send(AudioCommand::SetSpeed(speed.clamp(MIN_SPEED, MAX_SPEED)));
        }
        app
    }

    fn send(&self, command: AudioCommand) {
        let _ = self.commands.send(command);
    }

    /// Load a soundfont from the list, so there is sound the moment a song
    /// is chosen: last time's, if that's remembered and still there, else
    /// the first that exists.
    pub fn autoload_soundfont(&mut self) {
        let remembered = self.config.session.soundfont.as_ref().filter(|_| self.config.remember.soundfont);
        let last = remembered.and_then(|p| self.config.soundfonts.iter().position(|q| q == p && q.exists()));
        if let Some(idx) = last.or_else(|| self.config.soundfonts.iter().position(|p| p.exists())) {
            self.activate_soundfont(idx);
        }
    }

    /// Note what this session ends with, for the next one to start with
    /// (whatever Preferences > Playback says to remember), and save it.
    fn save_session(&mut self) {
        self.remember_position();
        let remember = self.config.remember;
        let speed = self.shared.lock().map(|s| s.view.speed).unwrap_or(1.0);
        let session = crate::config::Session {
            volume: remember.volume.then_some(self.volume),
            speed: remember.speed.then_some(speed),
            soundfont: self
                .active_sf
                .and_then(|i| self.config.soundfonts.get(i).cloned())
                .filter(|_| remember.soundfont),
            script: self.script.path().map(Path::to_path_buf).filter(|_| remember.script),
        };
        self.config.session = session;
        if let Err(e) = self.config.save() {
            log::warn!("couldn't save the session's settings: {e:#}");
        }
    }

    // --- actions -------------------------------------------------------

    /// A song picked straight from the browser, leaves any playlist.
    fn play_path(&mut self, path: PathBuf) {
        self.now_playing = None;
        self.listed = None;
        self.request_play(&path, None, None, false);
    }

    // --- layout ----------------------------------------------------

    /// The window's minimum size, `MIN_WINDOW` points at the UI scale
    /// it's at (set again when that changes), so the controls bar and the
    /// title bar always fit. A window smaller than that after the scale
    /// changes (zoomed in with Ctrl+) grows to it, on the next frame, once
    /// the window's size is measured at the new scale.
    fn keep_min_window(&mut self, ctx: &egui::Context) {
        let scale = ctx.pixels_per_point();
        if scale != self.min_window_scale {
            self.min_window_scale = scale;
            self.min_window_check = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(MIN_WINDOW));
            ctx.request_repaint();
        } else if std::mem::take(&mut self.min_window_check) {
            let size = ctx.content_rect().size();
            if size.x < MIN_WINDOW.x - 1.0 || size.y < MIN_WINDOW.y - 1.0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.max(MIN_WINDOW)));
            }
        }
    }

    /// Whether one of the app's modals (Rename script, Song info...) or the flow is up,
    /// or Preferences is waiting for keys. They get the keyboard: app shortcuts and the global
    /// keys pause.
    fn any_modal_open(&self) -> bool {
        self.keymap_capture.is_some()
            || self.flow.open()
            || self.rename.is_some()
            || self.song_details.is_some()
            || self.cleanup.is_some()
            || self.palette.is_some()
            || self.tags_for.is_some()
            || self.recording.prompt_open
            || self.recording.window.is_some()
            || self.script_window.is_some()
            || self.library.publish_open()
            || self.moderation.open()
            || self.feedback.open()
            || self.identity.open()
            || self.notifications.open()
            || self.copyright.open()
            || self.editor.history_open
            || self.layout_new.is_some()
            || self.layout_edit.is_some()
            || self.heavy_prompt.is_some()
            || self.sprite.new_sprite.is_some()
            || self.close_prompt.is_some()
    }

    /// Pick up scripts added, removed or changed on disk.
    fn refresh_changed_folders(&mut self, ctx: &egui::Context) {
        let watch = self.folder_watch.get_or_insert_with(|| FolderWatch::new(ctx.clone()));
        watch.watch(Watched::Scripts, &self.scripts_dir);
        for which in watch.poll() {
            match which {
                Watched::Scripts => self.rescan_scripts(),
            }
        }
    }

    /// Remember Loop, Shuffle and Autoplay for next launch.
    fn save_playback_modes(&mut self) {
        self.config.loop_mode = self.loop_mode;
        self.config.shuffle = self.shuffle;
        self.config.autoplay = self.autoplay;
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save loop/shuffle/autoplay: {e}");
        }
    }

    /// The main window's handle (an HWND on Windows), for the system's media
    /// controls.
    pub fn attach_window(&mut self, window: Option<usize>) {
        self.api.window = window;
    }

    /// Watch `flag`, raised when synththing is launched again here.
    pub fn listen_for_second_launch(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.second_launch = Some(flag);
    }

    /// Bring the main window back and to the front: un-hidden,
    /// un-minimized, focused (which puts the mini player away, `logic`).
    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Nothing was drawn last frame (hidden in the tray, minimized), so what
    /// only plays while it's on screen stops, as `ui` would have stopped it
    /// on noticing it gone: a script's held notes.
    fn nothing_drawn(&mut self) {
        if self.visualizer_was_drawn {
            self.send(AudioCommand::Live(crate::live::LiveCommand::StopAll));
            self.visualizer_was_drawn = false;
        }
    }

    fn set_loop_mode(&mut self, mode: LoopMode) {
        if mode != self.loop_mode {
            self.loop_mode = mode;
            self.plan_upcoming();
            self.plan_listed();
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
            if let Some(playing) = &mut self.listed {
                playing.history = vec![playing.at.path.clone()];
            }
            self.plan_upcoming();
            self.plan_listed();
            self.save_playback_modes();
        }
    }

}

impl App {
    /// Everything that has to keep going whether or not anything's drawn
    /// (`eframe::App::logic`, every frame, and on its own while the window
    /// is hidden or minimized; on Android also while the app is in the
    /// background, when eframe has no window and calls nothing).
    pub fn tick(&mut self, ctx: &egui::Context) {
        crate::select_list::show_star_look(ctx, self.songs.star());
        let ui_ran = std::mem::take(&mut self.ui_drawn);
        if !ui_ran {
            self.nothing_drawn();
        }
        self.minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let view = self.shared.lock().unwrap().view.clone();
        #[cfg(windows)]
        frame_timing::time("handle_tray", || self.handle_tray(ctx, &view));
        if self.second_launch.as_ref().is_some_and(|f| f.swap(false, std::sync::atomic::Ordering::Relaxed)) {
            log::info!("launched again: showing the window");
            self.show_window(ctx);
        }
        // The main window and the mini player are never up together: the
        // main window on screen, however it got there (`show_window`, or
        // restored from the taskbar), puts the mini player away.
        #[cfg(windows)]
        if !self.hidden && !self.minimized && self.tray_panel.is_open() {
            self.tray_panel.hide(ctx);
        }
        frame_timing::time("sync_script_song", || self.sync_script_song(&view));
        frame_timing::time("poll_loads", || self.poll_loads());
        frame_timing::time("audio_output", || self.output.tick());
        frame_timing::time("poll_imports", || self.poll_imports(ctx));
        frame_timing::time("poll_old_import", || self.poll_old_import());
        #[cfg(target_os = "android")]
        self.android_tick(&view);
        frame_timing::time("refresh_changed_folders", || self.refresh_changed_folders(ctx));
        if frame_timing::time("song_info", || self.song_info.poll()) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        frame_timing::time("note_song_meta", || self.note_song_meta());
        frame_timing::time("refresh_smart", || self.refresh_smart(false));
        frame_timing::time("wishlist_tick", || self.wishlist_tick(ctx));
        frame_timing::time("repair", || {
            self.poll_repair();
            self.suggest_repair();
        });
        frame_timing::time("playlist_tick", || self.playlist_tick(&view));
        frame_timing::time("poll_library", || self.poll_library(ctx));
        frame_timing::time("poll_moderation", || self.poll_moderation());
        frame_timing::time("poll_feedback", || self.poll_feedback());
        frame_timing::time("poll_identity", || self.poll_identity());
        frame_timing::time("poll_pairing", || self.poll_pairing());
        frame_timing::time("poll_notifications", || self.poll_notifications(ctx));
        frame_timing::time("poll_hub_live", || self.poll_hub_live(ctx));
        // (The update download gets the connection.)
        let held = self.update_offered();
        frame_timing::time("poll_sync", || self.poll_sync(held));
        frame_timing::time("lookup_tick", || self.lookup_tick(ctx, held));
        frame_timing::time("background_tick", || self.background_tick(ctx));
        frame_timing::time("api_tick", || self.api_tick(ctx, &view));
        // (Minimized, eframe still draws the window, but only 10 times a
        // second.)
        let on_screen = ui_ran && self.visualizer_was_drawn && !self.minimized;
        frame_timing::time("api_stream_unseen", || self.api_stream_unseen(ctx, on_screen, &view));
        if self.songs_moving().is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        frame_timing::time("poll_copyright", || self.poll_copyright());
        if self.status != self.logged_status {
            log::info!(target: STATUS_TARGET, "{}", self.status);
            self.logged_status = self.status.clone();
            if status_outcome(&self.status) == Some(false) {
                self.unseen_failure = true;
            }
        }
        frame_timing::time("keep_loaded", || self.keep_loaded(ctx));
        frame_timing::time("poll_starter", || self.poll_starter(ctx));
        frame_timing::time("editor_tick", || self.editor_tick(ctx));
        self.close_after_render_tick(ctx);

        // Stay idle otherwise, but while a song plays keep ticking, so the
        // end of a track is noticed and a playlist's next one starts, with
        // the window in the background, minimized or hidden.
        if !view.paused && (view.has_midi || view.has_audio_file) {
            ctx.request_repaint_after(Duration::from_millis(200));
            frame_timing::expect_next_within(Duration::from_millis(200));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame) {
        frame_timing::report(frame.info().cpu_usage);
        let ctx = ui.ctx().clone();
        if !crate::usage::hardware_known()
            && let Some(render) = frame.wgpu_render_state()
        {
            let info = render.adapter.get_info();
            crate::usage::set_hardware(&info.name, &format!("{:?}", info.backend));
        }
        let window = ctx.input(|i| i.viewport().inner_rect).map_or(0.0, |r| r.area() * ctx.pixels_per_point().powi(2));
        let scale = config::visualizer_scale_down(self.config.visualizer_scale.unwrap_or(config::DEFAULT_VISUALIZER_SCALE));
        crate::usage::set_view(self.mobile(), scale, window);
        frame_timing::time("sync_theme", || self.sync_theme(&ctx));
        if self.hidden {
            // Hidden in the tray, this still runs while a tray window (a
            // child of this one) is up: draw nothing, least of all the
            // visualizer, which would run the script at this window's
            // size between the mini player's frames. Only keep the tray
            // window there.
            #[cfg(windows)]
            if self.tray.is_some() {
                self.tray_panel.register(&ctx);
            }
            frame_timing::end();
            return;
        }
        let shared = self.shared.lock().unwrap().clone();
        let view = &shared.view;
        self.visualizer_drawn = false;
        select_list::set_touch(&ctx, self.mobile());
        crate::import::set_waker(&ctx);
        self.pad_frame = frame_timing::time("gamepads", || self.gamepads.poll(ctx.input(|i| i.focused)));
        if !self.pad_frame.down.is_empty() || !self.pad_frame.released.is_empty() {
            ctx.request_repaint();
        }

        // The keymap (commands.rs). Keys with a modifier and the function
        // keys work from anywhere: while typing, with the visualizer
        // focused, in either fullscreen (scripts never see the unmodified
        // ones). Single keys pause while some widget (namely the Songs
        // search box) has keyboard focus, so typing "v" there types a "v".
        // Escape is only taken when there's a fullscreen to leave, so it
        // still does its usual job elsewhere. All of it pauses while a
        // modal is up (Escape closes the modal instead) or a binding is
        // being captured.
        self.sync_keymap(&ctx);
        self.keyboard_list = self.find_keyboard_list();
        select_list::set_keyboard(&ctx, self.keyboard_list);
        if !self.any_modal_open() && self.binding_capture.is_none() {
            let typing = ctx.memory(|m| m.focused().is_some());
            let available: Vec<Command> = Command::ALL.into_iter().filter(|&c| self.command_available(c)).collect();
            let keymap = self.keymap.clone();
            let mut pending = self.keymap_pending.take();
            // Run outside `ctx.input_mut`: `send_viewport_cmd` (inside
            // `enter_dedicated`) takes the lock that closure holds, and
            // calling it from in there deadlocks.
            let pressed = ctx.input_mut(|i| {
                keymap.pressed(i, typing, |command| available.contains(&command), &mut pending)
            });
            self.keymap_pending = pending;
            for command in pressed {
                self.run_command(&ctx, command, view);
            }
        } else {
            self.keymap_pending = None;
        }
        self.visualizer_output = ShowOutput::default();

        self.handle_os_file_drops(&ctx);

        self.apply_window_frame(&ctx);

        self.song_list.clear_hovered();
        self.hovered_playlist_entry = None;
        self.tour.begin_frame();

        self.poll_launch_update_check(&ctx);
        self.collect_flow_steps();
        if self.dedicated.is_some() {
            frame_timing::time("dedicated", || self.dedicated_ui(ui, &shared));
        } else if self.mobile() {
            // (The whole screen: the flow comes first.)
            if self.flow.open() {
                self.flow_ui(ui);
            } else if self.song_details_open() {
                self.mobile_song_details_ui(ui);
            } else {
                frame_timing::time("mobile", || self.mobile_ui(ui, &shared));
            }
            self.open_mobile_pages(&ctx);
            self.rename_ui(&ctx);
            self.cleanup_ui(&ctx);
            self.choose_art_ui(&ctx);
            self.history_ui(&ctx);
            self.status_toast_ui(&ctx);
        } else {
            self.keep_min_window(&ctx);
            egui::Panel::top("top_bar")
                .show_separator_line(false)
                .exact_size(title_bar::HEIGHT)
                .show(ui, |ui| frame_timing::time("top bar", || self.title_bar_ui(ui)));
            // (Disabled while the flow's up: it's gone through first.)
            let flow_open = self.flow.open();
            egui::Panel::bottom("controls_panel").show_separator_line(false).show(ui, |ui| {
                let _timing = frame_timing::scope("controls bar");
                ui.add_enabled_ui(!flow_open, |ui| self.controls_bar_ui(ui, view));
                self.tour.mark(tour::Target::Controls, ui.min_rect());
            });
            if flow_open {
                self.flow_ui(ui);
            } else if self.preferences_open {
                self.preferences_ui(ui);
            } else if self.help.is_some() {
                self.help_ui(ui);
            } else {
                frame_timing::time("dock", || self.dock_ui(ui, &shared));
            }
            frame_timing::time("autosave_layout", || self.autosave_layout(&ctx));
            let windows = frame_timing::scope("windows");
            self.rename_ui(&ctx);
            #[cfg(windows)]
            self.uninstall_ui(&ctx);
            self.cleanup_ui(&ctx);
            self.choose_art_ui(&ctx);
            self.song_details_ui(&ctx);
            self.history_ui(&ctx);
            // (New layout after Edit layouts: Edit's + opens it over it.)
            self.layout_edit_ui(&ctx);
            self.layout_new_ui(&ctx);
            self.new_sprite_ui(&ctx);
            drop(windows);
        }
        let windows = frame_timing::scope("windows");
        self.palette_ui(&ctx, view);
        self.which_key_ui(&ctx);
        self.remove_songs_ui(&ctx);
        self.repair_ui(&ctx);
        self.tags_ui(&ctx);
        self.rules_editor_ui(&ctx);
        self.close_requested_ui(&ctx);
        self.close_prompt_ui(&ctx);
        #[cfg(windows)]
        if self.tray.is_some() {
            self.tray_panel.register(&ctx);
        }
        // Outside the layout: F9 can ask for ffmpeg from the dedicated
        // fullscreen too.
        self.ffmpeg_prompt_ui(&ctx);
        self.record_window_ui(&ctx, view);
        self.script_window_ui(&ctx);
        self.publish_ui(&ctx);
        self.document_ui(&ctx);
        self.update_ui(&ctx);
        self.report_ui(&ctx);
        self.identity_flow_ui(&ctx);
        self.pairing_flow_ui(&ctx);
        self.sync_toast_ui(&ctx);
        toast::ui(&ctx);
        self.notification_ui(&ctx);
        self.panel_modal_ui(&ctx, &shared);
        self.copyright_ui(&ctx);
        self.moderation_ui(&ctx);
        self.feedback_ui(&ctx);
        self.heavy_midi_ui(&ctx);
        self.poll_recordings(view);
        self.apply_cursor_confinement(&ctx);
        self.script_busy_ui(&ctx);
        self.tour_ui(&ctx, &shared);
        self.themes_window(&ctx);
        profiler::window(&ctx);
        drop(windows);

        drag_ghost_ui(&ctx);

        // The visualizer went away (tab closed or hidden): its script can't
        // let go of notes it's holding any more, so let go for it.
        if self.visualizer_was_drawn && !self.visualizer_drawn {
            self.send(AudioCommand::Live(crate::live::LiveCommand::StopAll));
        }
        self.visualizer_was_drawn = self.visualizer_drawn;

        if self.dedicated.is_some() || self.is_open(Section::Visualizer) {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        self.ui_drawn = true;
        frame_timing::end();
    }

    /// Everything that has to keep going whether or not anything's drawn:
    /// eframe calls this every frame before `ui`, and on its own while the
    /// window is hidden or minimized (when `ui` isn't called), so a
    /// playlist goes on to its next track, loads finish and the library
    /// keeps polling either way.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        frame_timing::begin(ctx.input(|i| i.viewport().inner_rect).map(|r| r.size()));
        let (minimized, focused) = ctx.input(|i| (i.viewport().minimized.unwrap_or(false), i.viewport().focused.unwrap_or(true)));
        frame_timing::set_window(if self.hidden {
            frame_timing::Window::Hidden
        } else if minimized {
            frame_timing::Window::Minimized
        } else if focused {
            frame_timing::Window::Focused
        } else {
            frame_timing::Window::Unfocused
        });
        frame_timing::time("tick", || self.tick(ctx));
        frame_timing::logic_done();
    }

    /// Last chance to save a layout change the once-a-second autosave
    /// hasn't caught yet, and what to start with next time.
    fn on_exit(&mut self) {
        self.flush_editor();
        // Stop the script thread, which saves the script's data.
        self.script.shutdown();
        // Don't lose a recording to closing the app: write it out first.
        if let Some(recorder) = self.recording.recorder.take()
            && let Err(e) = recorder.finish_now()
        {
            log::warn!("recording lost on exit: {e:#}");
        }
        self.recording.wait_for_saves();
        crate::song_store::flush();
        self.save_session();
        if serde_json::to_string(&self.dock).unwrap_or_default() != self.saved_layout_json {
            self.save_layout();
        }
    }
}

/// A song's length to read ("3:25"): "..." while it's being read, "--:--"
/// for one without a length.
fn length_text(info: Option<&crate::song_info::SongInfo>) -> String {
    match info {
        None => "...".to_string(),
        Some(info) => info.meta.length.map(format_length).unwrap_or_else(|| "--:--".to_string()),
    }
}

/// A song's length and an info button (greyed out when the file has no
/// details to show), for the end of a list row; laid out right to left.
/// A song's length (`with_length`) and a warning for a very large MIDI
/// file, right to left.
fn song_info_widgets(ui: &mut egui::Ui, cache: &mut SongInfoCache, path: &Path, with_length: bool) {
    let info = cache.get(path);
    if with_length {
        ui.weak(length_text(info.as_deref()));
    }
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

/// The window's smallest size, in points: room for the title bar, and the
/// controls bar's usual buttons with volume as a button.
const MIN_WINDOW: egui::Vec2 = egui::vec2(400.0, 420.0);

/// While something is being dragged, a small label following the pointer
/// saying what, egui's drag-and-drop carries a payload but draws nothing
/// for it on its own.
fn drag_ghost_ui(ctx: &egui::Context) {
    use egui::DragAndDrop;
    let text = if let Some(files) = DragAndDrop::payload::<DraggedFiles>(ctx) {
        files.label.clone()
    } else if let Some(sf) = DragAndDrop::payload::<DraggedSoundfont>(ctx) {
        format!("SoundFont: {}", nice_name(&sf.0))
    } else if let Some(entry) = DragAndDrop::payload::<playlist_panel::DraggedEntry>(ctx) {
        entry.label.clone()
    } else if let Some(rows) = DragAndDrop::payload::<song_list::DraggedRows>(ctx) {
        rows.label.clone()
    } else {
        return;
    };
    let Some(pos) = ctx.pointer_latest_pos() else {
        return;
    };
    egui::Area::new(egui::Id::unique("drag_ghost"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + egui::vec2(14.0, 10.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                // (On one line: the area's narrow by the window's edge.)
                ui.add(egui::Label::new(text).extend());
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
