//! GUI state and layout, built on eframe/egui. Talks to the render thread
//! (`audio.rs`) over a command channel and reads playback state back from a
//! shared snapshot, it never touches the engine or audio directly.
//!
//! Layout: a menu bar on top, the playback controls always at the bottom,
//! and the sections in between as dockable tabs (egui_dock), drag them
//! around, split, float them out into windows. Closing a tab only hides that
//! section; the View menu brings it back (see `layout.rs` for where).

mod api;
mod autoplay;
mod branding;
mod close;
mod editor;
mod feedback;
pub mod fonts;
pub use editor::ApplyMode;
mod loading;
mod mobile;
mod preferences;
mod profiler;
mod queue;
use preferences::PrefTab;
mod playlist_panel;
mod identity;
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
mod groupings;
mod recording;
mod repair;
mod script_windows;
mod smart;
mod song_details;
mod choose_art;
mod cleanup;
mod song_meta;
mod song_options;
mod song_names;
mod storage;
mod sprite_editor;
mod themes;
mod layouts;
mod title_bar;
mod toast;
mod tray_panel;
mod tour;
mod welcome;
mod which_key;
mod wishlist;
mod panels;
mod device_messages;
mod devices;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::{egui, Frame};
use egui_dock::{DockArea, DockState, TabViewer};

use crate::applog;
use crate::commands::{self, Command, Keymap};
use crate::frame_timing;
use crate::credits::{self, Contributors, ContributorsState};
use crate::audio::{AudioCommand, PlaybackShared, DEFAULT_BUFFER_MS, MAX_BUFFER_MS, MIN_BUFFER_MS};
use crate::config::{self, nice_name, Config};
use crate::engine::{EngineView, MAX_SPEED, MIN_SPEED};
use crate::song_list::{self, Autoplay, DraggedFiles, SongList};
use crate::song_store::{self, SongStore, Stored};
use crate::select_list::{self, Selection};
use crate::layout::{self, Section};
use crate::loader::{self, Asset, AssetCache, SoundFontProbe};
use crate::lua_completion::CompletionWorker;
use crate::lua_docs;
use crate::lua_visualizer::{
    self, DebugVar, PlaybackRequest, SettingDescriptor, SettingKind, SettingValue,
};
use crate::playlist::{Library, LoopMode, NowPlaying, Rng};
use crate::fields::Field;
use crate::song_info::{format_length, SongInfoCache};
use crate::song_title::Naming;
use crate::updater::{self, Track, UpdateState, Updater};
use crate::watch::{FolderWatch, Watched};
use crate::changelog;
use crate::script_host::{self, ScriptHost};
use crate::visualizer::{DisplayMode, NotesSnapshot, SampleTap, ShowOutput, VisualizerPanel};

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
    Naming::new(config, songs)
}

/// The song at `path` as the Songs list shows it: its name, file name, the
/// columns `keys` and its tags.
fn describe_song(cache: &mut SongInfoCache, naming: &Naming, store: &SongStore, path: &Path, keys: &[&str]) -> song_list::Described {
    let info = cache.get(path);
    let song = naming.song(path, info.as_deref());
    let values = keys.iter().map(|&key| Field::by_key(key).and_then(|field| song.value(field))).collect();
    let tags = store.tags_of(path).into_iter().map(|(_, tag)| (tag.name.clone(), tag.color)).collect();
    song_list::Described { name: naming.name(path, info.as_deref()), file_name: song.file_name.to_string(), values, tags, plays: plays(store, path) }
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

/// The Songs list's songs: the library's (with `extensions`, and only
/// those in `only` when given), as a query's filters keep them.
struct Listed<'a> {
    store: &'a SongStore,
    wishes: &'a crate::wishlist::Wishes,
    extensions: &'a [&'a str],
    only: Option<&'a std::collections::HashSet<&'a str>>,
}

impl song_list::Source for Listed<'_> {
    fn songs(&mut self, query: &str) -> (Vec<PathBuf>, crate::search::Search) {
        let search = crate::search::parse(query, self.store.tags());
        let songs = {
            let matches = search.rules.matcher(self.store, self.wishes, crate::settings_sync::now());
            self.store
                .songs()
                .filter(|(path, song)| song_store::has_extension(path, self.extensions) && matches(song))
                .map(|(path, _)| path)
                .filter(|p| self.only.is_none_or(|only| song_store::key_in(self.store.dir(), p).is_some_and(|k| only.contains(k))))
                .collect()
        };
        (songs, search)
    }

    fn values(&mut self, field: crate::fields::Field, query: &str) -> Vec<String> {
        let rules = crate::search::parse(query, self.store.tags()).rules;
        let matches = rules.matcher(self.store, self.wishes, crate::settings_sync::now());
        let songs = self.store.songs().filter(|(path, song)| song_store::has_extension(path, self.extensions) && matches(song)).map(|(_, song)| song);
        if field == crate::fields::Field::Wish {
            // (Those the songs kept fill.)
            let index = crate::wishlist::Index::new(self.wishes);
            let filled: std::collections::BTreeSet<&str> = songs.flat_map(|song| index.fills(song)).map(|id| index.name(id)).collect();
            return filled.into_iter().map(str::to_string).collect();
        }
        if field == crate::fields::Field::Tag {
            let used: std::collections::BTreeSet<&str> = songs.flat_map(|song| song.tags.iter().map(String::as_str)).collect();
            return used.into_iter().filter_map(|id| self.store.tags().get(id)).map(|t| t.name.clone()).collect();
        }
        // (Each once, however it's spelled: as most songs spell it.)
        let mut spellings: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for song in songs {
            for value in crate::fields::Facts::of(song).items(field) {
                spellings.entry(crate::fields::group_key(&value, "")).or_default().push(value.into_owned());
            }
        }
        spellings.values().filter_map(|s| crate::fields::best_spelling(s.iter().map(String::as_str)).map(str::to_string)).collect()
    }
}

/// The song at `path`'s name, by `naming`.
fn title_of(cache: &mut SongInfoCache, naming: &Naming, path: &Path) -> String {
    naming.name(path, cache.get(path).as_deref())
}

/// Start the one-time import of the songs from before the library (the
/// Songs folder, or the starter songs' folder; on Android, the files
/// imported into the library's folder under their own names) and the
/// playlists naming them, on a thread of its own: its result comes back
/// on the returned channel.
/// A song's art read for the script: the song's file, and its art.
type ScriptArtRead = (PathBuf, Option<Arc<crate::album_art::ScriptArt>>);

fn start_old_import(dir: PathBuf) -> std::sync::mpsc::Receiver<song_store::OldImport> {
    let folders: Vec<PathBuf> = if cfg!(target_os = "android") {
        vec![dir.clone()]
    } else {
        let starter = crate::starter::old_songs_dir();
        let old = Config::old_songs_folder().or_else(|| starter.parent().map(Path::to_path_buf));
        old.into_iter().filter(|d| d.is_dir()).collect()
    };
    let playlists = config::playlists_dir().unwrap_or_else(|_| PathBuf::from("playlists"));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(song_store::import_old(&folders, &dir, &playlists, SONG_EXTENSIONS));
    });
    rx
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
    /// The audio output, for sounds the app itself makes.
    mixer: rodio::mixer::Mixer,
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
    updates_open: bool,
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
    /// The Log viewer (Help > Log...).
    log_open: bool,
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
    /// The Tags modal is open.
    tags_open: bool,
    /// The tag just made, to name in the Tags modal.
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
    choose_art: Option<choose_art::ChooseArt>,
    /// Settings sync between the identity's devices.
    sync: sync::SyncState,
    /// Notifications from the server.
    notifications: notifications::NotificationsState,
    /// A copyright notice on one of the user's uploads, open.
    copyright: copyright::CopyrightState,
    /// The changelog window: `Some(since)` lists only the versions newer
    /// than that (What's new, after an update), `None` all of them
    /// (Help > Changelog...).
    changelog: Option<Option<changelog::Version>>,
    /// The welcome window and the starter pack download.
    welcome: welcome::Welcome,
    /// The category showing in Preferences.
    preferences_tab: PrefTab,
    /// The Themes window, and your themes.
    themes: themes::ThemesState,
    /// The guided tour (Help > Tour).
    tour: tour::Tour,
    /// Help > Credits & licenses.
    credits_open: bool,
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
    /// The play queue (Up next, see `queue.rs`).
    queue: Vec<PathBuf>,
    /// The song playing came from the queue: `now_playing` or `listed` is
    /// what it was queued after.
    playing_queued: bool,
    /// The Up next tab's selected rows.
    queue_selection: select_list::Selection,
    wishlist: wishlist::WishlistState,
    /// The Apps tab's panels (`panels.rs`).
    panels: panels::PanelsState,
    live: device_messages::LiveState,
    devices: devices::DevicesState,
    /// The Up next tab's rows as last described, and what from (a hash).
    queue_rows: Option<(u64, Vec<(PathBuf, song_list::Described)>)>,
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
    /// The playlist (by ID) a picture's being picked for.
    image_for: Option<String>,
    /// Delete was clicked once; the next click confirms.
    confirm_delete_playlist: bool,
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
    /// The notification-area icon (Windows; `None` elsewhere, or if it
    /// couldn't be made).
    tray: Option<crate::tray::Tray>,
    /// Raised when synththing is launched again for this config folder
    /// (`instance.rs`): the window comes back.
    second_launch: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The mini player, what clicking the tray icon shows.
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
        mixer: rodio::mixer::Mixer,
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
            mixer,
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
            updates_open: false,
            branches: crate::channels::Branches::default(),
            confirm_recopy: false,
            updates_message: None,
            launch_update_check: false,
            log_open: false,
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
            tags_open: false,
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
            moderation: Default::default(),
            feedback: Default::default(),
            storage: Default::default(),
            repair: Default::default(),
            identity: Default::default(),
            pairing: Default::default(),
            lookup: Default::default(),
            choose_art: None,
            sync: Default::default(),
            notifications: Default::default(),
            copyright: Default::default(),
            changelog: None,
            welcome: welcome::Welcome::default(),
            preferences_tab: PrefTab::default(),
            themes: themes::ThemesState::default(),
            tour: tour::Tour::default(),
            credits_open: false,
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
            stopped_in: None,
            seek_on_load: None,
            rng: Rng::from_time(),
            playlist_rename: None,
            playlist_find: String::new(),
            image_for: None,
            confirm_delete_playlist: false,
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
            docs_target: None,
            pending_focus: None,
            sprite: sprite_editor::SpriteEditorState::default(),
            sample_rate,
            editor: editor::EditorState::default(),
            editor_saved: String::new(),
            recording: recording::Recording::new(),
            wordmark: branding::Wordmark::default(),
            tray: None,
            second_launch: None,
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

    /// What the updater follows: this branch build's branch, or canary or
    /// releases as chosen in Preferences.
    fn update_track(&self) -> Track {
        Track::current(self.config.canary_updates())
    }

    /// Look for a new release in the background, if that's enabled. Not in
    /// development builds (`cargo run`), which shouldn't offer to replace
    /// themselves with a release.
    pub fn check_for_updates_on_launch(&mut self) {
        if cfg!(debug_assertions) || !self.config.check_updates_on_launch.unwrap_or(true) {
            return;
        }
        self.launch_update_check = true;
        self.updater.check(self.update_track());
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

    /// Settings files that couldn't be read and were set aside (at launch,
    /// or later for ones read later): what happened, and where the old file
    /// is.
    fn set_aside_ui(&mut self, ctx: &egui::Context) {
        self.set_aside.extend(crate::config::set_aside_notices());
        if self.set_aside.is_empty() {
            return;
        }
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("set_aside")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 480.0));
            ui.heading("Some settings couldn't be read");
            ui.add_space(4.0);
            for (note, kept) in &self.set_aside {
                ui.label(note);
                if ui.small_button("Show it").on_hover_text(kept.display().to_string()).clicked() {
                    open_in_file_manager(kept);
                }
                ui.add_space(4.0);
            }
            ui.weak("Nothing was deleted: the build that saved it can still read the kept copy.");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    close = true;
                }
            });
        });
        if close || response.should_close() {
            self.set_aside.clear();
        }
    }

    /// What's new (since a version), or the whole changelog, as a modal
    /// (the mobile layout shows it as a page instead).
    fn changelog_ui(&mut self, ctx: &egui::Context) {
        let Some(since) = self.changelog else { return };
        if self.mobile() {
            return;
        }
        let mut close = false;
        let mut show_all = false;
        let response = egui::Modal::new(egui::Id::new("changelog")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 560.0));
            ui.heading(if since.is_some() { "What's new" } else { "Changelog" });
            show_all = changelog_body_ui(ui, since, false);
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
            self.dock.set_focused_node_and_surface(path.node_path());
        }
    }

    /// The open section `step` places from the one in front (the focused
    /// tab), in `Section::ALL`'s order, round and round.
    fn step_section(&mut self, step: isize) {
        let open: Vec<Section> = Section::ALL.into_iter().filter(|&s| self.is_open(s)).collect();
        let current = self.dock.find_active_focused().map(|(_, tab)| *tab);
        let Some(at) = current.and_then(|c| open.iter().position(|&s| s == c)).or((!open.is_empty()).then_some(0)) else {
            return;
        };
        let next = (at as isize + step).rem_euclid(open.len() as isize) as usize;
        self.reveal_section(open[next]);
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
        self.config.layout = Some(self.dock.clone());
        self.store_in_active_layout();
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save layout: {e}");
        }
    }

    /// The named layout the showing slot is on, if any.
    fn active_layout(&self) -> Option<&String> {
        self.config.active_layout.as_ref()
    }

    fn set_active_layout(&mut self, name: Option<String>) {
        self.config.active_layout = name;
    }

    /// Changes to the arrangement go into the active named layout.
    fn store_in_active_layout(&mut self) {
        let Some(name) = self.active_layout().cloned() else { return };
        if let Some(saved) = self.config.layout_presets.iter_mut().find(|p| p.name == name) {
            saved.dock = self.dock.clone();
        }
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

    /// Show the named layout (as the user last left it), after keeping the
    /// current arrangement in the layout it belongs to.
    fn switch_layout(&mut self, name: &str) {
        if self.dock_in_use {
            return;
        }
        self.save_layout();
        let Some(saved) = self.config.layout_presets.iter().find(|p| p.name == name) else { return };
        let dock = layout::sanitize(saved.dock.clone());
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

    // --- actions -------------------------------------------------------

    /// A song picked straight from the browser, leaves any playlist.
    fn play_path(&mut self, path: PathBuf) {
        self.now_playing = None;
        self.listed = None;
        self.request_play(&path, None, None, false);
    }

    fn add_soundfont(&mut self, path: PathBuf) {
        let name = nice_name(&path);
        self.status = match self.try_add_soundfont(&path) {
            Ok(Some(note)) => format!("Added '{name}', but {note}"),
            Ok(None) => format!("Added soundfont: {name}"),
            Err(problem) => problem,
        };
    }

    /// Add `paths` to the soundfont list, saying how it went in one line.
    fn add_soundfonts(&mut self, paths: Vec<PathBuf>) {
        if let [path] = paths.as_slice() {
            self.add_soundfont(path.clone());
            return;
        }
        let mut added = 0;
        let mut problems = Vec::new();
        for path in &paths {
            match self.try_add_soundfont(path) {
                Ok(note) => {
                    added += 1;
                    if let Some(note) = note {
                        problems.push(format!("'{}' was added, but {note}", nice_name(path)));
                    }
                }
                Err(problem) => problems.push(problem),
            }
        }
        for problem in &problems {
            log::info!("adding soundfonts: {problem}");
        }
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        self.status = match problems.as_slice() {
            [] => format!("Added {added} soundfont{}.", plural(added)),
            [one] => format!("Added {added} soundfont{}. {one}", plural(added)),
            more => format!(
                "Added {added} soundfont{}; {} had problems (Help > Log has them all). {}",
                plural(added),
                more.len(),
                more[0]
            ),
        };
    }

    /// Add one soundfont to the list: `Ok` with a note if it's added but
    /// can't be played by this build, or why it wasn't added. Only files
    /// that aren't SoundFont containers at all are refused: a valid one
    /// this build of rustysynth can't render yet (a compressed SF3, say) is
    /// still added, with a note.
    fn try_add_soundfont(&mut self, path: &Path) -> Result<Option<String>, String> {
        let name = nice_name(path);
        let note = match loader::probe_soundfont(path) {
            Ok(SoundFontProbe::Playable(soundfont)) => {
                // Already parsed it, so keep it rather than parse it again
                // when it's picked.
                self.assets.insert_ready(path, Asset::SoundFont(soundfont), std::time::Instant::now());
                None
            }
            Ok(SoundFontProbe::Unplayable(reason)) => Some(reason),
            Ok(SoundFontProbe::NotSoundFont) => {
                return Err(format!("'{name}' is not a SoundFont file (no RIFF/sfbk header)."));
            }
            Err(e) => return Err(format!("Failed to read '{name}': {e}")),
        };
        if !self.config.add_soundfont(path) {
            return Err(format!("'{name}' is already in the list."));
        }
        if let Err(e) = self.config.save() {
            return Ok(Some(format!("the list couldn't be saved: {e}")));
        }
        Ok(note)
    }

    /// Take the soundfonts at `indices` off the list.
    fn remove_soundfonts(&mut self, indices: &[usize]) {
        let mut indices: Vec<usize> = indices.iter().copied().filter(|&i| i < self.config.soundfonts.len()).collect();
        // Highest first, so the rest stay valid.
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();
        let names: Vec<String> = indices.iter().map(|&i| nice_name(&self.config.soundfonts[i])).collect();
        for &idx in &indices {
            self.config.remove_soundfont(idx);
            self.active_sf = shift_active(self.active_sf, idx);
        }
        self.soundfont_selection.clear();
        self.pending_soundfont = None;
        let removed = match names.as_slice() {
            [] => return,
            [name] => format!("soundfont: {name}"),
            more => format!("{} soundfonts", more.len()),
        };
        self.status = match self.config.save() {
            Err(e) => format!("Removed {removed}, but failed to save list: {e}"),
            Ok(()) => format!("Removed {removed}"),
        };
    }

    /// Browse for soundfonts to add: one or several at once. (On
    /// Android, import them: `import.rs`.)
    fn open_soundfont_browser(&mut self) {
        #[cfg(target_os = "android")]
        crate::import::start(crate::import::Kind::Soundfonts, &["sf2"]);
        #[cfg(not(target_os = "android"))]
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("SoundFont", &["sf2"])
            .pick_files()
        {
            self.add_soundfonts(paths);
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
                self.script.load(Some(path.clone()), source);
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

    /// The seek bar's color: the tempo's (`tempo_color`), easing toward a
    /// new one when the tempo changes, flashing brighter on each beat (more
    /// on a bar's first) while it plays, unless that's turned off. None for
    /// a song without a tempo map (an audio file): the theme's accent.
    fn transport_accent(&mut self, ctx: &egui::Context, view: &EngineView) -> Option<egui::Color32> {
        let timing = self.song_notes.as_ref().filter(|_| view.has_midi).and_then(|n| n.timing().cloned());
        let Some(timing) = timing else {
            self.accent = None;
            return None;
        };
        let target = tempo_color(timing.tempo_at(view.position));
        let ease = 1.0 - (-ctx.input(|i| i.stable_dt).min(0.1) * 4.0).exp();
        let shown = match self.accent {
            Some(shown) => [0, 1, 2].map(|i| shown[i] + (target[i] - shown[i]) * ease),
            None => target,
        };
        self.accent = Some(shown);
        if (0..3).any(|i| (shown[i] - target[i]).abs() > 0.5) {
            ctx.request_repaint();
        }
        let playing = !view.paused && !view.finished;
        let strength = self.config.beat_pulse.unwrap_or(config::DEFAULT_BEAT_PULSE).clamp(0.0, 1.0);
        let length = self.config.beat_pulse_length.unwrap_or(config::DEFAULT_BEAT_PULSE_LENGTH).clamp(0.1, 1.0);
        let pulse = if playing && strength > 0.0 {
            ctx.request_repaint();
            let since_beat = timing.beat_at(view.position).fract() as f32;
            let downbeat = timing.bar_at(view.position).1 < 1.0;
            // (A flash `strength` of the way to white, so relative to how dark
            // the color is: 100% is white. It fades over `length` of the beat,
            // half as long again on a bar's first, which marks the bar.)
            let length = if downbeat { (length * 1.5).min(1.0) } else { length };
            let left = (1.0 - since_beat / length).max(0.0);
            left * left * strength
        } else {
            0.0
        };
        let [r, g, b] = shown.map(|c| (c + (255.0 - c) * pulse).round().clamp(0.0, 255.0) as u8);
        Some(egui::Color32::from_rgb(r, g, b))
    }

    /// The bottom transport bar, across the window's width: the song (title
    /// over a weak detail line) with the status line at the right, the seek
    /// bar between the times, then the view toggles, the transport in the
    /// middle, and speed and volume. Shown identically whether fullscreen or
    /// not, so it's the one thing that's always reachable.
    fn controls_bar_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        if self.following() {
            return self.following_bar_ui(ui, view);
        }
        ui.add_space(6.0);

        let line = egui::vec2(ui.available_width(), 34.0);
        ui.allocate_ui_with_layout(line, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.scope(|ui| {
                    ui.set_max_width(ui.available_width() * 0.45);
                    if status_line_ui(ui, &self.status, &mut self.unseen_failure, applog::elapsed, applog::entries) {
                        self.log_search = STATUS_TARGET.to_string();
                        self.log_open = true;
                    }
                });
                ui.add_space(12.0);
                let title = match self.current_song.clone() {
                    Some(path) => self.song_title(&path),
                    None => view.track_name.clone().unwrap_or_else(|| "Nothing playing".to_string()),
                };
                let detail = self.song_detail(view);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    select_list::two_lines(ui, std::iter::empty(), egui::RichText::new(title).strong(), Some(detail), true);
                });
            });
        });

        let accent = self.transport_accent(ui.ctx(), view);
        ui.horizontal(|ui| {
            ui.weak(fmt_time(view.position));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(fmt_time(view.length));
                ui.spacing_mut().slider_width = ui.available_width();
                let mut position = view.position;
                if seek_bar_ui(ui, &mut position, view.length, accent).changed() {
                    self.send(AudioCommand::Seek(position));
                }
            });
        });
        ui.add_space(2.0);

        // The devices button at the left, the transport centred, speed and
        // volume at the right; when they don't fit, the transport centred
        // over the rest.
        let width = ui.available_width();
        let center = TRANSPORT_WIDTH;
        let side = (width - center) / 2.0;
        let line = |ui: &mut egui::Ui| ui.allocate_exact_size(egui::vec2(width, TRANSPORT_HEIGHT), egui::Sense::hover()).0;
        let zone = |rect: egui::Rect, x: f32, width: f32, layout: egui::Layout| {
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_size(egui::pos2(rect.left() + x, rect.top()), egui::vec2(width, rect.height())))
                .layout(layout)
        };
        let (left, right) = (egui::Layout::left_to_right(egui::Align::Center), egui::Layout::right_to_left(egui::Align::Center));
        if side >= SIDE_WIDTH {
            let rect = line(ui);
            ui.scope_builder(zone(rect, 0.0, side, left), |ui| self.devices_button_ui(ui, view));
            ui.scope_builder(zone(rect, side, center, left), |ui| self.transport_ui(ui, view));
            ui.scope_builder(zone(rect, side + center, side, right), |ui| self.speed_volume_ui(ui, view));
        } else {
            let rect = line(ui);
            ui.scope_builder(zone(rect, side.max(0.0), center, left), |ui| self.transport_ui(ui, view));
            ui.add_space(4.0);
            if width >= DEVICES_WIDTH + SIDE_WIDTH {
                let rect = line(ui);
                ui.scope_builder(zone(rect, 0.0, DEVICES_WIDTH, left), |ui| self.devices_button_ui(ui, view));
                ui.scope_builder(zone(rect, DEVICES_WIDTH, width - DEVICES_WIDTH, right), |ui| self.speed_volume_ui(ui, view));
            } else {
                ui.horizontal_wrapped(|ui| {
                    self.devices_button_ui(ui, view);
                    self.speed_volume_ui(ui, view);
                });
            }
        }
        ui.add_space(6.0);
    }

    /// The bar's detail line: where the song is on its playlist, its
    /// soundfont, or why nothing plays.
    fn song_detail(&self, view: &EngineView) -> String {
        let mut parts = Vec::new();
        if let Some(np) = self.playing_entry()
            && let Some(list) = self.playlists.lists.get(np.list)
        {
            parts.push(format!("{} {}/{}", list.playlist.name, np.entry + 1, list.playlist.entries.len()));
        }
        if view.has_midi && view.has_soundfont {
            parts.push(view.soundfont_name.clone().unwrap_or_default());
        }
        match playback_state_text(view) {
            "no song" | "no soundfont" => parts.push(playback_state_text(view).to_string()),
            _ => {}
        }
        parts.retain(|p| !p.is_empty());
        parts.join("  ·  ")
    }

    /// Shuffle, stop, previous, play/pause, next and loop.
    fn transport_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        let hover = shuffle_text(self.shuffle);
        if ui.add(icon_button("shuffle").selected(self.shuffle)).on_hover_text(hover).clicked() {
            self.set_shuffle(!self.shuffle);
            toast::show(ui.ctx(), shuffle_text(self.shuffle));
        }
        if ui.add(icon_button("stop")).on_hover_text("Stop").clicked() {
            self.send(AudioCommand::Seek(0.0));
            if !view.paused {
                self.send(AudioCommand::TogglePause);
            }
        }
        if ui
            .add(icon_button("skip-back"))
            .on_hover_text(format!(
                "Previous: the previous playlist track, or this one from the start if it's been playing a few seconds{}",
                commands::hint(ui.ctx(), Command::PreviousTrack)
            ))
            .clicked()
        {
            self.previous_track(view.position);
        }
        let hover = if view.paused { "Play" } else { "Pause" };
        let hover = format!("{hover}{}", commands::hint(ui.ctx(), Command::TogglePause));
        let play = play_button(ui.ctx(), view.paused, 18.0, egui::vec2(PLAY_WIDTH, TRANSPORT_HEIGHT));
        if ui.add(play).on_hover_text(hover).clicked() {
            self.send(AudioCommand::TogglePause);
        }
        if ui
            .add_enabled(self.has_next(), icon_button("skip-forward"))
            .on_hover_text(format!(
                "Next: the next queued song, else the next playlist track, or the next song autoplay plays{}",
                commands::hint(ui.ctx(), Command::NextTrack)
            ))
            .on_disabled_hover_text("Queue songs (right-click them), or play something from a playlist or from Songs with Autoplay on, to have a next track")
            .clicked()
        {
            self.next_track();
        }
        let (glyph, hover) = match self.loop_mode {
            LoopMode::Off => ("repeat", "Loop: off (click: loop the playlist, or what autoplay plays)"),
            LoopMode::All => ("repeat", "Loop: the playlist, or what autoplay plays (click: this track)"),
            LoopMode::One => ("repeat-once", "Loop: this track (click: off)"),
        };
        if ui.add(icon_button(glyph).selected(self.loop_mode != LoopMode::Off)).on_hover_text(hover).clicked() {
            self.set_loop_mode(self.loop_mode.cycle());
            toast::show(ui.ctx(), loop_text(self.loop_mode));
        }
    }

    /// Speed and volume, each an icon, a slider and its value (in a
    /// right-to-left layout). The speed's icon puts it back to 1x.
    fn speed_volume_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        ui.add_sized([36.0, TRANSPORT_HEIGHT], egui::Label::new(egui::RichText::new(format!("{:.0}%", self.volume * 100.0)).weak()));
        ui.spacing_mut().slider_width = 96.0;
        if ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false)).changed() {
            self.send_volume();
        }
        let speaker = match self.volume {
            v if v <= 0.0 => "speaker-x",
            v if v < 0.5 => "speaker-low",
            _ => "speaker-high",
        };
        ui.label(egui::RichText::new(icon(speaker)).size(16.0)).on_hover_text("Volume");

        ui.add_space(12.0);
        ui.add_sized([40.0, TRANSPORT_HEIGHT], egui::Label::new(egui::RichText::new(format!("{:.2}x", view.speed)).weak()));
        let mut speed = view.speed;
        if ui.add(egui::Slider::new(&mut speed, MIN_SPEED..=MAX_SPEED).show_value(false)).changed() {
            self.send(AudioCommand::SetSpeed(speed));
        }
        if ui.add(icon_button("gauge").selected(view.speed != 1.0)).on_hover_text("Speed (click: back to 1x)").clicked() {
            self.send(AudioCommand::SetSpeed(1.0));
        }
    }

    fn soundfont_ui(&mut self, ui: &mut egui::Ui) {
        self.starter_progress_ui(ui);
        let mobile = self.mobile();
        let mut browse = false;
        let count = match self.config.soundfonts.len() {
            1 => "1 soundfont".to_string(),
            n => format!("{n} soundfonts"),
        };
        let left = |ui: &mut egui::Ui| {
            if mobile {
                if ui.button(format!("{}  Import soundfonts...", icon("plus"))).on_hover_text("Copy soundfonts into synththing").clicked() {
                    crate::import::start(crate::import::Kind::Soundfonts, &["sf2"]);
                }
            } else {
                browse = ui.add(icon_button("plus")).on_hover_text("Add a soundfont...").clicked();
            }
        };
        select_list::toolbar(ui, left, count, |_| {});
        if browse {
            self.open_soundfont_browser();
        }

        let active_sf = self.active_sf;
        let touch = select_list::touch(ui.ctx());
        // The header, as Up next's (Name, then the size at the right).
        let size_width = if self.config.soundfonts.is_empty() {
            placeholder(ui, "No soundfonts yet.\nAdd one with + to play MIDI songs.");
            0.0
        } else if touch {
            0.0
        } else {
            let id = egui::Id::new("soundfonts_header");
            let widths: select_list::Widths = ui.ctx().data_mut(|d| d.get_persisted(id.with("widths"))).unwrap_or_default();
            let width = select_list::column_width(ui.ctx(), id, "size", &widths, SOUNDFONT_SIZE_WIDTH);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ui.spacing().interact_size.y), egui::Sense::hover());
            let strong = |text: &str| egui::RichText::new(text).strong();
            let columns = [select_list::Column { key: "size", title: strong("Size"), width, numeric: true }];
            let header = select_list::Header { title: strong("Name"), columns: &columns, lead: 0.0, trail: 0.0, min_main: 120.0 };
            if let Some((key, width)) = select_list::header(ui, id, rect, header).resized {
                let mut widths = widths;
                match width {
                    Some(width) => widths.insert(key, width),
                    None => widths.remove(&key),
                };
                ui.ctx().data_mut(|d| d.insert_persisted(id.with("widths"), widths));
            }
            width
        };
        let mut remove = None;
        let mut use_idx = None;
        let output = select_list::show(ui, "soundfonts", &self.config.soundfonts, &mut self.soundfont_selection, active_sf, |ui, row, path| {
            if row.response.drag_started() {
                row.response.dnd_set_drag_payload(DraggedSoundfont(path.clone()));
            }
            crate::menu::context(&row.response, |ui| {
                if ui.button("Use").clicked() {
                    use_idx = Some(row.idx);
                }
                let rows = row.rows();
                let count = if rows.len() > 1 { format!(" ({})", rows.len()) } else { String::new() };
                if ui.button(format!("Remove from the list{count}")).clicked() {
                    remove = Some(rows);
                }
            });
            let active = Some(row.idx) == active_sf;
            let text = egui::RichText::new(nice_name(path));
            let text = if active { text.strong() } else { text };
            let scroll = select_list::scrolls(row, active);
            // Its size, or that it's gone.
            let size = std::fs::metadata(path).map_or_else(|_| "missing".to_string(), |m| crate::storage::size_text(m.len()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if touch {
                    let detail = if active { format!("in use · {size}") } else { size };
                    select_list::two_lines(ui, std::iter::empty(), text, Some(detail), scroll);
                    return;
                }
                select_list::cell(ui, size_width, egui::RichText::new(size).weak(), false, true);
                let hover = format!(
                    "{}\nClick to use; drag onto a MIDI playlist entry to override its soundfont",
                    path.display()
                );
                select_list::label(ui, text, Some(hover), scroll);
            });
        });
        if output.delete {
            remove = Some(self.soundfont_selection.rows().to_vec());
        }

        if let Some(i) = output.activated.or(use_idx) {
            self.activate_soundfont(i);
        }
        if let Some(rows) = remove {
            self.remove_soundfonts(&rows);
        }
    }


    /// The app log viewer, as a modal (the mobile layout shows it as a
    /// page instead).
    fn log_ui(&mut self, ctx: &egui::Context) {
        if !self.log_open || self.mobile() {
            return;
        }
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("app_log")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 720.0));
            ui.heading("Log");
            close = self.log_body_ui(ui, false);
        });
        if close || response.should_close() {
            self.log_open = false;
        }
    }

    /// The log: level filter, search, copy, and the log file's location.
    /// On a page the entries fill what's left, under the buttons, and
    /// there's no Close; whether Close was clicked.
    pub(super) fn log_body_ui(&mut self, ui: &mut egui::Ui, page: bool) -> bool {
        let entries = applog::entries();
        let search = self.log_search.to_lowercase();
        let shown: Vec<&applog::Entry> = entries
            .iter()
            .filter(|e| e.level <= self.log_min_level)
            .filter(|e| search.is_empty() || e.line().to_lowercase().contains(&search))
            .collect();
        ui.ctx().request_repaint_after(Duration::from_millis(250));

        let mut close = false;
        ui.horizontal_wrapped(|ui| {
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
        if page {
            log_actions_ui(ui, &shown);
            log_file_note_ui(ui);
        }
        ui.separator();
        egui::ScrollArea::both()
            .id_salt("app_log_scroll")
            .max_height(if page { f32::INFINITY } else { 420.0 })
            .auto_shrink([false, !page])
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
        if !page {
            ui.separator();
            log_file_note_ui(ui);
            ui.horizontal(|ui| {
                log_actions_ui(ui, &shown);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        }
        close
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
                UpdateState::Gone => {
                    self.launch_update_check = false;
                    self.updates_open = true;
                }
                _ => self.launch_update_check = false,
            }
        }
        if matches!(state, UpdateState::Checking | UpdateState::Downloading { .. }) || self.branches.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if !self.updates_open || self.mobile() {
            return;
        }
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("updates")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 440.0));
            ui.heading("Updates");
            ui.add_space(4.0);
            close = self.updates_body_ui(ui, false);
        });
        if close || response.should_close() {
            self.updates_open = false;
            self.updates_message = None;
        }
    }

    /// An update is offered (the Updates window or page is open with one
    /// available) or downloading: sync and the online lookup wait, so the
    /// download has the connection. Closing it, or installing it (the app
    /// restarts without it open), lets them go on.
    pub(super) fn update_offered(&self) -> bool {
        match self.updater.state() {
            UpdateState::Downloading { .. } => true,
            UpdateState::Available(_) if self.mobile() => self.on_updates_page(),
            UpdateState::Available(_) => self.updates_open,
            _ => false,
        }
    }

    /// What the update check found, release notes, and installing. On a
    /// page there's no Close (back leaves), and the release notes aren't
    /// a scroll area of their own; whether a way out (Close, Not now,
    /// Skip, Later) was clicked.
    pub(super) fn updates_body_ui(&mut self, ui: &mut egui::Ui, page: bool) -> bool {
        let state = self.updater.state();
        let current = updater::current_version();
        let mut close = false;
        let branch = match crate::build_info::channel() {
            crate::build_info::Channel::Branch(name) => Some(name),
            _ => None,
        };
        if let Some(name) = branch {
            ui.label(format!(
                "This is a test build of the {name} branch. It updates from that branch and keeps settings of \
                 its own, so your usual synththing isn't affected."
            ));
            ui.add_space(4.0);
        } else if self.config.canary_updates() {
            ui.weak("Canary updates: the newest build from master (Preferences > App).");
            ui.add_space(4.0);
        }
        match &state {
            UpdateState::Idle => {
                if ui.button("Check for updates").clicked() {
                    self.updater.check(self.update_track());
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
                    if page {
                        ui.label(release.notes.trim());
                    } else {
                        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                            ui.label(release.notes.trim());
                        });
                    }
                }
                ui.hyperlink_to("Release page", &release.page_url);
                ui.add_space(6.0);
                let dev_build = cfg!(debug_assertions);
                if let Some(announce) = self.android_update_ui(ui) {
                    if announce {
                        // Shown this once: next time, only if there's a newer one.
                        self.config.skipped_update = Some(release.version.to_string());
                        if let Err(e) = self.config.save() {
                            self.status = format!("Couldn't save preferences: {e}");
                        }
                        close = true;
                    }
                } else {
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
            }
            UpdateState::Downloading { release, downloaded } => {
                ui.label(format!("Downloading v{}...", release.version));
                let total = release.size().max(1);
                ui.add(
                    egui::ProgressBar::new(*downloaded as f32 / total as f32)
                        .text(format!("{:.1} / {:.1} MB", *downloaded as f64 / 1e6, total as f64 / 1e6)),
                );
            }
            UpdateState::Installed(version) if cfg!(target_os = "android") => {
                ui.label(format!("Android asks you to install v{version}; synththing restarts with it."));
                if ui.button("OK").clicked() {
                    close = true;
                }
            }
            UpdateState::Installed(version) => {
                ui.label(format!("v{version} is installed. It runs the next time synththing starts."));
                ui.horizontal(|ui| {
                    if ui.button("Restart now").clicked() {
                        match updater::relaunch() {
                            // (Quit, not close: closing may only hide it in the tray.)
                            Ok(()) => self.close_with(&ui.ctx().clone(), close::CloseAction::Quit),
                            Err(e) => self.updates_message = Some((format!("Couldn't restart: {e}"), true)),
                        }
                    }
                    if ui.button("Later").clicked() {
                        close = true;
                    }
                });
            }
            UpdateState::Gone if cfg!(target_os = "android") => {
                let name = branch.unwrap_or_default();
                ui.label(format!(
                    "The {name} branch's build is gone: the branch was merged or deleted, so this app won't get \
                     updates any more. Uninstall it when you're done with it."
                ));
            }
            UpdateState::Gone => {
                let name = branch.unwrap_or_default();
                ui.label(format!(
                    "The {name} branch's build is gone: the branch was merged or deleted, so this build won't \
                     get updates any more. Go back to your usual synththing, and remove this one there, under \
                     Help > Check for updates > Branch builds."
                ));
                if let Some(stable) = crate::channels::installed_by()
                    && ui.button("Open the usual synththing").clicked()
                {
                    match std::process::Command::new(&stable).spawn() {
                        Ok(_) => self.close_with(&ui.ctx().clone(), close::CloseAction::Quit),
                        Err(e) => {
                            self.updates_message = Some((format!("Couldn't start {}: {e}", stable.display()), true));
                        }
                    }
                }
            }
            UpdateState::Failed(error) => {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
                if ui.button("Try again").clicked() {
                    self.updater.check(self.update_track());
                }
            }
        }
        if branch.is_none() {
            ui.add_space(8.0);
            self.branch_builds_ui(ui);
        } else if !cfg!(target_os = "android") {
            ui.add_space(8.0);
            self.recopy_settings_ui(ui);
        }
        if let Some((message, error)) = &self.updates_message {
            ui.add_space(6.0);
            if *error {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), message);
            } else {
                ui.label(message);
            }
        }
        // Every state but Available has its own way out above, so a
        // plain Close for the rest. (A download carries on in the
        // background if this closes, and Help > Check for updates
        // shows how it went.)
        if !page && !matches!(state, UpdateState::Available(_) | UpdateState::Installed(_)) {
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        }
        close
    }

    /// In a branch build's Updates window: start its settings over from a
    /// copy of the usual synththing's, for a tester who's changed something
    /// there since. Takes a restart.
    fn recopy_settings_ui(&mut self, ui: &mut egui::Ui) {
        if !self.confirm_recopy {
            if ui
                .button("Copy settings from your usual synththing again...")
                .on_hover_text("This build's settings started as a copy of them; take a fresh copy")
                .clicked()
            {
                self.confirm_recopy = true;
            }
            return;
        }
        ui.label(
            "Copy your usual synththing's settings over this build's, and restart it? Scripts and other files only \
             this build has are kept.",
        );
        ui.horizontal(|ui| {
            if ui.button("Copy and restart").clicked() {
                self.confirm_recopy = false;
                match crate::channels::recopy_settings_on_restart().and_then(|()| updater::relaunch()) {
                    Ok(()) => self.close_with(&ui.ctx().clone(), close::CloseAction::Quit),
                    Err(e) => self.updates_message = Some((format!("Couldn't copy the settings: {e:#}"), true)),
                }
            }
            if ui.button("Cancel").clicked() {
                self.confirm_recopy = false;
            }
        });
    }

    /// The Updates window's Branch builds section: trying a feature before
    /// it's merged. Each installs into a folder of its own with a copy of
    /// the settings, or on Android as an app of its own (`channels.rs`), so
    /// this install isn't touched.
    fn branch_builds_ui(&mut self, ui: &mut egui::Ui) {
        let response = egui::CollapsingHeader::new("Branch builds").id_salt("branch_builds").show(ui, |ui| {
            if cfg!(target_os = "android") {
                ui.label(
                    "Try a feature before it's merged. Each branch build installs as an app of its own, with \
                     settings of its own, and its online features are off. This synththing isn't touched.",
                );
            } else {
                ui.label(
                    "Try a feature before it's merged. Each branch build installs separately, starting with a \
                     copy of your settings, and its online features are off. This synththing isn't touched.",
                );
            }
            // Android has to let this app install others first.
            #[cfg(target_os = "android")]
            let can_install = crate::android::can_install_packages();
            #[cfg(not(target_os = "android"))]
            let can_install = true;
            #[cfg(target_os = "android")]
            if !can_install {
                ui.label("Android doesn't allow synththing to install apps yet.");
                if ui.button("Allow it...").on_hover_text("Settings > Install unknown apps, for synththing").clicked() {
                    crate::android::open_install_settings();
                }
            }
            ui.add_space(4.0);
            let installed = crate::channels::installed();
            let list = self.branches.list();
            let install = self.branches.install_state();
            let installing = matches!(install, crate::channels::InstallState::Installing { .. });
            let builds: &[updater::Release] = match &list {
                crate::channels::ListState::Loaded(builds) => builds,
                _ => &[],
            };
            match &list {
                crate::channels::ListState::Idle => {}
                crate::channels::ListState::Loading => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Looking for branch builds...");
                    });
                }
                crate::channels::ListState::Failed(error) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
                }
                crate::channels::ListState::Loaded(builds) if builds.is_empty() && installed.is_empty() => {
                    ui.weak("There are no branch builds right now.");
                }
                crate::channels::ListState::Loaded(_) => {}
            }
            // Every branch with a build, then installed ones whose build is
            // gone.
            let mut rows: Vec<(String, Option<&updater::Release>)> =
                builds.iter().map(|b| (b.tag.clone(), Some(b))).collect();
            for channel in &installed {
                if !builds.iter().any(|b| &b.tag == channel) {
                    rows.push((channel.clone(), None));
                }
            }
            // A grid, or on a narrow screen (a phone's) each branch's name
            // over its version and buttons.
            let narrow = ui.available_width() < 480.0;
            if narrow {
                for (channel, build) in rows {
                    ui.strong(crate::channels::branch_name(&channel));
                    ui.horizontal_wrapped(|ui| self.branch_build_row_ui(ui, &channel, build, &installed, installing, can_install));
                    ui.add_space(4.0);
                }
            } else {
                egui::Grid::new("branch_builds_grid").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
                    for (channel, build) in rows {
                        ui.strong(crate::channels::branch_name(&channel));
                        self.branch_build_row_ui(ui, &channel, build, &installed, installing, can_install);
                        ui.end_row();
                    }
                });
            }
            match &install {
                crate::channels::InstallState::Idle => {}
                crate::channels::InstallState::Installing { channel, downloaded, size } => {
                    ui.label(format!("Installing the {} build...", crate::channels::branch_name(channel)));
                    ui.add(
                        egui::ProgressBar::new(*downloaded as f32 / (*size).max(1) as f32)
                            .text(format!("{:.1} / {:.1} MB", *downloaded as f64 / 1e6, *size as f64 / 1e6)),
                    );
                }
                crate::channels::InstallState::Done(channel) if cfg!(target_os = "android") => {
                    ui.label(format!(
                        "Android asks you to install the {} build. Once it's in, open it here or from your apps.",
                        crate::channels::branch_name(channel)
                    ));
                }
                crate::channels::InstallState::Done(channel) => {
                    ui.label(format!("The {} build is installed and starting.", crate::channels::branch_name(channel)));
                }
                crate::channels::InstallState::Failed(error) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
                }
            }
            if ui.add_enabled(!self.branches.busy(), egui::Button::new("Refresh")).clicked() {
                self.branches.refresh();
            }
        });
        // The list is fetched the first time the section opens.
        if response.body_returned.is_some() && matches!(self.branches.list(), crate::channels::ListState::Idle) {
            self.branches.refresh();
        }
    }

    /// A branch build's version and its buttons (Install, or Open and
    /// Remove), side by side.
    fn branch_build_row_ui(
        &mut self,
        ui: &mut egui::Ui,
        channel: &str,
        build: Option<&updater::Release>,
        installed: &[String],
        installing: bool,
        can_install: bool,
    ) {
        match build {
            Some(b) => {
                let commit = b.commit.as_deref().map(|c| &c[..c.len().min(7)]).unwrap_or_default();
                ui.weak(format!("{} {commit}", b.version)).on_hover_text(b.notes.trim());
            }
            None => {
                ui.weak("its build is gone (merged?)");
            }
        }
        ui.horizontal(|ui| {
            if installed.iter().any(|c| c == channel) {
                if ui.add_enabled(!installing, egui::Button::new("Open")).clicked()
                    && let Err(e) = crate::channels::launch(channel)
                {
                    self.updates_message = Some((format!("{e:#}"), true));
                }
                if ui
                    .add_enabled(!installing, egui::Button::new("Remove"))
                    .on_hover_text("Delete this branch build and its settings")
                    .clicked()
                {
                    let name = crate::channels::branch_name(channel);
                    self.updates_message = Some(match crate::channels::remove(channel) {
                        Ok(()) if cfg!(target_os = "android") => {
                            (format!("Android asks you to uninstall the {name} build."), false)
                        }
                        Ok(()) => (format!("Removed the {name} build."), false),
                        Err(e) => (format!("{e:#}"), true),
                    });
                }
            } else if let Some(b) = build
                && ui.add_enabled(!installing && can_install, egui::Button::new("Install")).clicked()
            {
                self.branches.install(b.clone());
            }
        });
    }

    /// Help > Credits & licenses: contributors (from GitHub), the font, Lua,
    /// and every library in the app with its license text.
    fn credits_ui(&mut self, ctx: &egui::Context) {
        if !self.credits_open || self.mobile() {
            return;
        }
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("credits")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 660.0));
            ui.heading("Credits & licenses");
            self.credits_body_ui(ui, false);
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

    /// Contributors (from GitHub), the font, Lua, and every library in
    /// the app with its license text. On a page the list fills what's
    /// left.
    pub(super) fn credits_body_ui(&mut self, ui: &mut egui::Ui, page: bool) {
        self.contributors.fetch_once();
        let contributors = self.contributors.state();
        if matches!(contributors, ContributorsState::Fetching) {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        let third_party = credits::third_party();
        let repo_url = format!("https://github.com/{}", updater::REPO);
        ui.horizontal_wrapped(|ui| {
            ui.weak(format!("synththing v{}", crate::build_info::describe()));
            ui.hyperlink_to(&repo_url, &repo_url);
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("synththing itself is released under the WTFPL: do whatever you want with it.");
            ui.hyperlink_to("License", format!("{repo_url}/blob/master/LICENSE"));
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("credits_scroll")
            .max_height(if page { f32::INFINITY } else { 480.0 })
            .auto_shrink([false, !page])
            .show(
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
                ui.strong("Fonts");
                ui.horizontal_wrapped(|ui| {
                    ui.label("Script text uses Monogram by Vinícius Menézio (datagoblin), released to the public domain (CC0). Attribution isn't required, but it's given gladly.");
                    ui.hyperlink_to("datagoblin.itch.io/monogram", "https://datagoblin.itch.io/monogram");
                });
                egui::CollapsingHeader::new("Monogram's credits").id_salt("credits_monogram").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::MONOGRAM_CREDITS.trim()).monospace());
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Japanese and Korean text uses Noto Sans JP and Noto Sans KR by Google and Adobe, under the SIL Open Font License 1.1.");
                    ui.hyperlink_to("github.com/notofonts/noto-cjk", "https://github.com/notofonts/noto-cjk");
                });
                egui::CollapsingHeader::new("Noto Sans CJK's license").id_salt("credits_noto_cjk").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::NOTO_CJK_LICENSE.trim()).monospace());
                });

                ui.separator();
                ui.strong("Starter pack soundfonts");
                ui.horizontal_wrapped(|ui| {
                    ui.label("GeneralUser GS by S. Christian Collins, used and hosted as its license asks.");
                    ui.hyperlink_to("schristiancollins.com/generaluser", "https://www.schristiancollins.com/generaluser");
                });
                egui::CollapsingHeader::new("GeneralUser GS's license").id_salt("credits_generaluser").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::GENERALUSER_LICENSE.trim()).monospace());
                });
                ui.label(
                    "TimGM6mb by Tim Brechbill (2004), with changes by David Bolton (2010), under the GNU GPL \
                     version 2. The soundfont file is its own source.",
                );
                egui::CollapsingHeader::new("TimGM6mb's license (GPL-2.0)").id_salt("credits_timgm6mb").show(ui, |ui| {
                    ui.label(egui::RichText::new(credits::TIMGM6MB_LICENSE.trim()).monospace());
                });
                ui.weak("The starter songs are synththing's own arrangements of public-domain music.");

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
                    // (A header of its own: a collapsing header's text never
                    // wraps or shortens, and long license names ran off a
                    // phone's screen.)
                    let id = ui.make_persistent_id(("credits_package", &package.name, &package.version));
                    let mut clicked = false;
                    let mut header = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                        .show_header(ui, |ui| {
                            let text = format!("{} {}   {}", package.name, package.version, package.license);
                            clicked = ui.add(egui::Label::new(text).truncate().sense(egui::Sense::click())).clicked();
                        });
                    if clicked {
                        header.toggle();
                    }
                    header.body(|ui| {
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
            ui.set_width(fit_width(ui, 380.0));
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
                    self.script.renamed(new_path.clone());
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
        let answer = self.script.rename(name);
        if let Some(rename) = &mut self.rename {
            rename.waiting = Some(answer);
        }
    }

    /// The running script's error, if any, with a "Go to line" button when
    /// it points at a line. `open_editor`: the button also opens the editor
    /// (for the copy shown above the visualizer).
    fn script_error_ui(&mut self, ui: &mut egui::Ui, open_editor: bool) {
        let Some(error) = self.script.error() else {
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

    /// Whether one of the app's modals (Log, Updates, Rename script) is up,
    /// or Preferences is waiting for keys. They get the keyboard: app shortcuts and the global
    /// keys pause.
    fn any_modal_open(&self) -> bool {
        self.keymap_capture.is_some()
            || self.log_open
            || self.updates_open
            || self.rename.is_some()
            || self.credits_open
            || self.changelog.is_some()
            || self.welcome.open
            || self.song_details.is_some()
            || self.cleanup.is_some()
            || self.palette.is_some()
            || self.tags_open
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
            || !self.set_aside.is_empty()
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

    /// The one-time import of the songs from before the library, once it's
    /// done: its songs join the library (with what's been read of them),
    /// and the playlists, now naming them, are read again.
    fn poll_old_import(&mut self) {
        let Some(done) = self.old_import.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        self.old_import = None;
        let count = done.moved.len();
        for (old, new) in &done.moved {
            self.song_info.rename(old, new);
        }
        self.songs = SongStore::open(self.songs.dir().to_path_buf());
        let (playlists, errors) = Library::load(
            config::playlists_dir().unwrap_or_else(|_| PathBuf::from("playlists")),
            self.songs.dir().to_path_buf(),
        );
        self.playlists = playlists;
        self.viewed_playlist = None;
        self.now_playing = None;
        let mut status = match count {
            0 => String::new(),
            1 => "Brought 1 song into the library.".to_string(),
            n => format!("Brought {n} songs into the library."),
        };
        if done.dropped > 0 {
            status.push_str(&format!(" {} playlist entries were left out: their files weren't there any more.", done.dropped));
        }
        if let Some(e) = done.failed.first().or(errors.first()) {
            log::warn!("bringing the old songs in: {:?} {:?}", done.failed, errors);
            status.push_str(&format!(" Some couldn't be brought in ({e})."));
        }
        if !status.is_empty() {
            self.status = status.trim().to_string();
        }
    }

    /// The scripts folder changed: refresh the picker, and if the running
    /// script's own file changed (edited in another editor, say), load the
    /// new version into the editor and visualizer. The app's own saves
    /// leave the file matching the editor, so they don't reload anything.
    fn rescan_scripts(&mut self) {
        let current = self.script.path().map(Path::to_path_buf);
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
                self.script.set_source(text, None);
                self.status = format!("Reloaded {name} (changed on disk).");
            }
            Ok(_) => {}
            Err(_) if !path.exists() => {
                self.status = format!("{name}.lua was moved or deleted; it keeps running until another script is picked.");
            }
            Err(e) => log::warn!("couldn't re-read {}: {e}", path.display()),
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

    /// The section tabs, filling everything between the menu bar and the
    /// controls. With every tab closed, a pointer to the View menu instead.
    fn dock_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        if self.open_sections.is_empty() {
            egui::CentralPanel::default().show(ui, |ui| {
                placeholder(ui, "Nothing open.\nUse the View menu at the top to show Songs (pick something to play), Playlists, the Visualizer, and more.");
            });
            return;
        }

        // DockArea needs `&mut` to the layout while the tabs it draws need
        // `&mut self`, so the layout is moved out for the duration.
        let mut dock = std::mem::replace(&mut self.dock, DockState::new(Vec::new()));
        self.dock_in_use = true;
        // A section alone in its spot has no tab bar: its top edge (with a
        // handle drawn on it) drags it instead.
        // The open tab joins its body, so where it's the first one the
        // body's corner under it is square.
        // A locked layout shows no handles, and its panels neither move
        // nor resize: what dragging a divider did is put back after.
        let locked = self.layout_locked();
        let (mut alone, mut joined) = (Vec::new(), Vec::new());
        for (_, leaf) in dock.iter_leaves_mut() {
            leaf.tab_bar_hidden = leaf.len() == 1;
            if leaf.tab_bar_hidden {
                alone.extend(leaf.tabs().first().copied().filter(|_| !locked));
            } else if leaf.active.0 == 0 {
                joined.extend(leaf.tabs().first().copied());
            }
        }
        let fractions = |dock: &mut DockState<Section>| -> Vec<f32> {
            dock.iter_all_nodes_mut()
                .filter_map(|(_, node)| match node {
                    egui_dock::Node::Horizontal(split) | egui_dock::Node::Vertical(split) => Some(split.fraction),
                    _ => None,
                })
                .collect()
        };
        let before = if locked { fractions(&mut dock) } else { Vec::new() };
        let top = if locked { f32::from(crate::theme::DOCK_MARGIN) } else { crate::theme::DOCK_HANDLE };
        let mut tabs = SectionTabs { app: self, shared, alone, joined, top };
        // (Nothing over the panels: the title bar has room below its buttons.)
        let frame = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 8 });
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            DockArea::new(&mut dock)
                .id(egui::Id::new("section_dock"))
                .style({
                    let mut style = crate::theme::dock_style(ui.style());
                    if locked {
                        style.separator.color_hovered = egui::Color32::TRANSPARENT;
                        style.separator.color_dragged = egui::Color32::TRANSPARENT;
                        // (No handles, so no room for them.)
                        style.tab.tab_body.inner_margin.top = crate::theme::DOCK_MARGIN;
                    }
                    style
                })
                .hidable_tab_bars(true)
                .draggable_tabs(!locked)
                .show_add_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut tabs);
        });
        if locked {
            let splits = dock.iter_all_nodes_mut().filter_map(|(_, node)| match node {
                egui_dock::Node::Horizontal(split) | egui_dock::Node::Vertical(split) => Some(split),
                _ => None,
            });
            for (split, fraction) in splits.zip(before) {
                split.fraction = fraction;
            }
        }
        self.dock_in_use = false;
        self.dock = dock;

        self.refresh_open_sections();
        for (section, open) in std::mem::take(&mut self.pending_layout) {
            self.set_open(section, open);
        }
        if let Some(section) = self.pending_focus.take() {
            self.reveal_section(section);
        }
    }

    /// The editor tab: plus the script bar when the visualizer (which
    /// normally hosts it) is closed.
    fn editor_section_ui(&mut self, ui: &mut egui::Ui) {
        if !self.is_open(Section::Visualizer) {
            self.script_bar_ui(ui, false);
        }
        self.script_error_ui(ui, false);
        self.visualizer_editor_ui(ui);
    }

    /// The revision of what the songs are listed from: it changes whenever
    /// the songs, what's been read of them, or how they're named do.
    fn songs_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.songs.revision(), self.song_info.revision(), &self.config.title_patterns, &self.config.group_ignore, self.config.sort_with_articles)
            .hash(&mut hasher);
        hasher.finish()
    }

    fn songs_ui(&mut self, ui: &mut egui::Ui) {
        if self.old_import.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Bringing your songs into the library...");
            });
            return;
        }
        let extensions = SONG_EXTENSIONS;
        // Only the songs a ruleset leaves out, when asked.
        let missing_only = self.songs_missing_only;
        let (missing_revision, missing) = self.missing_info();
        let any_missing = !missing.is_empty();
        let only: Option<std::collections::HashSet<&str>> = missing_only.then(|| missing.iter().flat_map(|m| m.keys.iter().map(String::as_str)).collect());
        let mut missing_clicked = false;
        // What the list's made from: it's made again when this changes.
        let revision = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (self.songs_revision(), self.wishlist.list.revision(), missing_only.then_some(missing_revision)).hash(&mut hasher);
            hasher.finish()
        };
        let selected = self.song_list.selected();
        let mut info_for: Option<Vec<PathBuf>> = None;
        let mut info_clicked = false;
        let can_undo = self.last_edit.is_some();
        let mut undo_clicked = false;
        let mut cleanup_clicked = false;
        let moving = self.songs_moving();
        let autoplay = self.autoplay;
        let mut autoplay_clicked = false;
        let mut tools = |ui: &mut egui::Ui| {
            #[cfg(target_os = "android")]
            if ui.add(icon_button("plus")).on_hover_text("Import songs: copy them into synththing's library").clicked() {
                crate::import::start(crate::import::Kind::Songs, SONG_EXTENSIONS);
            }
            #[cfg(not(target_os = "android"))]
            egui::containers::menu::MenuButton::from_button(icon_button("plus"))
                .config(crate::menu::config(ui.ctx()))
                .ui(ui, |ui| {
                    if ui.button("Songs...").on_hover_text("Copy songs into synththing's library").clicked() {
                        crate::import::start(crate::import::Kind::Songs, SONG_EXTENSIONS);
                    }
                    if ui.button("A folder...").on_hover_text("Every song in a folder and in the folders inside it").clicked() {
                        crate::import::start_folder(SONG_EXTENSIONS);
                    }
                })
                .0
                .on_hover_text("Import songs");
            #[cfg(not(target_os = "android"))]
            if ui
                .add_enabled(!selected.is_empty(), icon_button("info"))
                .on_hover_text("Song info: see and edit its details (several songs' at once)")
                .on_disabled_hover_text("Song info: select songs")
                .clicked()
            {
                info_clicked = true;
            }
            #[cfg(not(target_os = "android"))]
            if ui.add(icon_button("broom")).on_hover_text("Clean up: suggestions for your songs' details, like splitting several artists").clicked() {
                cleanup_clicked = true;
            }
            if can_undo && ui.add(icon_button("arrow-u-up-left")).on_hover_text("Undo the last edit of songs' details").clicked() {
                undo_clicked = true;
            }
            if (any_missing || missing_only)
                && ui
                    .add(icon_button("warning-circle").selected(missing_only))
                    .on_hover_text("Missing info: show only the songs a ruleset leaves out, as they don't say what it splits by (Preferences > Library lists them)")
                    .clicked()
            {
                missing_clicked = true;
            }
            sync::songs_moving_ui(ui, moving);
            autoplay_clicked |= autoplay::button_ui(ui, autoplay);
        };
        // (Both the names and the trailing widgets read the song info, one
        // after the other.)
        let cache = std::cell::RefCell::new(&mut self.song_info);
        let naming = naming(&self.config, &self.songs);
        let mut view = self.config.songs_view.clone();
        #[cfg(not(target_os = "android"))]
        let mut save_copy = None;
        let mut remove = Vec::new();
        let mut tag = None;
        let mut together: Option<(bool, Vec<PathBuf>)> = None;
        let mut queued: Option<(Vec<PathBuf>, bool)> = None;
        let store = &self.songs;
        let mut add_to: Option<(Option<usize>, Vec<PathBuf>)> = None;
        // With no dragging (touch), songs go onto playlists from their menu.
        let touch = select_list::touch(ui.ctx());
        // (Not the smart ones: their rules say what's on them.)
        let playlists: Vec<(usize, String)> = if touch {
            self.playlists.lists.iter().enumerate().filter(|(_, l)| l.playlist.smart.is_none()).map(|(i, l)| (i, l.playlist.name.clone())).collect()
        } else {
            Vec::new()
        };
        let mut wish = None;
        let (kept, wishes) = (&self.lookup.kept, &self.wishlist.list);
        let online = self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup);
        let mut menu = |ui: &mut egui::Ui, paths: &[PathBuf]| {
            if let Some(next) = queue::menu(ui) {
                queued = Some((paths.to_vec(), next));
            }
            if let [path] = paths
                && let Some(key) = store.key(path)
                && let Some(album) = wishlist::album_wish_menu(ui, store, kept, wishes, online, &[key])
            {
                wish = Some(album);
            }
            if touch {
                ui.menu_button("Add to playlist", |ui| {
                    for (i, name) in &playlists {
                        if ui.button(name).clicked() {
                            add_to = Some((Some(*i), paths.to_vec()));
                        }
                    }
                    if ui.button("New playlist").clicked() {
                        add_to = Some((None, paths.to_vec()));
                    }
                });
            }
            #[cfg(not(target_os = "android"))]
            if let [path] = paths
                && ui.button("Save a copy as...").on_hover_text("Write the song to a file, under its own name").clicked()
            {
                save_copy = Some(path.clone());
            }
            if ui.button("Song info").clicked() {
                info_for = Some(paths.to_vec());
            }
            if let Some(action) = tags::tags_menu(ui, store, paths) {
                tag = Some((action, paths.to_vec()));
            }
            if paths.len() > 1 && ui.button("Play together").on_hover_text("Play them in this order, as one song when shuffling").clicked() {
                together = Some((true, paths.to_vec()));
            }
            if paths.iter().any(|p| store.together_of(p).is_some()) && ui.button("Stop playing together").clicked() {
                together = Some((false, paths.to_vec()));
            }
            if ui.button("Remove from library...").on_hover_text("Delete synththing's copy on this device").clicked() {
                remove = paths.to_vec();
            }
        };
        let output = self.song_list.ui(
            ui,
            &mut Listed { store, wishes: &wishes.wishes, extensions, only: only.as_ref() },
            revision,
            &mut view,
            &self.config.group_ignore,
            self.config.sort_with_articles,
            &mut tools,
            &mut |path, keys| describe_song(&mut cache.borrow_mut(), &naming, &self.songs, path, keys),
            &mut |ui, path| song_info_widgets(ui, &mut cache.borrow_mut(), path, false),
            &mut menu,
        );
        if output.view_changed {
            self.config.songs_view = view;
            if let Err(e) = self.config.save() {
                log::warn!("saving the config: {e}");
            }
        }
        if !output.delete.is_empty() {
            remove = output.delete;
        }
        if let Some((path, on)) = output.star {
            tag = Some((tags::TagAction::Set(song_store::STARRED.into(), on), vec![path]));
        }
        if let Some(at) = output.play {
            self.play_listed(at);
        }
        if let Some((paths, next)) = queued {
            self.queue_songs(ui.ctx(), paths, next);
        }
        if let Some(wish) = wish {
            self.wish_for_album(ui.ctx(), wish);
        }
        if let Some(query) = output.save_search {
            self.smart_playlist_from_search(ui.ctx(), &query);
        }
        if autoplay_clicked {
            self.set_autoplay(self.autoplay.cycle());
            toast::show(ui.ctx(), autoplay::text(self.autoplay));
        }
        if undo_clicked {
            self.undo_edit();
        }
        if cleanup_clicked {
            self.cleanup = Some(Default::default());
        }
        if missing_clicked {
            self.songs_missing_only = !missing_only;
            toast::show(ui.ctx(), if missing_only { "Missing info: off" } else { "Missing info: only songs missing it" });
        }
        if let Some(paths) = info_for.or(info_clicked.then_some(selected)) {
            self.details_for = Some(paths);
        }
        #[cfg(not(target_os = "android"))]
        if let Some(path) = save_copy {
            self.save_song_copy(&path);
        }
        if !remove.is_empty() {
            self.remove_songs = remove;
        }
        if let Some((list, paths)) = add_to {
            self.add_to_playlist(list, paths);
        }
        if let Some((action, paths)) = tag {
            self.tag_songs(action, &paths);
        }
        if let Some((on, paths)) = together {
            let keys: Vec<String> = paths.iter().filter_map(|p| self.songs.key(p)).collect();
            if on {
                self.songs.set_together(&crate::library::new_nonce(), keys);
            } else {
                self.songs.leave_together(&keys);
            }
            self.songs_changed();
        }
    }

    /// Save a copy of the library's song at `path`, under its own name.
    #[cfg(not(target_os = "android"))]
    fn save_song_copy(&mut self, path: &Path) {
        let name = naming(&self.config, &self.songs).song(path, None).file_name.to_string();
        let Some(to) = rfd::FileDialog::new().set_file_name(&name).save_file() else { return };
        self.status = match std::fs::copy(path, &to) {
            Ok(_) => format!("Saved a copy as {}.", to.display()),
            Err(e) => format!("Couldn't save {}: {e}", to.display()),
        };
    }

    /// Put songs now in the store into the library: every way a song comes
    /// in goes through here, so playlists waiting on one stop showing it
    /// missing.
    pub(super) fn add_to_library(&mut self, songs: impl IntoIterator<Item = Stored>) {
        let mut added = std::collections::BTreeSet::new();
        for stored in songs {
            let path = self.songs.path(&stored.key);
            self.song_info.forget(&path);
            added.insert(path);
            self.songs.add(stored);
        }
        // (Only the entries for these songs can have come back: no need to
        // look for every entry's file.)
        for stored in &mut self.playlists.lists {
            for entry in &mut stored.playlist.entries {
                if added.contains(&entry.path) {
                    entry.missing = false;
                }
            }
        }
        self.songs_changed();
    }

    /// Mark playlist entries missing, or not, as their songs leave or come
    /// into the library.
    fn refresh_missing(&mut self) {
        let _timing = frame_timing::scope("refresh_missing");
        for stored in &mut self.playlists.lists {
            for entry in &mut stored.playlist.entries {
                entry.missing = !entry.path.exists();
            }
        }
        self.songs_changed();
    }

    /// Asked to remove songs from the library: are you sure?
    fn remove_songs_ui(&mut self, ctx: &egui::Context) {
        if self.remove_songs.is_empty() {
            return;
        }
        let what = match self.remove_songs.as_slice() {
            [one] => format!("\"{}\"", self.song_title(&one.clone())),
            more => format!("{} songs", more.len()),
        };
        let mut close = false;
        let mut remove = false;
        let synced = self.syncs_library();
        let everywhere = &mut self.remove_everywhere;
        let modal = egui::Modal::new(egui::Id::new("remove_songs")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.heading("Remove from library?");
            if synced {
                ui.checkbox(everywhere, "Remove from all my devices");
            }
            if synced && *everywhere {
                ui.label(format!(
                    "This deletes synththing's copy of {what} on this device and on all your other devices. \
                     Playlists keep them, shown as unavailable. Importing one again brings it back everywhere."
                ));
            } else {
                ui.label(format!(
                    "This deletes synththing's copy of {what} on this device. Playlists keep them, shown as unavailable here. \
                     Your other devices keep theirs, and this one won't fetch them from them again."
                ));
            }
            ui.horizontal(|ui| {
                remove = ui.button("Remove").clicked();
                close = ui.button("Cancel").clicked();
            });
        });
        if remove {
            let paths = std::mem::take(&mut self.remove_songs);
            let everywhere = synced && self.remove_everywhere;
            for path in &paths {
                let removed = match self.songs.key(path) {
                    Some(key) if everywhere => self.songs.remove_everywhere(&key),
                    _ => self.songs.remove(path),
                };
                if let Err(e) = removed {
                    self.status = format!("Couldn't remove a song: {e:#}");
                }
            }
            self.refresh_missing();
            self.song_list.clear_selection();
            if self.status.is_empty() || !self.status.starts_with("Couldn't") {
                let from = if everywhere { "all your devices" } else { "the library" };
                self.status = format!("Removed {what} from {from}.");
            }
        } else if close || modal.should_close() {
            self.remove_songs.clear();
        }
    }

    /// Imports that have finished (`import.rs`): songs join the library
    /// (and go where they were headed: a playlist), soundfonts join the
    /// list. What was picked but isn't of a type synththing takes is said,
    /// and audio files with audio files turned off bring an offer to turn
    /// them on.
    fn poll_imports(&mut self, ctx: &egui::Context) {
        // (Waiting for the one-time import, which writes the library's
        // index last: until then, nothing else may.)
        if self.old_import.is_some() {
            return;
        }
        for done in crate::import::take_finished() {
            let imported = match done.result {
                Ok(imported) => imported,
                Err(e) => {
                    self.status = format!("Couldn't import: {e}");
                    continue;
                }
            };
            let mut notes = Vec::new();
            match imported.skipped.as_slice() {
                [] => {}
                [one] => notes.push(format!("{one} isn't a file synththing can play.")),
                more => notes.push(format!("{} aren't files synththing can play.", more.join(", "))),
            }
            if let Some(e) = imported.failed.first() {
                log::warn!("importing: {:?}", imported.failed);
                notes.push(format!("{} couldn't be imported ({e}).", imported.failed.len()));
            }
            let n = imported.songs.len() + imported.soundfonts.len() + imported.images.len();
            if n == 0 {
                if !notes.is_empty() {
                    self.status = format!("Couldn't import: {}", notes.join(" "));
                }
                continue;
            }
            match done.kind {
                crate::import::Kind::Songs => {
                    let mut paths = Vec::new();
                    let mut soundfonts = Vec::new();
                    let mut stored = Vec::new();
                    for song in imported.songs {
                        paths.push(self.songs.path(&song.stored.key));
                        soundfonts.push(song.soundfont);
                        stored.push(song.stored);
                    }
                    self.add_to_library(stored);
                    let plural = if n == 1 { "" } else { "s" };
                    self.status = format!("Imported {n} song{plural}.");
                    self.after_import(ctx, done.then, paths, soundfonts);
                }
                crate::import::Kind::Soundfonts => self.add_soundfonts(imported.soundfonts),
                crate::import::Kind::Image => {
                    let id = self.image_for.take();
                    if let Some(list) = id.and_then(|id| self.playlists.find(&id)) {
                        self.set_playlist_image(list, imported.images.into_iter().next());
                    }
                }
            }
            if !notes.is_empty() {
                self.status = format!("{} {}", self.status, notes.join(" "));
            }
        }
    }

    /// Script picker + channel toggles + error banner + the live preview.
    fn visualizer_preview_ui(
        &mut self,
        ui: &mut egui::Ui,
        mut notes: NotesSnapshot,
        playback: EngineView,
    ) {
        self.script_bar_ui(ui, true);

        self.script_error_ui(ui, true);

        let channels = ui.scope(|ui| self.channels_ui(ui, &mut notes, playback.position)).response.rect;
        self.tour.mark(tour::Target::Channels, channels);

        self.show_visualizer(ui, &notes, &playback, DisplayMode::Window);
    }

    /// Draw (and run) the visualizer into the rest of `ui`, and pass on
    /// whatever it asked for.
    fn show_visualizer(&mut self, ui: &mut egui::Ui, notes: &NotesSnapshot, playback: &EngineView, mode: DisplayMode) {
        let _timing = frame_timing::scope("visualizer");
        // A render has its own copy of the script: this one rests meanwhile.
        if self.recording.render.is_some() {
            self.render_progress_ui(ui);
            return;
        }
        let transport = self.script_transport();
        self.script.set_app_log(self.config.script_log_to_app);
        let recording = self.recording.recorder.as_ref().is_some_and(|r| !r.paused());
        let take = self.recording.take();
        self.script.set_recording(recording, take);
        {
            let mut panel = lock_panel(&self.visualizer);
            panel.set_pads(self.pad_frame.clone());
            panel.set_mobile(self.mobile());
            panel.set_see_through(self.config.visualizer_see_through);
            panel.set_pixel_scale(self.config.visualizer_scale.unwrap_or(config::DEFAULT_VISUALIZER_SCALE));
        }
        // (A program watching through the API: drawn at the size it asked
        // for, unless a recording has a size already.)
        let watched = self.api_watched();
        self.script.set_streaming(watched.is_some());
        let recording_size = self.recording.frame_size();
        let fixed_size = recording_size.or(watched.flatten());
        lock_panel(&self.visualizer).set_off_screen(recording_size.is_none() && watched.flatten().is_some());
        let (hold, timestep) = self.pace_recording();
        self.visualizer_output = lock_panel(&self.visualizer).show(
            ui,
            &self.tap,
            notes,
            playback,
            transport,
            false,
            mode,
            fixed_size,
            hold,
            timestep,
        );
        self.feed_recording(ui);
        if self.visualizer_output.rendered && watched.is_some() {
            self.api_offer_frame(fixed_size);
        }
        self.visualizer_drawn = true;
        let effects = lock_panel(&self.visualizer).take_effects();
        self.apply_script_effects(effects, playback);
    }

    /// Carry out what the script asked for while drawing (in the main
    /// window, or the mini player).
    fn apply_script_effects(&mut self, effects: crate::script_host::Effects, playback: &EngineView) {
        // A script can ask to mute/unmute a channel itself (e.g. a game
        // script silencing a dead player's channel), the same command the
        // GUI's own toggles send.
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
                PlaybackRequest::RecordingReady => self.recording_ready(),
                PlaybackRequest::RecordingDone => self.recording_take_done(),
            }
        }
    }

    /// A song was just sent to the audio thread: the script gets it (its
    /// notes, id, path and a new `song_loads`) once the audio thread reports
    /// it loaded, so everything `playback()` says changes in the same frame
    /// (`sync_script_song`).
    pub(super) fn song_for_script(&mut self, path: &Path, notes: Arc<crate::midi_notes::NoteList>, id: String) {
        self.loads_sent += 1;
        let (tags, art) = self.script_song(path);
        self.pending_script_song = Some((self.loads_sent, path.to_path_buf(), notes, id, tags));
        let online = self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup);
        let (sender, path) = (self.script_art.0.clone(), path.to_path_buf());
        let spawned = std::thread::Builder::new().name("script art".into()).spawn(move || {
            let art = crate::album_art::for_script(&art, online).map(Arc::new);
            let _ = sender.send((path, art));
        });
        if let Err(e) = spawned {
            log::warn!("couldn't start reading art for the script: {e}");
        }
    }

    /// What a script's told of the song at `path` (`playback()`): its
    /// tags, and where its art comes from (`album_art()`).
    pub(super) fn script_song(&self, path: &Path) -> (lua_visualizer::SongTags, crate::album_art::Source) {
        let song = self.songs.get(path);
        let facts = song.map(crate::fields::Facts::of);
        let text = |field| facts.as_ref().and_then(|f| f.text(field)).map(|t| t.into_owned());
        let tags = lua_visualizer::SongTags {
            title: text(crate::fields::Field::Title),
            artist: text(crate::fields::Field::Artist),
            album: text(crate::fields::Field::Album),
        };
        (tags, crate::album_art::Source::of_song(Some(path.to_path_buf()), song))
    }

    /// Put synththing's icon in the tray. Once the window exists (desktop.rs,
    /// as eframe starts).
    pub fn attach_tray(&mut self, ctx: &egui::Context) {
        self.tray = crate::tray::Tray::new(ctx);
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

    /// Clicks on the tray icon since last frame, and what was clicked in
    /// the tray panel.
    fn handle_tray(&mut self, ctx: &egui::Context, view: &EngineView) {
        let Some(tray) = &self.tray else { return };
        for click in tray.clicks() {
            let native = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0);
            let snapshot = self.tray_snapshot(view);
            self.tray_panel.update(ctx, snapshot);
            match click {
                crate::tray::TrayClick::Primary { cursor } => {
                    // The mini player takes over from the main window.
                    if !self.tray_panel.is_open() {
                        self.hide_window(ctx);
                    }
                    self.tray_panel.toggle_mini(ctx, cursor, native * ctx.zoom_factor());
                }
                crate::tray::TrayClick::Secondary => self.show_window(ctx),
            }
        }
        for action in self.tray_panel.actions() {
            match action {
                tray_panel::Action::TogglePause => self.send(AudioCommand::TogglePause),
                tray_panel::Action::Previous => self.previous_track(view.position),
                tray_panel::Action::Next => {
                    if self.has_next() {
                        self.next_track();
                    }
                }
                tray_panel::Action::Volume(volume) => {
                    self.volume = volume.clamp(0.0, 1.0);
                    self.send_volume();
                }
                tray_panel::Action::Soundfont(index) => self.activate_soundfont(index),
                tray_panel::Action::Play(list, entry) => self.play_entry(list, entry),
                tray_panel::Action::ShowWindow => self.show_window(ctx),
                tray_panel::Action::Quit => self.close_with(ctx, close::CloseAction::Quit),
                tray_panel::Action::Effects(effects) => self.apply_script_effects(effects, view),
                tray_panel::Action::Script(index) => self.load_script(index),
            }
        }
        if self.tray_panel.is_open() {
            let snapshot = self.tray_snapshot(view);
            self.tray_panel.update(ctx, snapshot);
        }
    }

    /// What the tray panel shows.
    fn tray_snapshot(&mut self, view: &EngineView) -> tray_panel::Snapshot {
        tray_panel::Snapshot {
            title: self.current_title().or_else(|| view.track_name.clone()),
            paused: view.paused,
            position: view.position,
            length: view.length,
            volume: self.volume,
            soundfonts: self.config.soundfonts.iter().map(|p| nice_name(p)).collect(),
            active_soundfont: self.active_sf,
            playlists: self
                .playlists
                .lists
                .iter()
                .map(|l| {
                    let naming = naming(&self.config, &self.songs);
                    let names = l.playlist.entries.iter().map(|e| title_of(&mut self.song_info, &naming, &e.path)).collect();
                    (l.playlist.name.clone(), names)
                })
                .collect(),
            playing: self.playing_entry().map(|n| (n.list, n.entry)),
            script: self.script.path().map(nice_name),
            scripts: self.available_scripts.iter().map(|p| nice_name(p)).collect(),
            active_script: self.active_script,
            transport: self.script_transport(),
            main_shows_visualizer: !self.hidden && self.visualizer_was_drawn,
        }
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

    /// Hand the script the song the audio thread now reports as loaded.
    fn sync_script_song(&mut self, view: &EngineView) {
        if self.pending_script_song.as_ref().is_some_and(|(target, ..)| view.loads >= *target)
            && let Some((_, path, notes, id, tags)) = self.pending_script_song.take()
        {
            self.song_notes = Some(Arc::clone(&notes));
            self.script.set_song(path, id, notes, tags);
        }
        while let Ok((path, art)) = self.script_art.1.try_recv() {
            self.script.set_art(path, art);
        }
    }

    /// The playlist scripts see (`playlist()`): the one playing, else the
    /// one open in Groupings.
    fn script_playlist(&self) -> Option<usize> {
        self.now_playing
            .as_ref()
            .map(|np| np.list)
            .or(self.viewed_playlist)
            .filter(|&l| l < self.playlists.lists.len())
    }

    /// What scripts see of the playlist and the loop/shuffle modes.
    fn script_transport(&mut self) -> lua_visualizer::Transport {
        let naming = naming(&self.config, &self.songs);
        let playing_entry = self.playing_entry().map(|np| (np.list, np.entry));
        let playlist = self.script_playlist().map(|list| {
            let stored = &self.playlists.lists[list].playlist;
            let playing = playing_entry.filter(|(l, _)| *l == list).map(|(_, entry)| entry);
            let entries = stored
                .entries
                .iter()
                .map(|entry| lua_visualizer::PlaylistEntryView {
                    name: title_of(&mut self.song_info, &naming, &entry.path),
                    length: self.song_info.get(&entry.path).and_then(|info| info.meta.length),
                    missing: entry.missing,
                })
                .collect();
            lua_visualizer::PlaylistView {
                name: stored.name.clone(),
                playing: playing.is_some(),
                current: playing,
                entries,
            }
        });
        lua_visualizer::Transport { loop_mode: self.loop_mode, shuffle: self.shuffle, playlist }
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

    /// Carries out a command, whichever source it came from.
    fn run_command(&mut self, ctx: &egui::Context, command: Command, view: &EngineView) {
        if self.command_to_leader(command) {
            return;
        }
        match command {
            Command::TogglePause => self.send(AudioCommand::TogglePause),
            Command::Play | Command::Pause => {
                if view.paused == (command == Command::Play) {
                    self.send(AudioCommand::TogglePause);
                }
            }
            Command::SeekBack => self.send(AudioCommand::SeekRelative(-5.0)),
            Command::SeekForward => self.send(AudioCommand::SeekRelative(5.0)),
            Command::SeekBackLong => self.send(AudioCommand::SeekRelative(-30.0)),
            Command::SeekForwardLong => self.send(AudioCommand::SeekRelative(30.0)),
            Command::RestartSong => self.send(AudioCommand::Seek(0.0)),
            Command::NextTrack => {
                if self.has_next() {
                    self.next_track();
                }
            }
            Command::PreviousTrack => self.previous_track(view.position),
            Command::ToggleVisualizer if self.layout_locked() => toast::show(ctx, "The layout is locked (Layout > Lock layout)"),
            Command::ToggleVisualizer => self.set_open(Section::Visualizer, !self.is_open(Section::Visualizer)),
            Command::ExitFullscreen => self.exit_dedicated(ctx),
            Command::DedicatedVisualizer => {
                if self.dedicated.is_some() {
                    self.exit_dedicated(ctx);
                } else {
                    self.enter_dedicated(ctx);
                }
            }
            Command::RestartScript => self.restart_script(),
            Command::ToggleRecording => self.toggle_recording(view),
            Command::PauseRecording => self.toggle_recording_pause(),
            Command::Quit => self.close_with(ctx, close::CloseAction::Quit),
            Command::CommandPalette => self.palette = Some(palette::Palette::default()),
            Command::GoTo(_) | Command::NextSection | Command::PreviousSection => {
                match command {
                    Command::GoTo(section) => self.reveal_section(section),
                    Command::NextSection => self.step_section(1),
                    _ => self.step_section(-1),
                }
                // Off whatever has keyboard focus (a button Tab moved to),
                // so the keys go to the tab's list.
                ctx.memory_mut(|m| m.stop_text_input());
            }
            Command::Cursor(steer) => select_list::steer(ctx, steer, ctx.input(|i| i.modifiers.shift)),
        }
    }

    /// The list in the tab (or page) in front, for the keys to steer.
    fn find_keyboard_list(&mut self) -> Option<&'static str> {
        if self.dedicated.is_some() {
            return None;
        }
        let section = if self.mobile() {
            self.mobile_list_section()?
        } else {
            *self.dock.find_active_focused()?.1
        };
        match section {
            Section::Songs => Some("songs"),
            Section::Groupings => Some("playlist_rows"),
            Section::Soundfonts => Some("soundfonts"),
            Section::Queue => Some("queue"),
            Section::Wishlist => Some("wishlist"),
            _ => None,
        }
    }

    /// Rebuild the keymap when the config's keymaps changed (in
    /// Preferences, or by a sync), and put it in use.
    fn sync_keymap(&mut self, ctx: &egui::Context) {
        let keymap = Keymap::new(&self.config.keymap, &self.config.local_keymap);
        if *self.keymap != keymap {
            self.keymap = Arc::new(keymap);
            self.keymap.install(ctx);
        }
    }

    /// Whether a command would do anything now. One that wouldn't leaves
    /// its key alone and stays out of the palette.
    fn command_available(&self, command: Command) -> bool {
        match command {
            Command::ExitFullscreen => self.dedicated.is_some(),
            Command::NextTrack => self.has_next() || self.following(),
            // (The mobile layout has pages, not tabs.)
            Command::GoTo(section) => !self.mobile() && self.dedicated.is_none() && section != Section::Removed,
            Command::NextSection | Command::PreviousSection => !self.mobile() && self.dedicated.is_none(),
            Command::Cursor(_) => self.keyboard_list.is_some(),
            _ => true,
        }
    }

    /// Enter the dedicated visualizer fullscreen: OS fullscreen with only
    /// the visualizer, which gets keyboard focus.
    fn enter_dedicated(&mut self, ctx: &egui::Context) {
        let since = ctx.input(|i| i.time);
        self.dedicated = Some(Dedicated { since });
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        lock_panel(&self.visualizer).request_focus();
    }

    /// Start the running visualizer script over from scratch (F5, or the
    /// Restart button).
    fn restart_script(&mut self) {
        let name = self.script.path().map(nice_name).unwrap_or_else(|| "script".to_string());
        self.status = if self.script.restart() {
            format!("Restarted {name}.")
        } else {
            "No script running to restart.".to_string()
        };
    }

    /// Out of the dedicated fullscreen (F11 or Esc): back to the window.
    fn exit_dedicated(&mut self, ctx: &egui::Context) {
        if self.dedicated.take().is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
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
        let Some(busy) = self.script.busy_for() else { return };
        if !self.visualizer_drawn || busy < script_host::BUSY_NOTICE_AFTER || !rect.is_positive() {
            return;
        }
        let name = self.script.path().map(nice_name).unwrap_or_else(|| "The script".to_string());
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
            self.script.stop();
            self.status = format!("Stopped {name}.");
        }
    }

    /// Confine the cursor to the window while a script asks for it, but only
    /// while the visualizer has focus and the window is focused. Released the
    /// moment either stops being true (Escape or a click elsewhere included),
    /// so a script can never trap the cursor. `mouse_delta` is raw mouse
    /// motion while focused, so turning doesn't stop at the window's edge.
    fn apply_cursor_confinement(&mut self, ctx: &egui::Context) {
        let window_focused = ctx.input(|i| i.focused);
        let want = self.visualizer_output.focused
            && self.visualizer_output.cursor.confined
            && window_focused;
        if want != self.cursor_confined {
            self.cursor_confined = want;
            let grab = if want { egui::viewport::CursorGrab::Confined } else { egui::viewport::CursorGrab::None };
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(grab));
        }
    }

    /// The bar above the visualizer, or above the editor when the
    /// visualizer is off: the script picker (which also holds what's done
    /// to a script's file), Restart, and the script's Settings, Controls and
    /// Debug; with `visualizer`, Fullscreen and recording too.
    fn script_bar_ui(&mut self, ui: &mut egui::Ui, visualizer: bool) {
        let mut picked = None;
        // Wrapping onto a second row in a narrow tab, instead of cut off.
        ui.horizontal_wrapped(|ui| {
            let current = self
                .active_script
                .and_then(|i| self.available_scripts.get(i))
                .map(|p| lua_visualizer::display_name(p))
                .unwrap_or_else(|| "(none)".to_string());
            let picker = egui::ComboBox::from_id_salt("visualizer_script_picker")
                .selected_text(current)
                .height(f32::INFINITY)
                // Its sidebar has buttons (two-click ones among them).
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| {
                    picked = self.script_list_ui(ui, true);
                    if picked.is_some() || self.any_modal_open() {
                        ui.close();
                    }
                })
                .response
                .on_hover_text("The script drawing the visualizer; open it for more scripts, a new one, and this one's file");
            if ui
                .add(icon_button("arrow-clockwise"))
                .on_hover_text(format!(
                    "Restart: start the script over from scratch; its state resets, its settings stay{}",
                    commands::hint(ui.ctx(), Command::RestartScript)
                ))
                .clicked()
            {
                self.restart_script();
            }
            if self.script.options().assets
                && let Some(path) = self.active_script.and_then(|i| self.available_scripts.get(i))
            {
                let folder = lua_visualizer::assets_path(path);
                if ui
                    .add(icon_button("folder-open"))
                    .on_hover_text(format!("Open the script's assets folder ({}), for the files it reads", folder.display()))
                    .clicked()
                {
                    open_folder(&folder);
                }
            }
            if visualizer {
                self.tour.mark(tour::Target::ScriptPicker, picker.rect);
                let fullscreen = ui.add(icon_button("corners-out")).on_hover_text(
                    "Fullscreen visualizer: just the visualizer, filling the screen, with mouse and keyboard going to the script. F11 toggles it, Esc exits.",
                );
                self.tour.mark(tour::Target::Fullscreen, fullscreen.rect);
                if fullscreen.clicked() {
                    let ctx = ui.ctx().clone();
                    self.enter_dedicated(&ctx);
                }
                let see_through = self.config.visualizer_see_through;
                if ui
                    .add(icon_button("checkerboard").selected(see_through))
                    .on_hover_text(
                        "See-through: show what's behind a visualizer with see-through parts, instead of black. \
                         The local API's PNG stream keeps them see-through either way.",
                    )
                    .clicked()
                {
                    self.config.visualizer_see_through = !see_through;
                    toast::show(ui.ctx(), if see_through { "See-through: off" } else { "See-through: on" });
                    if let Err(e) = self.config.save() {
                        self.status = format!("Couldn't save the preferences: {e}");
                    }
                }
            }
            ui.separator();
            self.script_window_buttons_ui(ui, |name, color| match color {
                Some(color) => colored_icon_button(name, color),
                None => icon_button(name),
            });
            if visualizer {
                self.recording_buttons_ui(ui);
            }
        });
        match picked {
            Some(previews::Picked::Script(idx)) => self.load_script(idx),
            Some(previews::Picked::New(content)) => self.new_script_from_template(&content),
            None => {}
        }
    }

    /// In the picker's sidebar, for the running script (at `path`): what
    /// can be done to its file.
    fn script_file_buttons_ui(&mut self, ui: &mut egui::Ui, path: &Path) {
        // A re-sync when it's a bundled default (they're only written on
        // first run, so an older copy on disk won't have newer host
        // features).
        if let Some(bundled) = path.file_name().and_then(|n| n.to_str()).and_then(lua_visualizer::bundled_default)
            && !crate::library::installed_path(path).exists()
            && ui.button("Restore default").on_hover_text("Back to the script as synththing ships it").clicked()
        {
            self.editor_opened(path);
            self.editor_text = bundled.to_string();
            self.completion.request(self.editor_text.clone());
            self.apply_editor();
        }
        if ui.button("Rename...").on_hover_text("Rename this script's file (its settings come along)").clicked() {
            let name = lua_visualizer::display_name(path);
            self.rename = Some(RenameScript { path: path.to_path_buf(), name, error: None, focused_once: false, waiting: None });
        }
        self.script_share_button(ui);
    }

    /// Per-channel mute toggles for the current MIDI file (left-click
    /// toggle, right-click solo, "All" to re-enable everything). `notes` is
    /// updated optimistically so this frame's preview already reflects it.
    /// Hovering one says what it plays at `position` and its track's name.
    fn channels_ui(&mut self, ui: &mut egui::Ui, notes: &mut NotesSnapshot, position: f64) {
        if !notes.detected_channels.is_empty() {
            let mut toggle: Option<(u8, bool)> = None;
            let mut solo: Option<u8> = None;
            let mut enable_all = false;

            // In the colors the script draws them in, if it said; else
            // the keyboard visualizer's.
            let colors = self.script.channel_colors();
            ui.horizontal_wrapped(|ui| {
                ui.label("Channels:")
                    .on_hover_text("Left-click to toggle, right-click to solo");
                if ui.small_button("All").clicked() {
                    enable_all = true;
                }
                for &channel in &notes.detected_channels {
                    let mut enabled = notes.enabled_channels[channel as usize];
                    let [r, g, b] = colors[channel as usize].unwrap_or_else(|| lua_visualizer::default_channel_color(channel));
                    let color = egui::Color32::from_rgb(r, g, b);
                    let song = self.song_notes.as_deref().filter(|n| n.facts().tracks > 0);
                    let instrument =
                        song.map(|song| crate::midi_notes::program_name(channel, song.program_at(channel, position)));
                    let about = |ui: &mut egui::Ui| {
                        match song {
                            Some(song) => {
                                ui.strong(format!("Channel {}: {}", channel + 1, instrument.unwrap_or_default()));
                                if let Some(name) = song.channel_name(channel) {
                                    ui.label(format!("Track: {name}"));
                                }
                            }
                            None => {
                                ui.strong(format!("Channel {}", channel + 1));
                            }
                        }
                        ui.weak("Left-click to toggle, right-click to solo");
                    };
                    let response = if self.config.channel_chips {
                        let text = match instrument {
                            Some(instrument) => format!("{} \u{b7} {instrument}", channel + 1),
                            None => format!("Channel {}", channel + 1),
                        };
                        let response = toggle_chip(ui, enabled, &text, color).on_hover_ui(about);
                        if response.clicked() {
                            toggle = Some((channel, !enabled));
                        }
                        response
                    } else {
                        let response =
                            channel_toggle(ui, &mut enabled, &format!("{}", channel + 1), color).on_hover_ui(about);
                        if response.changed() {
                            toggle = Some((channel, enabled));
                        }
                        response
                    };
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
        (SettingKind::Color { alpha }, SettingValue::Color(rgba)) => {
            let mut rgba = *rgba;
            let edited = if *alpha {
                ui.color_edit_button_srgba_unmultiplied(&mut rgba).changed()
            } else {
                let mut rgb = [rgba[0], rgba[1], rgba[2]];
                let edited = ui.color_edit_button_srgb(&mut rgb).changed();
                rgba = [rgb[0], rgb[1], rgb[2], 255];
                edited
            };
            if edited {
                *changed = Some((d.key.clone(), SettingValue::Color(rgba)));
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

impl App {
    /// Everything that has to keep going whether or not anything's drawn
    /// (`eframe::App::logic`, every frame, and on its own while the window
    /// is hidden or minimized; on Android also while the app is in the
    /// background, when eframe has no window and calls nothing).
    pub fn tick(&mut self, ctx: &egui::Context) {
        let ui_ran = std::mem::take(&mut self.ui_drawn);
        if !ui_ran {
            self.nothing_drawn();
        }
        self.minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let view = self.shared.lock().unwrap().view.clone();
        frame_timing::time("handle_tray", || self.handle_tray(ctx, &view));
        if self.second_launch.as_ref().is_some_and(|f| f.swap(false, std::sync::atomic::Ordering::Relaxed)) {
            log::info!("launched again: showing the window");
            self.show_window(ctx);
        }
        // The main window and the mini player are never up together: the
        // main window on screen, however it got there (`show_window`, or
        // restored from the taskbar), puts the mini player away.
        if !self.hidden && !self.minimized && self.tray_panel.is_open() {
            self.tray_panel.hide(ctx);
        }
        frame_timing::time("sync_script_song", || self.sync_script_song(&view));
        frame_timing::time("poll_loads", || self.poll_loads());
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
        frame_timing::time("sync_theme", || self.sync_theme(&ctx));
        if self.hidden {
            // Hidden in the tray, this still runs while a tray window (a
            // child of this one) is up: draw nothing, least of all the
            // visualizer, which would run the script at this window's
            // size between the mini player's frames. Only keep the tray
            // window there.
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

        if self.dedicated.is_some() {
            frame_timing::time("dedicated", || self.dedicated_ui(ui, &shared));
        } else if self.mobile() {
            frame_timing::time("mobile", || self.mobile_ui(ui, &shared));
            // (Only for the launch check: the windows are pages here.)
            self.updates_ui(&ctx);
            self.open_mobile_pages(&ctx);
            self.rename_ui(&ctx);
            self.set_aside_ui(&ctx);
            self.welcome_ui(&ctx);
            self.cleanup_ui(&ctx);
            self.choose_art_ui(&ctx);
            self.song_details_ui(&ctx);
            self.history_ui(&ctx);
            self.status_toast_ui(&ctx);
        } else {
            egui::Panel::top("top_bar")
                .show_separator_line(false)
                .exact_size(title_bar::HEIGHT)
                .show(ui, |ui| frame_timing::time("top bar", || self.title_bar_ui(ui)));
            egui::Panel::bottom("controls_panel").show_separator_line(false).show(ui, |ui| {
                let _timing = frame_timing::scope("controls bar");
                self.controls_bar_ui(ui, view);
                self.tour.mark(tour::Target::Controls, ui.min_rect());
            });
            if self.preferences_open {
                self.preferences_ui(ui);
            } else {
                frame_timing::time("dock", || self.dock_ui(ui, &shared));
            }
            frame_timing::time("autosave_layout", || self.autosave_layout(&ctx));
            let windows = frame_timing::scope("windows");
            self.updates_ui(&ctx);
            self.log_ui(&ctx);
            self.rename_ui(&ctx);
            self.credits_ui(&ctx);
            self.changelog_ui(&ctx);
            self.set_aside_ui(&ctx);
            self.welcome_ui(&ctx);
            self.cleanup_ui(&ctx);
            self.choose_art_ui(&ctx);
            self.song_details_ui(&ctx);
            self.history_ui(&ctx);
            self.layout_new_ui(&ctx);
            self.layout_edit_ui(&ctx);
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

/// Draws each section's tab for `DockArea`.
struct SectionTabs<'a> {
    app: &'a mut App,
    shared: &'a PlaybackShared,
    /// The sections alone in their spots, so without a tab bar.
    alone: Vec<Section>,
    /// The sections open as the first of their spots' tabs.
    joined: Vec<Section>,
    /// How tall the top margin of the sections' bodies is.
    top: f32,
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
        let _timing = frame_timing::scope(tab.title());
        app.tour.mark(tour::Target::Tab(*tab), ui.max_rect());
        if self.alone.contains(tab) {
            drag_handle(ui);
        } else if self.joined.contains(tab) {
            square_corner(ui, self.top);
        }
        match tab {
            Section::Songs => app.songs_ui(ui),
            Section::Groupings => app.groupings_ui(ui),
            Section::Soundfonts => app.soundfont_ui(ui),
            Section::Queue => app.queue_ui(ui),
            Section::Wishlist => app.wishlist_ui(ui),
            Section::Apps => app.apps_ui(ui),
            Section::Visualizer => {
                app.visualizer_preview_ui(ui, self.shared.notes.clone(), self.shared.view.clone());
            }
            Section::Editor => app.editor_section_ui(ui),
            // (Dropped from layouts as they load.)
            Section::Removed => {}
            Section::Reference => app.docs_ui(ui),
            Section::Sprites => app.sprite_editor_ui(ui),
            Section::Library => app.library_ui(ui),
        }
    }

    /// Sections are shown and hidden from View (and the transport's
    /// toggles), not closed from their tabs.
    fn is_closeable(&self, _tab: &Section) -> bool {
        false
    }

    /// Every section scrolls its own content where it needs to; a second,
    /// outer scroll area from the dock would just fight it.
    fn scroll_bars(&self, _tab: &Section) -> [bool; 2] {
        [false, false]
    }
}

/// How tall a window's `categories_body` is.
const CATEGORIES_HEIGHT: f32 = 340.0;

/// The body of a page laid out like Preferences: `categories` down the
/// side (`selected` the one picked), and `body` drawing the picked one's
/// contents beside them. Each side scrolls by itself, `height` tall.

fn categories_body<S: AsRef<str>>(
    ui: &mut egui::Ui,
    id: &str,
    categories: &[S],
    selected: &mut usize,
    height: f32,
    body: impl FnOnce(&mut egui::Ui, usize),
) {
    // A fixed height: the divider takes the height it's given, so in a
    // window left to size itself, it grows a little every frame.
    *selected = (*selected).min(categories.len().saturating_sub(1));
    let size = egui::vec2(ui.available_width(), height);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.set_height(height);
        ui.vertical(|ui| {
            ui.set_width(140.0);
            egui::ScrollArea::vertical().id_salt((id, "categories")).max_height(height).show(ui, |ui| {
                for (i, title) in categories.iter().enumerate() {
                    let pill = egui::Button::selectable(*selected == i, title.as_ref()).corner_radius(12.0);
                    if ui.add(pill).clicked() {
                        *selected = i;
                    }
                }
            });
        });
        ui.separator();
        ui.vertical(|ui| {
            egui::ScrollArea::vertical()
                .id_salt((id, "body"))
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    body(ui, *selected);
                });
        });
    });
}

/// `width`, or less on a screen too narrow for it (a phone's): what windows
/// and dialogs set their width to.
fn fit_width(ui: &egui::Ui, width: f32) -> f32 {
    width.min(ui.ctx().content_rect().width() - 32.0).max(120.0)
}

/// Where a window may be: the screen, less the reach of its resize handles
/// past its edges, so none of them is cut off by the screen's side.
fn window_room(ctx: &egui::Context) -> egui::Rect {
    ctx.content_rect().shrink(ctx.global_style().interaction.resize_grab_radius_corner)
}

/// What an empty panel shows: `text` in weak text, centered in the room
/// left in `ui`, wrapped to a readable width (a line break in it starts a
/// new line). Only painted, so a list drawn after it (a drop target, say)
/// still gets the room.
pub fn placeholder(ui: &egui::Ui, text: &str) {
    let rect = ui.available_rect_before_wrap();
    let mut job = egui::text::LayoutJob::simple(text.to_string(), egui::TextStyle::Body.resolve(ui.style()), ui.visuals().weak_text_color(), (rect.width() - 32.0).clamp(80.0, 420.0));
    job.halign = egui::Align::Center;
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(rect.center() - egui::vec2(0.0, galley.size().y / 2.0), galley, ui.visuals().weak_text_color());
}

/// A Phosphor icon by its name (`shield-check`, `SCROLL`), for text: the
/// scroll if there's no such icon.
pub fn icon(name: &str) -> &'static str {
    let wanted = name.trim().replace('-', "_").to_uppercase();
    egui_phosphor::regular::ICONS
        .iter()
        .find(|(n, _)| *n == wanted)
        .map_or(egui_phosphor::regular::SCROLL, |(_, glyph)| glyph)
}

/// A setting on one line (`add`), with an (i) after it: hovering either
/// shows `info`, the longer explanation.
/// The (i) sits at the row's right end, so they line up down a page.
/// The (i) goes in first, so the setting gets only the room left of it.
fn with_info(ui: &mut egui::Ui, info: &str, add: impl FnOnce(&mut egui::Ui) -> egui::Response) {
    setting_row(ui, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            info_icon(ui, info);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| add(ui).on_hover_text(info));
        });
    });
}

/// One line of settings, across the whole width (text too long for it
/// wraps). In a `settings_group`, every other one is on the striped
/// background, as a list's rows are.
fn setting_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let id = egui::Id::new(SETTINGS_GROUP);
    let striped = ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Option<usize>>(id).as_mut().map(|rows| {
            *rows += 1;
            *rows % 2 == 0
        })
    });
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Frame::new().inner_margin(egui::Margin::symmetric(0, GROUP_ROW_MARGIN)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            add(ui)
        })
        .inner
    });
    if striped == Some(true) {
        let rect = inner.response.rect.expand2(egui::vec2(GROUP_MARGIN as f32, 0.0));
        ui.painter().set(background, egui::Shape::rect_filled(rect, 0.0, ui.visuals().faint_bg_color));
    }
    inner.inner
}

/// Where `setting_row` counts the rows of the `settings_group` being drawn.
const SETTINGS_GROUP: &str = "settings_group";
/// The room around a settings group's contents, and above and below each
/// of its rows.
const GROUP_MARGIN: i8 = 8;
const GROUP_ROW_MARGIN: i8 = 3;

/// A box of settings under a heading, drawn as a list is: outlined, the
/// heading (an icon, the title, a line of hint under it) on the header's
/// color, the rows (`setting_row`) striped. Built up, then drawn by `show`.
#[derive(Clone, Copy)]
struct SettingsGroup<'a> {
    title: &'a str,
    icon: Option<&'a str>,
    hint: Option<&'a str>,
    /// Opens and closes by its heading (closed at first), kept by this
    /// whatever the title says.
    closable: Option<&'a str>,
}

fn settings_group(title: &str) -> SettingsGroup<'_> {
    SettingsGroup { title, icon: None, hint: None, closable: None }
}

impl<'a> SettingsGroup<'a> {
    fn icon(self, icon: &'a str) -> Self {
        Self { icon: Some(icon), ..self }
    }

    fn hint(self, hint: &'a str) -> Self {
        Self { hint: Some(hint), ..self }
    }

    fn closable(self, id_salt: &'a str) -> Self {
        Self { closable: Some(id_salt), ..self }
    }

    /// Draw it, `add` drawing its contents (unless it's closed: `None`).
    fn show<R>(self, ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
        let id = egui::Id::new(SETTINGS_GROUP);
        let visuals = ui.visuals().clone();
        let radius = 6u8;
        let toggle = self.closable.map(|salt| ui.make_persistent_id(("settings_group_open", salt)));
        let open = toggle.is_none_or(|toggle| ui.ctx().data_mut(|d| *d.get_persisted_mut_or_default::<bool>(toggle)));
        // (No room of its own around it: the item spacing between groups
        // is about the room at its sides.)
        let group = ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let bottom = if open { 0 } else { radius };
            let header = egui::Frame::new()
                .fill(visuals.extreme_bg_color)
                .corner_radius(egui::CornerRadius { nw: radius, ne: radius, sw: bottom, se: bottom })
                .inner_margin(egui::Margin::symmetric(GROUP_MARGIN, 5))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if let Some(name) = self.icon {
                            ui.label(egui::RichText::new(icon(name)).size(16.0).color(visuals.weak_text_color()));
                        }
                        ui.vertical(|ui| {
                            ui.strong(self.title);
                            if let Some(hint) = self.hint {
                                ui.add(egui::Label::new(egui::RichText::new(hint).weak()).wrap());
                            }
                        });
                        if toggle.is_some() {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(icon(if open { "caret-up" } else { "caret-down" }));
                            });
                        }
                    });
                });
            if let Some(toggle) = toggle
                && ui
                    .interact(header.response.rect, toggle, egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
            {
                ui.ctx().data_mut(|d| d.insert_persisted(toggle, !open));
            }
            if !open {
                return None;
            }
            let outer = ui.ctx().data_mut(|d| d.get_temp::<Option<usize>>(id)).flatten();
            ui.ctx().data_mut(|d| d.insert_temp(id, Some(0usize)));
            let inner = egui::Frame::new().inner_margin(egui::Margin::symmetric(GROUP_MARGIN, 2)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            });
            ui.ctx().data_mut(|d| d.insert_temp(id, outer));
            Some(inner.inner)
        });
        let rect = group.response.rect;
        ui.painter().rect_stroke(rect, radius, visuals.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
        group.inner
    }
}

/// How wide a setting's label is (`setting_label`), so the controls after
/// the labels line up.
const LABEL_WIDTH: f32 = 160.0;
/// How wide a setting's dropdown or slider is, where there's room.
const CONTROL_WIDTH: f32 = 180.0;
/// The room a slider's value box (`setting_slider`) takes after it: the
/// widest of them ("1000 ms"), and the gap before it.
const SLIDER_VALUE_WIDTH: f32 = 64.0;

/// How wide a setting's control is, after its `setting_label`: its usual
/// width, less on a narrow screen, so it and the `beside` points after it
/// fit in the row.
fn control_width(ui: &egui::Ui, beside: f32) -> f32 {
    CONTROL_WIDTH.min(ui.available_width() - beside).max(0.0)
}

/// A setting's dropdown, as wide as `control_width` makes it: a choice too
/// long for it is cut off (not wrapped, as the rest of a `setting_row` is).
fn setting_combo(ui: &egui::Ui, id_salt: &str, beside: f32) -> egui::ComboBox {
    egui::ComboBox::from_id_salt(id_salt).width(control_width(ui, beside)).truncate()
}

/// A setting's slider, as wide as `control_width` makes it with its value
/// box after it (and `beside` points more).
fn setting_slider(ui: &mut egui::Ui, slider: egui::Slider, beside: f32) -> egui::Response {
    ui.spacing_mut().slider_width = control_width(ui, SLIDER_VALUE_WIDTH + beside);
    ui.add(slider)
}

/// A setting's name, in a column of its own width (less on a narrow
/// screen): the control goes after it.
fn setting_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let width = LABEL_WIDTH.min(ui.available_width() * 0.4);
    let height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(egui::vec2(width, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_width(width);
        ui.add(egui::Label::new(text).truncate())
    })
    .inner
}

/// A heading over a group of settings, with a little room above it.
fn setting_section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(title).strong().size(15.0));
}

/// A small circled "i": `info` on hover, and in the mobile layout's toast
/// on a tap (a touch screen has no hover; `mobile::INFO_TOAST`).
fn info_icon(ui: &mut egui::Ui, info: impl Into<String>) -> egui::Response {
    let info = info.into();
    let size = ui.text_style_height(&egui::TextStyle::Body).min(16.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    if response.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(mobile::INFO_TOAST), info.clone()));
    }
    let response = response.on_hover_text(info);
    let color = if response.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
    let painter = ui.painter();
    painter.circle_stroke(rect.center(), size * 0.45, egui::Stroke::new(1.2, color));
    painter.text(
        rect.center() + egui::vec2(0.0, 0.5),
        egui::Align2::CENTER_CENTER,
        "i",
        egui::FontId::proportional(size * 0.7),
        color,
    );
    response
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

/// A channel's toggle in its color: egui's own checkbox (so it's the
/// same size and shape as any other), filled with the color while the
/// channel's on (the check mark black or white, whichever reads better on
/// it), outlined in it while it's off.
fn channel_toggle(ui: &mut egui::Ui, on: &mut bool, label: &str, color: egui::Color32) -> egui::Response {
    ui.scope(|ui| {
        let mark = on_color(color);
        let visuals = ui.visuals_mut();
        // (The label keeps its usual color: the check mark's is the widget's.)
        visuals.override_text_color = Some(visuals.widgets.inactive.text_color());
        let fill = if *on { color } else { color.gamma_multiply(0.15) };
        for state in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
            state.bg_fill = fill;
            state.bg_stroke = egui::Stroke::new(state.bg_stroke.width.max(1.0), color);
            state.fg_stroke.color = mark;
        }
        ui.checkbox(on, label)
    })
    .inner
}

/// A toggle as a chip (a channel's, `Config::channel_chips`), like the
/// library's: filled with its color while it's on, washed out to grey
/// while it's off, outlined in the theme's hover color while hovered.
/// Clicking it toggles.
pub(super) fn toggle_chip(ui: &mut egui::Ui, on: bool, text: &str, color: egui::Color32) -> egui::Response {
    let fill = if on { color } else { washed_out(color, ui.visuals().dark_mode) };
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, on_color(fill));
    let padding = egui::vec2(8.0, 2.0);
    let (rect, response) = ui.allocate_exact_size(galley.size() + padding * 2.0, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let radius = rect.height() / 2.0;
        ui.painter().rect_filled(rect, radius, fill);
        if response.hovered() {
            let hover = ui.visuals().widgets.hovered.bg_stroke;
            let stroke = egui::Stroke::new(hover.width.max(1.5), hover.color);
            ui.painter().rect_stroke(rect, radius, stroke, egui::StrokeKind::Outside);
        }
        ui.painter().galley(rect.min + padding, galley, on_color(fill));
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, text));
    response
}

/// `color` with most of its color taken out and pushed toward the theme's
/// background: how a channel chip looks while its channel is off, still a
/// hint of which channel it is.
fn washed_out(color: egui::Color32, dark: bool) -> egui::Color32 {
    let grey = (0.2126 * f32::from(color.r()) + 0.7152 * f32::from(color.g()) + 0.0722 * f32::from(color.b())) as u8;
    let toward = |c: u8, to: u8, t: f32| (f32::from(c) + (f32::from(to) - f32::from(c)) * t).round() as u8;
    let [r, g, b] = [color.r(), color.g(), color.b()].map(|c| toward(c, grey, 0.65));
    let (to, t) = if dark { (20, 0.45) } else { (250, 0.4) };
    egui::Color32::from_rgb(toward(r, to, t), toward(g, to, t), toward(b, to, t))
}

/// The status line's color, by what it says: red for something that
/// failed ("Couldn't ...", "Failed to ..."), green for something done
/// ("Saved ...", "Installed ..."), none (grey) for anything else.
fn status_color(visuals: &egui::Visuals, status: &str) -> Option<egui::Color32> {
    match status_outcome(status)? {
        false => Some(visuals.error_fg_color),
        true => {
            // (The palette's green, darker on a light theme so it reads.)
            let green = palette_color(2);
            Some(if visuals.dark_mode { green } else { green.gamma_multiply(0.55).to_opaque() })
        }
    }
}

/// Whether a status says something failed (`Some(false)`), was done
/// (`Some(true)`), or neither, by its wording.
fn status_outcome(status: &str) -> Option<bool> {
    const FAILED: [&str; 6] = ["Couldn't", "Failed", "Can't", "Didn't", "Error", "Not "];
    const DONE: [&str; 22] = [
        "Saved", "Sent", "Installed", "Downloaded", "Published", "Deleted", "Updated", "Restored", "Renamed", "Rendered",
        "Reported", "Replaced", "Reloaded", "Formatted", "Imported", "Exported", "Disputed", "Added", "Removed",
        "Recorded", "Soundfont loaded", "Script applied",
    ];
    if FAILED.iter().any(|w| status.starts_with(w)) || status.contains(" not loaded") || status.contains("failed") {
        return Some(false);
    }
    DONE.iter().any(|w| status.starts_with(w)).then_some(true)
}

/// The status line: a history button (with a red dot while a failure that
/// came up hasn't been looked at) and the latest `status`, red when
/// something failed, green when something got done. The button opens the
/// recent messages from the log (`entries`, timed on `now`'s clock) in a
/// popup above it, which clears the dot. True when "Open the full log..."
/// was clicked.
fn status_line_ui(
    ui: &mut egui::Ui,
    status: &str,
    unseen_failure: &mut bool,
    now: impl FnOnce() -> f64,
    entries: impl FnOnce() -> Vec<applog::Entry>,
) -> bool {
    let button = ui
        .add(icon_button("clock-counter-clockwise"))
        .on_hover_text(if *unseen_failure {
            "Something failed: the recent messages"
        } else {
            "The recent messages"
        });
    if *unseen_failure {
        let corner = button.rect.right_top() + egui::vec2(-3.0, 3.0);
        ui.painter().circle_filled(corner, 3.5, ui.visuals().error_fg_color);
    }
    let mut open_log = false;
    let shown = egui::Popup::from_toggle_button_response(&button)
        .show(|ui| {
            ui.set_max_width(480.0);
            let now = now();
            let recent: Vec<applog::Entry> = entries()
                .into_iter()
                .rev()
                .filter(|e| e.target == STATUS_TARGET && !e.message.is_empty())
                .take(STATUS_HISTORY)
                .collect();
            ui.strong("Recent messages");
            if recent.is_empty() {
                ui.weak("Nothing yet.");
            }
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for entry in &recent {
                    ui.horizontal_wrapped(|ui| {
                        match status_color(ui.visuals(), &entry.message) {
                            Some(color) => ui.colored_label(color, &entry.message),
                            None => ui.label(&entry.message),
                        };
                        ui.weak(editor::ago_text((now - entry.secs).max(0.0) as u64));
                    });
                }
            });
            ui.separator();
            if ui.button("Open the full log...").on_hover_text("Help > Log, showing just these").clicked() {
                open_log = true;
                ui.close();
            }
        })
        .is_some();
    if shown {
        *unseen_failure = false;
    }
    match status_color(ui.visuals(), status) {
        Some(color) => ui.colored_label(color, status),
        None => ui.weak(status),
    };
    open_log
}

/// The visualizer panel, locked for a moment (`App::visualizer`), by the
/// main thread or the thread drawing it off screen for the local API
/// (`api.rs`). Locking it again on a thread that holds it already would
/// freeze the app: that panics instead, so it can't happen quietly.
pub(super) fn lock_panel(panel: &Mutex<VisualizerPanel>) -> PanelGuard<'_> {
    assert!(!HOLDING_PANEL.replace(true), "the visualizer panel was locked twice at once");
    PanelGuard(panel.lock().unwrap_or_else(|p| p.into_inner()))
}

thread_local! {
    /// Whether this thread has the visualizer panel locked (`lock_panel`).
    static HOLDING_PANEL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The visualizer panel while it's locked (`lock_panel`).
pub(super) struct PanelGuard<'a>(std::sync::MutexGuard<'a, VisualizerPanel>);

impl std::ops::Deref for PanelGuard<'_> {
    type Target = VisualizerPanel;
    fn deref(&self) -> &VisualizerPanel {
        &self.0
    }
}

impl std::ops::DerefMut for PanelGuard<'_> {
    fn deref_mut(&mut self) -> &mut VisualizerPanel {
        &mut self.0
    }
}

impl Drop for PanelGuard<'_> {
    fn drop(&mut self) {
        HOLDING_PANEL.set(false);
    }
}

/// The log target status messages go under (the status line's history).
const STATUS_TARGET: &str = "synththing::status";

/// How many of the status line's past messages its popup shows.
const STATUS_HISTORY: usize = 50;

/// A color of the channel palette (`lua_visualizer::default_channel_color`).
pub(super) fn palette_color(index: u8) -> egui::Color32 {
    let [r, g, b] = lua_visualizer::default_channel_color(index);
    egui::Color32::from_rgb(r, g, b)
}

/// A toolbar button: just its icon (Phosphor), a little larger than text.
/// Give it a hover text saying what it does.
pub(crate) fn icon_button(name: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(icon(name)).size(16.0)).min_size(egui::vec2(28.0, 24.0))
}

/// An icon button as tall as the other controls in a row of settings (or
/// any row of text-sized widgets), so everything in the row lines up on
/// its middle. Give it a hover text saying what it does.
pub(crate) fn row_icon_button(ui: &egui::Ui, name: &str) -> egui::Button<'static> {
    let height = ui.spacing().interact_size.y;
    egui::Button::new(egui::RichText::new(icon(name)).size(height - 4.0)).min_size(egui::vec2(height + 6.0, height))
}

/// The room a `row_icon_button` takes in a row, with the gap before it:
/// what to leave for one after a setting's control (`control_width`).
pub(crate) fn row_icon_button_room(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y + 6.0 + ui.spacing().item_spacing.x
}

/// How wide the transport (`App::transport_ui`) is: five icon buttons and
/// play, with the gaps between.
const TRANSPORT_WIDTH: f32 = 5.0 * 28.0 + PLAY_WIDTH + 5.0 * 8.0;
/// How wide each side of it has to be for the bar to fit on one line.
const SIDE_WIDTH: f32 = 380.0;
/// How wide the devices button (`App::devices_button_ui`) is.
const DEVICES_WIDTH: f32 = 28.0;
const PLAY_WIDTH: f32 = 44.0;
/// The Soundfonts list's Size column's own width.
const SOUNDFONT_SIZE_WIDTH: f32 = 72.0;
const TRANSPORT_HEIGHT: f32 = 28.0;

/// The handle on the top edge of a section without a tab bar (in the
/// margin over `ui`, its body), where it's dragged from: a short line,
/// brighter under the pointer.
fn drag_handle(ui: &egui::Ui) {
    let content = ui.max_rect();
    let margin = crate::theme::DOCK_HANDLE;
    let strip = egui::Rect::from_min_max(egui::pos2(content.left(), content.top() - margin), egui::pos2(content.right(), content.top()));
    let weak = ui.visuals().weak_text_color();
    let color = if ui.rect_contains_pointer(strip) { weak } else { weak.gamma_multiply(0.4) };
    let line = egui::Rect::from_center_size(strip.center(), egui::vec2(32.0, 3.0));
    ui.ctx().layer_painter(ui.layer_id()).rect_filled(line, 1.5, color);
}

/// Square off the top left corner of the body around `ui` (its top margin
/// `top` tall), under its tab.
fn square_corner(ui: &egui::Ui, top: f32) {
    let content = ui.max_rect();
    let corner = egui::pos2(content.left() - f32::from(crate::theme::DOCK_MARGIN), content.top() - top);
    let patch = egui::Rect::from_min_size(corner, egui::Vec2::splat(8.0));
    ui.ctx().layer_painter(ui.layer_id()).rect_filled(patch, 0.0, ui.visuals().window_fill);
}

/// The play/pause button: Play while `paused`, else Pause in the look of
/// the playing row (`select_list::paint_row`): tinted and outlined in the
/// theme's playing color.
pub(super) fn play_button(ctx: &egui::Context, paused: bool, icon_size: f32, size: egui::Vec2) -> egui::Button<'static> {
    if paused {
        return egui::Button::new(egui::RichText::new(icon("play")).size(icon_size)).min_size(size);
    }
    let color = crate::theme::playing_color(ctx);
    egui::Button::new(egui::RichText::new(icon("pause")).size(icon_size).color(color))
        .min_size(size)
        .fill(color.gamma_multiply(0.15))
        .stroke(egui::Stroke::new(1.0, color))
}

/// What Loop is set to, for the toast its button shows.
fn loop_text(mode: LoopMode) -> &'static str {
    match mode {
        LoopMode::Off => "Loop: off",
        LoopMode::All => "Loop: all",
        LoopMode::One => "Loop: this track",
    }
}

fn shuffle_text(on: bool) -> &'static str {
    if on { "Shuffle: on" } else { "Shuffle: off" }
}

/// As `icon_button`, its icon in `color`.
fn colored_icon_button(name: &str, color: egui::Color32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(icon(name)).size(16.0).color(color)).min_size(egui::vec2(28.0, 24.0))
}

/// The seek bar over a song `length` seconds long, in `accent` (the
/// tempo's color) when there is one.
fn seek_bar_ui(ui: &mut egui::Ui, position: &mut f64, length: f64, accent: Option<egui::Color32>) -> egui::Response {
    ui.scope(|ui| {
        let rail_radius = (ui.spacing().slider_rail_height / 2.0) as u8;
        let visuals = ui.visuals_mut();
        // egui runs the filled part past the handle by the rail's corner
        // radius, which a very round theme makes far longer than the rail
        // can be round: no rounder than the rail.
        let corners = &mut visuals.widgets.inactive.corner_radius;
        *corners = egui::CornerRadius::same(corners.nw.min(rail_radius));
        if let Some(accent) = accent {
            visuals.selection.bg_fill = accent;
            // (The handle ringed in black or white, whichever stands out on it.)
            for state in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
                state.bg_fill = accent;
                state.fg_stroke = egui::Stroke::new(1.5, on_color(accent));
            }
        }
        ui.add(egui::Slider::new(position, 0.0..=length.max(0.001)).show_value(false).trailing_fill(true))
    })
    .inner
}

/// The accent color for a tempo: slow songs blue, then cyan, green, yellow
/// and coral, fast ones pink, blended between (palette colors).
fn tempo_color(bpm: f64) -> [f32; 3] {
    const STOPS: [(f64, u8); 6] = [(60.0, 0), (90.0, 5), (115.0, 2), (135.0, 3), (160.0, 1), (190.0, 6)];
    let rgb = |i: u8| lua_visualizer::default_channel_color(i).map(f32::from);
    let Some(after) = STOPS.iter().position(|&(at, _)| bpm < at) else { return rgb(STOPS[STOPS.len() - 1].1) };
    if after == 0 {
        return rgb(STOPS[0].1);
    }
    let ((a, from), (b, to)) = (STOPS[after - 1], STOPS[after]);
    let t = ((bpm - a) / (b - a)) as f32;
    let (from, to) = (rgb(from), rgb(to));
    [0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * t)
}

/// Black or white, whichever reads better on `color` (by its relative
/// luminance): a check mark on a channel's color, a chip's text.
pub(super) fn on_color(color: egui::Color32) -> egui::Color32 {
    let [r, g, b, _] = color.to_array();
    let light = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b) > 140.0;
    if light { egui::Color32::BLACK } else { egui::Color32::WHITE }
}

/// A chip: `text` on a pill of `color`, black or white (`on_color`).
pub(super) fn chip(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, on_color(color));
    let padding = egui::vec2(7.0, 1.0);
    let (rect, response) = ui.allocate_exact_size(galley.size() + padding * 2.0, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, rect.height() / 2.0, color);
        ui.painter().galley(rect.min + padding, galley, on_color(color));
    }
    response
}

/// Open the folder `dir` in the system file manager, making it first.
fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(opener).arg(dir).spawn() {
        log::warn!("couldn't open the file manager: {e}");
    }
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
    egui::Area::new(egui::Id::new("drag_ghost"))
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

/// The changelog's text: what's new since a version, or all of it. On a
/// page it fills what's left, with "Full changelog" above it rather than
/// beside a Close button; whether that was tapped.
fn changelog_body_ui(ui: &mut egui::Ui, since: Option<changelog::Version>, page: bool) -> bool {
    let mut show_all = false;
    if let Some(since) = since {
        ui.weak(format!(
            "synththing v{}, since v{} (the version you had)",
            crate::build_info::VERSION,
            changelog::format_version(since)
        ));
        if page && ui.button("Full changelog").clicked() {
            show_all = true;
        }
    }
    ui.add_space(6.0);
    let releases: Vec<&changelog::Release> = match since {
        Some(since) => changelog::newer_than(since).collect(),
        None => changelog::releases().iter().collect(),
    };
    egui::ScrollArea::vertical()
        .id_salt("changelog_scroll")
        .max_height(if page { f32::INFINITY } else { 440.0 })
        .auto_shrink([!page, !page])
        .show(ui, |ui| {
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
    show_all
}

/// Where the log file is.
fn log_file_note_ui(ui: &mut egui::Ui) {
    if let Some(path) = applog::file_path() {
        ui.weak(format!("Also saved to {} (the previous run's is synththing.prev.log)", path.display()));
    }
}

/// The log's buttons: copy what's shown, open or share the file, clear.
fn log_actions_ui(ui: &mut egui::Ui, shown: &[&applog::Entry]) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Copy shown").clicked() {
            let text: Vec<String> = shown.iter().map(|e| e.line()).collect();
            ui.ctx().copy_text(text.join("\n"));
        }
        // (Android has no file manager to open: the log is shared, to
        // whatever app the user picks, instead.)
        #[cfg(not(target_os = "android"))]
        if let Some(path) = applog::file_path()
            && ui.button("Open log folder").clicked()
        {
            open_in_file_manager(&path);
        }
        #[cfg(target_os = "android")]
        if let Some(path) = applog::file_path()
            && ui.button("Share log").clicked()
        {
            crate::android::share_log(&path);
        }
        if ui.button("Clear").on_hover_text("Clears this view; the log file keeps everything").clicked() {
            applog::clear();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status history opens from its button, clears the red dot, and
    /// stays open on the frames after (the dot going away mustn't lose it).
    #[test]
    fn status_history_opens_and_stays_open() {
        let ctx = egui::Context::default();
        let entries = || {
            vec![applog::Entry {
                secs: 1.0,
                level: log::Level::Info,
                target: STATUS_TARGET.into(),
                message: "Couldn't save: denied".into(),
            }]
        };
        let button = std::cell::Cell::new(egui::Rect::NOTHING);
        let frame = |events: Vec<egui::Event>, unseen: &mut bool| {
            let input = egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                ui.horizontal(|ui| {
                    let start = ui.cursor().min;
                    status_line_ui(ui, "Couldn't save: denied", unseen, || 125.0, entries);
                    button.set(egui::Rect::from_min_size(start, egui::vec2(12.0, 12.0)));
                });
            });
            output.textures_delta.clear(); // (no renderer here to hand them to)
            egui::Popup::is_any_open(&ctx)
        };
        let mut unseen = true;
        assert!(!frame(vec![], &mut unseen));
        let at = button.get().center();
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(at), press(true)], &mut unseen);
        frame(vec![press(false)], &mut unseen);
        let mut open = 0;
        for _ in 0..5 {
            open += usize::from(frame(vec![], &mut unseen));
        }
        assert_eq!(open, 5, "open, and still open");
        assert!(!unseen, "seen");
    }

    /// Slow songs blue, fast ones pink, blended between; out of range,
    /// the nearest end.
    #[test]
    fn tempos_have_colors() {
        let rgb = |i: u8| lua_visualizer::default_channel_color(i).map(f32::from);
        assert_eq!(tempo_color(40.0), rgb(0));
        assert_eq!(tempo_color(60.0), rgb(0));
        assert_eq!(tempo_color(115.0), rgb(2));
        assert_eq!(tempo_color(250.0), rgb(6));
        let between = tempo_color(102.5);
        let (cyan, green) = (rgb(5), rgb(2));
        for i in 0..3 {
            assert!((between[i] - (cyan[i] + green[i]) / 2.0).abs() < 0.01);
        }
    }

    #[test]
    fn statuses_are_sorted_by_wording() {
        assert_eq!(status_outcome("Couldn't save preferences: denied"), Some(false));
        assert_eq!(status_outcome("Failed to create script: exists"), Some(false));
        assert_eq!(status_outcome("Soundfont not loaded"), Some(false));
        assert_eq!(status_outcome("Published \"Disco\" to synththing.p51.nl."), Some(true));
        assert_eq!(status_outcome("Soundfont loaded: GeneralUser-GS"), Some(true));
        assert_eq!(status_outcome("Click a song to play it, a soundfont to load it."), None);
        assert_eq!(status_outcome("Loading Canon in D..."), None);
    }
}
